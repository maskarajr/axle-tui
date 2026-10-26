use crate::sanitize::sanitize_terminal_text;
use crate::session::{self, Job, QueueItem, Session};
use crate::wua::{
    DriverInstallStatus, DriverScanResult, InstallEvent, InstallTarget, SelectableDriver,
    StagedDriverEntry, fetch_driver_state_blocking, stream_install_selected,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{error::Error, io, sync::mpsc, thread, time::Duration};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

enum AppState {
    Scanning,
    Browsing,
    Installing,
    Finished { reboot_required: bool, clean: bool },
}

thread_local! {
    static OPS_LOG_HELD: Cell<bool> = const { Cell::new(false) };
}

static OPS_LOG: Mutex<()> = Mutex::new(());

fn one_log_field(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn format_op_line(event: &InstallEvent) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match event {
        InstallEvent::ItemDownloading { index, title } => {
            format!(
                "{ts} ItemDownloading index={index} title={}",
                one_log_field(title)
            )
        }
        InstallEvent::ItemInstalling { index, title } => {
            format!(
                "{ts} ItemInstalling index={index} title={}",
                one_log_field(title)
            )
        }
        InstallEvent::ItemFinished {
            index,
            title,
            success,
            message,
            ..
        } => format!(
            "{ts} ItemFinished index={index} title={} success={success} message={}",
            one_log_field(title),
            one_log_field(message)
        ),
        InstallEvent::AllFinished { reboot_required } => format!(
            "{ts} AllFinished index=- title=- success=- message=reboot_required={reboot_required}"
        ),
        InstallEvent::FatalError { error } => format!(
            "{ts} FatalError index=- title=- success=false message={}",
            one_log_field(error)
        ),
    }
}

fn ops_log_dir() -> Option<PathBuf> {
    match std::env::var("AXLE_LOG_DIR") {
        Ok(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        Ok(_) => None,
        Err(_) => {
            #[cfg(test)]
            {
                None
            }
            #[cfg(not(test))]
            {
                let base = std::env::var_os("LOCALAPPDATA")?;
                Some(PathBuf::from(base).join("axle-tui"))
            }
        }
    }
}

fn record_install_event(event: &InstallEvent) {
    if OPS_LOG_HELD.with(Cell::get) {
        write_install_event(event);
        return;
    }
    let _guard = OPS_LOG.lock().unwrap_or_else(|err| err.into_inner());
    write_install_event(event);
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_symlink())
}

fn write_install_event(event: &InstallEvent) {
    let Some(dir) = ops_log_dir() else {
        return;
    };
    if is_symlink(&dir) {
        return;
    }
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("ops.log");
    if is_symlink(&path) {
        return;
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(file, "{}", format_op_line(event));
    let _ = file.flush();
}

fn apply_install_event(
    queue: &mut Vec<QueueItem>,
    drivers: &mut Vec<SelectableDriver>,
    staged_drivers: &mut Vec<StagedDriverEntry>,
    list_state: &mut ListState,
    state: &mut AppState,
    status_msg: &mut String,
    event: InstallEvent,
) {
    record_install_event(&event);
    let next = session::apply(
        &Session {
            queue: queue.clone(),
            staged: staged_drivers.clone(),
            status_text: status_msg.clone(),
            finish: None,
        },
        &event,
    );
    *queue = next.queue;
    *staged_drivers = next.staged;
    *status_msg = next.status_text;
    if let Some(finish) = next.finish {
        *state = AppState::Finished {
            reboot_required: finish.reboot_required,
            clean: finish.clean,
        };
    }
    if let InstallEvent::AllFinished { .. } = event {
        let installed_ids: Vec<String> = queue
            .iter()
            .filter(|item| item.status == DriverInstallStatus::Success)
            .map(|item| item.update_id.clone())
            .collect();
        drivers.retain(|driver| !installed_ids.contains(&driver.entry.update_id));
        if drivers.is_empty() {
            list_state.select(None);
        } else {
            list_state.select(Some(0));
        }
    }
}

pub async fn run_tui() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut list_state = ListState::default();
    let mut drivers: Vec<SelectableDriver> = Vec::new();
    let mut staged_drivers: Vec<StagedDriverEntry> = Vec::new();
    let mut queue: Vec<QueueItem> = Vec::new();
    let mut state = AppState::Scanning;
    let mut spinner_idx = 0;
    let mut status_msg = String::from("Connecting to Windows Update COM Agent...");

    let (scan_tx, mut scan_rx) = mpsc::channel::<DriverScanResult>();
    let (event_tx, event_rx) = mpsc::channel::<InstallEvent>();

    // Initial unified scan
    let init_tx = scan_tx.clone();
    thread::spawn(move || {
        let scan_result = fetch_driver_state_blocking();
        let _ = init_tx.send(scan_result);
    });

    loop {
        // Poll Background Scanner Results
        if let Ok(scan_result) = scan_rx.try_recv() {
            staged_drivers = scan_result.staged;
            drivers = scan_result
                .pending
                .into_iter()
                .map(|entry| SelectableDriver {
                    entry,
                    selected: false,
                })
                .collect();

            if !drivers.is_empty() {
                list_state.select(Some(0));
            } else {
                list_state.select(None);
            }

            status_msg = format!(
                "Scan complete: {} update(s) available, {} pending reboot.",
                drivers.len(),
                staged_drivers.len()
            );
            state = AppState::Browsing;
        }

        // Poll Live Installation Events
        while let Ok(event) = event_rx.try_recv() {
            apply_install_event(
                &mut queue,
                &mut drivers,
                &mut staged_drivers,
                &mut list_state,
                &mut state,
                &mut status_msg,
                event,
            );
        }

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(6),
                    Constraint::Length(7),
                    Constraint::Length(4),
                ])
                .split(f.area());

            // 1. Header
            let (spin_char, header_color, title_text) = match &state {
                AppState::Scanning => (
                    SPINNER[spinner_idx],
                    Color::Yellow,
                    "Windows Driver Manager - Scanning System...",
                ),
                AppState::Browsing => ("●", Color::Cyan, "Windows Driver Manager (WUA)"),
                AppState::Installing => (
                    SPINNER[spinner_idx],
                    Color::Magenta,
                    "Windows Driver Manager - Installing (Locked)...",
                ),
                AppState::Finished { clean, .. } => {
                    if *clean {
                        ("✔", Color::Green, "Windows Driver Manager - Completed")
                    } else {
                        (
                            "✖",
                            Color::Red,
                            "Windows Driver Manager - Install did not finish cleanly",
                        )
                    }
                }
            };

            let header = Paragraph::new(format!(" {} {}", spin_char, title_text))
                .style(
                    Style::default()
                        .fg(header_color)
                        .add_modifier(Modifier::BOLD),
                )
                .block(Block::default().borders(Borders::ALL).title(" Session Status "));
            f.render_widget(header, chunks[0]);

            // 2. Upper Split: Available Drivers (60%) & Metadata (40%)
            let top_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(chunks[1]);

            if let AppState::Scanning = state {
                let scanning_box = Paragraph::new(format!(
                    "\n  {} Querying Microsoft Catalog for available driver updates...",
                    SPINNER[spinner_idx]
                ))
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Available Driver Updates "),
                );
                f.render_widget(scanning_box, top_chunks[0]);
            } else if drivers.is_empty() {
                let empty_box = Paragraph::new("\n  No pending driver updates found. Everything is up to date.")
                    .style(Style::default().fg(Color::Green))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Available Driver Updates "),
                    );
                f.render_widget(empty_box, top_chunks[0]);
            } else {
                let items: Vec<ListItem> = drivers
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let mark = if item.selected { "[x]" } else { "[ ]" };
                        let style = if list_state.selected() == Some(i) {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else if item.selected {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default()
                        };
                        ListItem::new(format!(
                            " {} {}",
                            mark,
                            sanitize_terminal_text(&item.entry.title)
                        ))
                        .style(style)
                    })
                    .collect();

                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Available Driver Updates "),
                    )
                    .highlight_symbol(">> ");
                f.render_stateful_widget(list, top_chunks[0], &mut list_state);
            }

            // Driver Details Panel
            let detail_text = if let Some(idx) = list_state.selected() {
                if let Some(item) = drivers.get(idx) {
                    let title = sanitize_terminal_text(&item.entry.title);
                    let provider = sanitize_terminal_text(
                        item.entry.driver_provider.as_deref().unwrap_or("Microsoft / OEM"),
                    );
                    let driver_class = sanitize_terminal_text(
                        item.entry.driver_class.as_deref().unwrap_or("Hardware Extension"),
                    );
                    vec![
                        Line::from(vec![
                            Span::styled("Title: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                            Span::raw(title),
                        ]),
                        Line::from(""),
                        Line::from(vec![
                            Span::styled("Provider: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                            Span::raw(provider),
                        ]),
                        Line::from(vec![
                            Span::styled("Class: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                            Span::raw(driver_class),
                        ]),
                        Line::from(vec![
                            Span::styled("Payload: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                            Span::raw(format!("{} KB", item.entry.max_download_size.unwrap_or(0) / 1024)),
                        ]),
                    ]
                } else {
                    vec![Line::from("No driver selected.")]
                }
            } else {
                vec![Line::from("No driver selected.")]
            };

            let detail_panel = Paragraph::new(detail_text)
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title(" Driver Details "));
            f.render_widget(detail_panel, top_chunks[1]);

            // 3. Middle Section: Dedicated Staged Panel
            if staged_drivers.is_empty() {
                let empty_staged = Paragraph::new("  No staged drivers pending system restart.")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Staged / Reboot Pending Drivers (Read-Only) "),
                    );
                f.render_widget(empty_staged, chunks[2]);
            } else {
                let staged_items: Vec<ListItem> = staged_drivers
                    .iter()
                    .map(|s| {
                        let safe_title = sanitize_terminal_text(&s.title);
                        let badge = Span::styled("[REBOOT PENDING] ", Style::default().fg(Color::LightYellow).add_modifier(Modifier::BOLD));
                        let title = Span::styled(safe_title, Style::default().fg(Color::LightYellow));
                        let hint = Span::styled(" • Awaiting system reboot to bind", Style::default().fg(Color::DarkGray));
                        ListItem::new(Line::from(vec![badge, title, hint]))
                    })
                    .collect();

                let staged_list = List::new(staged_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Staged / Reboot Pending Drivers (Read-Only) "),
                    );
                f.render_widget(staged_list, chunks[2]);
            }

            // 4. Lower Section: Queue & Live Progress
            let queue_box_title = match &state {
                AppState::Finished {
                    reboot_required,
                    clean,
                } => {
                    if !*clean {
                        " Installation Summary (Incomplete) "
                    } else if *reboot_required {
                        " Installation Summary (Reboot Required ⚠) "
                    } else {
                        " Installation Summary (Complete ✔) "
                    }
                }
                AppState::Scanning | AppState::Browsing | AppState::Installing => {
                    " Installation Queue & Live Progress "
                }
            };

            if queue.is_empty() {
                let empty_queue = Paragraph::new("  Queue is empty. Select drivers above with [Space] and press [Enter] to install.")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(Block::default().borders(Borders::ALL).title(queue_box_title));
                f.render_widget(empty_queue, chunks[3]);
            } else {
                let current_spin = SPINNER[spinner_idx];
                let queue_items: Vec<ListItem> = queue
                    .iter()
                    .map(|q| {
                        let safe_title = sanitize_terminal_text(&q.title);
                        let (badge, inline_progress, style) = match &q.status {
                            DriverInstallStatus::Queued => (
                                Span::styled("[QUEUED]     ", Style::default().fg(Color::DarkGray)),
                                Span::styled(" • Pending...", Style::default().fg(Color::DarkGray)),
                                Style::default().fg(Color::DarkGray),
                            ),
                            DriverInstallStatus::Downloading => (
                                Span::styled("[DOWNLOADING]", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                                Span::styled(format!(" {} Downloading payload...", current_spin), Style::default().fg(Color::Yellow)),
                                Style::default().fg(Color::White),
                            ),
                            DriverInstallStatus::Installing => (
                                Span::styled("[INSTALLING] ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                                Span::styled(format!(" {} Staging and applying...", current_spin), Style::default().fg(Color::Magenta)),
                                Style::default().fg(Color::White),
                            ),
                            DriverInstallStatus::Success => (
                                Span::styled("[SUCCESS ✔]  ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                                Span::styled(
                                    format!(
                                        " • {}",
                                        if q.message.is_empty() {
                                            "Installed successfully".into()
                                        } else {
                                            sanitize_terminal_text(&q.message)
                                        }
                                    ),
                                    Style::default().fg(Color::Green),
                                ),
                                Style::default().fg(Color::Green),
                            ),
                            DriverInstallStatus::Failed(_) => (
                                Span::styled("[FAILED ✖]   ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                                Span::styled(
                                    format!(" • {}", sanitize_terminal_text(&q.message)),
                                    Style::default().fg(Color::Red),
                                ),
                                Style::default().fg(Color::Red),
                            ),
                        };

                        let title_span = Span::styled(format!(" {} ", safe_title), style);
                        ListItem::new(Line::from(vec![badge, title_span, inline_progress]))
                    })
                    .collect();

                let queue_list = List::new(queue_items)
                    .block(Block::default().borders(Borders::ALL).title(queue_box_title));
                f.render_widget(queue_list, chunks[3]);
            }

            // 5. Footer
            if let AppState::Installing = state {
                let total = queue.len();
                let completed = queue
                    .iter()
                    .filter(|i| matches!(i.status, DriverInstallStatus::Success | DriverInstallStatus::Failed(_)))
                    .count();
                let in_progress_weight = queue.iter().any(|i| {
                    matches!(i.status, DriverInstallStatus::Downloading | DriverInstallStatus::Installing)
                }) as usize;

                let percent = if total > 0 {
                    (((completed * 100) + (in_progress_weight * 50)) / total).min(100) as u16
                } else {
                    0
                };

                let gauge = Gauge::default()
                    .block(Block::default().borders(Borders::ALL).title(" Installing (Inputs Locked) "))
                    .gauge_style(Style::default().fg(Color::Magenta))
                    .percent(percent)
                    .label(format!("{} / {} Processed ({}%)", completed, total, percent));
                f.render_widget(gauge, chunks[4]);
            } else {
                let nav_guide = match state {
                    AppState::Browsing => "[Space] Toggle  •  [a] Select All  •  [Enter] Install  •  [r] Refresh  •  [q] Exit",
                    AppState::Finished { .. } => "[r] Refresh Scan  •  [q] Exit",
                    _ => "[q] Exit",
                };

                let safe_status = sanitize_terminal_text(&status_msg);
                let footer_lines = vec![
                    Line::from(vec![
                        Span::styled(" ● Status: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        Span::styled(safe_status, Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled(" ► Navigation: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                        Span::styled(nav_guide, Style::default().fg(Color::DarkGray)),
                    ]),
                ];

                let footer = Paragraph::new(footer_lines).block(Block::default().borders(Borders::ALL));
                f.render_widget(footer, chunks[4]);
            }
        })?;

        // Keystroke Handling
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if let AppState::Installing = state {
                        match key.code {
                            KeyCode::Down | KeyCode::Char('j') => {
                                if !drivers.is_empty() {
                                    let i = match list_state.selected() {
                                        Some(i) => {
                                            if i >= drivers.len() - 1 {
                                                0
                                            } else {
                                                i + 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    list_state.select(Some(i));
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if !drivers.is_empty() {
                                    let i = match list_state.selected() {
                                        Some(i) => {
                                            if i == 0 {
                                                drivers.len() - 1
                                            } else {
                                                i - 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    list_state.select(Some(i));
                                }
                            }
                            _ => {}
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Down | KeyCode::Char('j') => {
                                if !drivers.is_empty() {
                                    let i = match list_state.selected() {
                                        Some(i) => {
                                            if i >= drivers.len() - 1 {
                                                0
                                            } else {
                                                i + 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    list_state.select(Some(i));
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if !drivers.is_empty() {
                                    let i = match list_state.selected() {
                                        Some(i) => {
                                            if i == 0 {
                                                drivers.len() - 1
                                            } else {
                                                i - 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    list_state.select(Some(i));
                                }
                            }
                            KeyCode::Char(' ') => {
                                if matches!(state, AppState::Browsing) {
                                    if let Some(i) = list_state.selected() {
                                        if let Some(item) = drivers.get_mut(i) {
                                            item.selected = !item.selected;
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('a') => {
                                if matches!(state, AppState::Browsing) {
                                    let all_selected = drivers.iter().all(|d| d.selected);
                                    for d in &mut drivers {
                                        d.selected = !all_selected;
                                    }
                                }
                            }
                            KeyCode::Char('r') => {
                                if !matches!(state, AppState::Installing) {
                                    state = AppState::Scanning;
                                    status_msg =
                                        String::from("Refreshing Microsoft Catalog scan...");
                                    queue.clear();

                                    let (new_tx, new_rx) = mpsc::channel();
                                    scan_rx = new_rx;

                                    thread::spawn(move || {
                                        let scan_result = fetch_driver_state_blocking();
                                        let _ = new_tx.send(scan_result);
                                    });
                                }
                            }
                            KeyCode::Enter => {
                                if matches!(state, AppState::Browsing) {
                                    let selected: Vec<Job> = drivers
                                        .iter()
                                        .filter(|driver| driver.selected)
                                        .map(|driver| Job {
                                            update_id: driver.entry.update_id.clone(),
                                            title: driver.entry.title.clone(),
                                            max_download_size: driver.entry.max_download_size,
                                            driver_class: driver.entry.driver_class.clone(),
                                            superseded_update_ids: driver
                                                .entry
                                                .superseded_update_ids
                                                .clone(),
                                        })
                                        .collect();
                                    let jobs = session::plan(&selected);

                                    if !jobs.is_empty() {
                                        queue = jobs
                                            .iter()
                                            .map(|job| QueueItem {
                                                update_id: job.update_id.clone(),
                                                title: job.title.clone(),
                                                status: DriverInstallStatus::Queued,
                                                message: String::new(),
                                            })
                                            .collect();
                                        let selected_targets: Vec<InstallTarget> = jobs
                                            .iter()
                                            .map(|job| InstallTarget {
                                                update_id: job.update_id.clone(),
                                                title: job.title.clone(),
                                                max_download_size: job.max_download_size,
                                                driver_class: job.driver_class.clone(),
                                            })
                                            .collect();

                                        state = AppState::Installing;
                                        let etx = event_tx.clone();
                                        thread::spawn(move || {
                                            stream_install_selected(selected_targets, etx);
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        if matches!(state, AppState::Scanning | AppState::Installing) {
            spinner_idx = (spinner_idx + 1) % SPINNER.len();
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::symlink_file;

    fn queued_item(title: &str) -> QueueItem {
        QueueItem {
            update_id: format!("id:{title}"),
            title: title.to_string(),
            status: DriverInstallStatus::Queued,
            message: String::new(),
        }
    }

    fn apply_events(
        queue: &mut Vec<QueueItem>,
        staged: &mut Vec<StagedDriverEntry>,
        events: &[InstallEvent],
    ) -> (AppState, String) {
        let mut drivers = Vec::new();
        let mut list_state = ListState::default();
        let mut state = AppState::Installing;
        let mut status_msg = String::from("installing");
        for event in events {
            apply_install_event(
                queue,
                &mut drivers,
                staged,
                &mut list_state,
                &mut state,
                &mut status_msg,
                event.clone(),
            );
        }
        (state, status_msg)
    }

    #[test]
    fn applied_event_appends_one_ops_log_line() {
        let _guard = OPS_LOG.lock().unwrap_or_else(|err| err.into_inner());
        OPS_LOG_HELD.with(|held| held.set(true));
        let dir = std::env::temp_dir().join(format!("axle-tui-ops-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        unsafe {
            std::env::set_var("AXLE_LOG_DIR", &dir);
        }
        let mut queue = vec![queued_item("tiny update")];
        let mut staged = Vec::new();
        let _ = apply_events(
            &mut queue,
            &mut staged,
            &[InstallEvent::ItemDownloading {
                index: 0,
                title: "tiny update".to_string(),
            }],
        );
        let text = fs::read_to_string(dir.join("ops.log")).expect("ops log");
        let lines: Vec<_> = text.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("ItemDownloading"));
        assert!(lines[0].contains("index=0"));
        assert!(lines[0].contains("tiny update"));
        unsafe {
            std::env::remove_var("AXLE_LOG_DIR");
        }
        OPS_LOG_HELD.with(|held| held.set(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_symlink_ops_log() {
        let _guard = OPS_LOG.lock().unwrap_or_else(|err| err.into_inner());
        let dir = std::env::temp_dir().join(format!("axle-tui-ops-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("log dir");
        let link = dir.join("ops.log");
        let target = dir.join("secret.log");
        match symlink_file(&target, &link) {
            Err(err)
                if err.kind() == std::io::ErrorKind::PermissionDenied
                    || err.raw_os_error() == Some(1314) =>
            {
                let _ = fs::remove_dir_all(&dir);
                return;
            }
            Err(err) => panic!("create ops.log symlink: {err}"),
            Ok(()) => {}
        }
        unsafe {
            std::env::set_var("AXLE_LOG_DIR", &dir);
        }
        write_install_event(&InstallEvent::ItemDownloading {
            index: 0,
            title: "tiny update".to_string(),
        });
        let wrote_target = fs::read_to_string(&target)
            .unwrap_or_default()
            .contains("ItemDownloading");
        assert!(!wrote_target);
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        unsafe {
            std::env::remove_var("AXLE_LOG_DIR");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
