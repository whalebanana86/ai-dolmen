//! Snapshot folders in Git, through the user's own `git`, so their identity,
//! hooks, signing and credentials apply just as they would on the command line.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::Serialize;

use crate::Error;
use crate::snapshot::Snapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repository {
    pub root: PathBuf,
    /// The checked-out branch; `None` on a detached HEAD.
    pub branch: Option<String>,
    /// The branch's upstream, e.g. `origin/main`.
    pub upstream: Option<String>,
    /// Commits the upstream doesn't have yet, as of the last fetch.
    pub ahead: u32,
    /// Commits on the upstream the branch doesn't have yet, as of the last fetch.
    pub behind: u32,
    pub remotes: Vec<String>,
}

/// The repository containing `path`, which may not exist yet.
pub fn repository(path: &Path) -> Result<Option<Repository>, Error> {
    let Some(dir) = path.ancestors().find(|p| p.is_dir()) else {
        return Ok(None);
    };
    let toplevel = git(dir, &["rev-parse", "--show-toplevel"])?;
    if !toplevel.status.success() {
        return Ok(None);
    }
    let branch = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let upstream = git(
        dir,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;
    let upstream = upstream.status.success().then(|| stdout(&upstream));
    let (behind, ahead) = match upstream {
        Some(_) => {
            let counts = stdout(&run(
                dir,
                &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
            )?);
            let mut counts = counts.split_whitespace().map(|n| n.parse().unwrap_or(0));
            (counts.next().unwrap_or(0), counts.next().unwrap_or(0))
        }
        None => (0, 0),
    };
    Ok(Some(Repository {
        root: PathBuf::from(stdout(&toplevel)),
        branch: branch.status.success().then(|| stdout(&branch)),
        upstream,
        ahead,
        behind,
        remotes: remotes(dir)?,
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

/// Pushes the current branch to its upstream. The first time, it pushes to
/// `origin` (or the only remote) and makes that the upstream.
pub fn push(dir: &Path) -> Result<(), Error> {
    let has_upstream = git(
        dir,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?
    .status
    .success();
    if has_upstream {
        run(dir, &["push", "--quiet"])?;
        return Ok(());
    }
    let remotes = remotes(dir)?;
    let remote = if remotes.iter().any(|r| r == "origin") {
        "origin"
    } else {
        match remotes.as_slice() {
            [only] => only.as_str(),
            [] => return Err(storage("the repository has no remote to push to")),
            _ => {
                return Err(storage(
                    "the branch has no upstream and there are several remotes; set one with `git push -u <remote>`",
                ));
            }
        }
    };
    run(dir, &["push", "--quiet", "--set-upstream", remote, "HEAD"])?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    /// Author date in ISO 8601.
    pub date: String,
    pub subject: String,
    /// Branches and tags at this commit, e.g. `main`, `origin/main`, `tag: v1`.
    pub refs: Vec<String>,
    /// Whether this is the checked-out commit.
    pub head: bool,
    /// Whether the commit changed the snapshot folder.
    pub changes_snapshot: bool,
}

/// The repository's latest commits on all branches and tags, newest first and
/// never a parent before its children, so a graph can be drawn top to bottom.
pub fn history(dir: &Path, limit: usize) -> Result<Vec<Commit>, Error> {
    if !git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])?
        .status
        .success()
    {
        return Ok(Vec::new()); // No commits yet.
    }
    let max_count = format!("--max-count={limit}");
    let touched: HashSet<String> = stdout(&run(
        dir,
        &["log", "--all", "--format=%H", &max_count, "--", "."],
    )?)
    .lines()
    .map(str::to_owned)
    .collect();
    let log = run(
        dir,
        &[
            "log",
            "--all",
            "--topo-order",
            &max_count,
            "--format=%H%x1f%P%x1f%an%x1f%aI%x1f%s%x1f%D%x1e",
        ],
    )?;
    let commits = String::from_utf8_lossy(&log.stdout)
        .split('\x1e')
        .filter_map(|record| {
            let fields: Vec<&str> = record.trim_start_matches('\n').split('\x1f').collect();
            let [hash, parents, author, date, subject, decorations] = fields[..] else {
                return None;
            };
            let mut head = false;
            let refs = decorations
                .split(", ")
                .filter(|d| !d.is_empty())
                .filter_map(|d| {
                    if d == "HEAD" {
                        head = true;
                        return None;
                    }
                    Some(match d.strip_prefix("HEAD -> ") {
                        Some(branch) => {
                            head = true;
                            branch.to_owned()
                        }
                        None => d.to_owned(),
                    })
                })
                .collect();
            Some(Commit {
                changes_snapshot: touched.contains(hash),
                hash: hash.to_owned(),
                parents: parents.split_whitespace().map(str::to_owned).collect(),
                author: author.to_owned(),
                date: date.to_owned(),
                subject: subject.to_owned(),
                refs,
                head,
            })
        })
        .collect();
    Ok(commits)
}

/// The snapshot in `dir` as it was at `rev`, a commit, branch or tag.
pub fn snapshot_at(dir: &Path, rev: &str) -> Result<Snapshot, Error> {
    if rev.starts_with('-') {
        return Err(storage(format!("{rev:?} isn't a revision")));
    }
    let prefix = stdout(&run(dir, &["rev-parse", "--show-prefix"])?);
    let listing = run(
        dir,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--full-name",
            "--end-of-options",
            rev,
            "--",
            ".",
        ],
    )?;

    // Entries look like "<mode> blob <oid>\t<path>".
    let mut blobs = Vec::new();
    for entry in listing.stdout.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let entry = String::from_utf8_lossy(entry);
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut meta = meta.split(' ');
        let (Some(_), Some("blob"), Some(oid)) = (meta.next(), meta.next(), meta.next()) else {
            continue;
        };
        let Some(relative) = path.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if relative.ends_with(".sql") {
            blobs.push((relative.to_owned(), oid.to_owned()));
        }
    }

    let contents = read_blobs(dir, blobs.iter().map(|(_, oid)| oid.as_str()))?;
    let files: BTreeMap<String, String> = blobs
        .into_iter()
        .map(|(path, _)| path)
        .zip(contents)
        .collect();
    Ok(Snapshot::from_files(files))
}

/// Reads blobs in one `git cat-file --batch` process rather than one per file.
fn read_blobs<'a>(dir: &Path, oids: impl Iterator<Item = &'a str>) -> Result<Vec<String>, Error> {
    let input: String = oids.map(|oid| format!("{oid}\n")).collect();
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = command(dir)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(couldnt_run)?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    // Write on another thread so a full stdout pipe can't block us both.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().map_err(couldnt_run)?;
    writer.join().expect("writer thread").map_err(couldnt_run)?;
    if !output.status.success() {
        return Err(storage(format!(
            "git cat-file failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    // Each blob is "<oid> blob <size>\n<content>\n".
    let mut contents = Vec::new();
    let mut rest = output.stdout.as_slice();
    while let Some(newline) = rest.iter().position(|&b| b == b'\n') {
        let header = String::from_utf8_lossy(&rest[..newline]);
        let size: usize = header
            .rsplit(' ')
            .next()
            .and_then(|size| size.parse().ok())
            .ok_or_else(|| storage(format!("unexpected git cat-file output: {header}")))?;
        let start = newline + 1;
        contents.push(String::from_utf8_lossy(&rest[start..start + size]).into_owned());
        rest = &rest[(start + size + 1).min(rest.len())..];
    }
    Ok(contents)
}

fn remotes(dir: &Path) -> Result<Vec<String>, Error> {
    Ok(stdout(&run(dir, &["remote"])?)
        .lines()
        .map(str::to_owned)
        .collect())
}

fn run(dir: &Path, args: &[&str]) -> Result<Output, Error> {
    let output = git(dir, args)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(storage(format!(
        "git {} failed: {}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, Error> {
    command(dir).args(args).output().map_err(couldnt_run)
}

fn command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        // Never wait for a credential prompt nobody can answer from the app.
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn couldnt_run(err: std::io::Error) -> Error {
    storage(format!("couldn't run git, is it installed? {err}"))
}

fn storage(message: impl Into<String>) -> Error {
    Error::Storage {
        message: message.into(),
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
