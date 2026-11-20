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
/// worth a mutant. Either way, who picked and who made the change, as the
/// caller names them (slimfit#58): the gate holds only that they differ.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Record {
    /// The working tree the record was made of: two branches' records
    /// differ, and so do their names.
    #[serde(default)]
    pub tree: String,
    /// The agent that picked: an opaque id its caller gives (a session id,
    /// with harness and model). Empty in records from before slimfit#58.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub picker: String,
    /// The agent that made the change, named as `picker` is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub picks: BTreeMap<String, Vec<Pick>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none: Option<String>,
}

impl Record {
    /// Whether it names a picker other than the change's author: what the
    /// gate holds of a record, compared as given, never judged.
    pub fn picked_by_another(&self) -> bool {
        let (p, a) = (self.picker.trim(), self.author.trim());
        !p.is_empty() && !a.is_empty() && p != a
    }

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

/// The working tree as a command found it, read once, and its landing,
/// found once when first asked for: each git spawn costs CPU time, which
/// `test_seconds` counts (ADR 0010). A command that writes code reads its
/// tree after it writes.
pub struct Tree {
    pub wt: git::Worktree,
    landing: std::cell::OnceCell<Option<Landing>>,
}

impl Tree {
    pub fn read(head: &git::Head) -> Result<Tree> {
        Ok(Tree { wt: git::worktree(head)?, landing: Default::default() })
    }

    /// [`find`], of this tree.
    pub fn landing(&self, repo: &Path, check: &CheckConfig) -> Result<Option<&Landing>> {
        if let Some(l) = self.landing.get() {
            return Ok(l.as_ref());
        }
        let found = find(repo, check, &self.wt.head)?;
        Ok(self.landing.get_or_init(|| found).as_ref())
    }
}

/// The landing of the working tree, or `None` when there is no target to
/// take it from: `[check] target` unset and no `origin/HEAD`. A target set
/// in `slimfit.toml` that is not found is refused.
fn find(repo: &Path, check: &CheckConfig, head: &git::Head) -> Result<Option<Landing>> {
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
/// tool can be limited to a change (no other runs mutants): its source lines.
/// `tree` is the working tree's files, as [`git::Worktree`] lists them.
pub fn required(repo: &Path, cfg: &Config, tree: &[String], added: &Lines) -> Result<BTreeMap<String, Lines>> {
    let files: Vec<String> = added.keys().cloned().collect();
    let mut out = BTreeMap::new();
    for s in cfg.suites.iter().map(|s| s.resolve()) {
        if !s.mutates() {
            continue;
        }
        let totals =
            Classifier::of_worktree(&s, repo, tree)?.count(&files, |f| std::fs::read_to_string(repo.join(f)).ok());
        let mut lines = Lines::new();
        for (f, (_, l)) in totals.files.iter().filter(|(_, (k, _))| *k == Kind::Source) {
            let wanted = added[f].iter().filter(|(n, _)| l.is_source_line(**n));
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
    /// Every line it adds, by file: what `check` names the change's new
    /// uncovered spans from. Not printed.
    #[serde(skip)]
    pub added: Lines,
}

/// What a [`Held`] prints, as the command pages name it.
pub const HELD_HELP: &str = "`target` (the branch it lands on), `from` (HEAD's merge base with it, or the baseline's cut \
     point, mutants_from, when that is later), `lines` (the source lines the change adds whose mutants can be \
     picked) and `records` (the records of picked mutants it adds, in .slimfit/mutants/)";

/// Holds the landing to its mutants: per suite, a violation when it adds
/// source lines and the landing adds no record that names a picker other
/// than its author, nor did the measurement run picks (`picked`: `ratchet`
/// records them, and refuses a record without such a picker). `None`, and no violation,
/// when there is no branch it lands on.
pub fn hold(repo: &Path, cfg: &Config, tree: &Tree, picked: bool) -> Result<(Option<Held>, Vec<Violation>)> {
    let Some(l) = tree.landing(repo, &cfg.check)? else {
        eprintln!(
            "slimfit: slimfit.toml sets no [check] target and there is no {DEFAULT_TARGET}: the change is not held \
             to a record of its mutants (`slimfit doctor` says how to fix it)"
        );
        return Ok((None, vec![]));
    };
    let records: Vec<String> =
        l.added.keys().filter(|f| f.starts_with(&format!("{DIR}/")) && f.ends_with(".json")).cloned().collect();
    // A record that does not read names no picker: the gate does not refuse it, it just does not count.
    let named = |f: &String| {
        let text = std::fs::read_to_string(repo.join(f)).unwrap_or_default();
        serde_json::from_str::<Record>(&text).is_ok_and(|r| r.picked_by_another())
    };
    let held = picked || records.iter().any(named);
    let missing = if records.is_empty() {
        "no record of its mutants"
    } else {
        "no record of its mutants that names a picker other than its author"
    };
    let (mut lines, mut violations) = (0, vec![]);
    for (suite, required) in required(repo, cfg, &tree.wt.files, &l.added)? {
        let n: usize = required.values().map(BTreeMap::len).sum();
        lines += n;
        let Some((file, first)) = required.iter().next().filter(|_| !held) else { continue };
        let line = first.keys().next().expect("a file listed has a line");
        violations.push(Violation {
            suite,
            metric: Bound::UnrecordedMutants,
            baseline: 0.0,
            limit: 0.0,
            current: n as f64,
            fix: format!(
                "the change adds {n} source line(s) since {} ({}), the first at {file}:{line}, and {missing}: \
                 {PICK_THE_MUTANTS}",
                l.from, l.target
            ),
        });
    }
    Ok((
        Some(Held { target: l.target.clone(), from: l.from.clone(), lines, records, added: l.added.clone() }),
        violations,
    ))
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
        crate::measure::t::git(d.path(), &["init", "-q"]);
        crate::workspace::t::workspace(d.path());
        let added: Lines = ["crates/support/src/lib.rs", "crates/core/src/lib.rs"]
            .map(|f| {
                crate::workspace::t::write(d.path(), &[(f, "fn a() {}\n")]);
                (f.to_string(), [(1, "fn a() {}".to_string())].into())
            })
            .into();
        let cfg: Config = toml::from_str("[[suite]]\nname = \"rust\"\npreset = \"rust\"\n").unwrap();
        let got =
            required(d.path(), &cfg, &git::worktree(&git::open(d.path()).unwrap()).unwrap().files, &added).unwrap();
        assert_eq!(got["rust"].keys().collect::<Vec<_>>(), ["crates/core/src/lib.rs"]);
    }

    /// A file declared `#[cfg(test)] mod x;` is test code, though the change
    /// leaves the declaring file alone: its lines owe no mutants. Only a
    /// prod file's source lines do, not its blanks or `//` comments.
    #[test]
    fn a_test_module_file_owes_no_mutants() {
        let d = tempfile::tempdir().unwrap();
        crate::measure::t::git(d.path(), &["init", "-q"]);
        crate::workspace::t::write(
            d.path(),
            &[
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
                ("src/main.rs", "mod a;\n#[cfg(test)]\nmod fixtures;\nfn main() {}\n"),
                ("src/fixtures.rs", "fn f() {}\n"),
                ("src/a.rs", "// c\n\nfn a() {}\n"),
            ],
        );
        let line = |n: u32, t: &str| (n, t.to_string());
        let added: Lines = [
            ("src/fixtures.rs".to_string(), [line(1, "fn f() {}")].into()),
            ("src/a.rs".to_string(), [line(1, "// c"), line(2, ""), line(3, "fn a() {}")].into()),
        ]
        .into();
        let cfg: Config = toml::from_str("[[suite]]\nname = \"rust\"\npreset = \"rust\"\n").unwrap();
        let got =
            required(d.path(), &cfg, &git::worktree(&git::open(d.path()).unwrap()).unwrap().files, &added).unwrap();
        assert_eq!(got["rust"], [("src/a.rs".to_string(), [line(3, "fn a() {}")].into())].into());
    }
}
