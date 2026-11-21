//! The landing: the source lines a branch adds to the branch it lands on,
//! and whether it adds a record of the mutants picked for it (ADR 0008).
//! Each record is a file of its own in `.slimfit/mutants/`, named by its
//! contents, so two branches never write the same one.

use crate::config::{CheckConfig, Config, Suite};
use crate::fault::fault;
use crate::git;
use crate::loc::{Classifier, FileLines, Kind, Totals};
use crate::measure::Resolver;
use crate::model::{Measurement, MutantStatus, Pick};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
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

/// Where each pick's first outcome on the change waits for its record: a
/// survivor its author then kills reads `killed` in the run the record is
/// made of, and only this keeps that it survived first. In `.slimfit/out`:
/// of this working copy, never committed.
pub const FIRSTS: &str = ".slimfit/out/first-outcomes.json";

/// The first outcome of each mutant picked since the last record, by suite
/// and name.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Firsts(BTreeMap<String, BTreeMap<String, MutantStatus>>);

impl Firsts {
    fn load(repo: &Path) -> Self {
        let text = std::fs::read_to_string(repo.join(FIRSTS)).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// The first outcome of `name` in `suite`: by its name, or else by its
    /// file and description when one mutant alone has them, since an edit
    /// above it (the test that kills it) moves its line and column.
    fn of(&self, suite: &str, name: &str) -> Option<MutantStatus> {
        let seen = self.0.get(suite)?;
        if let Some(s) = seen.get(name) {
            return Some(*s);
        }
        // A name is `file:line:column: description`.
        fn place(n: &str) -> Option<(&str, &str)> {
            let p: Vec<&str> = n.splitn(4, ':').collect();
            Some((p.first().copied()?, p.get(3).copied()?))
        }
        let at = place(name)?;
        let mut same = seen.iter().filter(|(n, _)| place(n) == Some(at)).map(|(_, s)| *s);
        match (same.next(), same.next()) {
            (Some(s), None) => Some(s),
            _ => None,
        }
    }
}

/// Keeps the outcome of each pick `m` ran that is the first seen of its
/// mutant since the last record.
pub fn remember_firsts(repo: &Path, m: &Measurement) -> Result<()> {
    // Whether a mutant was seen is asked of what was kept before this run: two of its picks are two mutants.
    let before = Firsts::load(repo);
    let mut firsts = Firsts::load(repo);
    for s in &m.suites {
        for p in s.mutated.iter().flat_map(|d| &d.picks) {
            let Some(outcome) = p.outcome.filter(|_| before.of(&s.suite, &p.name).is_none()) else { continue };
            firsts.0.entry(s.suite.clone()).or_default().insert(p.name.clone(), outcome);
        }
    }
    std::fs::create_dir_all(repo.join(FIRSTS).parent().expect("a directory"))?;
    std::fs::write(repo.join(FIRSTS), serde_json::to_string_pretty(&firsts)?)?;
    Ok(())
}

impl Record {
    /// Sets each pick's `first_outcome`: the first seen of its mutant since
    /// the last record, else the outcome it holds.
    pub fn with_firsts(mut self, repo: &Path) -> Self {
        let firsts = Firsts::load(repo);
        for (suite, picks) in &mut self.picks {
            for p in picks {
                p.first_outcome = firsts.of(suite, &p.name).or(p.outcome);
            }
        }
        self
    }
}

/// Forgets the first outcomes once their record is written: the next
/// change's picks start afresh.
pub fn forget_firsts(repo: &Path) -> Result<()> {
    match std::fs::remove_file(repo.join(FIRSTS)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
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
    /// The files it adds, changes or deletes.
    pub touched: Vec<String>,
}

/// The working tree as a command found it, read once, and what is found of
/// it once when first asked for: its landing, and each suite's view of it.
/// Each git spawn costs CPU time, which `test_seconds` counts (ADR 0010).
/// A command that writes code reads its tree after it writes.
pub struct Tree {
    pub wt: git::Worktree,
    landing: OnceCell<Option<Landing>>,
    suites: OnceCell<BTreeMap<String, OnceCell<SuiteTree>>>,
}

/// One suite's view of the working tree: which files it counts, of what
/// kind, their lines, and where a tool's path for one of them resolves.
pub struct SuiteTree {
    pub suite: Suite,
    classifier: Classifier,
    pub totals: Totals,
    pub resolver: Resolver,
}

impl SuiteTree {
    /// `path`'s kind in the suite (relative to the repository root), whether
    /// or not it is in the tree; `None` when the suite does not count it.
    pub fn kind(&self, path: &str) -> Option<Kind> {
        self.classifier.kind(path)
    }

    /// The lines of `file`, when it is a source file of the tree.
    pub fn source_lines(&self, file: &str) -> Option<&FileLines> {
        self.totals.files.get(file).filter(|(k, _)| *k == Kind::Source).map(|(_, l)| l)
    }

    /// The source lines `file` would have with `text`: 0 unless it is a
    /// source file of the suite.
    pub fn prod(&self, file: &str, text: &str) -> u64 {
        self.classifier.file(file, text).filter(|(k, _)| *k == Kind::Source).map_or(0, |(_, l)| l.prod)
    }
}

impl Tree {
    pub fn read(head: &git::Head) -> Result<Tree> {
        Ok(Tree::of(git::worktree(head)?))
    }

    fn of(wt: git::Worktree) -> Tree {
        Tree { wt, landing: Default::default(), suites: Default::default() }
    }

    /// The tree of `files` under `top`, with no repository: a test's sample
    /// directory.
    #[cfg(test)]
    pub fn at(top: &Path, files: Vec<String>) -> Tree {
        Tree::of(git::Worktree::of(top, files))
    }

    /// [`find`], of this tree.
    pub fn landing(&self, repo: &Path, check: &CheckConfig) -> Result<Option<&Landing>> {
        if let Some(l) = self.landing.get() {
            return Ok(l.as_ref());
        }
        let found = find(repo, check, &self.wt.head)?;
        Ok(self.landing.get_or_init(|| found).as_ref())
    }

    /// The suite of `cfg` named `name`, resolved, and its view of this tree,
    /// built when first asked for.
    pub fn suite(&self, cfg: &Config, name: &str) -> Result<&SuiteTree> {
        let suites = self.suites.get_or_init(|| cfg.suites.iter().map(|s| (s.name.clone(), OnceCell::new())).collect());
        let Some((sc, cell)) = cfg.suites.iter().find(|s| s.name == name).zip(suites.get(name)) else {
            anyhow::bail!("no suite {name} in slimfit.toml");
        };
        if let Some(s) = cell.get() {
            return Ok(s);
        }
        let (repo, files) = (self.wt.head.top.as_path(), &self.wt.files);
        let suite = sc.resolve();
        let read = |f: &str| std::fs::read_to_string(repo.join(f)).ok();
        let classifier = Classifier::new(&suite, repo, files, read)?;
        let totals = classifier.count(files, read);
        let resolver = Resolver::new(repo, &totals);
        Ok(cell.get_or_init(|| SuiteTree { suite, classifier, totals, resolver }))
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
    Ok(Some(Landing { target, from, to, added: added_lines(&patch, ""), touched: touched(&patch) }))
}

/// The files a unified diff made with `--no-renames` touches: each named
/// on its `diff --git a/<path> b/<path>` line, the two paths being one.
fn touched(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| l.strip_prefix("diff --git a/"))
        .filter_map(|both| both.get(..both.len().checked_sub(3)? / 2))
        .map(String::from)
        .collect()
}

/// Of `added`, the lines a suite's mutants run must measure, per suite whose
/// tool can be limited to a change (no other runs mutants): its source lines.
pub fn required(cfg: &Config, tree: &Tree, added: &Lines) -> Result<BTreeMap<String, Lines>> {
    let mut out = BTreeMap::new();
    for sc in &cfg.suites {
        let s = tree.suite(cfg, &sc.name)?;
        if !s.suite.mutates() {
            continue;
        }
        let mut lines = Lines::new();
        for (f, added) in added {
            let Some(l) = s.source_lines(f) else { continue };
            let wanted: BTreeMap<u32, String> =
                added.iter().filter(|(n, _)| l.is_source_line(**n)).map(|(n, t)| (*n, t.clone())).collect();
            if !wanted.is_empty() {
                lines.insert(f.clone(), wanted);
            }
        }
        out.insert(sc.name.clone(), lines);
    }
    Ok(out)
}

/// What `check` found of the landing it holds.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
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
    /// The source lines it adds net, in all suites (ADR 0012).
    pub growth: i64,
    /// The growth records it adds.
    pub growth_records: Vec<String>,
    /// Per suite whose source lines it changes, how: what `next` names. Not printed.
    #[serde(skip)]
    pub grown: BTreeMap<String, crate::growth::Grown>,
    /// Whether a growth record it adds names a reviewer other than its
    /// author: what clears `unrecorded_growth`. Not printed.
    #[serde(skip)]
    pub growth_reviewed: bool,
    /// Every line it adds, by file: what `check` names the change's new
    /// uncovered spans from. Not printed.
    #[serde(skip)]
    pub added: Lines,
    /// Per suite that adds source lines with no record that holds them:
    /// what `unrecorded_mutants` measures. Not printed.
    #[serde(skip)]
    pub unrecorded: BTreeMap<String, Unrecorded>,
}

/// The source lines a suite adds that no record holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Unrecorded {
    pub lines: usize,
    /// The first of them, as `file:line`.
    pub first: String,
}

/// What a [`Held`] prints, as the command pages name it.
pub const HELD_HELP: &str = "`target` (the branch it lands on), `from` (HEAD's merge base with it, or the baseline's cut \
     point, mutants_from, when that is later), `lines` (the source lines the change adds whose mutants can be \
     picked), `records` (the records of picked mutants it adds, in .slimfit/mutants/), `growth` (the source lines it \
     adds net, in all suites: in the files it touches, their lines now less their lines where the change starts) and `growth_records` (the \
     growth records it adds, in .slimfit/growth/)";

/// Holds the landing to its records: its growth (ADR 0012), and its
/// mutants: per suite that adds source lines, it
/// is unrecorded when the landing adds no record that names a picker other
/// than its author, nor did the measurement run picks (`picked`: `ratchet`
/// records them, and refuses a record without such a picker). `None` when
/// there is no branch it lands on.
pub fn hold(repo: &Path, cfg: &Config, tree: &Tree, picked: bool) -> Result<Option<Held>> {
    let Some(l) = tree.landing(repo, &cfg.check)? else {
        eprintln!(
            "slimfit: slimfit.toml sets no [check] target and there is no {DEFAULT_TARGET}: the change is not held \
             to a record of its mutants (`slimfit doctor` says how to fix it)"
        );
        return Ok(None);
    };
    let added_in = |dir: &str| -> Vec<String> {
        l.added.keys().filter(|f| f.starts_with(&format!("{dir}/")) && f.ends_with(".json")).cloned().collect()
    };
    let (records, growth_records) = (added_in(DIR), added_in(crate::growth::DIR));
    // A record that does not read names no picker (nor reviewer): the gate does not refuse it, it just does not count.
    let text = |f: &String| std::fs::read_to_string(repo.join(f)).unwrap_or_default();
    let named = |f: &String| serde_json::from_str::<Record>(&text(f)).is_ok_and(|r| r.picked_by_another());
    let reviewed =
        |f: &String| serde_json::from_str::<crate::growth::Record>(&text(f)).is_ok_and(|r| r.reviewed_by_another());
    let held = picked || records.iter().any(named);
    let grown = crate::growth::grown(repo, cfg, tree, l)?;
    let (mut lines, mut unrecorded) = (0, BTreeMap::new());
    for (suite, required) in required(cfg, tree, &l.added)? {
        let n: usize = required.values().map(BTreeMap::len).sum();
        lines += n;
        let Some((file, first)) = required.iter().next().filter(|_| !held) else { continue };
        let line = first.keys().next().expect("a file listed has a line");
        unrecorded.insert(suite, Unrecorded { lines: n, first: format!("{file}:{line}") });
    }
    Ok(Some(Held {
        target: l.target.clone(),
        from: l.from.clone(),
        lines,
        records,
        growth: grown.values().map(|g| g.lines).sum(),
        growth_reviewed: growth_records.iter().any(reviewed),
        growth_records,
        grown,
        added: l.added.clone(),
        unrecorded,
    }))
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

    /// A pick's first outcome is found by its name, or, once an edit above
    /// it moved its line, by its file and description when one mutant alone
    /// has them; a later run does not replace it.
    #[test]
    fn a_first_outcome_follows_its_mutant_down_the_file() {
        use crate::model::{Mutated, SuiteMeasurement};
        let d = tempfile::tempdir().unwrap();
        let ran = |picks: &[(&str, MutantStatus)]| {
            let picks = picks
                .iter()
                .map(|(n, s)| Pick { name: n.to_string(), why: String::new(), outcome: Some(*s), first_outcome: None })
                .collect();
            let s = SuiteMeasurement {
                suite: "s".into(),
                metrics: Default::default(),
                tests: vec![],
                uncovered: vec![],
                mutants: vec![],
                reach: None,
                mutated: Some(Mutated { picks, change: Default::default() }),
                command_seconds: 0.0,
                untimed: false,
            };
            remember_firsts(d.path(), &Measurement { tree: String::new(), with_mutants: true, suites: vec![s] })
                .unwrap();
        };
        use MutantStatus::*;
        let g = |line: u32| format!("a.rs:{line}:9: replace + with - in g");
        ran(&[("a.rs:3:5: replace f -> bool with true", Survived), (&g(7), Survived), (&g(8), Killed)]);
        ran(&[("a.rs:4:5: replace f -> bool with true", Killed)]);
        let firsts = Firsts::load(d.path());
        assert_eq!(firsts.of("s", "a.rs:4:5: replace f -> bool with true"), Some(Survived));
        assert_eq!(firsts.of("s", &g(8)), Some(Killed));
        // Two mutants of that file and description: neither is known to be the moved one.
        assert_eq!(firsts.of("s", &g(9)), None);
        assert_eq!(firsts.of("other", "a.rs:3:5: replace f -> bool with true"), None);
        forget_firsts(d.path()).unwrap();
        forget_firsts(d.path()).unwrap();
        assert!(Firsts::load(d.path()).0.is_empty());
    }

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

    /// A suite's view of the tree is built once per command: asked again, the
    /// tree answers the same view, though a file changed since. A suite the
    /// config does not name is refused.
    #[test]
    fn a_suites_view_is_built_once() {
        let d = tempfile::tempdir().unwrap();
        crate::workspace::t::write(d.path(), &[("src/a.rs", "fn a() {}\n")]);
        let tree = Tree::at(d.path(), vec!["src/a.rs".into()]);
        let cfg = Config::one("rust", crate::config::Preset::Rust);
        let first = tree.suite(&cfg, "rust").unwrap();
        assert_eq!((first.kind("src/a.rs"), first.totals.prod), (Some(Kind::Source), 1));
        crate::workspace::t::write(d.path(), &[("src/a.rs", "fn a() {}\nfn b() {}\n")]);
        let again = tree.suite(&cfg, "rust").unwrap();
        assert!(std::ptr::eq(first, again));
        assert_eq!(again.source_lines("src/a.rs").map(|l| l.prod), Some(1));
        assert!(tree.suite(&cfg, "go").is_err());
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
        let got = required(&cfg, &crate::measure::t::tree(d.path()), &added).unwrap();
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
        let got = required(&cfg, &crate::measure::t::tree(d.path()), &added).unwrap();
        assert_eq!(got["rust"], [("src/a.rs".to_string(), [line(3, "fn a() {}")].into())].into());
    }
}
