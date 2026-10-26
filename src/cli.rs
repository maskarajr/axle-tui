use crate::sanitize::sanitize_terminal_text;
use crate::wua::fetch_driver_state_blocking;
use indicatif::{ProgressBar, ProgressStyle};
use std::error::Error;

pub async fn run_list_mode() -> Result<(), Box<dyn Error>> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("{spinner:.cyan} {msg}")?,
    );
    pb.set_message("Scanning Microsoft Update Catalog & staged drivers...");
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let scan_result = tokio::task::spawn_blocking(fetch_driver_state_blocking).await?;
    pb.finish_and_clear();

    if scan_result.pending.is_empty() && scan_result.staged.is_empty() {
        println!("No pending driver updates found.");
        return Ok(());
    }

    if !scan_result.pending.is_empty() {
        println!("Available Driver Updates ({}):", scan_result.pending.len());
        println!(
            "{:<4} {:<55} {:<15} {:<10}",
            "#", "Title", "Provider", "Size"
        );
        println!("{}", "-".repeat(88));
        for (i, d) in scan_result.pending.iter().enumerate() {
            let size = format!("{} KB", d.max_download_size.unwrap_or(0) / 1024);
            let title = truncate_title(sanitize_terminal_text(&d.title));
            println!(
                "{:<4} {:<55} {:<15} {:<10}",
                i + 1,
                title,
                sanitize_terminal_text(d.driver_provider.as_deref().unwrap_or("Unknown")),
                size
            );
        }
        println!();
    }

    if !scan_result.staged.is_empty() {
        println!(
            "Staged / Reboot Pending Drivers ({}):",
            scan_result.staged.len()
        );
        println!("{:<4} {:<55} {:<25}", "#", "Title", "Status");
        println!("{}", "-".repeat(88));
        for (i, s) in scan_result.staged.iter().enumerate() {
            let title = truncate_title(sanitize_terminal_text(&s.title));
            println!("{:<4} {:<55} {:<25}", i + 1, title, "Awaiting Reboot");
        }
    }

    Ok(())
}

fn truncate_title(safe_title: String) -> String {
    if safe_title.len() > 52 {
        let mut end = 50;
        while !safe_title.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &safe_title[..end])
    } else {
        safe_title
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_title;

    #[test]
    fn truncates_on_char_boundary() {
        let mut title = "a".repeat(49);
        title.push('\u{00E9}');
        title.push_str("tail");
        let truncated = truncate_title(title);
        assert_eq!(truncated, format!("{}...", "a".repeat(49)));
        assert!(truncated.ends_with("..."));
        assert!(!truncated.contains('\u{00E9}'));
    }
}
