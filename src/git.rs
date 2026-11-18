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

/// Tracked files plus untracked ones git does not ignore, relative to `repo`.
pub fn files(repo: &Path) -> Result<Vec<String>> {
    let s = run(repo, &["ls-files", "-co", "--exclude-standard", "-z"])?;
    Ok(s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect())
}

/// HEAD plus a hash of every local change outside `.slimfit/`, so a
/// measurement can be reused exactly while the tree is unchanged.
pub fn tree_key(repo: &Path) -> Result<String> {
    use std::hash::{Hash, Hasher};
    let head = run(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "unborn".into());
    let exclude = ":(exclude).slimfit";
    let diff = run(repo, &["diff", "HEAD", "--", ".", exclude]).unwrap_or_default();
    let untracked = run(repo, &["ls-files", "-o", "--exclude-standard", "--", ".", exclude])?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    diff.hash(&mut h);
    for f in untracked.lines() {
        f.hash(&mut h);
        if let Ok(bytes) = std::fs::read(repo.join(f)) {
            bytes.hash(&mut h);
        }
    }
    Ok(format!("{}+{:016x}", head.trim(), h.finish()))
}

/// HEAD and the files that differ from it, outside `.slimfit/`; `None`
/// before the first commit.
pub fn snapshot(repo: &Path) -> Result<Option<Snapshot>> {
    let Ok(head) = run(repo, &["rev-parse", "--verify", "HEAD"]) else { return Ok(None) };
    let commit = head.trim().to_string();
    let dirty = differing(repo, &commit)?.into_iter().collect();
    Ok(Some(Snapshot { commit, dirty }))
}

/// The files whose contents may differ between the tree `then` names and
/// the working tree now: those changed since its commit, and those that
/// differed from it then. Fails when the commit is not in this repository.
pub fn changed_since(repo: &Path, then: &Snapshot) -> Result<BTreeSet<String>> {
    let mut out = differing(repo, &then.commit)?;
    out.extend(then.dirty.iter().cloned());
    Ok(out)
}

/// Files of the working tree that differ from `commit` (both sides of a
/// rename, deleted ones too), plus untracked ones.
fn differing(repo: &Path, commit: &str) -> Result<BTreeSet<String>> {
    let exclude = ":(exclude).slimfit";
    let diff = run(repo, &["diff", "--no-renames", "--name-only", "-z", commit, "--", ".", exclude])?;
    let untracked = run(repo, &["ls-files", "-o", "--exclude-standard", "-z", "--", ".", exclude])?;
    Ok(diff.split('\0').chain(untracked.split('\0')).filter(|p| !p.is_empty()).map(String::from).collect())
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
