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
        println!("{:<4} {:<55} {:<15} {:<10}", "#", "Title", "Provider", "Size");
        println!("{}", "-".repeat(88));
        for (i, d) in scan_result.pending.iter().enumerate() {
            let size = format!("{} KB", d.max_download_size.unwrap_or(0) / 1024);
            let safe_title = sanitize_terminal_text(&d.title);
            let title = if safe_title.len() > 52 {
                format!("{}...", &safe_title[..50])
            } else {
                safe_title
            };
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
        println!("Staged / Reboot Pending Drivers ({}):", scan_result.staged.len());
        println!("{:<4} {:<55} {:<25}", "#", "Title", "Status");
        println!("{}", "-".repeat(88));
        for (i, s) in scan_result.staged.iter().enumerate() {
            let safe_title = sanitize_terminal_text(&s.title);
            let title = if safe_title.len() > 52 {
                format!("{}...", &safe_title[..50])
            } else {
                safe_title
            };
            println!("{:<4} {:<55} {:<25}", i + 1, title, "Awaiting Reboot");
        }
    }

    Ok(())
}