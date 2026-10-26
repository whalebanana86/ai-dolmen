//! Snapshot folders in Git, through the user's own `git`, so their identity,
//! hooks, signing and credentials apply just as they would on the command line.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::Serialize;

use crate::Error;
use crate::snapshot::{MODEL_FILE, Snapshot};

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

/// Clones `url` into `into`, which must not exist yet.
pub fn clone(url: &str, into: &Path) -> Result<(), Error> {
    if url.starts_with('-') {
        return Err(storage(format!("{url:?} isn't a repository URL")));
    }
    let parent = into
        .parent()
        .ok_or_else(|| storage(format!("{} has no parent folder", into.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|err| storage(format!("{}: {err}", parent.display())))?;
    let into = into.to_string_lossy();
    run(parent, &["clone", "--quiet", "--", url, &into])?;
    Ok(())
}

/// Brings the branch up to date with its upstream, by fast-forward only, so
/// Dolmen never merges on its own. Returns whether new commits arrived.
pub fn sync(dir: &Path) -> Result<bool, Error> {
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
    if !has_upstream {
        // A clone of an empty repository has none until its first push.
        return Ok(false);
    }
    run(dir, &["fetch", "--quiet"])?;
    let before = stdout(&run(dir, &["rev-parse", "HEAD"])?);
    if git(dir, &["merge", "--ff-only", "--quiet", "@{upstream}"])?
        .status
        .success()
    {
        Ok(stdout(&run(dir, &["rev-parse", "HEAD"])?) != before)
    } else {
        Err(storage(format!(
            "The branch and its upstream have both changed, so they can't be fast-forwarded; \
             reconcile them with git in {}.",
            dir.display()
        )))
    }
}

/// The URL of the `origin` remote, if there is one.
pub fn origin_url(dir: &Path) -> Result<Option<String>, Error> {
    let output = git(dir, &["remote", "get-url", "origin"])?;
    Ok(output.status.success().then(|| stdout(&output)))
}

/// Points the `origin` remote at `url`, e.g. to switch from HTTPS to SSH.
pub fn set_origin_url(dir: &Path, url: &str) -> Result<(), Error> {
    if url.starts_with('-') {
        return Err(storage(format!("{url:?} isn't a repository URL")));
    }
    run(dir, &["remote", "set-url", "origin", url])?;
    Ok(())
}

/// Whether two URLs name the same repository, such as its HTTPS address and
/// an SSH address through a `Host` alias in `~/.ssh/config`.
pub fn same_repository(a: &str, b: &str) -> bool {
    same_repository_with(a, b, &ssh_config())
}

fn same_repository_with(a: &str, b: &str, ssh_config: &str) -> bool {
    let aliases = ssh_aliases(ssh_config);
    match (remote_address(a), remote_address(b)) {
        (Some(a), Some(b)) => {
            a.real_host(&aliases)
                .eq_ignore_ascii_case(b.real_host(&aliases))
                && a.path.eq_ignore_ascii_case(b.path)
        }
        (None, None) => local_path(a) == local_path(b),
        _ => false,
    }
}

/// SSH addresses to suggest for a repository URL, one per `Host` alias in
/// `~/.ssh/config` that points at its host. Pushing over HTTPS uses whichever
/// account the credential helper has saved, and SSH to the plain host uses the
/// default key; with several accounts on one machine, an alias picks the
/// account by its key. HTTPS URLs also get the plain host's address; SSH URLs
/// that already use an alias get nothing.
pub fn ssh_alternatives(url: &str) -> Vec<String> {
    ssh_alternatives_with(url, &ssh_config())
}

fn ssh_alternatives_with(url: &str, ssh_config: &str) -> Vec<String> {
    let Some(address) = remote_address(url) else {
        return Vec::new();
    };
    let aliases = ssh_aliases(ssh_config);
    let host = address.real_host(&aliases);
    if address.ssh && !address.host.eq_ignore_ascii_case(host) {
        return Vec::new();
    }
    let mut hosts: Vec<&str> = aliases
        .iter()
        .filter(|(alias, host_name)| {
            host_name.eq_ignore_ascii_case(host) && !alias.eq_ignore_ascii_case(host)
        })
        .map(|(alias, _)| alias.as_str())
        .collect();
    if !address.ssh {
        hosts.push(host);
    }
    hosts.dedup();
    hosts
        .into_iter()
        .map(|alias| format!("git@{alias}:{}.git", address.path))
        .collect()
}

/// A folder name for a clone of `url`: its host and path, lower-cased, with
/// everything but letters, digits, `.` and `-` turned into `_`. SSH aliases
/// count as their real host, so a repository keeps one folder whether it's
/// reached over HTTPS or SSH. For example `git@github.com:you/schema.git`
/// becomes `github.com_you_schema`.
pub fn clone_dir_name(url: &str) -> String {
    clone_dir_name_with(url, &ssh_config())
}

fn clone_dir_name_with(url: &str, ssh_config: &str) -> String {
    let source = match remote_address(url) {
        Some(address) => {
            let aliases = ssh_aliases(ssh_config);
            let host = address.real_host(&aliases);
            match address.port {
                Some(port) => format!("{host}:{port}/{}", address.path),
                None => format!("{host}/{}", address.path),
            }
        }
        None => local_path(url).to_owned(),
    };

    let mut name = String::new();
    for ch in source.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
            name.push(ch);
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    let name = name.trim_matches('_');
    if name.is_empty() {
        "repository".to_owned()
    } else {
        name.to_owned()
    }
}

/// The host and path of a network repository URL.
struct RemoteAddress<'a> {
    host: &'a str,
    port: Option<&'a str>,
    /// Without slashes around it or a `.git` suffix, e.g. `you/schema`.
    path: &'a str,
    /// Whether Git reaches it over SSH, where the host may be an alias.
    ssh: bool,
}

impl RemoteAddress<'_> {
    /// The host an SSH alias stands for, or the host itself.
    fn real_host<'a>(&'a self, aliases: &'a [(String, String)]) -> &'a str {
        if !self.ssh {
            return self.host;
        }
        aliases
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(self.host))
            .map_or(self.host, |(_, host_name)| host_name.as_str())
    }
}

/// Parses `scheme://[user@]host[:port]/path` and scp-like `[user@]host:path`
/// URLs; anything else, such as a local path, is `None`.
fn remote_address(url: &str) -> Option<RemoteAddress<'_>> {
    let (ssh, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.contains("ssh"), rest),
        // Git reads `host:path` as SSH when the colon comes before any slash.
        None => match (url.find(':'), url.find('/')) {
            (Some(colon), slash) if slash.is_none_or(|slash| colon < slash) => (true, url),
            _ => return None,
        },
    };
    // Drop user info such as `git@` or `user:token@`.
    let rest = match rest.split_once('@') {
        Some((user, after)) if !user.contains('/') => after,
        _ => rest,
    };
    let (authority, path) = if url.contains("://") {
        rest.split_once('/')?
    } else {
        rest.split_once(':')?
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) if url.contains("://") => (host, Some(port)),
        _ => (authority, None),
    };
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    (!host.is_empty() && !path.is_empty()).then_some(RemoteAddress {
        host,
        port,
        path,
        ssh,
    })
}

fn local_path(url: &str) -> &str {
    let path = url.trim_end_matches('/');
    path.strip_suffix(".git").unwrap_or(path)
}

fn ssh_config() -> String {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".ssh").join("config"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
}

/// The `Host` aliases in an SSH config that set a `HostName`, with that name.
fn ssh_aliases(ssh_config: &str) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in ssh_config.lines() {
        let line = line.trim();
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((key, value)) => (
                key,
                value
                    .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
                    .trim(),
            ),
            None => continue,
        };
        if key.eq_ignore_ascii_case("Host") {
            current = value
                .split_whitespace()
                .filter(|p| !p.contains(['*', '?', '!']))
                .collect();
        } else if key.eq_ignore_ascii_case("Match") {
            current.clear();
        } else if key.eq_ignore_ascii_case("HostName") {
            // As in ssh, the first HostName for an alias wins.
            for alias in current.drain(..) {
                aliases.push((alias.to_owned(), value.to_owned()));
            }
        }
    }
    aliases
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
    let mut model_blob = None;
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
        } else if relative == MODEL_FILE {
            model_blob = Some(oid.to_owned());
        }
    }

    // The model, if any, is read last in the same batch.
    let oids = blobs
        .iter()
        .map(|(_, oid)| oid.as_str())
        .chain(model_blob.as_deref());
    let mut contents = read_blobs(dir, oids)?;
    let model_json = model_blob.and_then(|_| contents.pop());
    let files: BTreeMap<String, String> = blobs
        .into_iter()
        .map(|(path, _)| path)
        .zip(contents)
        .collect();
    Snapshot::from_files(files, model_json.as_deref())
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

#[cfg(test)]
mod tests {
    use super::*;

    const SSH_CONFIG: &str = "Host work
    HostName example.org
Host github-personal gh-alt
    HostName github.com
    IdentityFile ~/.ssh/personal
Host *
    ServerAliveInterval 30
";

    #[test]
    fn suggests_ssh_aliases() {
        assert_eq!(
            ssh_alternatives_with(
                "https://github.com/whalebanana86/ai-dolmen-test.git",
                SSH_CONFIG
            ),
            [
                "git@github-personal:whalebanana86/ai-dolmen-test.git",
                "git@gh-alt:whalebanana86/ai-dolmen-test.git",
                "git@github.com:whalebanana86/ai-dolmen-test.git",
            ]
        );
        assert_eq!(
            ssh_alternatives_with("https://user:token@github.com/a/b/", ""),
            ["git@github.com:a/b.git"]
        );
        // SSH to the plain host uses the default key, which may be another account's.
        assert_eq!(
            ssh_alternatives_with("git@github.com:a/b.git", SSH_CONFIG),
            ["git@github-personal:a/b.git", "git@gh-alt:a/b.git"]
        );
        assert!(ssh_alternatives_with("git@github.com:a/b.git", "").is_empty());
        assert!(ssh_alternatives_with("git@github-personal:a/b.git", SSH_CONFIG).is_empty());
        assert!(ssh_alternatives_with("https://github.com/", SSH_CONFIG).is_empty());
        assert!(ssh_alternatives_with("/srv/git/a.git", SSH_CONFIG).is_empty());
    }

    #[test]
    fn names_clone_folders_after_host_and_path() {
        let name = |url| clone_dir_name_with(url, SSH_CONFIG);
        assert_eq!(
            name("git@github.com:you/schema.git"),
            "github.com_you_schema"
        );
        assert_eq!(
            name("git@github-personal:whalebanana86/ai-dolmen-test.git"),
            "github.com_whalebanana86_ai-dolmen-test"
        );
        assert_eq!(
            name("https://user:token@github.com/You/Schema/"),
            "github.com_you_schema"
        );
        assert_eq!(name("ssh://git@host:2222/a/b.git"), "host_2222_a_b");
        assert_eq!(name("ssh://git@gh-alt/a/b.git"), "github.com_a_b");
        assert_eq!(name("/srv/git/schema.git"), "srv_git_schema");
        assert_eq!(name("@@@"), "repository");
    }

    #[test]
    fn knows_https_and_ssh_addresses_of_one_repository() {
        let same = |a, b| same_repository_with(a, b, SSH_CONFIG);
        let https = "https://github.com/whalebanana86/ai-dolmen-test.git";
        assert!(same(
            https,
            "git@github.com:whalebanana86/ai-dolmen-test.git"
        ));
        assert!(same(
            https,
            "git@github-personal:whalebanana86/ai-dolmen-test.git"
        ));
        assert!(same(https, "ssh://git@gh-alt/whalebanana86/ai-dolmen-test"));
        assert!(same(
            https,
            "https://github.com/whalebanana86/ai-dolmen-test/"
        ));
        assert!(!same(https, "git@github.com:whalebanana86/other.git"));
        assert!(!same(https, "git@work:whalebanana86/ai-dolmen-test.git"));
        assert!(!same(
            "git@github.com:a_b/c.git",
            "git@github.com:a/b_c.git"
        ));
        assert!(same("/srv/git/a.git", "/srv/git/a"));
        assert!(!same("/srv/git/a.git", https));
    }
}
