//! The few git questions slimfit asks, each one command.

use crate::model::Snapshot;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn run(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn toplevel(dir: &Path) -> Result<std::path::PathBuf> {
    let s = run(dir, &["rev-parse", "--show-toplevel"]).context("slimfit runs inside a git repository")?;
    Ok(s.trim().into())
}

/// Tracked files plus untracked ones git does not ignore, relative to
/// `repo`, each with whether git tracks it.
fn listing(repo: &Path) -> Result<Vec<(bool, String)>> {
    let s = run(repo, &["ls-files", "-co", "-t", "--exclude-standard", "-z"])?;
    s.split('\0')
        .filter(|e| !e.is_empty())
        .map(|e| {
            let (tag, path) = e.split_once(' ').with_context(|| format!("ls-files -t entry {e:?}"))?;
            Ok((tag != "?", path.to_string()))
        })
        .collect()
}

/// The working tree, read with three git commands: each spawn costs more
/// on a loaded host than the load probe corrects for (ADR 0003).
pub struct Worktree {
    /// Tracked files plus untracked ones git does not ignore, relative to `repo`.
    pub files: Vec<String>,
    /// HEAD and the files that differ from it, outside `.slimfit/`; `None`
    /// before the first commit.
    pub snapshot: Option<Snapshot>,
    /// HEAD plus a hash of every local change outside `.slimfit/`, so a
    /// measurement can be reused exactly while the tree is unchanged.
    pub key: String,
    untracked: Vec<String>,
}

pub fn worktree(repo: &Path) -> Result<Worktree> {
    use std::hash::{Hash, Hasher};
    let outside = |f: &String| !f.starts_with(".slimfit/");
    let listing = listing(repo)?;
    let untracked: Vec<String> = listing.iter().filter(|(t, f)| !t && outside(f)).map(|(_, f)| f.clone()).collect();
    let files: Vec<String> = listing.into_iter().map(|(_, f)| f).collect();
    let head = run(repo, &["rev-parse", "--verify", "HEAD"]).ok().map(|h| h.trim().to_string());
    // Before the first commit every file is a local change.
    let changed = match &head {
        Some(commit) => differing(repo, commit, &untracked)?,
        None => files.iter().filter(|f| outside(f)).cloned().collect(),
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in &changed {
        f.hash(&mut h);
        let p = repo.join(f);
        // A symlink by its target, as git records it; the rest by contents and mode.
        std::fs::read_link(&p).ok().hash(&mut h);
        std::fs::read(&p).ok().hash(&mut h);
        std::fs::symlink_metadata(&p)
            .ok()
            .map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()))
            .hash(&mut h);
    }
    let key = format!("{}+{:016x}", head.as_deref().unwrap_or("unborn"), h.finish());
    let snapshot = head.map(|commit| Snapshot { commit, dirty: changed.into_iter().collect() });
    Ok(Worktree { files, snapshot, key, untracked })
}

/// The files whose contents may differ between the tree `then` names and
/// the working tree now: those changed since its commit, and those that
/// differed from it then. Fails when the commit is not in this repository.
pub fn changed_since(repo: &Path, now: &Worktree, then: &Snapshot) -> Result<BTreeSet<String>> {
    let mut out = differing(repo, &then.commit, &now.untracked)?;
    out.extend(then.dirty.iter().cloned());
    Ok(out)
}

/// Files of the working tree that differ from `commit` (both sides of a
/// rename, deleted ones too), plus the untracked ones.
fn differing(repo: &Path, commit: &str, untracked: &[String]) -> Result<BTreeSet<String>> {
    let diff = run(repo, &["diff", "--no-renames", "--name-only", "-z", commit, "--", ".", ":(exclude).slimfit"])?;
    Ok(diff.split('\0').filter(|p| !p.is_empty()).map(String::from).chain(untracked.iter().cloned()).collect())
}

/// The blobs of `commit`: (path, contents), read through one `cat-file --batch`.
pub fn tree_blobs(repo: &Path, commit: &str, keep: impl Fn(&str) -> bool) -> Result<Vec<(String, String)>> {
    let listing = run(repo, &["ls-tree", "-r", "-z", commit])?;
    let mut wanted = Vec::new();
    for entry in listing.split('\0').filter(|e| !e.is_empty()) {
        let (meta, path) = entry.split_once('\t').context("ls-tree entry")?;
        let mut parts = meta.split(' ');
        let (_mode, kind, oid) = (parts.next(), parts.next(), parts.next());
        if kind == Some("blob") && keep(path) {
            wanted.push((path.to_string(), oid.unwrap_or_default().to_string()));
        }
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().context("stdin")?;
    let oids: Vec<String> = wanted.iter().map(|(_, o)| o.clone()).collect();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for o in oids {
            writeln!(stdin, "{o}")?;
        }
        Ok(())
    });
    let mut reader = BufReader::new(child.stdout.take().context("stdout")?);
    let mut out = Vec::with_capacity(wanted.len());
    for (path, _) in wanted {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let size: usize = header.split(' ').nth(2).context("cat-file header")?.trim().parse()?;
        let mut buf = vec![0; size + 1];
        reader.read_exact(&mut buf)?;
        buf.pop();
        out.push((path, String::from_utf8_lossy(&buf).into_owned()));
    }
    writer.join().expect("writer thread")?;
    child.wait()?;
    Ok(out)
}
