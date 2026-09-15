mod app;
mod diff;
mod editor;
mod git;
mod highlight;
mod tree;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use std::io::stdout;

/// TUI to explore the diff between two git branches, GitHub-PR style.
#[derive(Parser)]
#[command(name = "branchdiff", version, about, long_about = None)]
struct Cli {
    /// Base ref — the branch you'd merge into (e.g. `main`).
    base: String,
    /// Head ref — the branch with your changes. Defaults to the current branch
    /// when omitted.
    head: Option<String>,
    /// Run against a repository at this path instead of the current directory.
    #[arg(short = 'C', long = "repo", default_value = ".")]
    repo: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    std::env::set_current_dir(&cli.repo)
        .with_context(|| format!("cannot enter repo path: {}", cli.repo))?;

    git::verify_repo()?;

    // With a single argument, diff the given ref against the current branch.
    let base = cli.base;
    let head = match cli.head {
        Some(h) => h,
        None => git::current_branch()?,
    };

    git::verify_ref(&base)?;
    git::verify_ref(&head)?;

    let files = git::changed_files(&base, &head)?;
    if files.is_empty() {
        println!("No differences between {base} and {head}.");
        return Ok(());
    }

    let mut app = app::App::new(base, head, files);

    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableMouseCapture);
    let result = app.run(&mut terminal);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
