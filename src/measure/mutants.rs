//! The change's mutants (ADR 0006-0008): what each suite's tool lists of
//! the change, the agent's picks among them, and the run of the picks
//! alone, each with the tests that ran its function. Measuring a suite
//! asks this module for one thing: run the suite's picks on its test run.

use super::{Resolver, Run, Scratch, quoted, tail, unreadable};
use crate::config::{Config, MutantTests, Suite};
use crate::fault::{self, Faults, fault};
use crate::formats;
use crate::git;
use crate::landing;
use crate::model::*;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// How much mutation testing a measurement runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Mutants {
    None,
    /// The mutants of the change the agent picked, by the names the
    /// suite's tool lists them by, each with why (ADR 0008): the tool runs
    /// only those. There is no other: no run mutates the whole codebase
    /// (ADR 0006), nor every mutant of a change.
    Picks(Vec<Pick>),
}

/// The most mutants one change may pick (ADR 0008).
pub const PICKS_MAX: usize = 5;

/// The names of the picks a measurement ran, in every suite.
pub(super) fn picked(m: &Measurement) -> BTreeSet<&str> {
    m.suites.iter().filter_map(|s| s.mutated.as_ref()).flat_map(|d| &d.picks).map(|p| p.name.as_str()).collect()
}

/// Refuses `--mutants` when a suite's tool cannot list a change's mutants
/// and run them by name: nothing runs every mutant.
pub fn refuse_unpickable(cfg: &Config) -> Result<()> {
    fault::all(cfg.suites.iter().map(|s| s.resolve()).map(|s| {
        if s.mutates() {
            return Ok(());
        }
        Err(fault(
            format!(
                "suite {}: its mutation tool cannot list a change's mutants and run them by name, so `--mutants` \
                 would run every mutant",
                s.name
            ),
            "if its tool takes a diff and a mutant's name, set the suite's `mutants_diff_flag` and \
             `mutants_pick_flag` in slimfit.toml; otherwise run `slimfit check` without `--mutants`: slimfit never \
             mutates the whole codebase",
        ))
    }))?;
    Ok(())
}

/// The change's mutants in one suite: the source lines it adds, and what
/// its tool lists of them.
#[derive(Debug, Default)]
pub(super) struct Listed {
    pub(super) lines: landing::Lines,
    pub(super) candidates: Vec<Candidate>,
}

/// The candidates the agent picks from, per suite whose tool can pick:
/// what `check --mutants` with no names answers. Nothing builds.
pub fn candidates(repo: &Path, cfg: &Config, tree: &landing::Tree) -> Result<BTreeMap<String, Vec<Candidate>>> {
    let scratch = Scratch::new(repo, false)?;
    let listed = listed(repo, cfg, tree, &scratch.0)?;
    Ok(listed.into_iter().map(|(s, l)| (s, l.candidates)).collect())
}

/// What each suite whose tool can pick lists of the change (ADR 0008): its
/// files that add a source line, as a diff the tool lists the mutants of.
/// Refuses with no branch to land on, rather than list more than the change.
fn listed(repo: &Path, cfg: &Config, tree: &landing::Tree, scratch: &Path) -> Result<BTreeMap<String, Listed>> {
    let Some(l) = tree.landing(repo, &cfg.check)? else {
        return Err(fault(
            format!(
                "no branch to take the change from: slimfit.toml sets no [check] target and the repository has \
                 no {}, so `--mutants` would list more than the change",
                landing::DEFAULT_TARGET
            ),
            "set `target` under [check] in slimfit.toml to the branch changes land on (e.g. \"origin/main\"), \
             or `git remote set-head origin --auto`",
        ));
    };
    eprintln!("slimfit: the change is what HEAD adds since its merge base with {}, {}", l.target, l.from);
    let mut out = BTreeMap::new();
    for (name, lines) in landing::required(repo, cfg, &tree.wt.files, &l.added)? {
        let listed = list_suite(repo, cfg, (&l.from, &l.to), &name, lines, scratch);
        out.insert(name.clone(), listed.map_err(|e| fault::within(&format!("suite {name}"), e))?);
    }
    Ok(out)
}

/// What one suite's tool lists of `lines`, those a diff `from` → `to` adds
/// in it: nothing, and the tool not run, when the suite adds no source line.
pub(super) fn list_suite(
    repo: &Path,
    cfg: &Config,
    (from, to): (&str, &str),
    name: &str,
    lines: landing::Lines,
    scratch: &Path,
) -> Result<Listed> {
    let suite = cfg.suites.iter().find(|s| s.name == name).expect("a configured suite").resolve();
    let files: Vec<String> = lines.keys().cloned().collect();
    if files.is_empty() {
        return Ok(Listed::default());
    }
    let patch = git::diff(repo, from, to, &suite.root, &files)?;
    let run = Run::new(repo, &suite, scratch.join(name));
    std::fs::create_dir_all(&run.out)?;
    let (diff, list) = (run.out.join("change.diff"), run.out.join("mutants.list.json"));
    std::fs::write(&diff, &patch)?;
    let command = format!(
        "{} {} {} --list --json > {}",
        suite.mutants_command,
        suite.mutants_diff_flag,
        quoted(&diff.to_string_lossy()),
        quoted(&list.to_string_lossy())
    );
    let (log, ok) = run.shell(&command, "mutants-list", &[])?;
    let listing = std::fs::read_to_string(&list).unwrap_or_default();
    let read = formats::read_mutants_list(&listing).ok().filter(|_| ok);
    let Some(mut candidates) = read else {
        return Err(fault(
            format!("its mutation tool listed no mutants of the change; the command's log ends:\n{}", tail(&log)),
            format!("fix the command's failure in its log {}", log.display()),
        ));
    };
    let prefix = run.prefix();
    candidates.iter_mut().for_each(|c| c.file = format!("{prefix}{}", c.file));
    Ok(Listed { lines, candidates })
}

/// The change's picks, per suite: each suite's share, taken as it is measured.
pub(super) struct Change(BTreeMap<String, Picked>);

impl Change {
    /// The change's mutants listed by each suite's tool, and the picks among
    /// them; refuses, before anything runs, a suite whose tool cannot pick,
    /// a name no suite lists, a pick named twice, and one that cannot run
    /// without a field deletion not picked.
    pub(super) fn pick(
        repo: &Path,
        cfg: &Config,
        tree: &landing::Tree,
        scratch: &Path,
        picks: &[Pick],
    ) -> Result<Self> {
        refuse_unpickable(cfg)?;
        choose(listed(repo, cfg, tree, scratch)?, picks).map(Self)
    }

    /// The picks of suite `name`: none when it adds no source line.
    pub(super) fn take(&mut self, name: &str) -> Picked {
        self.0.remove(name).unwrap_or_default()
    }
}

/// One suite's picks, and what its tool listed of the change.
#[derive(Debug, Default)]
pub(super) struct Picked {
    listed: Listed,
    picks: Vec<Pick>,
}

/// The picks, each in the suite whose tool lists it; refuses a name no
/// suite lists, and a pick named twice.
fn choose(listed: BTreeMap<String, Listed>, picks: &[Pick]) -> Result<BTreeMap<String, Picked>> {
    let mut out: BTreeMap<String, Picked> =
        listed.into_iter().map(|(s, listed)| (s, Picked { listed, picks: Vec::new() })).collect();
    let mut faults = Vec::new();
    for (i, p) in picks.iter().enumerate() {
        let suite = out.values_mut().find(|s| s.listed.candidates.iter().any(|c| c.name == p.name));
        match suite {
            _ if picks[..i].iter().any(|q| q.name == p.name) => faults.push(crate::fault::Fault {
                fault: format!("`{}` is picked twice", p.name),
                fix: "name each pick once".into(),
            }),
            Some(s) => s.picks.push(p.clone()),
            None => faults.push(crate::fault::Fault {
                fault: format!("no mutant of the change is named `{}`", p.name),
                fix: "pick from what `slimfit check --mutants` lists now: a name holds its line and column, so an \
                      edit above it renames it"
                    .into(),
            }),
        }
    }
    for s in out.values() {
        faults.extend(run_lines(&s.listed, &s.picks).err().into_iter().flatten());
    }
    if !faults.is_empty() {
        return Err(Faults(faults).into());
    }
    Ok(out)
}

impl Picked {
    /// Runs the picks on `tested`, this tree's test run of `suite` (which
    /// tests each mutant runs is chosen from it), and records in it the
    /// mutants the run reports and each pick's outcome.
    pub(super) fn run(&self, run: &Run, resolver: &Resolver, tested: &mut SuiteMeasurement) -> Result<()> {
        mutate_picks(run, resolver, &self.listed, &self.picks, tested, None)?.record(tested);
        Ok(())
    }
}

/// A sample's stop: its picks are its draw, `ranks` their ranks in it, and
/// once `viable` of those drawn before a pick have built (`known`: in an
/// earlier run), it is not built (see [`crate::select::Budget`]).
pub(super) struct Stop {
    pub(super) viable: usize,
    pub(super) ranks: Vec<usize>,
    pub(super) known: Vec<usize>,
}

/// Whether a run of `suite` on `tested` can [`Stop`]: its mutants run
/// through the shim.
pub(super) fn stops(suite: &Suite, tested: &SuiteMeasurement, out: &Path) -> bool {
    suite.mutants_tests == MutantTests::Reached
        && tested.reach.is_some()
        && crate::select::real_nextest(&out.join("shim")).is_some()
}

/// Runs the suite's picked mutants of the change, and reads each pick's
/// outcome. `tested` is this tree's test run: which tests each mutant runs
/// is chosen from it.
pub(super) fn mutate_picks(
    run: &Run,
    resolver: &Resolver,
    listed: &Listed,
    picks: &[Pick],
    tested: &SuiteMeasurement,
    stop: Option<&Stop>,
) -> Result<Mutation> {
    // A suite none of whose mutants is picked runs none: no `-F` would run them all.
    if picks.is_empty() {
        return Ok(Mutation { mutants: Vec::new(), mutated: Mutated::default() });
    }
    let suite = run.suite;
    let diff = run.out.join("change.diff");
    std::fs::write(&diff, run_diff(&run.root, &run.prefix(), listed, picks)?)?;
    eprintln!("slimfit: {}: running the {} picked mutant(s) of the change", suite.name, picks.len());
    let mut command =
        format!("{} {} {}", suite.mutants_command, suite.mutants_diff_flag, quoted(&diff.to_string_lossy()));
    for p in picks {
        command.push_str(&format!(" {} {}", suite.mutants_pick_flag, quoted(&anchored(&p.name))));
    }
    let env = match suite.mutants_tests {
        MutantTests::Reached => selecting(run, tested, &command, picks, stop)?,
        MutantTests::All => Vec::new(),
    };
    let reported = mutate(run, resolver, &command, &env)?;
    // A pick is the mutant the run reports at its place, as its name describes it.
    let ran = |name: &str| {
        let c = listed.candidates.iter().find(|c| c.name == name)?;
        let described = name.splitn(4, ':').nth(3).map(str::trim)?;
        reported.iter().find(|m| m.file == c.file && m.line == c.line && m.description == described)
    };
    let mutants: Vec<Mutant> = picks.iter().filter_map(|p| ran(&p.name)).cloned().collect();
    let picks = picks.iter().map(|p| Pick { outcome: ran(&p.name).map(|m| m.status), ..p.clone() }).collect();
    Ok(Mutation { mutated: Mutated { picks, change: MutantCounts::of(&mutants) }, mutants })
}

/// The diff a run of `picks` is limited to: one line of each, chosen so
/// that no field deletion not picked touches it, a line the change adds
/// where there is one. cargo-mutants 27 runs every field deletion on its
/// diff's lines whatever its name filter says (it never asks the filter of
/// them), so the change's whole diff would run them all. Refuses a pick
/// every line of which a field deletion not picked touches.
pub(super) fn run_diff(root: &Path, prefix: &str, listed: &Listed, picks: &[Pick]) -> Result<String> {
    let mut diff = String::new();
    for (file, at) in run_lines(listed, picks).map_err(Faults)? {
        let file = file.strip_prefix(prefix).unwrap_or(file);
        let text = std::fs::read_to_string(root.join(file))?;
        let text: Vec<&str> = text.lines().collect();
        diff.push_str(&format!("--- a/{file}\n+++ b/{file}\n"));
        for (k, l) in at.into_iter().enumerate() {
            let line = text.get(l as usize - 1).copied().unwrap_or_default();
            diff.push_str(&format!("@@ -{},0 +{l},1 @@\n+{line}\n", l as usize - 1 - k));
        }
    }
    Ok(diff)
}

/// The lines of [`run_diff`], per file; or a fault per pick that cannot run alone.
fn run_lines<'a>(
    listed: &'a Listed,
    picks: &[Pick],
) -> Result<BTreeMap<&'a str, BTreeSet<u32>>, Vec<crate::fault::Fault>> {
    let picked: BTreeSet<&str> = picks.iter().map(|p| p.name.as_str()).collect();
    let mut lines: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    let mut faults = Vec::new();
    for p in picks {
        let Some(c) = listed.candidates.iter().find(|c| c.name == p.name) else { continue };
        match line_alone(listed, c, &picked) {
            Ok(l) => {
                lines.entry(c.file.as_str()).or_default().insert(l);
            }
            Err(held) => faults.push(crate::fault::Fault {
                fault: format!(
                    "`{}` cannot run alone: cargo-mutants runs every field deletion on the lines it is given, \
                     whatever its name filter says, and on each of its lines is one not picked: {}",
                    c.name,
                    held.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
                ),
                fix: "pick those field deletions too, or another mutant".into(),
            }),
        }
    }
    if faults.is_empty() { Ok(lines) } else { Err(faults) }
}

/// A line of `c` no field deletion outside `picked` touches, one the
/// change adds where there is one; or the field deletions that hold every
/// line of it.
fn line_alone<'a>(listed: &'a Listed, c: &Candidate, picked: &BTreeSet<&str>) -> Result<u32, Vec<&'a str>> {
    let holding = |l: u32| {
        listed.candidates.iter().filter(move |o| {
            o.field && o.file == c.file && o.line <= l && l <= o.end.max(o.line) && !picked.contains(o.name.as_str())
        })
    };
    let span = c.line..=c.end.max(c.line);
    let free: Vec<u32> = span.clone().filter(|l| holding(*l).next().is_none()).collect();
    let added = |l: &&u32| listed.lines.get(&c.file).is_some_and(|a| a.contains_key(l));
    if let Some(l) = free.iter().find(added).or(free.first()) {
        return Ok(*l);
    }
    let held: BTreeSet<&str> = span.flat_map(|l| holding(l).map(|o| o.name.as_str())).collect();
    Err(held.into_iter().collect())
}

/// Whether `c` can run without a field deletion beside it: what a sample
/// may draw.
pub(super) fn runs_alone(listed: &Listed, c: &Candidate) -> bool {
    line_alone(listed, c, &BTreeSet::from([c.name.as_str()])).is_ok()
}

/// `name` as a regular expression that matches it alone.
fn anchored(name: &str) -> String {
    let mut re = String::from("^");
    for c in name.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            re.push('\\');
        }
        re.push(c);
    }
    re + "$"
}

/// The environment in which cargo-mutants' `command` runs each mutant's
/// tests only (ADR 0007): the plan, from the mutants it lists and the tests
/// that ran each function, and the shim first on PATH. Empty, and every
/// test runs, when the test run recorded no reach, the real nextest is not
/// found, or the mutants cannot be listed; stderr says which.
fn selecting(
    run: &Run,
    tested: &SuiteMeasurement,
    command: &str,
    picks: &[Pick],
    stop: Option<&Stop>,
) -> Result<Vec<(String, std::ffi::OsString)>> {
    let suite = run.suite;
    let every = |why: String| {
        eprintln!("slimfit: {}: {why}: every mutant runs every test", suite.name);
        Ok(Vec::new())
    };
    let Some(reach) = &tested.reach else { return every("the test run recorded no test's functions".into()) };
    let shims = run.out.join("shim");
    let Some(real) = crate::select::real_nextest(&shims) else {
        return every(format!("no {} on PATH or in cargo's bin directory", crate::select::SHIM));
    };
    let list = run.out.join("mutants.list.json");
    let listing = format!("{command} --list --json > {}", quoted(&list.to_string_lossy()));
    let (log, ok) = run.shell(&listing, "mutants-list", &[])?;
    let listed = std::fs::read_to_string(&list).unwrap_or_default();
    let prefix = run.prefix();
    let mut plan = match crate::select::plan(&run.root, &listed, |f| format!("{prefix}{f}"), reach, &tested.tests) {
        Ok(p) if ok => p,
        _ => return every(format!("cargo-mutants listed no mutants (log {})", log.display())),
    };
    let rank = |name: &str| picks.iter().position(|p| p.name == name);
    if let Some(s) = stop {
        let mutants = crate::select::drawn(&listed, |n| Some(s.ranks[rank(n)?]))?;
        plan.budget = Some(crate::select::Budget { viable: s.viable, known: s.known.clone(), mutants });
    }
    let unknown = plan.functions.iter().filter(|f| f.tests.is_none()).count();
    eprintln!(
        "slimfit: {}: each mutant runs only the tests that ran its function ({} function(s), {unknown} of them \
         run by tests unknown)",
        suite.name,
        plan.functions.len()
    );
    // One directory per run: the plan, the shim, and the configuration
    // it writes for each mutant.
    let _ = std::fs::remove_dir_all(&shims);
    std::fs::create_dir_all(&shims)?;
    let plan_path = shims.join("plan.json");
    std::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?)?;
    std::os::unix::fs::symlink(std::env::current_exe()?, shims.join(crate::select::SHIM))?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(std::iter::once(shims).chain(std::env::split_paths(&path)))?;
    Ok(vec![
        ("PATH".into(), path),
        (crate::select::PLAN_ENV.into(), plan_path.into()),
        (crate::select::NEXTEST_ENV.into(), real.into()),
    ])
}

/// One run of the mutation tool, its paths resolved to the suite's files.
/// A run limited to a change may have no mutant to test, and a tool may
/// then write no report: a clean exit then reads as none.
fn mutate(run: &Run, resolver: &Resolver, command: &str, env: &[(String, std::ffi::OsString)]) -> Result<Vec<Mutant>> {
    let suite = run.suite;
    run.clear(&suite.mutants_report)?;
    let (log, ok) = run.shell(command, "mutants", env)?;
    let reports = match run.texts("mutants_report", &suite.mutants_report, &log) {
        Err(_) if ok => vec![],
        r => r?,
    };
    let mut mutants = formats::read_mutants(suite.mutants_report.format, &reports)
        .map_err(|e| unreadable("mutants_report", suite.mutants_report.format, e))?;
    for m in &mut mutants {
        m.file = resolver.resolve(&m.file).unwrap_or_else(|| m.file.clone());
    }
    Ok(mutants)
}

/// A mutation run's mutants, and what it covered.
pub(crate) struct Mutation {
    mutants: Vec<Mutant>,
    pub(super) mutated: Mutated,
}

impl Mutation {
    /// Records the run in `s`, the suite's measurement, over what it held.
    pub(super) fn record(self, s: &mut SuiteMeasurement) {
        (s.mutants, s.mutated) = (self.mutants, Some(self.mutated));
    }
}

/// Mutants read from reports alone, none of them in a change.
#[cfg(test)]
impl From<Vec<Mutant>> for Mutation {
    fn from(mutants: Vec<Mutant>) -> Self {
        Self { mutants, mutated: Mutated::default() }
    }
}

#[cfg(test)]
mod t {
    use super::*;

    /// The real list of a crate whose struct literals cargo-mutants lists
    /// field deletions of (`testdata/rust-fields`), the whole file a change.
    fn fields() -> (std::path::PathBuf, Listed) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rust-fields");
        let list = std::fs::read_to_string(root.join("reports/mutants.list.json")).unwrap();
        let text = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let added = text.lines().enumerate().map(|(i, l)| (i as u32 + 1, l.to_string())).collect();
        let listed = Listed {
            lines: [("src/lib.rs".into(), added)].into(),
            candidates: formats::read_mutants_list(&list).unwrap(),
        };
        (root, listed)
    }

    fn picks(names: &[&str]) -> Vec<Pick> {
        names.iter().map(|n| Pick { name: n.to_string(), why: String::new(), outcome: None }).collect()
    }

    const CORNER: &str = "src/lib.rs:13:5: replace corner -> Point with Default::default()";
    const X: &str = "src/lib.rs:14:9: delete field x from struct Point expression in corner";
    const Y: &str = "src/lib.rs:15:9: delete field y from struct Point expression in corner";
    const TIMES: &str = "src/lib.rs:14:14: replace * with + in corner";

    /// cargo-mutants runs every field deletion on the lines it is given, so
    /// a run is given one line of each pick that no field deletion not
    /// picked touches: `corner`'s body is 13-17, and 13 is free; a pick on
    /// a field deletion's line runs only with it picked too, else is refused
    /// naming it. A mutant no line of which is free of another's field
    /// deletion is not drawn.
    #[test]
    fn a_run_holds_no_field_deletion_not_picked() {
        let (root, listed) = fields();
        let hunks = |names: &[&str]| -> Vec<String> {
            let diff = run_diff(&root, "", &listed, &picks(names)).unwrap();
            diff.lines().filter(|l| l.starts_with('@')).map(String::from).collect()
        };
        assert_eq!(hunks(&[CORNER, Y]), ["@@ -12,0 +13,1 @@", "@@ -13,0 +15,1 @@"]);
        assert_eq!(hunks(&[TIMES, X]), ["@@ -13,0 +14,1 @@"]);
        let refused = crate::fault::of(run_diff(&root, "", &listed, &picks(&[TIMES])).unwrap_err());
        assert!(refused[0].fault.starts_with(&format!("`{TIMES}` cannot run alone")), "{refused:?}");
        assert!(refused[0].fault.ends_with(&format!(": `{X}`")) && refused.len() == 1, "{refused:?}");
        let alone: Vec<&str> =
            listed.candidates.iter().filter(|c| runs_alone(&listed, c)).map(|c| c.name.as_str()).collect();
        assert_eq!(alone, [CORNER, X, Y]);
    }
}
