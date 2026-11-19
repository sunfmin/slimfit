//! The change's mutants (ADR 0006-0008): what each suite's tool lists of
//! the change, the agent's picks among them, and the run of the picks
//! alone, each with the tests that ran its function. Measuring a suite
//! asks this module for one thing: run the suite's picks on its test run.

use super::{Resolver, Scratch, prefix, quoted, read_reports, report_paths, shell, tail, unreadable};
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

/// The change's mutants in one suite: its diff of the landing, and what its
/// tool lists of it.
#[derive(Debug, Default)]
struct Listed {
    patch: String,
    candidates: Vec<Candidate>,
}

/// The candidates the agent picks from, per suite whose tool can pick:
/// what `check --mutants` with no names answers. Nothing builds.
pub fn candidates(repo: &Path, cfg: &Config) -> Result<BTreeMap<String, Vec<Candidate>>> {
    let scratch = Scratch::new(repo, false)?;
    let listed = listed(repo, cfg, &git::head(repo)?, &scratch.0)?;
    Ok(listed.into_iter().map(|(s, l)| (s, l.candidates)).collect())
}

/// What each suite whose tool can pick lists of the change (ADR 0008): its
/// files that add a source line, as a diff the tool lists the mutants of.
/// Refuses with no branch to land on, rather than list more than the change.
fn listed(repo: &Path, cfg: &Config, head: &git::Head, scratch: &Path) -> Result<BTreeMap<String, Listed>> {
    let Some(l) = landing::find(repo, &cfg.check, head)? else {
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
    for (name, lines) in landing::required(repo, cfg, &l.added)? {
        let listed = list_suite(repo, cfg, &l, &name, lines, scratch);
        out.insert(name.clone(), listed.map_err(|e| fault::within(&format!("suite {name}"), e))?);
    }
    Ok(out)
}

/// What one suite's tool lists of the landing's `lines` in it: nothing,
/// and the tool not run, when the suite adds no source line.
fn list_suite(
    repo: &Path,
    cfg: &Config,
    l: &landing::Landing,
    name: &str,
    lines: landing::Lines,
    scratch: &Path,
) -> Result<Listed> {
    let suite = cfg.suites.iter().find(|s| s.name == name).expect("a configured suite").resolve();
    let files: Vec<String> = lines.into_keys().collect();
    if files.is_empty() {
        return Ok(Listed::default());
    }
    let patch = git::diff(repo, &l.from, &l.to, &suite.root, &files)?;
    let dir = scratch.join(name);
    std::fs::create_dir_all(&dir)?;
    let (diff, list) = (dir.join("change.diff"), dir.join("mutants.list.json"));
    std::fs::write(&diff, &patch)?;
    let command = format!(
        "{} {} {} --list --json > {}",
        suite.mutants_command,
        suite.mutants_diff_flag,
        quoted(&diff.to_string_lossy()),
        quoted(&list.to_string_lossy())
    );
    let (log, ok) = shell(&repo.join(&suite.root), &dir, &command, "mutants-list", &[])?;
    let listing = std::fs::read_to_string(&list).unwrap_or_default();
    let read = formats::read_mutants_list(&listing).ok().filter(|_| ok);
    let Some(mut candidates) = read else {
        return Err(fault(
            format!("its mutation tool listed no mutants of the change; the command's log ends:\n{}", tail(&log)),
            format!("fix the command's failure in its log {}", log.display()),
        ));
    };
    let prefix = prefix(&suite);
    candidates.iter_mut().for_each(|c| c.file = format!("{prefix}{}", c.file));
    Ok(Listed { patch, candidates })
}

/// The change's picks, per suite: each suite's share, taken as it is measured.
pub(super) struct Change(BTreeMap<String, Picked>);

impl Change {
    /// The change's mutants listed by each suite's tool, and the picks among
    /// them; refuses, before anything runs, a suite whose tool cannot pick,
    /// a name no suite lists, and a pick named twice.
    pub(super) fn pick(repo: &Path, cfg: &Config, head: &git::Head, scratch: &Path, picks: &[Pick]) -> Result<Self> {
        refuse_unpickable(cfg)?;
        choose(listed(repo, cfg, head, scratch)?, picks).map(Self)
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
    if !faults.is_empty() {
        return Err(Faults(faults).into());
    }
    Ok(out)
}

impl Picked {
    /// Runs the picks on `tested`, this tree's test run of `suite` (which
    /// tests each mutant runs is chosen from it), and records in it the
    /// mutants the run reports and each pick's outcome. `out` is the
    /// suite's scratch directory.
    pub(super) fn run(
        &self,
        root: &Path,
        out: &Path,
        suite: &Suite,
        resolver: &Resolver,
        tested: &mut SuiteMeasurement,
    ) -> Result<()> {
        mutate_picks(root, out, suite, resolver, &self.listed, &self.picks, tested)?.record(tested);
        Ok(())
    }
}

/// Runs the suite's picked mutants of the change, and reads each pick's
/// outcome. `tested` is this tree's test run: which tests each mutant runs
/// is chosen from it.
fn mutate_picks(
    root: &Path,
    out: &Path,
    suite: &Suite,
    resolver: &Resolver,
    listed: &Listed,
    picks: &[Pick],
    tested: &SuiteMeasurement,
) -> Result<Mutation> {
    // A suite none of whose mutants is picked runs none: no `-F` would run them all.
    if picks.is_empty() {
        return Ok(Mutation { mutants: Vec::new(), mutated: Mutated::default() });
    }
    let diff = out.join("change.diff");
    std::fs::write(&diff, &listed.patch)?;
    eprintln!("slimfit: {}: running the {} picked mutant(s) of the change", suite.name, picks.len());
    let mut command =
        format!("{} {} {}", suite.mutants_command, suite.mutants_diff_flag, quoted(&diff.to_string_lossy()));
    for p in picks {
        command.push_str(&format!(" {} {}", suite.mutants_pick_flag, quoted(&anchored(&p.name))));
    }
    let env = match suite.mutants_tests {
        MutantTests::Reached => selecting(root, out, suite, &prefix(suite), tested, &command)?,
        MutantTests::All => Vec::new(),
    };
    let run = mutate(root, out, suite, resolver, &command, &env)?;
    // A pick is the mutant the run reports at its place, as its name describes it.
    let ran = |name: &str| {
        let c = listed.candidates.iter().find(|c| c.name == name)?;
        let described = name.splitn(4, ':').nth(3).map(str::trim)?;
        run.iter().find(|m| m.file == c.file && m.line == c.line && m.description == described)
    };
    let mutants: Vec<Mutant> = picks.iter().filter_map(|p| ran(&p.name)).cloned().collect();
    let picks = picks.iter().map(|p| Pick { outcome: ran(&p.name).map(|m| m.status), ..p.clone() }).collect();
    Ok(Mutation { mutated: Mutated { picks, change: MutantCounts::of(&mutants) }, mutants })
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
    root: &Path,
    out: &Path,
    suite: &Suite,
    prefix: &str,
    tested: &SuiteMeasurement,
    command: &str,
) -> Result<Vec<(String, std::ffi::OsString)>> {
    let every = |why: String| {
        eprintln!("slimfit: {}: {why}: every mutant runs every test", suite.name);
        Ok(Vec::new())
    };
    let Some(reach) = &tested.reach else { return every("the test run recorded no test's functions".into()) };
    let shims = out.join("shim");
    let Some(real) = crate::select::real_nextest(&shims) else {
        return every(format!("no {} on PATH or in cargo's bin directory", crate::select::SHIM));
    };
    let list = out.join("mutants.list.json");
    let listing = format!("{command} --list --json > {}", quoted(&list.to_string_lossy()));
    let (log, ok) = shell(root, out, &listing, "mutants-list", &[])?;
    let listed = std::fs::read_to_string(&list).unwrap_or_default();
    let plan = match crate::select::plan(root, &listed, |f| format!("{prefix}{f}"), reach, &tested.tests) {
        Ok(p) if ok => p,
        _ => return every(format!("cargo-mutants listed no mutants (log {})", log.display())),
    };
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
fn mutate(
    root: &Path,
    out: &Path,
    suite: &Suite,
    resolver: &Resolver,
    command: &str,
    env: &[(String, std::ffi::OsString)],
) -> Result<Vec<Mutant>> {
    // A report left by an earlier run would be read as this run's.
    for old in report_paths(root, out, &suite.mutants_report)? {
        std::fs::remove_file(old)?;
    }
    let (log, ok) = shell(root, out, command, "mutants", env)?;
    let reports = match read_reports(root, out, "mutants_report", &suite.mutants_report, &log) {
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
    mutated: Mutated,
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
