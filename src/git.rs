//! The few git questions slimfit asks, each one command.

use crate::fault::fault;
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn run(repo: &Path, args: &[&str]) -> Result<String> {
    run_with_index(repo, None, args)
}

/// `run`, with `GIT_INDEX_FILE` set to `index` when given.
fn run_with_index(repo: &Path, index: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(i) = index {
        cmd.env("GIT_INDEX_FILE", i);
    }
    let out = cmd.arg("-C").arg(repo).args(args).output().with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The repository `dir` is in, by its HEAD: one `rev-parse`, as each spawn
/// costs CPU time (ADR 0010).
pub fn open(dir: &Path) -> Result<Head> {
    let fix = "run slimfit inside the repository, or pass `--repo <its path>`; a project with no repository yet \
               needs `git init` and a first commit";
    if !dir.is_dir() {
        return Err(fault(format!("{} is not a directory", dir.display()), fix));
    }
    const PATHS: [&str; 7] = [
        "rev-parse",
        "--show-toplevel",
        "--path-format=absolute",
        "--git-path",
        "index",
        "--git-path",
        "slimfit-index",
    ];
    let with_head = [&PATHS[..], &["--verify", "HEAD"]].concat();
    // Before the first commit `--verify HEAD` fails the whole call.
    let (out, born) = match run(dir, &with_head) {
        Ok(out) => (out, true),
        Err(_) => match run(dir, &PATHS) {
            Ok(out) => (out, false),
            Err(e) => return Err(fault(format!("{} is not inside a git repository ({e:#})", dir.display()), fix)),
        },
    };
    let mut lines = out.lines().map(str::trim);
    let top = lines.next().context("the repository's top level")?.into();
    let index = lines.next().context("git's index path")?.into();
    let scratch = lines.next().context("a scratch index path")?.into();
    let commit = if born { Some(lines.next().context("HEAD")?.to_string()) } else { None };
    Ok(Head { top, commit, index, scratch })
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

/// The working tree, read with three git commands: each spawn costs CPU
/// time, which `test_seconds` counts (ADR 0010).
pub struct Worktree {
    /// Tracked files plus untracked ones git does not ignore, relative to `repo`.
    pub files: Vec<String>,
    pub head: Head,
    /// HEAD plus a hash of every local change outside `.slimfit/`, so a
    /// measurement can be reused exactly while the tree is unchanged.
    pub key: String,
}

/// The working tree of `head`'s repository.
pub fn worktree(head: &Head) -> Result<Worktree> {
    let repo = head.top.as_path();
    use std::hash::{Hash, Hasher};
    let outside = |f: &String| !f.starts_with(".slimfit/");
    let listing = listing(repo)?;
    let untracked: Vec<String> = listing.iter().filter(|(t, f)| !t && outside(f)).map(|(_, f)| f.clone()).collect();
    let files: Vec<String> = listing.into_iter().map(|(_, f)| f).collect();
    // Before the first commit every file is a local change.
    let changed = match &head.commit {
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
    let key = format!("{}+{:016x}", head.commit.as_deref().unwrap_or("unborn"), h.finish());
    Ok(Worktree { files, head: head.clone(), key })
}

#[cfg(test)]
impl Worktree {
    /// The tree of `files` under `top`, with no repository: a test's sample
    /// directory.
    pub fn of(top: &Path, files: Vec<String>) -> Self {
        let head = Head { top: top.into(), commit: None, index: Default::default(), scratch: Default::default() };
        Self { files, head, key: String::new() }
    }
}

/// Files of the working tree that differ from `commit` (both sides of a
/// rename, deleted ones too), plus the untracked ones.
fn differing(repo: &Path, commit: &str, untracked: &[String]) -> Result<Vec<String>> {
    let diff = run(repo, &["diff", "--no-renames", "--name-only", "-z", commit, "--", ".", ":(exclude).slimfit"])?;
    let mut out: Vec<String> =
        diff.split('\0').filter(|p| !p.is_empty()).map(String::from).chain(untracked.iter().cloned()).collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// The working tree (untracked files git does not ignore included) written
/// as a tree object, through a copy of the index kept in the git directory:
/// the real index is not touched.
pub fn write_tree(repo: &Path, head: &Head) -> Result<String> {
    match std::fs::copy(&head.index, &head.scratch) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => _ = std::fs::remove_file(&head.scratch),
        Err(e) => return Err(e.into()),
    }
    // slimfit's own files are not the code: writing the baseline moves no tree.
    run_with_index(repo, Some(&head.scratch), &["add", "-A", "--", ".", ":(exclude).slimfit"])?;
    Ok(run_with_index(repo, Some(&head.scratch), &["write-tree"])?.trim().to_string())
}

/// A repository by its HEAD, and where git keeps the index and slimfit its
/// copy of it.
#[derive(Clone)]
pub struct Head {
    /// The repository's top level.
    pub top: std::path::PathBuf,
    /// `None` before the first commit.
    pub commit: Option<String>,
    index: std::path::PathBuf,
    scratch: std::path::PathBuf,
}

/// Where HEAD forks from `target`: `None` when the repository has no such
/// ref, they share no commit, or before the first commit.
pub fn merge_base(repo: &Path, target: &str) -> Option<String> {
    run(repo, &["merge-base", "HEAD", target]).ok().map(|s| s.trim().to_string())
}

/// Whether commit `a` is `b` or one of its ancestors: `false` too when
/// either is not a commit of this repository.
pub fn is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    run(repo, &["merge-base", "--is-ancestor", a, b]).is_ok()
}

/// The unified diff from `from` to `to` of `files` (repository relative),
/// its paths relative to `root` (a directory of the repository, or ".").
pub fn diff(repo: &Path, from: &str, to: &str, root: &str, files: &[String]) -> Result<String> {
    let relative = format!("--relative={}", root.trim_end_matches('/'));
    let mut args = vec!["diff", "--no-ext-diff", "--no-color", "--no-renames"];
    if root != "." {
        args.push(&relative);
    }
    args.extend([from, to, "--"]);
    let specs: Vec<String> = files.iter().map(|f| format!(":(literal){f}")).collect();
    args.extend(specs.iter().map(String::as_str));
    run(repo, &args)
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
