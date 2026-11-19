//! Measuring the working tree: run each suite's commands, read their
//! reports, and reduce them to `Measurement`. The result is cached under
//! `.slimfit/out/` and reused while the tree is unchanged.

use crate::config::{Config, MutantTests, Report, Suite};
use crate::fault::{self, Faults, fault};
use crate::formats;
use crate::git;
use crate::landing;
use crate::load;
use crate::loc::{Classifier, Kind, Totals};
use crate::model::*;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub const OUT: &str = ".slimfit/out";

/// What every measuring command may refuse, as the command pages say it.
pub fn refusals_help() -> String {
    format!(
        "It refuses with exit {} and `{{\"error\": [{{\"fault\": \"...\", \"fix\": \"...\"}}, ...]}}`: every fault it found at once, each with the `fix` that clears it. Faults: a bad command line; not in a git repository; no `slimfit.toml` (`slimfit init` writes one), or one that is not valid; a `.slimfit/baseline.json` it cannot read; a suite's tool missing, or writing no report: the fault names the report it looked for (`no tests_report at ...`) and ends with the command's log. A failing test is measured, not refused: it counts in `failures`.",
        crate::EXIT_ERROR
    )
}

/// The cache's pitfall, as the command pages say it.
pub const CACHE_HELP: &str = "The measurement is cached in `.slimfit/out/` and reused while HEAD and the contents of every file git does not ignore are unchanged. A change outside them is not seen: a tool's new version, the host's load, an environment variable, a flaky test. `--fresh` measures again.";

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

pub struct Options {
    pub mutants: Mutants,
    pub fresh: bool,
    /// Write nothing in the repo: the commands write their reports to a
    /// directory of their own outside it, and the measurement is not cached.
    pub dry_run: bool,
    /// `test_seconds` must come from a test run of the caller's own
    /// priority, as a gate's `check` times it: a cached test run made inside
    /// `--mutants` (`untimed`) runs again, and its mutants are kept.
    pub timed: bool,
}

/// The measurement of the tree as it is now: from the cache when it was
/// taken of this exact tree (with as many mutants as are wanted, of a change
/// taken from the same commit). A cached measurement of the tree that lacks
/// those mutants keeps its test run: only the mutation tool runs.
pub fn current(repo: &Path, cfg: &Config, opts: &Options) -> Result<Measurement> {
    let wt = git::worktree(repo)?;
    let tree = wt.key.clone();
    let cache = repo.join(OUT).join("measurement.json");
    let picks = match &opts.mutants {
        Mutants::Picks(p) => p.as_slice(),
        Mutants::None => &[],
    };
    let with_mutants = !picks.is_empty();
    // The cached measurement of this tree, when not asked to measure afresh.
    let tested = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|text| serde_json::from_str::<Measurement>(&text).ok())
        .filter(|m| !opts.fresh && m.tree == tree)
        .filter(|m| m.suites.iter().map(|s| &s.suite).eq(cfg.suites.iter().map(|s| &s.name)));
    let wanted: BTreeSet<&str> = picks.iter().map(|p| p.name.as_str()).collect();
    let fresh_mutants = |m: &Measurement| m.with_mutants && picked(m) == wanted;
    let timed = |m: &Measurement| !opts.timed || m.suites.iter().all(|s| !s.untimed);
    if let Some(m) = tested.as_ref().filter(|m| (!with_mutants || fresh_mutants(m)) && timed(m)) {
        let mut m = m.clone();
        // The same picks, run already: only why each was picked may be new.
        for p in m.suites.iter_mut().filter_map(|s| s.mutated.as_mut()).flat_map(|d| d.picks.iter_mut()) {
            if let Some(w) = picks.iter().find(|w| w.name == p.name) {
                p.why = w.why.clone();
            }
        }
        if with_mutants && !opts.dry_run {
            std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
        }
        return Ok(m);
    }
    // Only the test run is measured again: the cached mutants of this tree stand.
    let kept = tested.as_ref().is_some_and(|m| !with_mutants && m.with_mutants);
    let scratch = Scratch::new(repo, opts.dry_run)?;
    let mut tested = tested.map(|m| m.suites.into_iter()).into_iter().flatten();
    // Every pick is one the change's mutants list, before anything runs.
    let mut change = if with_mutants {
        refuse_unpickable(cfg)?;
        Some(choose(listed(repo, cfg, &wt.head, &scratch.0)?, picks)?)
    } else {
        None
    };
    let mut suites = Vec::new();
    for s in &cfg.suites {
        let suite = s.resolve();
        eprintln!(
            "slimfit: measuring {} ({}){}",
            suite.name,
            suite.root,
            if with_mutants { " with the picked mutants of the change" } else { "" }
        );
        let mutants = change.as_mut().map(|c| c.remove(&suite.name).unwrap_or_default());
        let r = measure_suite(repo, &scratch.0, &wt.files, &suite, mutants, tested.next());
        suites.push(r.map_err(|e| fault::within(&format!("suite {}", suite.name), e)));
    }
    // Every suite is measured, so one that fails does not hide another's faults.
    let suites = fault::all(suites)?;
    let m = Measurement { tree, with_mutants: with_mutants || kept, suites };
    if !opts.dry_run {
        std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
    }
    Ok(m)
}

/// The names of the picks a measurement ran, in every suite.
fn picked(m: &Measurement) -> BTreeSet<&str> {
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

/// The picks, each in the suite whose tool lists it; refuses a name no
/// suite lists, and a pick named twice.
fn choose(listed: BTreeMap<String, Listed>, picks: &[Pick]) -> Result<BTreeMap<String, (Listed, Vec<Pick>)>> {
    let mut out: BTreeMap<String, (Listed, Vec<Pick>)> =
        listed.into_iter().map(|(s, l)| (s, (l, Vec::new()))).collect();
    let mut faults = Vec::new();
    for (i, p) in picks.iter().enumerate() {
        let suite = out.values_mut().find(|(l, _)| l.candidates.iter().any(|c| c.name == p.name));
        match suite {
            _ if picks[..i].iter().any(|q| q.name == p.name) => faults.push(crate::fault::Fault {
                fault: format!("`{}` is picked twice", p.name),
                fix: "name each pick once".into(),
            }),
            Some((_, chosen)) => chosen.push(p.clone()),
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

/// The suite's root as a prefix of repository paths: empty at the root.
fn prefix(suite: &Suite) -> String {
    if suite.root == "." { String::new() } else { format!("{}/", suite.root.trim_end_matches('/')) }
}

/// Where the suites' commands write: `.slimfit/out`, or in a dry run a
/// directory outside the repo, removed when the measurement is done.
struct Scratch(PathBuf, bool);

impl Scratch {
    fn new(repo: &Path, dry_run: bool) -> Result<Self> {
        let dir = if dry_run {
            std::env::temp_dir().join(format!("slimfit-dry-run-{}", std::process::id()))
        } else {
            repo.join(OUT)
        };
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir, dry_run))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn measure_suite(
    repo: &Path,
    scratch: &Path,
    files: &[String],
    suite: &Suite,
    mutants: Option<(Listed, Vec<Pick>)>,
    tested: Option<SuiteMeasurement>,
) -> Result<SuiteMeasurement> {
    let root = repo.join(&suite.root);
    let out = scratch.join(&suite.name);
    let classifier = Classifier::new(suite)?;
    let totals = classifier.count(files, |f| std::fs::read_to_string(repo.join(f)).ok());
    let resolver = Resolver::new(repo, &totals);
    // The tests of this tree already ran: only the mutants are new. Without
    // mutants a cached run is passed here only to be timed again (`untimed`),
    // keeping its mutants.
    let (mutants, kept) = match (tested, mutants) {
        (Some(mut s), Some((listed, picks))) => {
            std::fs::create_dir_all(&out)?;
            let m = mutate_picks(&root, &out, suite, &resolver, &listed, &picks, &s)?;
            (s.mutants, s.mutated) = (m.mutants, Some(m.mutated));
            return Ok(s);
        }
        (tested, mutants) => (mutants, tested),
    };
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out)?;

    // Reports left by an earlier run would be read as this run's.
    for old in report_paths(&root, &out, &suite.tests_report)?.into_iter().chain(report_paths(
        &root,
        &out,
        &suite.coverage_report,
    )?) {
        std::fs::remove_file(old)?;
    }
    let started = Instant::now();
    let since_epoch =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let host_load = load::per_core();
    let (run, slowdown) = load::slowdown_during(|| shell(&root, &out, &suite.test_command, "test", &[]));
    let (log, _) = run?;
    let command_seconds = started.elapsed().as_secs_f64();
    let tests = read_report(&root, &out, "tests_report", &suite.tests_report, &log, formats::read_tests)?;
    let coverage = read_report(&root, &out, "coverage_report", &suite.coverage_report, &log, formats::read_coverage)?;
    let reach = read_reach(&root, &out, suite)?;

    let timing = Timing { command_seconds, slowdown, host_load, started: since_epoch };
    let mut s = reduce(suite, &totals, &resolver, tests, coverage, reach, None, timing);
    if let Some((listed, picks)) = mutants {
        let m = mutate_picks(&root, &out, suite, &resolver, &listed, &picks, &s)?;
        (s.mutants, s.mutated) = (m.mutants, Some(m.mutated));
        // At the priority mutants run at, not a gate's: see `untimed`.
        s.untimed = true;
    } else if let Some(k) = kept {
        (s.mutants, s.mutated) = (k.mutants, k.mutated);
    }
    Ok(s)
}

/// Which functions each test ran, when the suite's test run records it. A
/// run that wrote none (a test command of its own, a runner of the repo's
/// own) records nothing, and every mutant then runs every test.
fn read_reach(root: &Path, out: &Path, suite: &Suite) -> Result<Option<Vec<formats::TestReach>>> {
    if suite.reach_report.path.is_empty() {
        return Ok(None);
    }
    let paths: Vec<PathBuf> = report_paths(root, out, &suite.reach_report)?;
    if paths.is_empty() {
        eprintln!(
            "slimfit: {}: the test run wrote no reach_report at {}: no test is known to run any function",
            suite.name,
            report_pattern(root, out, &suite.reach_report)
        );
        return Ok(None);
    }
    let texts = paths.iter().map(std::fs::read_to_string).collect::<std::io::Result<Vec<_>>>()?;
    formats::read_reach(suite.reach_report.format, &texts)
        .map(Some)
        .map_err(|e| unreadable("reach_report", suite.reach_report.format, e))
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
    pub mutants: Vec<Mutant>,
    mutated: Mutated,
}

/// Mutants read from reports alone, none of them in a change.
#[cfg(test)]
impl From<Vec<Mutant>> for Mutation {
    fn from(mutants: Vec<Mutant>) -> Self {
        Self { mutants, mutated: Mutated::default() }
    }
}

/// Runs `command` with `sh -c` in the suite root, with `env` added. A
/// failing exit status is not an error: failing tests still write reports.
/// The log is kept; the flag says whether the command exited 0.
fn shell(
    root: &Path,
    out: &Path,
    command: &str,
    label: &str,
    env: &[(String, std::ffi::OsString)],
) -> Result<(PathBuf, bool)> {
    if command.trim().is_empty() {
        return Err(fault(
            format!("no {label} command configured"),
            format!("set `{label}_command` in the suite's [[suite]] in slimfit.toml"),
        ));
    }
    let log = out.join(format!("{label}.log"));
    let file = std::fs::File::create(&log)?;
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .env(crate::tools::OUT_ENV, out)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()
        .with_context(|| format!("running {label} command: {command}"))?;
    eprintln!("slimfit: {label} command exited {status}; log {}", log.display());
    Ok((log, status.success()))
}

fn report_pattern<F>(root: &Path, out: &Path, report: &Report<F>) -> String {
    let pattern = report.path.replace(&format!("${}", crate::tools::OUT_ENV), &out.to_string_lossy());
    if Path::new(&pattern).is_absolute() { pattern } else { root.join(&pattern).to_string_lossy().into_owned() }
}

fn report_paths<F>(root: &Path, out: &Path, report: &Report<F>) -> Result<Vec<PathBuf>> {
    expand(&report_pattern(root, out, report))
}

/// The report `field` names, read in its format by `read`.
fn read_report<F: Copy + serde::Serialize, T>(
    root: &Path,
    out: &Path,
    field: &str,
    report: &Report<F>,
    log: &Path,
    read: impl FnOnce(F, &[String]) -> Result<T>,
) -> Result<T> {
    read(report.format, &read_reports(root, out, field, report, log)?).map_err(|e| unreadable(field, report.format, e))
}

/// A report that is not in the format slimfit.toml names: its fault, with
/// the fix. Faults the reader names itself are kept as they are.
fn unreadable(field: &str, format: impl serde::Serialize, e: anyhow::Error) -> anyhow::Error {
    if e.is::<Faults>() {
        return e;
    }
    let format = serde_json::to_value(format).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
    fault(
        format!("its {field} is not {format}: {e:#}"),
        format!(
            "make the command write {format} there, or set `{field}` in slimfit.toml to the format the tool writes"
        ),
    )
}

/// The texts of the report's files; `field` names it in slimfit.toml.
/// The last 30 lines of a command's log.
fn tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

fn read_reports<F>(root: &Path, out: &Path, field: &str, report: &Report<F>, log: &Path) -> Result<Vec<String>> {
    let pattern = report_pattern(root, out, report);
    // An empty file is no report: a shell redirect creates one even when
    // the tool behind it is missing.
    let paths: Vec<PathBuf> =
        expand(&pattern)?.into_iter().filter(|p| p.metadata().is_ok_and(|m| m.len() > 0)).collect();
    if paths.is_empty() {
        return Err(fault(
            format!("no {field} at {pattern}; the command's log ends:\n{}", tail(log)),
            format!(
                "fix the command's failure in its log {}; if it ran, make it write the report there, or set \
                 `{field}` path in slimfit.toml to where it writes",
                log.display()
            ),
        ));
    }
    paths.into_iter().map(|p| std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))).collect()
}

/// Paths matching an absolute glob, walking only below its literal prefix.
fn expand(pattern: &str) -> Result<Vec<PathBuf>> {
    if !pattern.contains(['*', '?', '[']) {
        return Ok(if Path::new(pattern).exists() { vec![pattern.into()] } else { vec![] });
    }
    let literal: String = pattern.split('/').take_while(|c| !c.contains(['*', '?', '['])).collect::<Vec<_>>().join("/");
    let matcher = globset::GlobBuilder::new(pattern).literal_separator(true).build()?.compile_matcher();
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::from(if literal.is_empty() { "/" } else { &literal })];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if name != ".git" && name != "node_modules" {
                    stack.push(p);
                }
            } else if matcher.is_match(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Maps a path as a tool reported it (absolute, module-qualified,
/// package-qualified) to a file of the suite, by the longest matching
/// path suffix.
pub struct Resolver {
    repo: String,
    files: Vec<String>,
}

impl Resolver {
    pub fn new(repo: &Path, totals: &Totals) -> Self {
        Self { repo: format!("{}/", repo.to_string_lossy()), files: totals.files.keys().cloned().collect() }
    }

    /// The text of `file`, a path in the repository.
    pub fn read(&self, file: &str) -> Option<String> {
        std::fs::read_to_string(format!("{}{file}", self.repo)).ok()
    }

    pub fn resolve(&self, raw: &str) -> Option<String> {
        let p = raw.strip_prefix("file://").unwrap_or(raw);
        let p = p.strip_prefix(&self.repo).unwrap_or(p).trim_start_matches("./");
        if p.starts_with('/') {
            return None; // outside the repository: std, dependencies
        }
        if self.files.iter().any(|f| f == p) {
            return Some(p.to_string());
        }
        let parts: Vec<&str> = p.split('/').collect();
        let mut best: Option<(usize, &String)> = None;
        for f in &self.files {
            let common = f.split('/').rev().zip(parts.iter().rev()).take_while(|(a, b)| a == *b).count();
            if common > 0 && best.is_none_or(|(n, _)| common > n) {
                best = Some((common, f));
            }
        }
        best.map(|(_, f)| f.clone())
    }
}

/// How long the test command took, how much the host's load slowed it,
/// how loaded the host was, and when it started (seconds since the Unix epoch).
pub(crate) struct Timing {
    pub command_seconds: f64,
    pub slowdown: f64,
    /// The host's 1-minute load per core as the command started.
    pub host_load: f64,
    pub started: f64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn reduce(
    suite: &Suite,
    totals: &Totals,
    resolver: &Resolver,
    mut tests: Vec<TestCase>,
    coverage: Coverage,
    reach: Option<Vec<formats::TestReach>>,
    mutation: Option<Mutation>,
    timing: Timing,
) -> SuiteMeasurement {
    // A test's duration as the runner timed it, less what the host's load
    // added to it (ADR 0003). A test whose report says it started before
    // the command (a report the command did not write) ran under a load
    // the probe never saw: it stays as reported.
    for t in &mut tests {
        // Report timestamps are rounded to the millisecond.
        if t.start.is_none_or(|s| s >= timing.started - 0.01) {
            t.seconds /= timing.slowdown;
        }
    }
    // Coverage of prod lines only: test files and inline test blocks are out.
    let mut lines: BTreeMap<String, BTreeMap<u32, u64>> = BTreeMap::new();
    for (raw, hits) in coverage.lines {
        let Some(file) = resolver.resolve(&raw) else { continue };
        let Some((Kind::Source, fl)) = totals.files.get(&file) else { continue };
        let entry = lines.entry(file).or_default();
        for (n, h) in hits {
            if !fl.is_test_line(n) {
                let e = entry.entry(n).or_insert(0);
                *e = (*e).max(h);
            }
        }
    }
    let mut functions: BTreeMap<String, Vec<Function>> = BTreeMap::new();
    for f in coverage.functions {
        if let Some(file) = resolver.resolve(&f.file) {
            functions.entry(file.clone()).or_default().push(Function { file, ..f });
        }
    }
    let reach = reach.map(|r| reached(r, &functions, totals, resolver));
    let covered = lines.values().flat_map(|l| l.values()).filter(|h| **h > 0).count() as u64;
    let uncovered_total = lines.values().flat_map(|l| l.values()).filter(|h| **h == 0).count() as u64;
    let mut uncovered = Vec::new();
    for (file, hits) in &lines {
        uncovered.extend(spans(file, hits, functions.get(file).map(Vec::as_slice).unwrap_or(&[])));
    }
    uncovered.sort_by(|a, b| b.lines.cmp(&a.lines).then(a.file.cmp(&b.file)).then(a.start.cmp(&b.start)));

    let (mutants, mutated) = match mutation {
        Some(m) => (m.mutants, Some(m.mutated)),
        None => (Vec::new(), None),
    };
    let metrics = Metrics {
        prod_lines: totals.prod,
        test_lines: totals.test,
        tests: tests.len() as u64,
        failures: tests.iter().filter(|t| !t.passed).count() as u64,
        test_seconds: (tests.iter().map(|t| t.seconds).sum::<f64>() * 1000.0).round() / 1000.0,
        covered_lines: covered,
        uncovered_lines: uncovered_total,
    };
    SuiteMeasurement {
        suite: suite.name.clone(),
        metrics,
        tests,
        uncovered,
        mutants,
        reach,
        mutated,
        command_seconds: (timing.command_seconds * 1000.0).round() / 1000.0,
        load_slowdown: (timing.slowdown * 1000.0).round() / 1000.0,
        host_load: (timing.host_load * 1000.0).round() / 1000.0,
        untimed: false,
    }
}

/// Which tests ran each source function, from what each test ran by symbol:
/// every function the coverage report names in a source file, outside
/// tests, keyed by its first line; and the source lines each ran, outside
/// tests. Each test's own function is located by its path, and hashed.
fn reached(
    tests: Vec<formats::TestReach>,
    functions: &BTreeMap<String, Vec<Function>>,
    totals: &Totals,
    resolver: &Resolver,
) -> Reach {
    let mut reach = Reach { lines: Some(Vec::new()), ..Reach::default() };
    let mut at: BTreeMap<&str, (&str, u32)> = BTreeMap::new();
    for (file, fs) in functions {
        let Some((Kind::Source, fl)) = totals.files.get(file) else { continue };
        for f in fs.iter().filter(|f| !fl.is_test_line(f.line)) {
            reach.functions.entry(file.clone()).or_default().entry(f.line).or_default();
            at.insert(if f.symbol.is_empty() { &f.name } else { &f.symbol }, (file, f.line));
        }
    }
    let fns = fn_lines(totals, resolver);
    let every: Vec<(String, u32)> =
        reach.functions.iter().flat_map(|(f, ls)| ls.keys().map(|l| (f.clone(), *l))).collect();
    for (i, t) in tests.into_iter().enumerate() {
        let i = i as u32;
        let mut ran = |file: &str, line: u32| {
            let tests = reach.functions.get_mut(file).and_then(|f| f.get_mut(&line)).expect("listed above");
            if tests.last() != Some(&i) {
                tests.push(i);
            }
        };
        match &t.functions {
            Some(names) => names.iter().filter_map(|n| at.get(n.as_str())).for_each(|(f, l)| ran(f, *l)),
            None => every.iter().for_each(|(f, l)| ran(f, *l)),
        }
        let mut lines: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for (raw, ls) in &t.lines {
            let Some(file) = resolver.resolve(raw) else { continue };
            let Some((Kind::Source, fl)) = totals.files.get(&file) else { continue };
            let entry = lines.entry(file).or_default();
            entry.extend(ls.iter().filter(|n| !fl.is_test_line(**n)));
            entry.sort_unstable();
            entry.dedup();
        }
        lines.retain(|_, ls| !ls.is_empty());
        reach.lines.as_mut().expect("set above").push(lines);
        let source = test_function(&fns, &t.name).and_then(|(file, line)| test_source(file, line, resolver));
        reach.tests.push(ReachTest { binary: t.binary, name: t.name, source });
    }
    reach
}

/// Every `fn` of the suite's Rust files, by name: where each is. The
/// coverage report cannot say: cargo llvm-cov leaves integration tests out.
fn fn_lines(totals: &Totals, resolver: &Resolver) -> BTreeMap<String, Vec<(String, u32)>> {
    let mut out: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
    for file in totals.files.keys().filter(|f| f.ends_with(".rs")) {
        let Some(text) = resolver.read(file) else { continue };
        for (i, line) in text.lines().enumerate() {
            if let Some(name) = fn_name(line) {
                out.entry(name.to_string()).or_default().push((file.clone(), i as u32 + 1));
            }
        }
    }
    out
}

/// The name a line declares a function of: `fn x(`, after any of `pub`,
/// `async`, `unsafe`, `const`, `extern`.
fn fn_name(line: &str) -> Option<&str> {
    let mut words = line.split_whitespace();
    let rest = loop {
        match words.next()? {
            "fn" => break words.next()?,
            w if w.starts_with("pub") || ["async", "unsafe", "const", "extern"].contains(&w) => {}
            _ => return None,
        }
    };
    let name = &rest[..rest.find(['(', '<']).unwrap_or(rest.len())];
    (!name.is_empty()).then_some(name)
}

/// Where test `name` (nextest's: `module::path::function`) is: the one `fn`
/// of its last segment; of several, the one whose file's path names the
/// most of its modules (`queue::x` in `tests/scenarios/queue.rs`).
fn test_function<'a>(fns: &'a BTreeMap<String, Vec<(String, u32)>>, name: &str) -> Option<(&'a str, u32)> {
    let mut segments: Vec<&str> = name.split("::").collect();
    let found = fns.get(segments.pop()?)?;
    let named = |file: &str| {
        let parts: Vec<&str> = file.trim_end_matches(".rs").split('/').collect();
        segments.iter().filter(|m| parts.contains(m)).count()
    };
    let best = found.iter().map(|(f, _)| named(f)).max()?;
    match found.iter().filter(|(f, _)| named(f) == best).collect::<Vec<_>>().as_slice() {
        [(file, line)] => Some((file, *line)),
        _ => None,
    }
}

/// Where the test function at `file:line` is and the hash of its text: from
/// its `fn` line to the brace that closes it, found as rustfmt places it
/// (alone on its line, indented as the `fn`), or on the `fn` line itself.
fn test_source(file: &str, line: u32, resolver: &Resolver) -> Option<TestSource> {
    let text = resolver.read(file)?;
    let lines: Vec<&str> = text.lines().skip(line.checked_sub(1)? as usize).collect();
    let first = lines.first()?;
    let indent = &first[..first.len() - first.trim_start().len()];
    let one_line = first.contains('{') && first.matches('{').count() == first.matches('}').count();
    let close = format!("{indent}}}");
    let n = if one_line { 0 } else { lines.iter().position(|l| l.trim_end() == close)? };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in lines[..=n].join("\n").bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    Some(TestSource { file: file.into(), line, hash: format!("{hash:016x}") })
}

/// Runs of zero-hit lines, not interrupted by a hit line (lines the report
/// does not mention, like blanks, don't break a run). Each span is named
/// after the function it starts in, when the report names functions.
fn spans(file: &str, hits: &BTreeMap<u32, u64>, functions: &[Function]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut open: Option<Span> = None;
    for (&n, &h) in hits {
        match (&mut open, h) {
            (Some(s), 0) => {
                s.end = n;
                s.lines += 1;
            }
            (None, 0) => open = Some(Span { file: file.into(), start: n, end: n, lines: 1, function: None }),
            (_, _) => out.extend(open.take()),
        }
    }
    out.extend(open);
    for s in &mut out {
        s.function = functions.iter().filter(|f| f.line <= s.start).max_by_key(|f| f.line).map(|f| f.name.clone());
    }
    out
}

#[cfg(test)]
pub(crate) mod t {
    use super::*;
    use crate::ratchet::{Baseline, Bound};

    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.email=t@slimfit", "-c", "user.name=t", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// The Rust sample, its tools replaced by copies of their real reports;
    /// the mutation "tool" also records the arguments it was given (a
    /// listing's in `list-args`).
    pub(crate) fn sample_repo() -> (tempfile::TempDir, Config) {
        let reports = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rust/reports");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for f in ["Cargo.toml", "src/lib.rs"] {
            std::fs::copy(reports.join("..").join(f), dir.path().join(f)).unwrap();
        }
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "sample"]);
        let r = reports.to_string_lossy();
        let cfg: Config = toml::from_str(&format!(
            r#"
            [[suite]]
            name = "rust"
            preset = "rust"
            test_command = "cp {r}/junit.xml {r}/lcov.info \"$SLIMFIT_OUT\""
            tests_report = {{ format = "junit", path = "$SLIMFIT_OUT/junit.xml" }}
            mutants_command = "f() {{ case \" $* \" in *\" --list \"*) echo \"$@\" > \"$SLIMFIT_OUT/list-args\"; cat {r}/mutants.list.json;; *) echo \"$@\" > \"$SLIMFIT_OUT/args\"; mkdir -p \"$SLIMFIT_OUT/mutants.out\"; cp {r}/outcomes.json \"$SLIMFIT_OUT/mutants.out\";; esac; }}; f"
            "#
        ))
        .unwrap();
        (dir, cfg)
    }

    /// Picks, each with a reason.
    pub(crate) fn picks(names: &[&str]) -> Vec<Pick> {
        names.iter().map(|n| Pick { name: n.to_string(), why: format!("why {n}"), outcome: None }).collect()
    }

    /// The measurement with `names` picked, and the arguments the mutation
    /// "tool" got (`None`: it did not run). Not `fresh`: a cached test run of
    /// the same tree is kept, so only the mutants run.
    fn run(repo: &Path, cfg: &Config, names: &[&str]) -> Result<(Measurement, Option<String>)> {
        let args = repo.join(OUT).join("rust/args");
        let _ = std::fs::remove_file(&args);
        let mutants = Mutants::Picks(picks(names));
        let m = current(repo, cfg, &Options { mutants, fresh: false, dry_run: false, timed: false })?;
        Ok((m, std::fs::read_to_string(args).ok()))
    }

    fn mutated(m: &Measurement) -> Mutated {
        m.suites[0].mutated.clone().unwrap()
    }

    const KILLED: &str = "src/lib.rs:12:9: replace >= with < in is_adult";
    const MISSED: &str = "src/lib.rs:12:5: replace is_adult -> bool with true";

    /// One fixture through picking. The candidates are what the tool lists
    /// of the landing's diff (what HEAD adds since its merge base with the
    /// target, untracked files included); with no target, or a tool that
    /// cannot run a mutant by name, `--mutants` refuses. A pick no candidate
    /// is named is refused before anything runs; the tool runs only the
    /// picks, each by its anchored name, and each pick gets its outcome.
    /// The same picks of the same tree do not run again.
    #[test]
    fn only_the_picked_mutants_of_the_landing_run() {
        let (dir, mut cfg) = sample_repo();
        // Every test on every mutant: which tests run is not this test's.
        cfg.suites[0].overrides.mutants_tests = Some(MutantTests::All);
        let repo = dir.path();
        let refused = |cfg: &Config, names: &[&str]| {
            let faults = crate::fault::of(run(repo, cfg, names).unwrap_err());
            assert!(!repo.join(OUT).join("rust/args").exists(), "a mutant ran");
            faults.into_iter().map(|f| f.fault).collect::<Vec<_>>().join("\n")
        };
        // No target named and no origin/HEAD: refused, no mutant run.
        assert!(refused(&cfg, &[KILLED]).contains("no branch to take the change from"));
        assert!(candidates(repo, &cfg).is_err());
        git(repo, &["branch", "base"]);
        cfg.check.target = Some("base".into());
        // A tool that cannot run a mutant by name: refused up front.
        let mut unnamed = cfg.clone();
        unnamed.suites[0].overrides.mutants_pick_flag = Some(String::new());
        assert!(refused(&unnamed, &[KILLED]).contains("cannot list a change's mutants and run them by name"));
        // No source line added: nothing listed, the tool not run.
        assert_eq!(candidates(repo, &cfg).unwrap()["rust"], []);
        assert!(!repo.join(OUT).join("rust/list-args").exists());

        let text = std::fs::read_to_string(repo.join("src/lib.rs")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), text.replacen("    age >= 18\n", "    age >= 18 // x\n", 1)).unwrap();
        std::fs::write(repo.join("src/new.rs"), "pub fn n() -> u8 {\n    1\n}\n").unwrap();
        let listed = candidates(repo, &cfg).unwrap();
        let first = &listed["rust"][0];
        assert_eq!((listed["rust"].len(), first.file.as_str(), first.line), (17, "src/lib.rs", 2));
        assert_eq!((first.function.as_str(), first.replacement.as_str()), ("add", "0"));
        let diff = repo.join(OUT).join("rust/change.diff");
        let list = std::fs::read_to_string(repo.join(OUT).join("rust/list-args")).unwrap();
        assert_eq!(list, format!("--in-diff {} --list --json\n", diff.display()));
        let patch = std::fs::read_to_string(&diff).unwrap();
        assert!(patch.contains("+    age >= 18 // x") && patch.contains("+++ b/src/new.rs"), "{patch}");

        // Not listed, or picked twice: refused, nothing run.
        let refusal = refused(&cfg, &[KILLED, "src/lib.rs:99:1: replace nothing", KILLED]);
        assert!(refusal.contains("no mutant of the change is named `src/lib.rs:99:1: replace nothing`"), "{refusal}");
        assert!(refusal.contains("is picked twice"), "{refusal}");

        // The tool runs the picks alone, each by its anchored name; of what it
        // reports, only the picks are the change's.
        let (m, args) = run(repo, &cfg, &[KILLED, MISSED]).unwrap();
        let args = args.unwrap();
        assert!(
            args.starts_with(&format!(
                "--in-diff {} -F ^src/lib\\.rs:12:9: replace >= with < in is_adult$ -F ",
                diff.display()
            )),
            "{args}"
        );
        assert!(args.ends_with("-F ^src/lib\\.rs:12:5: replace is_adult \\-> bool with true$\n"), "{args}");
        let outcomes: Vec<_> = mutated(&m).picks.iter().map(|p| (p.name.clone(), p.why.clone(), p.outcome)).collect();
        assert_eq!(
            outcomes,
            [
                (KILLED.to_string(), format!("why {KILLED}"), Some(MutantStatus::Killed)),
                (MISSED.to_string(), format!("why {MISSED}"), Some(MutantStatus::Survived))
            ]
        );
        assert_eq!(mutated(&m).change, MutantCounts { killed: 1, survived: 1, unviable: 0 });
        assert_eq!(m.suites[0].mutants.len(), 2);
        let v = crate::ratchet::check(&Baseline::default(), &m, &Default::default());
        assert_eq!(v.violations.iter().map(|v| v.metric).collect::<Vec<_>>(), [Bound::SurvivedMutants]);
        // The same picks again: nothing runs, a new reason is kept.
        let mutants = Mutants::Picks(vec![
            Pick { why: "new".into(), ..picks(&[KILLED])[0].clone() },
            picks(&[MISSED])[0].clone(),
        ]);
        std::fs::remove_file(repo.join(OUT).join("rust/args")).unwrap();
        let again = current(repo, &cfg, &Options { mutants, fresh: false, dry_run: false, timed: false }).unwrap();
        assert!(!repo.join(OUT).join("rust/args").exists());
        assert_eq!(mutated(&again).picks[0].why, "new");
        // Other picks of the same tree run.
        assert!(run(repo, &cfg, &[KILLED]).unwrap().1.is_some());

        // The suite moved to `sub/`, its report found by a glob: the diff is
        // relative to its root, the candidates' files to the repository.
        std::fs::create_dir(repo.join("sub")).unwrap();
        for f in ["Cargo.toml", "src"] {
            std::fs::rename(repo.join(f), repo.join("sub").join(f)).unwrap();
        }
        cfg.suites[0].root = "sub".into();
        let glob = "$SLIMFIT_OUT/mutants.out/outcome*.json";
        cfg.suites[0].overrides.mutants_report =
            Some(crate::config::Report { format: crate::formats::MutantsFormat::CargoMutants, path: glob.into() });
        let (m, _) = run(repo, &cfg, &[KILLED]).unwrap();
        assert_eq!(mutated(&m).picks[0].outcome, Some(MutantStatus::Killed));
        assert_eq!(candidates(repo, &cfg).unwrap()["rust"][0].file, "sub/src/lib.rs");
        let patch = std::fs::read_to_string(repo.join(OUT).join("rust/change.diff")).unwrap();
        assert!(patch.contains("+++ b/src/lib.rs"), "{patch}");
    }

    /// `--mutants` with no cached test run of the tree runs the tests at its
    /// own (background) priority: that run is `untimed`, and holds no
    /// `test_seconds`. A plain, timed measurement of the same tree runs the
    /// tests again and keeps the mutants; then the time is held.
    #[test]
    fn a_test_run_inside_mutants_is_timed_again_by_a_plain_check() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        git(repo, &["branch", "base"]);
        cfg.check.target = Some("base".into());
        cfg.suites[0].overrides.mutants_tests = Some(MutantTests::All);
        let counter = tempfile::tempdir().unwrap();
        let runs = counter.path().join("test-runs");
        let test = format!("echo run >> '{}'; {}", runs.display(), cfg.suites[0].resolve().test_command);
        cfg.suites[0].overrides.test_command = Some(test);
        std::fs::write(repo.join("src/new.rs"), "pub fn n() -> u8 {\n    1\n}\n").unwrap();
        let ran = || std::fs::read_to_string(&runs).map_or(0, |t| t.lines().count());
        let measure = |timed| {
            current(repo, &cfg, &Options { mutants: Mutants::None, fresh: false, dry_run: false, timed }).unwrap()
        };

        let (m, args) = run(repo, &cfg, &[KILLED]).unwrap();
        assert!(args.is_some() && m.suites[0].untimed && ran() == 1);
        // A baseline it would exceed: no violation, and the ratchet keeps it.
        let held = crate::config::CheckConfig { time_slack: 0.0, ..Default::default() };
        let tight = Metrics { test_seconds: 0.001, ..m.suites[0].metrics.clone() };
        let base = Baseline { suites: [("rust".to_string(), tight)].into(), ..Baseline::default() };
        let v = crate::ratchet::check(&base, &m, &held);
        assert!(!v.violations.iter().any(|v| v.metric == Bound::TestSeconds), "{v:?}");
        let loosen = [Bound::SurvivedMutants];
        let next = crate::ratchet::ratchet(&base, &m, &held, &loosen, Some("t")).unwrap();
        assert_eq!(next.suites["rust"].test_seconds, 0.001);
        // Measurements that take no time from it reuse it as it is.
        assert_eq!((measure(false), ran()), (m.clone(), 1));

        // A plain check times the tests again, and keeps the mutants.
        let timed = measure(true);
        assert_eq!(ran(), 2);
        assert!(!timed.suites[0].untimed && timed.with_mutants);
        assert_eq!((&timed.suites[0].mutants, &timed.suites[0].mutated), (&m.suites[0].mutants, &m.suites[0].mutated));
        // Held, on a host quiet enough for it (ADR 0003): this one may not be.
        let mut quiet = timed.clone();
        quiet.suites[0].host_load = 0.0;
        let v = crate::ratchet::check(&base, &quiet, &held);
        assert!(v.violations.iter().any(|v| v.metric == Bound::TestSeconds), "{v:?}");
        // Cached now, timed: nothing runs again, `--mutants` included.
        assert_eq!((measure(true), run(repo, &cfg, &[KILLED]).unwrap().1, ran()), (timed, None, 2));
    }

    #[test]
    /// A test whose report says it started before the command, as reported.
    fn the_tests_durations_are_divided_by_the_hosts_slowdown() {
        let totals = Totals::default();
        let tests = vec![
            TestCase { id: "a".into(), seconds: 3.0, passed: true, start: None },
            TestCase { id: "b".into(), seconds: 1.5, passed: true, start: Some(102.0) },
            TestCase { id: "c".into(), seconds: 2.0, passed: true, start: Some(50.0) },
        ];
        let timing = Timing { command_seconds: 9.87654, slowdown: 1.23456, host_load: 0.25, started: 100.0 };
        let m = reduce(
            &crate::config::preset(crate::config::Preset::Rust),
            &totals,
            &Resolver::new(Path::new("/r"), &totals),
            tests,
            Coverage::default(),
            None,
            None,
            timing,
        );
        assert_eq!(
            m.tests.iter().map(|t| (t.seconds * 1e6).round()).collect::<Vec<_>>(),
            [2_430_016.0, 1_215_008.0, 2_000_000.0]
        );
        assert_eq!(
            (m.metrics.test_seconds, m.load_slowdown, m.command_seconds, m.host_load),
            (5.645, 1.235, 9.877, 0.25)
        );
    }

    /// Every function of a source file's coverage is keyed by its first
    /// line, with the tests that ran it by symbol; a test whose profile was
    /// unreadable ran them all; test code is left out, of functions and of
    /// lines. A test's own function is found by its path and hashed.
    #[test]
    fn reach_keys_each_source_function_by_its_first_line() {
        let repo = tempfile::tempdir().unwrap();
        let mut totals = Totals::default();
        let classifier = Classifier::new(&crate::config::preset(crate::config::Preset::Rust)).unwrap();
        let text = "fn a() {}\nfn b() {}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        a();\n    }\n}\n";
        std::fs::create_dir(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/a.rs"), text).unwrap();
        totals.files = classifier.count(&["src/a.rs".to_string()], |_| Some(text.into())).files;
        let at = format!("{}/src/a.rs", repo.path().display());
        let f = |name: &str, line, symbol: &str| Function {
            file: at.clone(),
            name: name.into(),
            line,
            hits: 0,
            symbol: symbol.into(),
        };
        // Two symbols of one line: a generic function's instantiations.
        let t_symbol = "_ZN4calc5tests1t17h0123456789abcdefE";
        let functions = vec![f("a", 1, "_Ra"), f("a2", 1, "_Ra2"), f("b", 2, "_Rb"), f("t", 5, t_symbol)];
        let coverage = Coverage { lines: Default::default(), functions };
        let t = |name: &str, ran: Option<&[&str]>, lines: &[u32]| formats::TestReach {
            binary: "calc".into(),
            name: name.into(),
            functions: ran.map(|r| r.iter().map(|s| s.to_string()).collect()),
            lines: [(at.clone(), lines.iter().copied().collect())].into(),
        };
        let reach = vec![
            t("tests::t", Some(&["_Ra", "_Ra2", t_symbol]), &[1, 5, 6, 7]),
            t("y", None, &[]),
            t("z", Some(&["_Rgone"]), &[2]),
        ];
        let timing = Timing { command_seconds: 0.0, slowdown: 1.0, host_load: 0.0, started: 0.0 };
        let suite = crate::config::preset(crate::config::Preset::Rust);
        let resolver = Resolver::new(repo.path(), &totals);
        let m = reduce(&suite, &totals, &resolver, vec![], coverage, Some(reach), None, timing);
        let reach = m.reach.unwrap();
        assert_eq!(reach.functions["src/a.rs"], [(1, vec![0, 1]), (2, vec![1])].into());
        assert_eq!(reach.tests.iter().map(ReachTest::id).collect::<Vec<_>>(), ["calc::tests::t", "calc::y", "calc::z"]);
        let lines = |ls: &[u32]| BTreeMap::from([("src/a.rs".to_string(), ls.to_vec())]);
        assert_eq!(reach.lines.unwrap(), [lines(&[1]), BTreeMap::new(), lines(&[2])]);
        let source = reach.tests[0].source.clone().unwrap();
        assert_eq!((source.file.as_str(), source.line), ("src/a.rs", 5));
        assert_eq!(reach.tests[1].source, None);
        // The hash is of the function's text: an edit inside it changes it, one outside does not.
        let hash = |text: &str| {
            std::fs::write(repo.path().join("src/a.rs"), text).unwrap();
            test_source("src/a.rs", 5, &resolver).map(|s| s.hash)
        };
        assert_eq!(hash(&text.replace("fn b() {}", "fn c() {}")), Some(source.hash.clone()));
        assert_ne!(hash(&text.replace("a();", "b();")), Some(source.hash));
        assert!(hash("fn a() {}\n").is_none(), "no line 5");
        assert!(hash(&text.replace("    }\n}", "  }\n}")).is_none(), "no closing brace");
        assert!(hash("\n\n\n\n    fn t() { a(); }\n").is_some(), "one line");
    }

    /// A test is found by its function's name; of two of that name, by the
    /// modules its path names; when that does not tell them apart, not at all.
    #[test]
    fn a_test_is_found_by_its_name_and_modules() {
        assert_eq!(fn_name("    pub(crate) async fn x<T>(t: T) {"), Some("x"));
        assert_eq!(fn_name("    let fn_x = 1;"), None);
        let at = |f: &str, l| (f.to_string(), l);
        let fns: BTreeMap<String, Vec<(String, u32)>> = [
            ("a".to_string(), vec![at("src/lib.rs", 3)]),
            ("b".to_string(), vec![at("tests/s/queue.rs", 5), at("tests/s/line.rs", 9)]),
        ]
        .into();
        assert_eq!(test_function(&fns, "tests::a"), Some(("src/lib.rs", 3)));
        assert_eq!(test_function(&fns, "line::b"), Some(("tests/s/line.rs", 9)));
        assert_eq!(test_function(&fns, "b"), None);
        assert_eq!(test_function(&fns, "c"), None);
    }

    /// No reach_report configured, or none written: nothing is recorded,
    /// and every mutant runs every test. One that is not the format
    /// configured is refused, with its fix.
    #[test]
    fn a_reach_report_is_read_when_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut suite = crate::config::preset(crate::config::Preset::Rust);
        let read = |s: &Suite| read_reach(dir.path(), dir.path(), s);
        assert!(read(&suite).unwrap().is_none());
        std::fs::write(dir.path().join(crate::tools::REACH), "test calc\tx\n").unwrap();
        assert_eq!(read(&suite).unwrap().unwrap().len(), 1);
        std::fs::write(dir.path().join(crate::tools::REACH), "garbled\n").unwrap();
        let f = crate::fault::of(read(&suite).unwrap_err());
        assert!(f[0].fault.starts_with("its reach_report is not lcov"), "{f:?}");
        suite.reach_report.path = String::new();
        assert!(read(&suite).unwrap().is_none());
    }

    #[test]
    fn resolver_matches_longest_suffix() {
        let mut totals = Totals::default();
        for f in ["svc/pkg/a.go", "svc/other/a.go", "app/src/main/kotlin/com/x/A.kt"] {
            totals.files.insert(f.into(), (Kind::Source, Default::default()));
        }
        let r = Resolver::new(Path::new("/r"), &totals);
        assert_eq!(r.resolve("example.com/m/pkg/a.go").as_deref(), Some("svc/pkg/a.go"));
        assert_eq!(r.resolve("/r/svc/other/a.go").as_deref(), Some("svc/other/a.go"));
        assert_eq!(r.resolve("com/x/A.kt").as_deref(), Some("app/src/main/kotlin/com/x/A.kt"));
        assert_eq!(r.resolve("/rustc/library/core/src/x.rs"), None);
    }

    #[test]
    fn spans_join_zero_runs_and_name_functions() {
        let hits: BTreeMap<u32, u64> = [(1, 1), (3, 0), (4, 0), (6, 0), (7, 2), (9, 0)].into();
        let f = vec![Function { file: "a".into(), name: "g".into(), line: 2, hits: 0, symbol: String::new() }];
        let s = spans("a", &hits, &f);
        assert_eq!(s.iter().map(|s| (s.start, s.end, s.lines)).collect::<Vec<_>>(), vec![(3, 6, 3), (9, 9, 1)]);
        assert_eq!(s[0].function.as_deref(), Some("g"));
    }
}

#[cfg(test)]
mod faults {
    use super::*;

    /// One suite's fault does not hide another's: each failing suite is
    /// named with its fault and its fix, and the healthy one is measured.
    #[test]
    fn every_suite_that_cannot_be_measured_names_its_fault_and_fix() {
        let (dir, mut cfg) = t::sample_repo();
        let bad: Config = toml::from_str(
            r#"
            [[suite]]
            name = "nocmd"
            preset = "kotlin"
            test_command = ""
            [[suite]]
            name = "noreport"
            preset = "rust"
            test_command = "echo built"
            [[suite]]
            name = "garbled"
            preset = "rust"
            test_command = "echo '<testsuites' > \"$SLIMFIT_OUT/junit.xml\""
            tests_report = { format = "junit", path = "$SLIMFIT_OUT/junit.xml" }
            "#,
        )
        .unwrap();
        cfg.suites.extend(bad.suites);
        let opts = Options { mutants: Mutants::None, fresh: true, dry_run: false, timed: false };
        let faults = crate::fault::of(current(dir.path(), &cfg, &opts).unwrap_err());
        let got: Vec<(&str, &str)> =
            faults.iter().map(|f| (f.fault.split(':').next().unwrap(), f.fix.as_str())).collect();
        assert_eq!(got.len(), 3, "{faults:?}");
        assert_eq!(got[0], ("suite nocmd", "set `test_command` in the suite's [[suite]] in slimfit.toml"));
        assert_eq!(got[1].0, "suite noreport");
        assert!(faults[1].fault.contains("no tests_report at") && faults[1].fault.contains("built"), "{faults:?}");
        assert!(got[1].1.ends_with("or set `tests_report` path in slimfit.toml to where it writes"), "{faults:?}");
        assert_eq!(got[2].0, "suite garbled");
        assert!(faults[2].fault.contains("its tests_report is not junit"), "{faults:?}");
        assert!(got[2].1.starts_with("make the command write junit there, or set `tests_report`"), "{faults:?}");
    }
}
