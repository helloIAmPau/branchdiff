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
    /// True for files that exist on disk but are not yet tracked by git. These
    /// need a `--no-index` diff since `git diff <branch>` ignores them.
    pub untracked: bool,
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

/// List files that differ between `branch` and the current working tree (the
/// files as they exist on disk, staged or not).
pub fn changed_files(branch: &str) -> Result<Vec<ChangedFile>> {
    let out = run_git(&["diff", "--name-status", "-M", branch])?;
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
                untracked: false,
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
                untracked: false,
            });
        }
    }

    // `git diff` only knows about tracked paths, so files that exist on disk but
    // have never been `git add`ed are invisible above. Surface them too, as
    // additions, so the working tree is shown in full.
    for path in untracked_files()? {
        files.push(ChangedFile {
            status: 'A',
            path,
            old_path: None,
            untracked: true,
        });
    }

    Ok(files)
}

/// Paths that exist in the working tree but are not tracked by git, honoring
/// `.gitignore` (so build artifacts and the like are left out).
fn untracked_files() -> Result<Vec<String>> {
    let out = run_git(&["ls-files", "--others", "--exclude-standard"])?;
    if !out.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Name of the branch currently checked out in the working tree, or a short
/// commit hash when in a detached-HEAD state.
pub fn current_branch() -> Result<String> {
    let out = run_git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    if out.status.success() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() && name != "HEAD" {
            return Ok(name);
        }
    }
    // Detached HEAD (or the rev-parse above failed): fall back to a short hash.
    let out = run_git(&["rev-parse", "--short", "HEAD"])?;
    if !out.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(format!(
        "@{}",
        String::from_utf8_lossy(&out.stdout).trim()
    ))
}

/// Raw unified diff text for a single changed file (branch vs. working tree).
pub fn file_diff(branch: &str, file: &ChangedFile) -> Result<String> {
    // Untracked files aren't in git's index, so `git diff <branch>` skips them.
    // Diff the on-disk file against /dev/null instead, so every line reads as an
    // addition.
    if file.untracked {
        let out = run_git(&["diff", "--no-color", "--no-index", "--", "/dev/null", &file.path])?;
        // `--no-index` exits 1 when the files differ (the normal case for a new
        // file) and 0 when they're identical; only anything else is an error.
        return match out.status.code() {
            Some(0) | Some(1) => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
            _ => bail!(
                "git diff failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        };
    }

    let mut args: Vec<String> = vec![
        "diff".into(),
        "-M".into(),
        "--no-color".into(),
        branch.into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;

    // The functions under test shell out to `git` in the *process* working
    // directory, which is global state. Serialize the git tests (and the cwd
    // change they need) behind one mutex so they never race each other.
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    fn git_in(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Build a repo with a `base` branch and a `head` branch that adds, modifies,
    /// deletes and renames files. Returns the repo path.
    fn make_repo() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("branchdiff_git_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        git_in(&dir, &["init", "-q"]);
        git_in(&dir, &["config", "user.email", "t@t.com"]);
        git_in(&dir, &["config", "user.name", "t"]);

        // Base branch.
        fs::write(dir.join("keep.txt"), "alpha\nbeta\ngamma\n").unwrap();
        fs::write(dir.join("del.txt"), "remove me\n").unwrap();
        fs::write(dir.join("old.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n").unwrap();
        git_in(&dir, &["add", "-A"]);
        git_in(&dir, &["commit", "-qm", "base"]);
        git_in(&dir, &["branch", "-M", "base"]);

        // Head branch: modify / delete / rename / add.
        git_in(&dir, &["checkout", "-qb", "head"]);
        fs::write(dir.join("keep.txt"), "alpha\nBETA\ngamma\n").unwrap();
        fs::remove_file(dir.join("del.txt")).unwrap();
        fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();
        fs::write(dir.join("add.txt"), "brand new\n").unwrap();
        git_in(&dir, &["add", "-A"]);
        git_in(&dir, &["commit", "-qm", "head"]);

        dir
    }

    #[test]
    fn changed_files_reports_every_status() {
        let _guard = CWD_LOCK.lock().unwrap();
        let dir = make_repo();
        std::env::set_current_dir(&dir).unwrap();

        // The working tree currently matches the `head` branch, so diffing
        // `base` against the working tree is equivalent to base…head.
        let files = changed_files("base").unwrap();
        let by_path: HashMap<&str, &ChangedFile> =
            files.iter().map(|f| (f.path.as_str(), f)).collect();

        assert_eq!(by_path["keep.txt"].status, 'M');
        assert_eq!(by_path["add.txt"].status, 'A');
        assert_eq!(by_path["del.txt"].status, 'D');

        // Rename is detected (-M); path is the new name, old_path the source.
        let renamed = &by_path["new.txt"];
        assert_eq!(renamed.status, 'R');
        assert_eq!(renamed.old_path.as_deref(), Some("old.txt"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn untracked_files_are_listed_and_diffed() {
        let _guard = CWD_LOCK.lock().unwrap();
        let dir = make_repo();
        std::env::set_current_dir(&dir).unwrap();

        // A brand-new file that has never been `git add`ed, plus one that is
        // git-ignored (which must NOT show up).
        fs::write(dir.join("untracked.txt"), "fresh\nlines\n").unwrap();
        fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.join("ignored.txt"), "nope\n").unwrap();

        let files = changed_files("base").unwrap();
        let by_path: HashMap<&str, &ChangedFile> =
            files.iter().map(|f| (f.path.as_str(), f)).collect();

        let untracked = by_path["untracked.txt"];
        assert_eq!(untracked.status, 'A');
        assert!(untracked.untracked);
        assert!(!by_path.contains_key("ignored.txt"));

        // Its diff shows every line as an addition.
        let raw = file_diff("base", untracked).unwrap();
        assert!(raw.contains("+fresh"));
        assert!(raw.contains("+lines"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_diff_and_read_and_branch() {
        let _guard = CWD_LOCK.lock().unwrap();
        let dir = make_repo();
        std::env::set_current_dir(&dir).unwrap();

        // Per-file diff for the modified file shows the swapped line.
        let modified = ChangedFile {
            status: 'M',
            path: "keep.txt".into(),
            old_path: None,
            untracked: false,
        };
        let raw = file_diff("base", &modified).unwrap();
        assert!(raw.contains("-beta"));
        assert!(raw.contains("+BETA"));

        // verify_repo / verify_ref behaviour.
        assert!(verify_repo().is_ok());
        assert!(verify_ref("base").is_ok());
        assert!(verify_ref("does-not-exist").is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
