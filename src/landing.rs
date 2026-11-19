//! The landing: the source lines a branch adds to the branch it lands on,
//! and whether it adds a record of the mutants picked for it (ADR 0008).
//! Each record is a file of its own in `.slimfit/mutants/`, named by its
//! contents, so two branches never write the same one.

use crate::config::{CheckConfig, Config};
use crate::fault::fault;
use crate::git;
use crate::loc::{Classifier, Kind};
use crate::model::Pick;
use crate::ratchet::{Bound, PICK_THE_MUTANTS, Violation};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Where the records of picked mutants are kept, one file each.
pub const DIR: &str = ".slimfit/mutants";

/// The branch a change lands on when `slimfit.toml` names none.
pub const DEFAULT_TARGET: &str = "origin/HEAD";

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

/// A record of one change's mutants: the picks a ratcheted run ran, each
/// with why and its outcome, per suite; or, with none picked, why none was
/// worth a mutant.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Record {
    /// The working tree the record was made of: two branches' records
    /// differ, and so do their names.
    #[serde(default)]
    pub tree: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub picks: BTreeMap<String, Vec<Pick>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none: Option<String>,
}

impl Record {
    /// Its path, named by its contents, and its text.
    pub fn file(&self) -> Result<(String, String)> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        Ok((format!("{DIR}/{}.json", hash(&text)), text))
    }
}

/// How many records `.slimfit/mutants/` holds: with picks, with none (a
/// reason instead), and older ones that are neither (per-line records from
/// before picks).
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Records {
    pub with_picks: usize,
    pub without_picks: usize,
    pub older: usize,
}

/// What [`Records`] prints, as the command pages name it.
pub const RECORDS_HELP: &str = "`with_picks` (records of picked mutants run), `without_picks` (records of none \
     picked, with a reason) and `older` (records from before picks, of lines measured)";

/// Counts the records in `.slimfit/mutants/`.
pub fn records(repo: &Path) -> Result<Records> {
    let mut n = Records::default();
    let Ok(dir) = std::fs::read_dir(repo.join(DIR)) else { return Ok(n) };
    for p in dir.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "json")) {
        let r: Record = serde_json::from_str(&std::fs::read_to_string(&p)?).map_err(|e| {
            fault(
                format!("{} is not a record of picked mutants: {e}", p.display()),
                format!("restore it from git: `git checkout -- {DIR}`"),
            )
        })?;
        n.add(&r);
    }
    Ok(n)
}

impl Records {
    /// Counts `r` as the kind of record it is.
    pub fn add(&mut self, r: &Record) {
        match (r.picks.values().any(|p| !p.is_empty()), r.none.is_some()) {
            (true, _) => self.with_picks += 1,
            (false, true) => self.without_picks += 1,
            (false, false) => self.older += 1,
        }
    }
}

/// The change a landing brings: from where the branch forks from its target
/// to the working tree (untracked files included).
pub struct Landing {
    /// The branch it lands on, as named.
    pub target: String,
    /// The merge base of HEAD and the target, or the baseline's
    /// `mutants_from` when that is later.
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
    // Code from before the rule is not the change: the baseline's
    // `mutants_from`, while HEAD holds it and the target is behind it.
    let from = match crate::ratchet::load(repo)?.mutants_from {
        Some(cut) if git::is_ancestor(repo, &from, &cut) && git::is_ancestor(repo, &cut, "HEAD") => cut,
        _ => from,
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
        let totals = Classifier::new(&s, repo)?.count(&files, |f| std::fs::read_to_string(repo.join(f)).ok());
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

/// What `check` found of the landing it holds.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Held {
    /// The branch it lands on, as named.
    pub target: String,
    /// HEAD's merge base with it, or the baseline's `mutants_from` when that
    /// is later: the landing is what was added since.
    pub from: String,
    /// The source lines it adds in suites whose mutants can be picked.
    pub lines: usize,
    /// The records of picked mutants it adds.
    pub records: Vec<String>,
}

/// What a [`Held`] prints, as the command pages name it.
pub const HELD_HELP: &str = "`target` (the branch it lands on), `from` (HEAD's merge base with it, or the baseline's cut \
     point, mutants_from, when that is later), `lines` (the source lines the change adds whose mutants can be \
     picked) and `records` (the records of picked mutants it adds, in .slimfit/mutants/)";

/// Holds the landing to its mutants: per suite, a violation when it adds
/// source lines and the landing adds no record, nor did the measurement
/// run picks (`picked`: `ratchet` records them). `None`, and no violation,
/// when there is no branch it lands on.
pub fn hold(repo: &Path, cfg: &Config, picked: bool) -> Result<(Option<Held>, Vec<Violation>)> {
    let Some(l) = find(repo, &cfg.check, &git::head(repo)?)? else {
        eprintln!(
            "slimfit: slimfit.toml sets no [check] target and there is no {DEFAULT_TARGET}: the change is not held \
             to a record of its mutants (`slimfit doctor` says how to fix it)"
        );
        return Ok((None, vec![]));
    };
    let records: Vec<String> =
        l.added.keys().filter(|f| f.starts_with(&format!("{DIR}/")) && f.ends_with(".json")).cloned().collect();
    let (mut lines, mut violations) = (0, vec![]);
    for (suite, required) in required(repo, cfg, &l.added)? {
        let n: usize = required.values().map(BTreeMap::len).sum();
        lines += n;
        let Some((file, first)) = required.iter().next().filter(|_| records.is_empty() && !picked) else { continue };
        let line = first.keys().next().expect("a file listed has a line");
        violations.push(Violation {
            suite,
            metric: Bound::UnrecordedMutants,
            baseline: 0.0,
            limit: 0.0,
            current: n as f64,
            fix: format!(
                "the change adds {n} source line(s) since {} ({}), the first at {file}:{line}, and no record of its \
                 mutants: {PICK_THE_MUTANTS}",
                l.from, l.target
            ),
        });
    }
    Ok((Some(Held { target: l.target, from: l.from, lines, records }), violations))
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

    /// FNV-1a 64, as published: a record's name is the hash of its text.
    #[test]
    fn a_text_hashes_as_fnv_1a() {
        assert_eq!((hash(""), hash("a")), ("cbf29ce484222325".into(), "af63dc4c8601ec8c".into()));
    }

    /// The lines a unified diff adds, counted through context, removals
    /// that look like headers, and several hunks and files.
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

    /// Lines added to a crate reached only through `[dev-dependencies]` are
    /// test lines: no mutant is owed for them; a prod crate's lines are.
    #[test]
    fn a_test_crate_owes_no_mutants() {
        let d = tempfile::tempdir().unwrap();
        crate::workspace::t::workspace(d.path());
        let added: Lines = ["crates/support/src/lib.rs", "crates/core/src/lib.rs"]
            .map(|f| {
                crate::workspace::t::write(d.path(), &[(f, "fn a() {}\n")]);
                (f.to_string(), [(1, "fn a() {}".to_string())].into())
            })
            .into();
        let cfg: Config = toml::from_str("[[suite]]\nname = \"rust\"\npreset = \"rust\"\n").unwrap();
        let got = required(d.path(), &cfg, &added).unwrap();
        assert_eq!(got["rust"].keys().collect::<Vec<_>>(), ["crates/core/src/lib.rs"]);
    }
}
