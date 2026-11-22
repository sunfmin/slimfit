//! The lines a change's removed tests ran (slimfit#87), from the
//! measurement of the tree the change starts from: the last measurement
//! before it, the same in every clone. Its mutants run against the tests
//! left, so a survivor is a test removed too many.

use super::{OUT, Options};
use crate::config::Config;
use crate::git;
use crate::landing::removed::Removed;
use crate::landing::{self, Landing, Lines, Tree};
use crate::model::Measurement;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The lines one suite's removed tests ran, as they are now, and which of
/// those tests ran each.
#[derive(Debug, Default)]
pub(super) struct Reached {
    pub(super) lines: Lines,
    pub(super) by: BTreeMap<(String, u32), BTreeSet<String>>,
}

impl Reached {
    /// The removed tests that ran a line of `file` in `lines`.
    pub(super) fn tests(&self, file: &str, lines: std::ops::RangeInclusive<u32>) -> Vec<String> {
        let mut out = BTreeSet::new();
        for l in lines {
            out.extend(self.by.get(&(file.to_string(), l)).into_iter().flatten().cloned());
        }
        out.into_iter().collect()
    }
}

/// Per suite that removes tests, the source lines they ran where the change
/// starts, moved to where they are now: a line the change deletes or
/// rewrites is left out (one it rewrites is an added line). Measures the
/// tree the change starts from once, when a suite removes a test.
pub(super) fn reached(repo: &Path, cfg: &Config, tree: &Tree, l: &Landing) -> Result<BTreeMap<String, Reached>> {
    let removed = landing::removed::of(repo, cfg, tree, l)?;
    if removed.is_empty() {
        return Ok(BTreeMap::new());
    }
    let suites = cfg.suites.iter().filter(|s| removed.contains_key(&s.name)).cloned().collect();
    let before = before(repo, &Config { suites, ..cfg.clone() }, &l.from)?;
    let moves = Moves::of(&git::diff(repo, &l.from, &l.to, ".", &[])?);
    let mut out = BTreeMap::new();
    for s in &before.suites {
        let Some(gone) = removed.get(&s.suite) else { continue };
        let Some((tests, Some(lines))) = s.reach.as_ref().map(|r| (&r.tests, r.lines.as_ref())) else {
            eprintln!(
                "slimfit: {}: the test run where the change starts recorded no lines per test: the lines its \
                 removed tests ran are not known",
                s.suite
            );
            continue;
        };
        let st = tree.suite(cfg, &s.suite)?;
        let mut r = Reached::default();
        for (t, ran) in tests.iter().zip(lines) {
            let Some(g) = gone.iter().find(|g| is(t, g)) else { continue };
            for (file, ls) in ran {
                let Some(now) = st.source_lines(file) else { continue };
                let text = std::fs::read_to_string(repo.join(file)).unwrap_or_default();
                let text: Vec<&str> = text.lines().collect();
                for n in ls.iter().filter_map(|n| moves.to(file, *n)).filter(|n| now.is_source_line(*n)) {
                    let line = text.get(n as usize - 1).copied().unwrap_or_default().to_string();
                    r.lines.entry(file.clone()).or_default().insert(n, line);
                    r.by.entry((file.clone(), n)).or_default().insert(g.id());
                }
            }
        }
        out.insert(s.suite.clone(), r);
    }
    Ok(out)
}

/// Whether the measured test `t` is the removed `g`: its function at the
/// same place; or, where the measurement did not find its function, one of
/// that name.
fn is(t: &crate::model::ReachTest, g: &Removed) -> bool {
    match &t.source {
        Some(s) => s.file == g.file && s.line == g.line,
        None => t.name.rsplit("::").next() == Some(g.name.as_str()),
    }
}

/// Where the change starts, checked out in a worktree of its own: in
/// `.slimfit/out/before/`, kept between commands so its build stays warm.
fn checkout(repo: &Path, from: &str) -> Result<PathBuf> {
    let dir = repo.join(OUT).join("before");
    // Inside the repository, a directory that is no worktree of its own answers for the repository.
    let at = dir.join(".git").is_file().then(|| git::run(&dir, &["rev-parse", "HEAD"]).ok()).flatten();
    match at {
        Some(c) if c.trim() == from => {}
        Some(_) => {
            git::run(&dir, &["checkout", "-q", "--detach", "--force", from])?;
        }
        None => {
            let _ = std::fs::remove_dir_all(&dir);
            git::run(repo, &["worktree", "prune"])?;
            git::run(repo, &["worktree", "add", "-q", "--detach", &dir.to_string_lossy(), from])?;
        }
    }
    Ok(dir)
}

/// The measurement of the tree the change starts from, with `cfg`'s
/// suites: cached in its worktree while it is the same.
fn before(repo: &Path, cfg: &Config, from: &str) -> Result<Measurement> {
    eprintln!("slimfit: the change removes tests: measuring the tree it starts from, {from}, for the lines they ran");
    let dir = checkout(repo, from)?;
    let tree = Tree::read(&git::open(&dir)?)?;
    super::current(&dir, &tree, cfg, &Options::default())
}

/// Where a unified diff moves each line of the files it touches: per file
/// (its old path), each hunk's old start and length, and the new line of
/// each of its old lines that it keeps (context), with the shift after it.
struct Moves(BTreeMap<String, Vec<Hunk>>);

struct Hunk {
    start: u32,
    len: u32,
    kept: BTreeMap<u32, u32>,
    /// New line less old line, after the hunk.
    shift: i64,
}

impl Moves {
    fn of(patch: &str) -> Moves {
        let mut out: BTreeMap<String, Vec<Hunk>> = BTreeMap::new();
        let mut file = String::new();
        let (mut old, mut new, mut left) = (0u32, 0u32, 0u32);
        for l in patch.lines() {
            if left > 0 {
                let h = out.get_mut(&file).and_then(|h| h.last_mut()).expect("a hunk is open");
                match l.as_bytes().first() {
                    Some(b' ') => {
                        h.kept.insert(old, new);
                        (old, new, left) = (old + 1, new + 1, left - 1);
                    }
                    Some(b'-') => (old, left) = (old + 1, left - 1),
                    Some(b'+') => new += 1,
                    _ => {}
                }
            } else if let Some(p) = l.strip_prefix("--- ") {
                file = p.strip_prefix("a/").unwrap_or_default().to_string();
            } else if let Some(h) = l.strip_prefix("@@ -") {
                let range = |r: &str| -> (u32, u32) {
                    let (a, n) = r.split_once(',').unwrap_or((r, "1"));
                    (a.parse().unwrap_or(0), n.parse().unwrap_or(0))
                };
                let mut parts = h.split(' ');
                let (o, on) = range(parts.next().unwrap_or(""));
                let (n, nn) = range(parts.next().unwrap_or("").trim_start_matches('+'));
                // A side of no line starts after the line it names.
                let (start, at) = (if on == 0 { o + 1 } else { o }, if nn == 0 { n + 1 } else { n });
                let shift = i64::from(at + nn) - i64::from(start + on);
                out.entry(file.clone()).or_default().push(Hunk { start, len: on, kept: BTreeMap::new(), shift });
                // Once its old lines are read, nothing it adds after them moves a line.
                (old, new, left) = (start, at, on);
            }
        }
        out.remove("");
        Moves(out)
    }

    /// Where line `n` of `file` is after the diff; `None` when it deletes or
    /// rewrites it, or deletes the file.
    fn to(&self, file: &str, n: u32) -> Option<u32> {
        let Some(hunks) = self.0.get(file) else { return Some(n) };
        let mut shift = 0;
        for h in hunks {
            if n < h.start {
                break;
            }
            if n < h.start + h.len {
                return h.kept.get(&n).copied();
            }
            shift = h.shift;
        }
        u32::try_from(i64::from(n) + shift).ok()
    }
}

#[cfg(test)]
mod t {
    use super::*;

    /// The worktree of where the change starts follows it: made when there
    /// is none (or a directory that is no worktree), checked out again when
    /// the change starts elsewhere, kept as it is when it starts there still.
    #[test]
    fn the_tree_before_follows_where_the_change_starts() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let g = crate::measure::t::git;
        g(repo, &["init", "-q"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        g(repo, &["add", "-A"]);
        g(repo, &["commit", "-qm", "one"]);
        let one = git::run(repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        std::fs::write(repo.join("a"), "2").unwrap();
        g(repo, &["commit", "-qam", "two"]);
        let two = git::run(repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        std::fs::create_dir_all(repo.join(OUT).join("before")).unwrap();
        let read = |dir: &Path| std::fs::read_to_string(dir.join("a")).unwrap();
        assert_eq!(read(&checkout(repo, &one).unwrap()), "1");
        let dir = checkout(repo, &two).unwrap();
        assert_eq!(read(&dir), "2");
        std::fs::write(dir.join("kept"), "").unwrap();
        assert!(checkout(repo, &two).unwrap().join("kept").exists());
    }

    /// A measured test is the removed one by its function's place; when the
    /// measurement found no function for it, by its name's last segment.
    #[test]
    fn a_measured_test_is_the_removed_one_by_place_or_name() {
        let g = Removed { file: "src/a.rs".into(), line: 7, name: "x".into() };
        let t = |name: &str, at: Option<(&str, u32)>| crate::model::ReachTest {
            binary: "b".into(),
            name: name.into(),
            source: at.map(|(f, l)| crate::model::TestSource {
                file: f.into(),
                line: l,
                hash: String::new(),
                shape: None,
            }),
        };
        assert!(is(&t("m::x", Some(("src/a.rs", 7))), &g) && is(&t("m::x", None), &g));
        assert!(!is(&t("m::x", Some(("src/a.rs", 8))), &g) && !is(&t("m::x", Some(("src/b.rs", 7))), &g));
        assert!(!is(&t("x::y", None), &g));
    }

    /// A line outside every hunk moves by what the hunks above it add less
    /// what they delete; one a hunk keeps moves with it; one it deletes or
    /// rewrites is gone, and so is every line of a deleted file.
    #[test]
    fn a_line_moves_with_the_hunks_above_it() {
        let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -2,3 +2,4 @@\n x\n-y\n+y2\n+y3\n z\n\
                     @@ -10,0 +12,2 @@\n+n\n+m\n@@ -20,2 +22,0 @@\n-p\n-q\n\
                     diff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n";
        let m = Moves::of(patch);
        let to = |f: &str, n: u32| m.to(f, n);
        assert_eq!((to("a.rs", 1), to("a.rs", 2), to("a.rs", 3), to("a.rs", 4)), (Some(1), Some(2), None, Some(5)));
        assert_eq!((to("a.rs", 9), to("a.rs", 10), to("a.rs", 11)), (Some(10), Some(11), Some(14)));
        assert_eq!((to("a.rs", 20), to("a.rs", 21), to("a.rs", 22)), (None, None, Some(23)));
        assert_eq!((to("gone.rs", 1), to("other.rs", 7)), (None, Some(7)));
    }
}
