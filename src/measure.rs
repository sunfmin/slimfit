//! Measuring the working tree: run each suite's commands, read their
//! reports, and reduce them to `Measurement`. The result is cached under
//! `.slimfit/out/` and reused while the tree is unchanged.

use crate::config::{Config, Report, Suite};
use crate::fault::{self, Faults, fault};
use crate::formats::{self, MutationRun};
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
    /// as the suite's tool can be limited (ADR 0005).
    Change,
    /// Every mutant. The change's are still told apart, and held.
    All,
}

pub struct Options {
    pub mutants: Mutants,
    pub fresh: bool,
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
    if let Some(m) = tested.as_ref().filter(|m| {
        (!with_mutants || (m.with_mutants && m.since == base.mutants_at))
            && (opts.mutants != Mutants::All || m.suites.iter().all(|s| s.mutated.as_ref().is_some_and(|x| x.all)))
    }) {
        return Ok(m.clone());
    }
    let mut tested = tested.map(|m| m.suites.into_iter()).into_iter().flatten();
    let change = if with_mutants { Some(Change::since(repo, &wt, base.mutants_at.as_ref())?) } else { None };
    let mut suites = Vec::new();
    for s in &cfg.suites {
        let suite = s.resolve();
        eprintln!(
            "slimfit: measuring {} ({}){}",
            suite.name,
            suite.root,
            match opts.mutants {
                Mutants::None => "",
                Mutants::Change => " with the mutants of the change",
                Mutants::All => " with every mutant",
            }
        );
        let mutants = change.as_ref().map(|c| (c, opts.mutants == Mutants::All));
        let r = measure_suite(repo, &wt.files, &suite, mutants, tested.next());
        suites.push(r.map_err(|e| fault::within(&format!("suite {}", suite.name), e)));
    }
    // Every suite is measured, so one that fails does not hide another's faults.
    let suites = fault::all(suites)?;
    let m = Measurement {
        tree,
        with_mutants,
        snapshot: change.and_then(|c| c.snapshot),
        since: if with_mutants { base.mutants_at.clone() } else { None },
        suites,
    };
    std::fs::create_dir_all(repo.join(OUT))?;
    std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
    Ok(m)
}

/// The tree as it is now, as `mutants_at` records it; `None` before the
/// first commit.
pub fn snapshot(repo: &Path) -> Result<Option<Snapshot>> {
    let head = git::run(repo, &["rev-parse", "--verify", "HEAD"]).ok().map(|h| h.trim().to_string());
    let tree = git::write_tree(repo)?;
    Ok(head.map(|commit| Snapshot { commit, tree: Some(tree) }))
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
    /// tree; from HEAD when it lacks the commit too, or nothing is recorded.
    fn since(repo: &Path, wt: &git::Worktree, at: Option<&Snapshot>) -> Result<Change> {
        let to = git::write_tree(repo)?;
        let head = || -> Result<String> {
            match &wt.head {
                Some(h) => Ok(h.clone()),
                None => git::empty_tree(repo),
            }
        };
        let from = match at {
            Some(Snapshot { tree: Some(t), .. }) if git::has_tree(repo, t) => t.clone(),
            Some(s) if git::has_tree(repo, &s.commit) => {
                eprintln!(
                    "slimfit: the tree the baseline's mutants were measured at is not in this repository \
                     (another clone, or pruned): the change is taken from its commit {}",
                    s.commit
                );
                s.commit.clone()
            }
            Some(s) => {
                eprintln!(
                    "slimfit: the baseline's mutants_at commit {} is not in this repository: \
                     the change is taken from HEAD",
                    s.commit
                );
                head()?
            }
            None => {
                eprintln!("slimfit: the baseline records no mutants_at: the change is taken from HEAD");
                head()?
            }
        };
        let files = git::changed_files(repo, &from, &to)?;
        let snapshot = wt.head.clone().map(|commit| Snapshot { commit, tree: Some(to.clone()) });
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
    files: &[String],
    suite: &Suite,
    mutants: Option<(&Change, bool)>,
    tested: Option<SuiteMeasurement>,
) -> Result<SuiteMeasurement> {
    let root = repo.join(&suite.root);
    let out = repo.join(OUT).join(&suite.name);
    let classifier = Classifier::new(suite)?;
    let totals = classifier.count(files, |f| std::fs::read_to_string(repo.join(f)).ok());
    let resolver = Resolver::new(repo, &totals);
    // The tests of this tree already ran: only the mutants are new.
    if let (Some(mut s), Some((change, all))) = (tested, mutants) {
        std::fs::create_dir_all(&out)?;
        let m = mutate_change(repo, &root, &out, suite, &totals, &resolver, change, all)?;
        (s.mutants, s.mutation_tests, s.mutated) = (m.run.mutants, m.run.tests, Some(m.mutated));
        return Ok(s);
    }
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
    let (run, slowdown) = load::slowdown_during(|| shell(&root, &out, &suite.test_command, "test"));
    let (log, _) = run?;
    let command_seconds = started.elapsed().as_secs_f64();
    let tests = read_report(&root, &out, "tests_report", &suite.tests_report, &log, formats::read_tests)?;
    let coverage = read_report(&root, &out, "coverage_report", &suite.coverage_report, &log, formats::read_coverage)?;

    let mutation = match mutants {
        None => None,
        Some((change, all)) => Some(mutate_change(repo, &root, &out, suite, &totals, &resolver, change, all)?),
    };
    Ok(reduce(suite, &totals, &resolver, tests, coverage, mutation, Timing { command_seconds, slowdown }))
}

/// Runs the suite's mutants of `change` (every mutant when `all`, or when
/// the tool cannot be limited), and counts those in the change's lines.
#[allow(clippy::too_many_arguments)]
fn mutate_change(
    repo: &Path,
    root: &Path,
    out: &Path,
    suite: &Suite,
    totals: &Totals,
    resolver: &Resolver,
    change: &Change,
    all: bool,
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
    let full = |why: &str| -> Result<MutationRun> {
        if !why.is_empty() {
            eprintln!("slimfit: {}: mutating every file: {why}", suite.name);
        }
        mutate(root, out, suite, resolver, &suite.mutants_command, false)
    };
    // Whether the run holds only the change's mutants, so every one it finds
    // is in the change; otherwise they are told apart by line.
    let (run, by_tool, every) = if all {
        (full("")?, false, true)
    } else if files.is_empty() {
        eprintln!("slimfit: {}: no source line changed since the baseline's mutants: none to run", suite.name);
        (MutationRun::default(), true, false)
    } else if !suite.mutants_diff_flag.is_empty() {
        let diff = out.join("change.diff");
        std::fs::write(&diff, &patch)?;
        eprintln!("slimfit: {}: mutating the lines changed in {} file(s)", suite.name, files.len());
        let command =
            format!("{} {} {}", suite.mutants_command, suite.mutants_diff_flag, quoted(&diff.to_string_lossy()));
        (mutate(root, out, suite, resolver, &command, true)?, true, false)
    } else if !suite.mutants_file_flag.is_empty() {
        eprintln!("slimfit: {}: mutating the {} file(s) the change touches", suite.name, files.len());
        (mutate(root, out, suite, resolver, &limited(suite, &files), true)?, false, false)
    } else {
        (full("its mutation tool cannot be limited to a change")?, false, true)
    };
    let in_change: Vec<Mutant> = run
        .mutants
        .iter()
        .filter(|m| by_tool || lines.get(&m.file).is_some_and(|l| l.contains(&m.line)))
        .cloned()
        .collect();
    Ok(Mutation { mutated: Mutated { all: every, files, change: MutantCounts::of(&in_change) }, run })
}

/// One run of the mutation tool, its paths resolved to the suite's files.
/// `may_find_none`: a limited run may have no mutant to test, and a tool
/// may then write no report; a clean exit then reads as none.
fn mutate(
    root: &Path,
    out: &Path,
    suite: &Suite,
    resolver: &Resolver,
    command: &str,
    may_find_none: bool,
) -> Result<MutationRun> {
    // A report left by an earlier run would be read as this run's.
    for old in report_paths(root, out, &suite.mutants_report)? {
        std::fs::remove_file(old)?;
    }
    let (log, ok) = shell(root, out, command, "mutants")?;
    let reports = match read_reports(root, out, "mutants_report", &suite.mutants_report, &log) {
        Err(_) if may_find_none && ok => vec![],
        r => r?,
    };
    let mut run = formats::read_mutants(suite.mutants_report.format, &reports)
        .map_err(|e| unreadable("mutants_report", suite.mutants_report.format, e))?;
    for m in &mut run.mutants {
        m.file = resolver.resolve(&m.file).unwrap_or_else(|| m.file.clone());
    }
    Ok(run)
}

/// A mutation run, and what it covered.
pub(crate) struct Mutation {
    pub run: MutationRun,
    mutated: Mutated,
}

/// A run over every mutant, none of them in a change.
impl From<MutationRun> for Mutation {
    fn from(run: MutationRun) -> Self {
        Self { run, mutated: Mutated { all: true, files: vec![], change: MutantCounts::default() } }
    }
}

/// Runs `command` with `sh -c` in the suite root. A failing exit status is
/// not an error: failing tests still write reports. The log is kept; the
/// flag says whether the command exited 0.
fn shell(root: &Path, out: &Path, command: &str, label: &str) -> Result<(PathBuf, bool)> {
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
    let texts: Vec<String> = read_reports(root, out, field, report, log)?.into_iter().map(|(_, text)| text).collect();
    read(report.format, &texts).map_err(|e| unreadable(field, report.format, e))
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

/// The report's files, each with its path; `field` names it in slimfit.toml.
fn read_reports<F>(
    root: &Path,
    out: &Path,
    field: &str,
    report: &Report<F>,
    log: &Path,
) -> Result<Vec<(PathBuf, String)>> {
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
    paths
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
            Ok((p, text))
        })
        .collect()
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

/// How long the test command took, and how much the host's load slowed it.
pub(crate) struct Timing {
    pub command_seconds: f64,
    pub slowdown: f64,
}

pub(crate) fn reduce(
    suite: &Suite,
    totals: &Totals,
    resolver: &Resolver,
    mut tests: Vec<TestCase>,
    coverage: Coverage,
    mutation: Option<Mutation>,
    timing: Timing,
) -> SuiteMeasurement {
    // A test's duration as the runner timed it, less what the host's load
    // added to it (ADR 0003).
    for t in &mut tests {
        t.seconds /= timing.slowdown;
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
    let covered = lines.values().flat_map(|l| l.values()).filter(|h| **h > 0).count() as u64;
    let uncovered_total = lines.values().flat_map(|l| l.values()).filter(|h| **h == 0).count() as u64;
    let mut uncovered = Vec::new();
    for (file, hits) in &lines {
        uncovered.extend(spans(file, hits, functions.get(file).map(Vec::as_slice).unwrap_or(&[])));
    }
    uncovered.sort_by(|a, b| b.lines.cmp(&a.lines).then(a.file.cmp(&b.file)).then(a.start.cmp(&b.start)));

    let (mutants, mutation_tests, mutated) = match mutation {
        Some(m) => (m.run.mutants, m.run.tests, Some(m.mutated)),
        None => (Vec::new(), None, None),
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
        mutation_tests,
        mutated,
        command_seconds: (timing.command_seconds * 1000.0).round() / 1000.0,
        load_slowdown: (timing.slowdown * 1000.0).round() / 1000.0,
    }
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
            mutants_command = "f() {{ echo \"$@\" > \"$SLIMFIT_OUT/args\"; mkdir -p \"$SLIMFIT_OUT/mutants.out\"; cp {r}/outcomes.json \"$SLIMFIT_OUT/mutants.out\"; ln -sfn {r}/log \"$SLIMFIT_OUT/mutants.out/log\"; }}; f"
            "#
        ))
        .unwrap();
        (dir, cfg)
    }

    fn run(repo: &Path, cfg: &Config, base: &Baseline, mutants: Mutants) -> (Measurement, Option<String>) {
        let m = current(repo, cfg, base, &Options { mutants, fresh: true }).unwrap();
        (m, std::fs::read_to_string(repo.join(OUT).join("rust/args")).ok())
    }

    fn edit(repo: &Path, f: &str, from: &str, to: &str) {
        let text = std::fs::read_to_string(repo.join(f)).unwrap();
        std::fs::write(repo.join(f), text.replacen(from, to, 1)).unwrap();
    }

    fn at(m: &Measurement) -> Baseline {
        let loosen = [Bound::SurvivedMutants];
        crate::ratchet::ratchet(&Baseline::default(), m, &Default::default(), &loosen, Some("t")).unwrap()
    }

    /// `init` records the tree; `--mutants` then runs only the lines changed
    /// since (cargo-mutants given the diff), and a ratchet moves the tree on,
    /// so committing what was measured leaves nothing to run.
    #[test]
    fn mutants_run_on_the_lines_changed_since_the_tree_last_measured() {
        let (dir, cfg) = sample_repo();
        let repo = dir.path();
        let base = Baseline { mutants_at: snapshot(repo).unwrap(), ..Default::default() };

        let none = Mutated { all: false, files: vec![], change: MutantCounts::default() };
        // An edited line, and a new untracked file: the tool gets their diff.
        edit(repo, "src/lib.rs", "    age >= 18\n", "    age >= 18 // x\n");
        std::fs::write(repo.join("src/new.rs"), "pub fn n() -> u8 {\n    1\n}\n").unwrap();
        let (m, args) = run(repo, &cfg, &base, Mutants::Change);
        let diff = repo.join(OUT).join("rust/change.diff");
        assert_eq!(args.unwrap(), format!("--in-diff {}\n", diff.display()));
        let patch = std::fs::read_to_string(diff).unwrap();
        assert!(patch.contains("+    age >= 18 // x") && patch.contains("+++ b/src/new.rs"), "{patch}");
        let s = &m.suites[0];
        let mutated = s.mutated.as_ref().unwrap();
        assert_eq!(mutated.files, ["src/lib.rs", "src/new.rs"]);
        // The tool ran only the change's mutants: every one it reports counts.
        assert_eq!(mutated.change, MutantCounts::of(&s.mutants));
        // Its killers are read from the run's own logs.
        let killed: Vec<_> = s.mutants.iter().filter(|m| m.status == MutantStatus::Killed).collect();
        assert!(!killed.is_empty() && killed.iter().all(|m| m.killed_by.as_ref().is_some_and(|k| !k.is_empty())));
        let v = crate::ratchet::check(&base, &m, &Default::default());
        assert_eq!(v.violations.iter().map(|v| v.metric).collect::<Vec<_>>(), [Bound::SurvivedMutants]);

        // Ratcheted: the same tree, even cached, has no change left.
        let moved = at(&m);
        let again = current(repo, &cfg, &moved, &Options { mutants: Mutants::Change, fresh: false }).unwrap();
        assert_eq!(again.suites[0].mutated.as_ref(), Some(&none));
        // Committed as measured: still none, though HEAD moved.
        git(repo, &["add", "-A", "src"]);
        git(repo, &["commit", "-qm", "change"]);
        assert_eq!(run(repo, &cfg, &moved, Mutants::Change).1, None);

        // The tree pruned: from its commit, which is before the change.
        let mut pruned = moved.clone();
        pruned.mutants_at.as_mut().unwrap().tree = Some("0".repeat(40));
        let (m, _) = run(repo, &cfg, &pruned, Mutants::Change);
        assert_eq!(m.suites[0].mutated.as_ref().unwrap().files, ["src/lib.rs", "src/new.rs"]);
        // The commit gone too: from HEAD, clean.
        pruned.mutants_at.as_mut().unwrap().commit = "0".repeat(40);
        assert_eq!(run(repo, &cfg, &pruned, Mutants::Change).0.suites[0].mutated.as_ref(), Some(&none));
    }

    /// Before the first commit every line is the change's; a suite in a
    /// subdirectory gets the diff relative to its root.
    #[test]
    fn an_unborn_repository_is_all_change_and_a_suites_diff_is_relative_to_its_root() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        std::fs::remove_dir_all(repo.join(".git")).unwrap();
        git(repo, &["init", "-q"]);
        std::fs::create_dir(repo.join("sub")).unwrap();
        for f in ["Cargo.toml", "src"] {
            std::fs::rename(repo.join(f), repo.join("sub").join(f)).unwrap();
        }
        cfg.suites[0].root = "sub".into();
        // A report found by a glob.
        let glob = "$SLIMFIT_OUT/mutants.out/outcome*.json";
        cfg.suites[0].overrides.mutants_report =
            Some(crate::config::Report { format: crate::formats::MutantsFormat::CargoMutants, path: glob.into() });
        let (m, args) = run(repo, &cfg, &Baseline::default(), Mutants::Change);
        let mutated = m.suites[0].mutated.as_ref().unwrap();
        assert_eq!(mutated.files, ["sub/src/lib.rs"]);
        assert_eq!(mutated.change, MutantCounts::of(&m.suites[0].mutants));
        assert!(!m.suites[0].mutants.is_empty());
        assert!(args.unwrap().starts_with("--in-diff "));
        let patch = std::fs::read_to_string(repo.join(OUT).join("rust/change.diff")).unwrap();
        assert!(patch.contains("+++ b/src/lib.rs"), "{patch}");
        assert_eq!(m.snapshot, None);
    }

    /// A tool with only a file flag runs the touched files; only the
    /// survivors in the added lines are the change's (as when a tool with
    /// neither flag runs every mutant).
    #[test]
    fn without_a_diff_flag_the_changes_mutants_are_told_apart_by_line() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        let base = Baseline { mutants_at: snapshot(repo).unwrap(), ..Default::default() };
        edit(repo, "src/lib.rs", "    age >= 18\n", "    age >= 18 // x\n");
        // The report's mutants of line 12: two killed, one missed.
        let line12 = MutantCounts { killed: 2, survived: 1, unviable: 0 };
        cfg.suites[0].overrides.mutants_diff_flag = Some(String::new());
        cfg.suites[0].overrides.mutants_file_flag = Some("--file".into());
        let (m, got) = run(repo, &cfg, &base, Mutants::Change);
        assert_eq!(got.as_deref(), Some("--file src/lib.rs\n"));
        let mutated = m.suites[0].mutated.clone().unwrap();
        assert_eq!((mutated.all, mutated.files, mutated.change), (false, vec!["src/lib.rs".into()], line12));
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
    fn the_tests_durations_are_divided_by_the_hosts_slowdown() {
        let totals = Totals::default();
        let tests = vec![
            TestCase { id: "a".into(), seconds: 3.0, passed: true },
            TestCase { id: "b".into(), seconds: 1.5, passed: true },
        ];
        let timing = Timing { command_seconds: 9.87654, slowdown: 1.23456 };
        let m = reduce(
            &crate::config::preset(crate::config::Preset::Rust),
            &totals,
            &Resolver::new(Path::new("/r"), &totals),
            tests,
            Coverage::default(),
            None,
            timing,
        );
        assert_eq!(m.tests.iter().map(|t| (t.seconds * 1e6).round()).collect::<Vec<_>>(), [2_430_016.0, 1_215_008.0]);
        assert_eq!((m.metrics.test_seconds, m.load_slowdown, m.command_seconds), (3.645, 1.235, 9.877));
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
        let f = vec![Function { file: "a".into(), name: "g".into(), line: 2, hits: 0 }];
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
        let opts = Options { mutants: Mutants::None, fresh: true };
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
