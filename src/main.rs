use clap::Parser;
use std::error::Error;

#[cfg(test)]
use std::io::Write;

mod cli;
mod sanitize;
mod session;
mod tui;
mod wua;

#[derive(Parser, Debug)]
#[command(name = "driver-tui", about = "TUI & CLI driver updater for Windows")]
struct CliArgs {
    /// List pending driver updates without opening the full TUI
    #[arg(short, long)]
    list: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = CliArgs::parse();

    if args.list {
        cli::run_list_mode().await?;
    } else {
        tui::run_tui().await?;
    }

    Ok(())
}

#[test]
#[ignore = "contacts Windows Update"]
fn com_scan_returns_a_result() {
    let result = wua::fetch_driver_state_com().expect("com scan");
    println!(
        "pending.len()={} staged.len()={}",
        result.pending.len(),
        result.staged.len()
    );
}

#[test]
#[ignore = "downloads and installs every pending driver"]
fn com_install_pending_drivers() {
    assert_eq!(
        std::env::var("AXLE_LIVE_INSTALL").ok().as_deref(),
        Some("1"),
        "refusing to install without AXLE_LIVE_INSTALL=1"
    );
    let scan = wua::fetch_driver_state_com().expect("com scan");
    let targets: Vec<wua::InstallTarget> = scan
        .pending
        .iter()
        .map(|entry| wua::InstallTarget {
            update_id: entry.update_id.clone(),
            title: entry.title.clone(),
            max_download_size: entry.max_download_size,
            driver_class: entry.driver_class.clone(),
        })
        .collect();
    let log_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".audit/live-e2e.log");
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("live log");
    let _ = writeln!(log, "pending {}", targets.len());
    for target in &targets {
        let _ = writeln!(log, "queue\t{}\t{}", target.update_id, target.title);
        println!("queue\t{}\t{}", target.update_id, target.title);
    }
    let _ = log.flush();
    assert!(!targets.is_empty(), "no pending drivers to install");
    let expected = targets.len();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || wua::stream_install_selected(targets, tx));
    let mut finished = 0usize;
    let mut all_finished = false;
    while let Ok(event) = rx.recv() {
        let _ = writeln!(log, "{event:?}");
        let _ = log.flush();
        println!("{event:?}");
        match &event {
            wua::InstallEvent::ItemFinished { .. } => finished += 1,
            wua::InstallEvent::AllFinished { .. } => all_finished = true,
            wua::InstallEvent::FatalError { error } => panic!("{error}"),
            _ => {}
        }
    }
    worker.join().expect("install thread");
    assert!(all_finished, "install ended without AllFinished");
    assert_eq!(finished, expected, "missing per-driver results");
}
