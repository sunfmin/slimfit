//! The landing: the source lines a branch adds to the branch it lands on,
//! and which of them a ratcheted mutants run measured (ADR 0008). Each such
//! run leaves a record of its own in `.slimfit/mutants/`, so branches that
//! measure their own changes never write the same file.

use crate::config::{CheckConfig, Config};
use crate::fault::fault;
use crate::git;
use crate::loc::{Classifier, Kind};
use crate::model::Measurement;
use crate::ratchet::{Bound, MEASURE_THE_CHANGE, Violation};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Where the records of ratcheted mutants runs are kept, one file each.
pub const DIR: &str = ".slimfit/mutants";

/// The branch a change lands on when `slimfit.toml` names none.
pub const DEFAULT_TARGET: &str = "origin/HEAD";

/// Lines by file (repository relative), each by the hash of its text.
pub type Hashes = BTreeMap<String, BTreeSet<String>>;

/// Lines by file (repository relative): number → text.
pub type Lines = BTreeMap<String, BTreeMap<u32, String>>;

/// FNV-1a 64 of `text`, in hex.
pub fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// `lines`, each by its text's hash.
pub fn hashes(lines: &Lines) -> Hashes {
    lines.iter().map(|(f, l)| (f.clone(), l.values().map(|t| hash(t)).collect())).collect()
}

/// A record of one ratcheted mutants run: the lines it measured.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Record {
    pub lines: Hashes,
}

/// The record of `measured`: its path, named by its contents so two
/// branches never write the same one, and its text.
pub fn record(measured: &Hashes) -> Result<(String, String)> {
    let text = serde_json::to_string_pretty(&Record { lines: measured.clone() })? + "\n";
    Ok((format!("{DIR}/{}.json", hash(&text)), text))
}

/// Every line some ratcheted mutants run measured: the union of the records.
pub fn recorded(repo: &Path) -> Result<Hashes> {
    let mut all = Hashes::new();
    let Ok(dir) = std::fs::read_dir(repo.join(DIR)) else { return Ok(all) };
    let mut paths: Vec<_> = dir.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths.into_iter().filter(|p| p.extension().is_some_and(|e| e == "json")) {
        let text = std::fs::read_to_string(&p)?;
        let r: Record = serde_json::from_str(&text).map_err(|e| {
            fault(
                format!("{} is not a record of measured lines: {e}", p.display()),
                format!("restore it from git: `git checkout -- {DIR}`"),
            )
        })?;
        for (f, h) in r.lines {
            all.entry(f).or_default().extend(h);
        }
    }
    Ok(all)
}

/// The change a landing brings: from where the branch forks from its target
/// to the working tree (untracked files included).
pub struct Landing {
    /// The branch it lands on, as named.
    pub target: String,
    /// The merge base of HEAD and the target.
    pub from: String,
    /// The working tree, written as a tree object.
    pub to: String,
    /// The lines it adds.
    pub added: Lines,
}

/// The landing of the working tree, or `None` when there is no target to
/// take it from: `[check] target` unset and no `origin/HEAD`. A target set
/// in `slimfit.toml` that is not found is refused.
pub fn find(repo: &Path, check: &CheckConfig, head: &git::Head) -> Result<Option<Landing>> {
    let target = check.target.clone().unwrap_or_else(|| DEFAULT_TARGET.into());
    let Some(from) = git::merge_base(repo, &target) else {
        if check.target.is_none() {
            return Ok(None);
        }
        return Err(fault(
            format!("slimfit.toml's [check] target {target} shares no commit with HEAD (or is not in this repository)"),
            "fetch it (`git fetch`), or set `target` to the branch changes land on",
        ));
    };
    let to = git::write_tree(repo, head)?;
    let patch = git::diff(repo, &from, &to, ".", &[])?;
    Ok(Some(Landing { target, from, to, added: added_lines(&patch, "") }))
}

/// Of `added`, the lines a suite's mutants run must measure, per suite whose
/// tool can be limited to a change (no other runs mutants): in a source file
/// and outside its inline tests, neither blank nor only a `//` comment.
pub fn required(repo: &Path, cfg: &Config, added: &Lines) -> Result<BTreeMap<String, Lines>> {
    let files: Vec<String> = added.keys().cloned().collect();
    let mut out = BTreeMap::new();
    for s in cfg.suites.iter().map(|s| s.resolve()) {
        if !s.mutates() {
            continue;
        }
        let totals = Classifier::new(&s)?.count(&files, |f| std::fs::read_to_string(repo.join(f)).ok());
        let mut lines = Lines::new();
        for (f, (_, l)) in totals.files.iter().filter(|(_, (k, _))| *k == Kind::Source) {
            let wanted = added[f].iter().filter(|(n, text)| {
                let t = text.trim();
                !t.is_empty() && !t.starts_with("//") && !l.is_test_line(**n)
            });
            let wanted: BTreeMap<u32, String> = wanted.map(|(n, t)| (*n, t.clone())).collect();
            if !wanted.is_empty() {
                lines.insert(f.clone(), wanted);
            }
        }
        out.insert(s.name.clone(), lines);
    }
    Ok(out)
}

/// The lines of `lines` whose text no record in `measured` holds for its file.
pub fn unmeasured(lines: &Lines, measured: &Hashes) -> Lines {
    lines
        .iter()
        .filter_map(|(f, l)| {
            let have = measured.get(f);
            let left: BTreeMap<u32, String> = l
                .iter()
                .filter(|(_, t)| !have.is_some_and(|h| h.contains(&hash(t))))
                .map(|(n, t)| (*n, t.clone()))
                .collect();
            (!left.is_empty()).then(|| (f.clone(), left))
        })
        .collect()
}

/// What `check` found of the landing it holds.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Held {
    /// The branch it lands on, as named.
    pub target: String,
    /// HEAD's merge base with it: the landing is what was added since.
    pub from: String,
    /// The source lines it adds that a mutants run must measure.
    pub lines: usize,
    /// Of those, the lines no ratcheted mutants run measured (nor this one).
    pub unmeasured: usize,
}

/// What a [`Held`] prints, as the command pages name it.
pub const HELD_HELP: &str = "`target` (the branch it lands on), `from` (HEAD's merge base with it), `lines` (the \
     source lines the change adds that mutants must measure) and `unmeasured` (those no ratcheted mutants run measured)";

/// Holds the landing to its mutants: per suite, a violation when it adds
/// lines that neither a record nor `m` (when it ran mutants) measured.
/// `None`, and no violation, when there is no branch it lands on.
pub fn hold(repo: &Path, cfg: &Config, m: &Measurement) -> Result<(Option<Held>, Vec<Violation>)> {
    let Some(l) = find(repo, &cfg.check, &git::head(repo)?)? else {
        eprintln!(
            "slimfit: slimfit.toml sets no [check] target and there is no {DEFAULT_TARGET}: the change's lines are \
             not held to a mutants measurement (`slimfit doctor` says how to fix it)"
        );
        return Ok((None, vec![]));
    };
    let mut measured = recorded(repo)?;
    for mutated in m.suites.iter().filter_map(|s| s.mutated.as_ref()) {
        for (f, h) in &mutated.lines {
            measured.entry(f.clone()).or_default().extend(h.iter().cloned());
        }
    }
    let (mut lines, mut left, mut violations) = (0, 0, vec![]);
    for (suite, required) in required(repo, cfg, &l.added)? {
        let missing = unmeasured(&required, &measured);
        let n: usize = missing.values().map(BTreeMap::len).sum();
        lines += required.values().map(BTreeMap::len).sum::<usize>();
        left += n;
        let Some((file, first)) = missing.iter().next() else { continue };
        let line = first.keys().next().expect("a file listed has a line");
        violations.push(Violation {
            suite,
            metric: Bound::UnmeasuredMutants,
            baseline: 0.0,
            limit: 0.0,
            current: n as f64,
            fix: format!(
                "{n} source line(s) the change adds since {} ({}) are in no ratcheted mutants run, the first at \
                 {file}:{line}: {MEASURE_THE_CHANGE}",
                l.from, l.target
            ),
        });
    }
    Ok((Some(Held { target: l.target, from: l.from, lines, unmeasured: left }), violations))
}

/// The lines each file gains in a unified diff (new-side numbers, with
/// their text), keyed by `prefix` + the diff's `b/` path.
pub fn added_lines(patch: &str, prefix: &str) -> Lines {
    let mut out = Lines::new();
    // "" while in a deleted file, which adds no line.
    // Once a hunk's new side is read, what is left of it are removals, which
    // neither a `+++ ` nor an `@@ -` header can start with.
    let (mut file, mut line, mut new_left) = (String::new(), 0u32, 0u32);
    for l in patch.lines() {
        if new_left > 0 {
            match l.as_bytes().first() {
                Some(b' ') => (new_left, line) = (new_left - 1, line + 1),
                Some(b'+') => {
                    out.entry(file.clone()).or_default().insert(line, l[1..].to_string());
                    (new_left, line) = (new_left - 1, line + 1);
                }
                _ => {}
            }
        } else if let Some(p) = l.strip_prefix("+++ ") {
            file = p.strip_prefix("b/").map(|p| format!("{prefix}{p}")).unwrap_or_default();
        } else if let Some(h) = l.strip_prefix("@@ -") {
            let count = |r: &str| -> (u32, u32) {
                let (a, n) = r.split_once(',').unwrap_or((r, "1"));
                (a.parse().unwrap_or(0), n.parse().unwrap_or(0))
            };
            let (start, n) = count(h.split(' ').nth(1).unwrap_or("").trim_start_matches('+'));
            (line, new_left) = (start, n);
        }
    }
    out.remove("");
    out
}

#[cfg(test)]
mod t {
    use super::*;

    /// The lines a unified diff adds, counted through context, removals
    /// that look like headers, and several hunks and files.
    /// FNV-1a 64, as published: a record written by one build is read by the next.
    #[test]
    fn a_line_hashes_as_fnv_1a() {
        assert_eq!((hash(""), hash("a")), ("cbf29ce484222325".into(), "af63dc4c8601ec8c".into()));
    }

    #[test]
    fn added_lines_follow_the_hunks() {
        let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,4 +1,4 @@\n x\n--- y\n+++ z\n w\n \n\
                     @@ -10 +10,2 @@\n-old\n\\ No newline at end of file\n+new\n+more\n\
                     diff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n\
                     diff --git a/n.rs b/n.rs\n--- /dev/null\n+++ b/n.rs\n@@ -0,0 +1 @@\n+n\n";
        let got = added_lines(patch, "sub/");
        let l = |v: &[(u32, &str)]| v.iter().map(|(n, t)| (*n, t.to_string())).collect::<BTreeMap<_, _>>();
        let want: Lines =
            [("sub/a.rs".into(), l(&[(2, "++ z"), (10, "new"), (11, "more")])), ("sub/n.rs".into(), l(&[(1, "n")]))]
                .into();
        assert_eq!(got, want);
    }
}
