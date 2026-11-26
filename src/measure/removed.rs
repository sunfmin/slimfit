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
use std::path::Path;

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

/// The measurement of the tree the change starts from, with `cfg`'s
/// suites: measured once, in a tree made for it and removed as soon as it
/// is taken, and kept in `.slimfit/out/before.json` while `from` and the
/// suites are the same.
fn before(repo: &Path, cfg: &Config, from: &str) -> Result<Measurement> {
    let key = serde_json::to_value((from, &cfg.suites))?;
    kept(&repo.join(OUT).join("before.json"), key, || {
        eprintln!(
            "slimfit: the change removes tests: measuring the tree it starts from, {from}, for the lines they ran"
        );
        let head = git::run(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
        let change = match head.trim() {
            "HEAD" | "" => {
                format!("HEAD {}", git::run(repo, &["rev-parse", "--short", "HEAD"]).unwrap_or_default().trim())
            }
            b => b.to_string(),
        };
        let what = format!(
            "slimfit: the tree {change} starts from, to measure the tests it removes; removed when the measurement is taken"
        );
        let commands = cfg.check.before_tree(|v| std::env::var(v).ok());
        in_tree(repo, from, &what, &commands, |dir| {
            let tree = Tree::read(&git::open(dir)?)?;
            super::current(dir, &tree, cfg, &Options::default())
        })
    })
}

/// What `cache` holds for `key`, or else `make`'s, written there with it.
fn kept<T: serde::Serialize + serde::de::DeserializeOwned>(
    cache: &Path,
    key: serde_json::Value,
    make: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let held: Option<(serde_json::Value, T)> =
        std::fs::read_to_string(cache).ok().and_then(|t| serde_json::from_str(&t).ok());
    if let Some((_, v)) = held.filter(|(k, _)| *k == key) {
        return Ok(v);
    }
    let v = make()?;
    std::fs::write(cache, serde_json::to_string(&(key, &v))?)?;
    Ok(v)
}

/// `f` of the tree at `rev`, made by the first of `commands` and removed by
/// the second once `f` is done, however it ends. Each runs in `sh` in
/// `repo`, given the tree's path (`$SLIMFIT_TREE`), `rev` (`$SLIMFIT_REV`)
/// and `what` it is for (`$SLIMFIT_DESCRIPTION`); the path is
/// `.slimfit/out/before/` unless the make command prints another, as its
/// last line.
fn in_tree<T>(
    repo: &Path,
    rev: &str,
    what: &str,
    commands: &(String, String),
    f: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let (make, remove) = commands;
    // One of the tree's commands, given the tree at `tree`; its stdout.
    let sh = |command: &str, tree: &Path, label: &str| -> Result<String> {
        let out = std::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(repo)
            .env("SLIMFIT_TREE", tree)
            .env("SLIMFIT_REV", rev)
            .env("SLIMFIT_DESCRIPTION", what)
            .stderr(std::process::Stdio::inherit())
            .output()?;
        if !out.status.success() {
            return Err(crate::fault::fault(
                format!("the command to {label} the tree the change starts from failed ({}): {command}", out.status),
                "fix `[check] before_make`/`before_remove` in slimfit.toml (or $SLIMFIT_BEFORE_MAKE/$SLIMFIT_BEFORE_REMOVE), or unset them for git's worktree",
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let asked = repo.join(OUT).join("before");
    let _ = std::fs::remove_dir_all(&asked);
    std::fs::create_dir_all(repo.join(OUT))?;
    let printed = sh(make, &asked, "make")?;
    let dir = printed.lines().map(str::trim).rfind(|l| !l.is_empty()).map_or(asked, |p| repo.join(p));
    let out = f(&dir);
    if let Err(e) = sh(remove, &dir, "remove") {
        eprintln!("slimfit: {e:#}");
    }
    out
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
    use crate::config::CheckConfig;

    /// A repository of two commits of `a`, "1" then "2"; the first's rev.
    fn two_commits(repo: &Path) -> String {
        let g = crate::measure::t::git;
        g(repo, &["init", "-q"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        g(repo, &["add", "-A"]);
        g(repo, &["commit", "-qm", "one"]);
        let one = git::run(repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        std::fs::write(repo.join("a"), "2").unwrap();
        g(repo, &["commit", "-qam", "two"]);
        one
    }

    /// Unset, the tree is a worktree of git's own at the rev, made over a
    /// directory left there, and removed once measured, even when the
    /// measurement fails.
    #[test]
    fn unset_the_tree_is_a_git_worktree_removed_once_measured() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let one = two_commits(repo);
        std::fs::create_dir_all(repo.join(OUT).join("before")).unwrap();
        let git_only = CheckConfig::default().before_tree(|_| None);
        let mut at = None;
        let read = in_tree(repo, &one, "w", &git_only, |dir| {
            at = Some(dir.to_path_buf());
            Ok(std::fs::read_to_string(dir.join("a"))?)
        });
        assert_eq!(read.unwrap(), "1");
        assert!(!at.unwrap().exists());
        let failed = in_tree(repo, &one, "w", &git_only, |_| -> Result<()> { anyhow::bail!("no") });
        assert!(failed.is_err() && !repo.join(OUT).join("before").exists());
        assert_eq!(git::run(repo, &["worktree", "list"]).unwrap().lines().count(), 1);
    }

    /// Set, the given commands make and remove the tree, each given its
    /// path, rev and description, and the path the make command prints is
    /// the tree; git makes no worktree.
    #[test]
    fn set_the_given_commands_make_and_remove_the_tree() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let one = two_commits(repo);
        let log = repo.join("calls");
        let call = |what: &str| format!("echo \"{what} $SLIMFIT_TREE|$SLIMFIT_REV|$SLIMFIT_DESCRIPTION\" >> calls");
        let make = format!(
            "{} && mkdir -p elsewhere && git show \"$SLIMFIT_REV:a\" > elsewhere/a && echo elsewhere",
            call("make")
        );
        let remove = format!("{} && rm -r \"$SLIMFIT_TREE\"", call("remove"));
        let set = CheckConfig { before_make: Some(make), before_remove: Some(remove), ..Default::default() };
        let read = in_tree(repo, &one, "the tree", &set.before_tree(|_| None), |dir| {
            Ok(std::fs::read_to_string(dir.join("a"))?)
        });
        assert_eq!(read.unwrap(), "1");
        let asked = repo.join(OUT).join("before");
        let made = repo.join("elsewhere");
        let want = format!("make {}|{one}|the tree\nremove {}|{one}|the tree\n", asked.display(), made.display());
        assert_eq!(std::fs::read_to_string(log).unwrap(), want);
        assert!(!made.exists());
        assert_eq!(git::run(repo, &["worktree", "list"]).unwrap().lines().count(), 1);
    }

    /// The measurement is kept while its key is the same, and taken again
    /// when it changes.
    #[test]
    fn the_measurement_is_kept_while_its_key_is_the_same() {
        let d = tempfile::tempdir().unwrap();
        let cache = d.path().join("before.json");
        let k = |s: &str| serde_json::json!([s]);
        assert_eq!(kept(&cache, k("a"), || Ok(1)).unwrap(), 1);
        assert_eq!(kept(&cache, k("a"), || -> Result<u32> { anyhow::bail!("measured again") }).unwrap(), 1);
        assert_eq!(kept(&cache, k("b"), || Ok(2)).unwrap(), 2);
    }

    /// Each command is taken from its environment variable, then
    /// slimfit.toml, then git's worktree.
    #[test]
    fn the_commands_come_from_the_environment_then_the_file_then_git() {
        let file = CheckConfig { before_make: Some("m".into()), ..Default::default() };
        let env = |v: &str| (v == crate::tools::BEFORE_REMOVE_ENV).then(|| "r".to_string());
        let (make, remove) = file.before_tree(env);
        assert_eq!((make.as_str(), remove.as_str()), ("m", "r"));
        let (make, remove) = CheckConfig::default().before_tree(|_| None);
        assert!(make.contains("git worktree add") && remove.contains("git worktree remove"));
        let (make, _) = file.before_tree(|_| Some("e".into()));
        assert_eq!(make, "e");
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
