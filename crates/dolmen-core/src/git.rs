//! Committing snapshots with the user's own `git`, so their identity, hooks and
//! signing settings apply just as they would on the command line.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Serialize;

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repository {
    pub root: PathBuf,
    /// The checked-out branch; `None` on a detached HEAD.
    pub branch: Option<String>,
}

/// The repository containing `path`, which may not exist yet.
pub fn repository(path: &Path) -> Result<Option<Repository>, Error> {
    let Some(existing) = path.ancestors().find(|p| p.is_dir()) else {
        return Ok(None);
    };
    let toplevel = git(existing, &["rev-parse", "--show-toplevel"])?;
    if !toplevel.status.success() {
        return Ok(None);
    }
    let branch = git(existing, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    Ok(Some(Repository {
        root: PathBuf::from(stdout(&toplevel)),
        branch: branch.status.success().then(|| stdout(&branch)),
    }))
}

/// Commits everything under `dir`, and nothing outside it, even if other changes
/// are staged. Returns the new commit's hash, or `None` if nothing changed.
pub fn commit(dir: &Path, message: &str) -> Result<Option<String>, Error> {
    run(dir, &["add", "--all", "--", "."])?;
    let staged = git(dir, &["diff", "--cached", "--quiet", "--", "."])?;
    if staged.status.success() {
        return Ok(None);
    }
    run(dir, &["commit", "--quiet", "--message", message, "--", "."])?;
    Ok(Some(stdout(&run(dir, &["rev-parse", "HEAD"])?)))
}

fn run(dir: &Path, args: &[&str]) -> Result<Output, Error> {
    let output = git(dir, args)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(Error::Storage {
        message: format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    })
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, Error> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|err| Error::Storage {
            message: format!("couldn't run git, is it installed? {err}"),
        })
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
