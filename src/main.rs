mod app;
mod diff;
mod git;
mod highlight;
mod pty;
mod tree;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use std::io::stdout;

/// TUI to explore the diff between your working tree (the current folder) and a
/// git branch, GitHub-PR style.
#[derive(Parser)]
#[command(name = "branchdiff", version, about, long_about = None)]
struct Cli {
    /// Branch (or any ref) to diff the current folder against, e.g. `main`.
    branch: String,
    /// Run against a repository at this path instead of the current directory.
    #[arg(short = 'C', long = "repo", default_value = ".")]
    repo: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    std::env::set_current_dir(&cli.repo)
        .with_context(|| format!("cannot enter repo path: {}", cli.repo))?;

    git::verify_repo()?;

    let branch = cli.branch;
    git::verify_ref(&branch)?;

    // Open the app even when there are no differences: an empty file list still
    // renders (empty tree + an explanatory message in the diff panel).
    let files = git::changed_files(&branch)?;

    let mut app = app::App::new(branch, files);

    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableMouseCapture);
    let result = app.run(&mut terminal);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
