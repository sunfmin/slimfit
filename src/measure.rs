//! Measuring the working tree: run each suite's commands, read their
//! reports, and reduce them to `Measurement`. The result is cached under
//! `.slimfit/out/` and reused while the tree is unchanged.

use crate::config::{Config, MutantTests, Report, Suite};
use crate::fault::{self, Faults, fault};
use crate::formats;
use crate::git;
use crate::load;
use crate::loc::{Classifier, Kind, Totals};
use crate::model::*;
use crate::ratchet::Baseline;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutants {
    None,
    /// The mutants of the change: the lines that differ from the tree the
    /// baseline's mutants were last measured at (`mutants_at`), as narrowly
    /// as the suite's tool can be limited (ADR 0005). There is no other:
    /// no run mutates the whole codebase (ADR 0006).
    Change,
}

pub struct Options {
    pub mutants: Mutants,
    pub fresh: bool,
    /// Write nothing in the repo: the commands write their reports to a
    /// directory of their own outside it, and the measurement is not cached.
    pub dry_run: bool,
}

/// The measurement of the tree as it is now: from the cache when it was
/// taken of this exact tree (with as many mutants as are wanted, from the
/// same `mutants_at`). A cached measurement of the tree that lacks those
/// mutants keeps its test run: only the mutation tool runs. `base` is the baseline whose `mutants_at` the
/// change is taken from.
pub fn current(repo: &Path, cfg: &Config, base: &Baseline, opts: &Options) -> Result<Measurement> {
    let wt = git::worktree(repo)?;
    let tree = wt.key.clone();
    let cache = repo.join(OUT).join("measurement.json");
    let with_mutants = opts.mutants != Mutants::None;
    // The cached measurement of this tree, when not asked to measure afresh.
    let tested = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|text| serde_json::from_str::<Measurement>(&text).ok())
        .filter(|m| !opts.fresh && m.tree == tree)
        .filter(|m| m.suites.iter().map(|s| &s.suite).eq(cfg.suites.iter().map(|s| &s.name)));
    if let Some(m) = tested.as_ref().filter(|m| !with_mutants || (m.with_mutants && m.since == base.mutants_at)) {
        return Ok(m.clone());
    }
    let scratch = Scratch::new(repo, opts.dry_run)?;
    let mut tested = tested.map(|m| m.suites.into_iter()).into_iter().flatten();
    // Nothing runs every mutant: a suite whose tool cannot be limited to a
    // change refuses `--mutants` before anything runs.
    if with_mutants {
        fault::all(cfg.suites.iter().map(|s| s.resolve()).map(|s| {
            if s.mutants_diff_flag.is_empty() && s.mutants_file_flag.is_empty() {
                Err(fault(
                    format!(
                        "suite {}: its mutation tool cannot be limited to a change, so `--mutants` would run every mutant",
                        s.name
                    ),
                    "if its tool takes a diff or a file, set the suite's `mutants_diff_flag` or `mutants_file_flag` in \
                     slimfit.toml; otherwise run `slimfit check` without `--mutants`: slimfit never mutates the whole \
                     codebase",
                ))
            } else {
                Ok(())
            }
        }))?;
    }
    let mut change = Lazy { repo, wt: &wt, at: base.mutants_at.as_ref(), change: None };
    let mut suites = Vec::new();
    for s in &cfg.suites {
        let suite = s.resolve();
        eprintln!(
            "slimfit: measuring {} ({}){}",
            suite.name,
            suite.root,
            if with_mutants { " with the mutants of the change" } else { "" }
        );
        let mutants = with_mutants.then_some(&mut change);
        let r = measure_suite(repo, &scratch.0, &wt.files, &suite, mutants, tested.next());
        suites.push(r.map_err(|e| fault::within(&format!("suite {}", suite.name), e)));
    }
    // Every suite is measured, so one that fails does not hide another's faults.
    let suites = fault::all(suites)?;
    let m = Measurement {
        tree,
        with_mutants,
        snapshot: change.change.and_then(|c| c.snapshot),
        since: if with_mutants { base.mutants_at.clone() } else { None },
        suites,
    };
    if !opts.dry_run {
        std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
    }
    Ok(m)
}

/// The tree as it is now, as `mutants_at` records it; `None` before the
/// first commit.
pub fn snapshot(repo: &Path) -> Result<Option<Snapshot>> {
    let head = git::head(repo)?;
    let tree = git::write_tree(repo, &head)?;
    Ok(head.commit.map(|commit| Snapshot { commit, tree: Some(tree) }))
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

/// The change, taken the first time a suite's mutants need it: a run that
/// fails before then spends no git calls on it.
struct Lazy<'a> {
    repo: &'a Path,
    wt: &'a git::Worktree,
    at: Option<&'a Snapshot>,
    change: Option<Change>,
}

impl Lazy<'_> {
    fn get(&mut self) -> Result<&Change> {
        if self.change.is_none() {
            self.change = Some(Change::since(self.repo, self.wt, self.at)?);
        }
        Ok(self.change.as_ref().expect("just taken"))
    }
}

/// The change a mutants run measures: from the tree the baseline's mutants
/// were last measured at to the working tree now.
struct Change {
    from: String,
    to: String,
    /// The files that differ, repository relative.
    files: Vec<String>,
    /// HEAD and the tree now: what `ratchet` records as `mutants_at`.
    snapshot: Option<Snapshot>,
}

impl Change {
    /// From `at`'s tree; from its commit when this repository lacks the
    /// tree; from the merge base with the branch's upstream when it lacks
    /// the commit too, or nothing is recorded. With none of them it
    /// refuses, rather than measure more than the change.
    fn since(repo: &Path, wt: &git::Worktree, at: Option<&Snapshot>) -> Result<Change> {
        let to = git::write_tree(repo, &wt.head)?;
        // A tree or commit this repository lacks fails the diff: the next is tried.
        let changed = |from: &str| git::changed_files(repo, from, &to).ok().map(|f| (from.to_string(), f));
        let recorded = at.and_then(|s| {
            s.tree.as_deref().and_then(changed).or_else(|| {
                let c = changed(&s.commit);
                if s.tree.is_some() && c.is_some() {
                    eprintln!(
                        "slimfit: the tree the baseline's mutants were measured at is not in this repository \
                         (another clone, or pruned): the change is taken from its commit {}",
                        s.commit
                    );
                }
                c
            })
        });
        let why = match at {
            None => "the baseline records no mutants_at".to_string(),
            Some(s) => format!("the baseline's mutants_at commit {} is not in this repository", s.commit),
        };
        let found = recorded.or_else(|| {
            let base = git::merge_base_with_upstream(repo)?;
            eprintln!("slimfit: {why}: the change is taken from the merge base with the branch's upstream, {base}");
            changed(&base)
        });
        let Some((from, files)) = found else {
            return Err(fault(
                format!(
                    "{why}, and the branch has no upstream to take the change from: `--mutants` would run more than the change"
                ),
                "record where changes start: `slimfit ratchet` records the tree now as mutants_at (commit the baseline); \
                 or set the branch's upstream (`git branch --set-upstream-to <branch it builds on>`) to take the change \
                 since their merge base",
            ));
        };
        let snapshot = wt.head.commit.clone().map(|commit| Snapshot { commit, tree: Some(to.clone()) });
        Ok(Change { from, to, files, snapshot })
    }
}

/// The lines each file gains in a unified diff (new-side numbers), keyed by
/// `prefix` + the diff's `b/` path.
fn added_lines(patch: &str, prefix: &str) -> BTreeMap<String, BTreeSet<u32>> {
    let mut out: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    // "" while in a deleted file, which adds no line.
    let (mut file, mut line, mut old_left, mut new_left) = (String::new(), 0u32, 0u32, 0u32);
    for l in patch.lines() {
        if old_left > 0 || new_left > 0 {
            match l.as_bytes().first() {
                Some(b' ') => {
                    (old_left, new_left, line) = (old_left.saturating_sub(1), new_left.saturating_sub(1), line + 1)
                }
                Some(b'-') => old_left = old_left.saturating_sub(1),
                Some(b'+') => {
                    out.entry(file.clone()).or_default().insert(line);
                    (new_left, line) = (new_left.saturating_sub(1), line + 1);
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
            let mut parts = h.split(' ');
            let (_, o) = count(parts.next().unwrap_or(""));
            let (start, n) = count(parts.next().unwrap_or("").trim_start_matches('+'));
            (line, old_left, new_left) = (start, o, n);
        }
    }
    out
}

/// `command` limited to `files` (repository relative) with the suite's file flag.
fn limited(suite: &Suite, files: &[String]) -> String {
    let mut command = suite.mutants_command.clone();
    for f in files {
        command.push_str(&format!(" {} {}", suite.mutants_file_flag, quoted(relative(suite, f))));
    }
    command
}

/// `f` (repository relative) relative to the suite's root.
fn relative<'a>(suite: &Suite, f: &'a str) -> &'a str {
    let prefix = if suite.root == "." { String::new() } else { format!("{}/", suite.root.trim_end_matches('/')) };
    f.strip_prefix(&prefix).unwrap_or(f)
}

fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn measure_suite(
    repo: &Path,
    scratch: &Path,
    files: &[String],
    suite: &Suite,
    mutants: Option<&mut Lazy>,
    tested: Option<SuiteMeasurement>,
) -> Result<SuiteMeasurement> {
    let root = repo.join(&suite.root);
    let out = scratch.join(&suite.name);
    let classifier = Classifier::new(suite)?;
    let totals = classifier.count(files, |f| std::fs::read_to_string(repo.join(f)).ok());
    let resolver = Resolver::new(repo, &totals);
    // The tests of this tree already ran: only the mutants are new.
    let mutants = match (tested, mutants) {
        (Some(mut s), Some(change)) => {
            std::fs::create_dir_all(&out)?;
            let m = mutate_change(repo, &root, &out, suite, &totals, &resolver, change.get()?, &s)?;
            (s.mutants, s.mutated) = (m.mutants, Some(m.mutated));
            return Ok(s);
        }
        (_, mutants) => mutants,
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
    let (run, slowdown) = load::slowdown_during(|| shell(&root, &out, &suite.test_command, "test", &[]));
    let (log, _) = run?;
    let command_seconds = started.elapsed().as_secs_f64();
    let tests = read_report(&root, &out, "tests_report", &suite.tests_report, &log, formats::read_tests)?;
    let coverage = read_report(&root, &out, "coverage_report", &suite.coverage_report, &log, formats::read_coverage)?;
    let reach = read_reach(&root, &out, suite)?;

    let timing = Timing { command_seconds, slowdown, started: since_epoch };
    let mut s = reduce(suite, &totals, &resolver, tests, coverage, reach, None, timing);
    if let Some(change) = mutants {
        let m = mutate_change(repo, &root, &out, suite, &totals, &resolver, change.get()?, &s)?;
        (s.mutants, s.mutated) = (m.mutants, Some(m.mutated));
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

/// Runs the suite's mutants of `change`, and counts those in the change's
/// lines. `tested` is this tree's test run: which tests each mutant runs is
/// chosen from it.
#[allow(clippy::too_many_arguments)]
fn mutate_change(
    repo: &Path,
    root: &Path,
    out: &Path,
    suite: &Suite,
    totals: &Totals,
    resolver: &Resolver,
    change: &Change,
    tested: &SuiteMeasurement,
) -> Result<Mutation> {
    let sources: Vec<String> =
        change.files.iter().filter(|f| matches!(totals.files.get(*f), Some((Kind::Source, _)))).cloned().collect();
    let patch = if sources.is_empty() {
        String::new()
    } else {
        git::diff(repo, &change.from, &change.to, &suite.root, &sources)?
    };
    let prefix = if suite.root == "." { String::new() } else { format!("{}/", suite.root.trim_end_matches('/')) };
    let lines = added_lines(&patch, &prefix);
    let files: Vec<String> = lines.keys().cloned().collect();
    // Whether the run holds only the change's mutants, so every one it finds
    // is in the change; otherwise they are told apart by line.
    let (run, by_tool) = if files.is_empty() {
        eprintln!("slimfit: {}: no source line changed since the baseline's mutants: none to run", suite.name);
        (Vec::new(), true)
    } else if !suite.mutants_diff_flag.is_empty() {
        let diff = out.join("change.diff");
        std::fs::write(&diff, &patch)?;
        eprintln!("slimfit: {}: mutating the lines changed in {} file(s)", suite.name, files.len());
        let command =
            format!("{} {} {}", suite.mutants_command, suite.mutants_diff_flag, quoted(&diff.to_string_lossy()));
        let env = match suite.mutants_tests {
            MutantTests::Reached => selecting(root, out, suite, &prefix, tested, &command)?,
            MutantTests::All => Vec::new(),
        };
        (mutate(root, out, suite, resolver, &command, &env)?, true)
    } else {
        // `current` refuses `--mutants` for a tool with neither flag.
        eprintln!("slimfit: {}: mutating the {} file(s) the change touches", suite.name, files.len());
        (mutate(root, out, suite, resolver, &limited(suite, &files), &[])?, false)
    };
    let in_change: Vec<Mutant> =
        run.iter().filter(|m| by_tool || lines.get(&m.file).is_some_and(|l| l.contains(&m.line))).cloned().collect();
    Ok(Mutation { mutated: Mutated { files, change: MutantCounts::of(&in_change) }, mutants: run })
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
        Self { mutants, mutated: Mutated { files: vec![], change: MutantCounts::default() } }
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
fn read_reports<F>(root: &Path, out: &Path, field: &str, report: &Report<F>, log: &Path) -> Result<Vec<String>> {
    let pattern = report_pattern(root, out, report);
    // An empty file is no report: a shell redirect creates one even when
    // the tool behind it is missing.
    let paths: Vec<PathBuf> =
        expand(&pattern)?.into_iter().filter(|p| p.metadata().is_ok_and(|m| m.len() > 0)).collect();
    if paths.is_empty() {
        let tail = std::fs::read_to_string(log).unwrap_or_default();
        let tail: Vec<&str> = tail.lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(fault(
            format!("no {field} at {pattern}; the command's log ends:\n{}", tail.join("\n")),
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
/// and when it started (seconds since the Unix epoch).
pub(crate) struct Timing {
    pub command_seconds: f64,
    pub slowdown: f64,
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
    // Every function by its whole demangled path, `crate::module::name`.
    let paths: Vec<(String, &Function)> = functions
        .values()
        .flatten()
        .filter_map(|f| Some((format!("{:#}", rustc_demangle::try_demangle(&f.symbol).ok()?), f)))
        .collect();
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
        let source = test_function(&paths, &t.binary, &t.name).and_then(|f| test_source(f, resolver));
        reach.tests.push(ReachTest { binary: t.binary, name: t.name, source });
    }
    reach
}

/// The function of test `name` in nextest binary `binary`: the one whose
/// path ends in `::name`; of several, the one in the binary's crate.
fn test_function<'a>(paths: &[(String, &'a Function)], binary: &str, name: &str) -> Option<&'a Function> {
    let suffix = format!("::{name}");
    let found: Vec<&(String, &Function)> = paths.iter().filter(|(p, _)| p.ends_with(&suffix)).collect();
    if let [(_, f)] = found.as_slice() {
        return Some(f);
    }
    // nextest names a binary `package`, `package::target` or `package::kind/target`.
    let target = binary.rsplit("::").next().unwrap_or(binary);
    let krate = target.rsplit('/').next().unwrap_or(target).replace('-', "_");
    let mine: Vec<&&(String, &Function)> = found.iter().filter(|(p, _)| *p == format!("{krate}{suffix}")).collect();
    if let [(_, f)] = mine.as_slice() { Some(f) } else { None }
}

/// Where test function `f` is and the hash of its text: from its `fn` line
/// to the brace that closes it, found as rustfmt places it (alone on its
/// line, indented as the `fn`), or on the `fn` line itself.
fn test_source(f: &Function, resolver: &Resolver) -> Option<TestSource> {
    let text = resolver.read(&f.file)?;
    let lines: Vec<&str> = text.lines().skip(f.line.checked_sub(1)? as usize).collect();
    let first = lines.first()?;
    let indent = &first[..first.len() - first.trim_start().len()];
    let one_line = first.contains('{') && first.matches('{').count() == first.matches('}').count();
    let close = format!("{indent}}}");
    let n = if one_line { 0 } else { lines.iter().position(|l| l.trim_end() == close)? };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in lines[..=n].join("\n").bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    Some(TestSource { file: f.file.clone(), line: f.line, hash: format!("{hash:016x}") })
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
    use crate::ratchet::Bound;

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
    /// the mutation "tool" also records the arguments it was given.
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
            mutants_command = "f() {{ echo \"$@\" > \"$SLIMFIT_OUT/args\"; mkdir -p \"$SLIMFIT_OUT/mutants.out\"; cp {r}/outcomes.json \"$SLIMFIT_OUT/mutants.out\"; }}; f"
            "#
        ))
        .unwrap();
        (dir, cfg)
    }

    /// The measurement, and the arguments the mutation "tool" got (`None`:
    /// it did not run). Not `fresh`: a cached test run of the same tree is
    /// kept, so only the mutants run.
    fn run(repo: &Path, cfg: &Config, base: &Baseline) -> (Measurement, Option<String>) {
        let args = repo.join(OUT).join("rust/args");
        let _ = std::fs::remove_file(&args);
        let m = current(repo, cfg, base, &Options { mutants: Mutants::Change, fresh: false, dry_run: false }).unwrap();
        (m, std::fs::read_to_string(args).ok())
    }

    fn mutated(m: &Measurement) -> Mutated {
        m.suites[0].mutated.clone().unwrap()
    }

    /// One fixture through the change-only rule. `init`'s tree is the base;
    /// a tool with only a file flag runs the touched files and the change's
    /// mutants are told apart by line; cargo-mutants is given the diff and
    /// all it reports is the change's; a ratchet moves the tree on, so
    /// committing what was measured leaves nothing; a pruned tree falls back
    /// to its commit. With no mutants_at it refuses unless the branch has an
    /// upstream, whose merge base it then takes; a tool with neither flag
    /// refuses `--mutants`. Nothing runs every mutant.
    #[test]
    fn mutants_run_on_the_lines_changed_since_the_tree_last_measured() {
        let (dir, mut cfg) = sample_repo();
        // Every test on every mutant: which tests run is not this test's.
        cfg.suites[0].overrides.mutants_tests = Some(MutantTests::All);
        let repo = dir.path();
        let base = Baseline { mutants_at: snapshot(repo).unwrap(), ..Default::default() };
        let text = std::fs::read_to_string(repo.join("src/lib.rs")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), text.replacen("    age >= 18\n", "    age >= 18 // x\n", 1)).unwrap();
        let files = ["src/lib.rs", "src/new.rs"];

        // Only a file flag: the touched files; of the report's mutants, those
        // of line 12 (two killed, one missed) are the change's.
        let mut by_file = cfg.clone();
        by_file.suites[0].overrides.mutants_diff_flag = Some(String::new());
        by_file.suites[0].overrides.mutants_file_flag = Some("--file".into());
        let (m, args) = run(repo, &by_file, &base);
        assert_eq!(args.as_deref(), Some("--file src/lib.rs\n"));
        let line12 = MutantCounts { killed: 2, survived: 1, unviable: 0 };
        assert_eq!((mutated(&m).files, mutated(&m).change), (vec![files[0].into()], line12));

        // A new untracked file too (another tree, so measured again).
        std::fs::write(repo.join("src/new.rs"), "pub fn n() -> u8 {\n    1\n}\n").unwrap();

        // A diff flag: the tool gets the change's diff, and every mutant it
        // reports is the change's.
        let (m, args) = run(repo, &cfg, &base);
        let diff = repo.join(OUT).join("rust/change.diff");
        assert_eq!(args.unwrap(), format!("--in-diff {}\n", diff.display()));
        let patch = std::fs::read_to_string(diff).unwrap();
        assert!(patch.contains("+    age >= 18 // x") && patch.contains("+++ b/src/new.rs"), "{patch}");
        let s = &m.suites[0];
        assert_eq!(
            (mutated(&m).files, mutated(&m).change),
            (files.map(String::from).to_vec(), MutantCounts::of(&s.mutants))
        );
        assert!(s.mutants.iter().any(|m| m.status == MutantStatus::Killed));
        let v = crate::ratchet::check(&base, &m, &Default::default());
        assert_eq!(v.violations.iter().map(|v| v.metric).collect::<Vec<_>>(), [Bound::SurvivedMutants]);

        // Ratcheted: the same tree has no change left; committed as
        // measured, none either, though HEAD moved.
        let loosen = [Bound::SurvivedMutants];
        let moved = crate::ratchet::ratchet(&Baseline::default(), &m, &Default::default(), &loosen, Some("t")).unwrap();
        let none = Mutated { files: vec![], change: MutantCounts::default() };
        assert_eq!((run(repo, &cfg, &moved).0.suites[0].mutated.clone(), None), (Some(none.clone()), None::<()>));
        git(repo, &["add", "-A", "src"]);
        git(repo, &["commit", "-qm", "change"]);
        let (m, args) = run(repo, &cfg, &moved);
        assert_eq!((mutated(&m), args), (none, None));
        // The tree pruned: from its commit, which is before the change.
        let mut pruned = moved;
        pruned.mutants_at.as_mut().unwrap().tree = Some("0".repeat(40));
        assert_eq!(mutated(&run(repo, &cfg, &pruned).0).files, files);

        // No mutants_at and no upstream: refused, no mutant run.
        let refused = |cfg: &Config| {
            let _ = std::fs::remove_file(repo.join(OUT).join("rust/args"));
            let e = current(
                repo,
                cfg,
                &Baseline::default(),
                &Options { mutants: Mutants::Change, fresh: false, dry_run: false },
            );
            let faults = crate::fault::of(e.unwrap_err());
            assert!(!repo.join(OUT).join("rust/args").exists(), "a mutant ran");
            faults.into_iter().map(|f| f.fault).collect::<Vec<_>>().join("\n")
        };
        assert!(refused(&cfg).contains("records no mutants_at, and the branch has no upstream"));
        // A tool that takes neither a diff nor a file: `--mutants` refuses up
        // front, rather than run every mutant.
        by_file.suites[0].overrides.mutants_file_flag = Some(String::new());
        assert!(refused(&by_file).contains("cannot be limited to a change"));

        // No mutants_at, an upstream: the change since the merge base with
        // it. The suite moved to `sub/`, its report found by a glob: the
        // diff is relative to its root.
        git(repo, &["branch", "base", "HEAD~1"]);
        git(repo, &["branch", "--set-upstream-to", "base"]);
        std::fs::create_dir(repo.join("sub")).unwrap();
        for f in ["Cargo.toml", "src"] {
            std::fs::rename(repo.join(f), repo.join("sub").join(f)).unwrap();
        }
        cfg.suites[0].root = "sub".into();
        let glob = "$SLIMFIT_OUT/mutants.out/outcome*.json";
        cfg.suites[0].overrides.mutants_report =
            Some(crate::config::Report { format: crate::formats::MutantsFormat::CargoMutants, path: glob.into() });
        let (m, args) = run(repo, &cfg, &Baseline::default());
        assert_eq!(mutated(&m).files, ["sub/src/lib.rs", "sub/src/new.rs"]);
        assert!(!m.suites[0].mutants.is_empty() && args.unwrap().starts_with("--in-diff "));
        let patch = std::fs::read_to_string(repo.join(OUT).join("rust/change.diff")).unwrap();
        assert!(patch.contains("+++ b/src/lib.rs"), "{patch}");
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
        let want: BTreeMap<String, BTreeSet<u32>> =
            [("sub/a.rs".into(), [2, 10, 11].into()), ("sub/n.rs".into(), [1].into())].into();
        assert_eq!(got, want);
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
        let timing = Timing { command_seconds: 9.87654, slowdown: 1.23456, started: 100.0 };
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
        assert_eq!((m.metrics.test_seconds, m.load_slowdown, m.command_seconds), (5.645, 1.235, 9.877));
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
        let timing = Timing { command_seconds: 0.0, slowdown: 1.0, started: 0.0 };
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
            test_source(&Function { file: "src/a.rs".into(), ..f("t", 5, t_symbol) }, &resolver).map(|s| s.hash)
        };
        assert_eq!(hash(&text.replace("fn b() {}", "fn c() {}")), Some(source.hash.clone()));
        assert_ne!(hash(&text.replace("a();", "b();")), Some(source.hash));
        assert!(hash("fn a() {}\n").is_none(), "no line 5");
        assert!(hash(&text.replace("    }\n}", "  }\n}")).is_none(), "no closing brace");
        assert!(hash("\n\n\n\n    fn t() { a(); }\n").is_some(), "one line");
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
        let opts = Options { mutants: Mutants::None, fresh: true, dry_run: false };
        let faults = crate::fault::of(current(dir.path(), &cfg, &Baseline::default(), &opts).unwrap_err());
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
