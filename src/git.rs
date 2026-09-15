//! Thin wrapper around the `git` CLI. We shell out rather than link libgit2 so
//! the tool has no native build dependencies beyond a `git` binary on PATH.

use anyhow::{bail, Context, Result};
use std::process::Command;

/// A single file changed between the two refs.
#[derive(Clone, Debug)]
pub struct ChangedFile {
    /// Git status letter: A(dded), M(odified), D(eleted), R(enamed), C(opied)...
    pub status: char,
    /// Path of the file on the head side (new name for renames).
    pub path: String,
    /// Original path when the file was renamed/copied.
    pub old_path: Option<String>,
}

fn run_git(args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .output()
        .context("failed to spawn `git` — is it installed and on PATH?")
}

/// Error out early if we are not inside a git work tree.
pub fn verify_repo() -> Result<()> {
    let out = run_git(&["rev-parse", "--is-inside-work-tree"])?;
    if !out.status.success() {
        bail!("not inside a git repository");
    }
    Ok(())
}

/// Error out early if a ref cannot be resolved.
pub fn verify_ref(r: &str) -> Result<()> {
    let out = run_git(&["rev-parse", "--verify", "--quiet", r])?;
    if !out.status.success() {
        bail!("unknown git ref: {r}");
    }
    Ok(())
}

/// List files that differ between `base` and `head` using PR semantics
/// (three-dot: everything on `head` since it diverged from `base`).
pub fn changed_files(base: &str, head: &str) -> Result<Vec<ChangedFile>> {
    let spec = format!("{base}...{head}");
    let out = run_git(&["diff", "--name-status", "-M", &spec])?;
    if !out.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut files = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let status_field = parts.next().unwrap_or("");
        let status = status_field.chars().next().unwrap_or('?');
        if status == 'R' || status == 'C' {
            let old = parts.next().unwrap_or("").to_string();
            let new = parts.next().unwrap_or("").to_string();
            if new.is_empty() {
                continue;
            }
            files.push(ChangedFile {
                status,
                path: new,
                old_path: Some(old),
            });
        } else {
            let path = parts.next().unwrap_or("").to_string();
            if path.is_empty() {
                continue;
            }
            files.push(ChangedFile {
                status,
                path,
                old_path: None,
            });
        }
    }
    Ok(files)
}

/// Name of the currently checked-out branch, or `HEAD` when detached.
pub fn current_branch() -> Result<String> {
    let out = run_git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    if !out.status.success() {
        bail!(
            "could not determine current branch: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Contents of a file at a given revision (`git show <rev>:<path>`).
pub fn read_file_at(rev: &str, path: &str) -> Result<String> {
    let spec = format!("{rev}:{path}");
    let out = run_git(&["show", &spec])?;
    if !out.status.success() {
        bail!(
            "git show failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Raw unified diff text for a single changed file.
pub fn file_diff(base: &str, head: &str, file: &ChangedFile) -> Result<String> {
    let spec = format!("{base}...{head}");
    let mut args: Vec<String> = vec![
        "diff".into(),
        "-M".into(),
        "--no-color".into(),
        spec,
        "--".into(),
    ];
    if let Some(old) = &file.old_path {
        args.push(old.clone());
    }
    args.push(file.path.clone());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = run_git(&arg_refs)?;
    if !out.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
