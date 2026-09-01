use clap::Parser;
use std::error::Error;

mod cli;
mod sanitize;
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