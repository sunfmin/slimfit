//! Measuring the working tree: run each suite's commands, read their
//! reports, and reduce them to `Measurement`. The result is cached under
//! `.slimfit/out/` and reused while the tree is unchanged.

use crate::config::{Config, Report, Suite};
use crate::formats::{self, MutationRun};
use crate::git;
use crate::load;
use crate::loc::{Classifier, Kind, Totals};
use crate::model::*;
use crate::ratchet::Baseline;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub const OUT: &str = ".slimfit/out";

/// How much mutation testing a measurement runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutants {
    None,
    /// Only the source files changed since the baseline's mutants were
    /// measured, where the tool can be limited to files and the baseline
    /// has per-file counts; every file otherwise.
    Changed,
    /// `Changed`, then some of the files the baseline has no count for yet
    /// (all of a suite's files when it has no mutants yet), one run of the
    /// tool per file, while the budget lasts. At least one file is filled.
    Fill(Budget),
    All,
}

/// How many pending files a `Mutants::Fill` run may measure: at most
/// `files` of them, and none started once the command has run for
/// `seconds` less the longest file so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub files: Option<usize>,
    pub seconds: Option<u64>,
}

/// A budget being spent, across the suites of one command.
struct Spending {
    budget: Budget,
    started: Instant,
    filled: usize,
    longest: f64,
}

impl Spending {
    fn more(&self) -> bool {
        let fits = |s: u64| self.started.elapsed().as_secs_f64() + self.longest <= s as f64;
        self.filled == 0 || (self.budget.files.is_none_or(|n| self.filled < n) && self.budget.seconds.is_none_or(fits))
    }
}

pub struct Options {
    pub mutants: Mutants,
    pub fresh: bool,
}

/// The measurement of the tree as it is now: from the cache when it was
/// taken of this exact tree (and with as many mutants as are wanted).
/// `base` is the baseline a `Mutants::Changed` run builds on.
pub fn current(repo: &Path, cfg: &Config, base: &Baseline, opts: &Options) -> Result<Measurement> {
    let started = Instant::now();
    let tree = git::tree_key(repo)?;
    let cache = repo.join(OUT).join("measurement.json");
    let filling = matches!(opts.mutants, Mutants::Fill(_));
    if !opts.fresh
        && !filling
        && let Ok(text) = std::fs::read_to_string(&cache)
        && let Ok(m) = serde_json::from_str::<Measurement>(&text)
        && m.tree == tree
        && (m.with_mutants || opts.mutants == Mutants::None)
        && (opts.mutants != Mutants::All || m.suites.iter().all(|s| s.mutated.is_none()))
        && m.suites.iter().map(|s| &s.suite).eq(cfg.suites.iter().map(|s| &s.name))
    {
        return Ok(m);
    }
    let files = git::files(repo)?;
    let snapshot = git::snapshot(repo)?;
    let changed = match (&base.mutants_at, opts.mutants) {
        (Some(then), Mutants::Changed | Mutants::Fill(_)) => match git::changed_since(repo, then) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("slimfit: mutating every file: no diff from the baseline's commit ({e:#})");
                None
            }
        },
        (None, Mutants::Changed) => {
            eprintln!("slimfit: mutating every file: the baseline records no commit its mutants were measured at");
            None
        }
        _ => None,
    };
    let mut spending = match opts.mutants {
        Mutants::Fill(budget) => Some(Spending { budget, started, filled: 0, longest: 0.0 }),
        _ => None,
    };
    let mut suites = Vec::new();
    for s in &cfg.suites {
        let suite = s.resolve();
        eprintln!(
            "slimfit: measuring {} ({}){}",
            suite.name,
            suite.root,
            match opts.mutants {
                Mutants::None => "",
                Mutants::Changed => " with the mutants of changed files",
                Mutants::Fill(_) => " with the mutants of changed files and of some with no count yet",
                Mutants::All => " with every mutant",
            }
        );
        let prior = match opts.mutants {
            Mutants::None => None,
            Mutants::Changed | Mutants::Fill(_) => {
                Some(Prior { changed: changed.as_ref(), base: base.suites.get(&suite.name), all: false })
            }
            Mutants::All => Some(Prior { changed: None, base: None, all: true }),
        };
        suites.push(measure_suite(repo, &files, &suite, prior, spending.as_mut())?);
    }
    let m = Measurement { tree, with_mutants: opts.mutants != Mutants::None, piece: filling, snapshot, suites };
    std::fs::create_dir_all(repo.join(OUT))?;
    std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
    Ok(m)
}

/// What a run that mutates only some files builds on: the files changed
/// since the baseline's mutants were measured (`None`: unknown), and the
/// suite's baseline. `all`: every file was asked for.
#[derive(Clone, Copy)]
struct Prior<'a> {
    changed: Option<&'a BTreeSet<String>>,
    base: Option<&'a Metrics>,
    all: bool,
}

/// The mutants to run, the per-file counts kept from the baseline for the
/// files not mutated, and the files that have no count yet.
struct Partial {
    targets: Vec<String>,
    kept: BTreeMap<String, MutantCounts>,
    pending: Vec<String>,
}

/// Why a suite's mutants cannot be limited to some files, if so. A run that
/// fills pending files may start a suite with no mutants yet: all its
/// source files are then pending.
fn partial(suite: &Suite, totals: &Totals, prior: Prior, filling: bool) -> std::result::Result<Partial, String> {
    if prior.all {
        return Err("asked for every mutant".into());
    }
    if suite.mutants_file_flag.is_empty() {
        return Err("its mutation tool cannot be limited to files".into());
    }
    let source = |f: &String| matches!(totals.files.get(f), Some((Kind::Source, _)));
    let Some((b, total)) = prior.base.and_then(|b| Some((b, b.mutants?))) else {
        if filling {
            let pending = totals.files.keys().filter(|f| source(f)).cloned().collect();
            return Ok(Partial { targets: vec![], kept: BTreeMap::new(), pending });
        }
        return Err("no mutants in the baseline".into());
    };
    let Some(changed) = prior.changed else { return Err("no files known to be unchanged".into()) };
    if MutantCounts::sum(b.mutant_files.values()) != total {
        return Err("the baseline has no per-file mutant counts (ratchet once with --all-mutants)".into());
    }
    let pending: Vec<String> = b.mutants_pending.iter().filter(|f| source(f)).cloned().collect();
    let targets = changed.iter().filter(|f| source(f) && !pending.contains(f)).cloned().collect();
    let kept = b.mutant_files.iter().filter(|(f, _)| !changed.contains(*f)).map(|(f, c)| (f.clone(), *c)).collect();
    Ok(Partial { targets, kept, pending })
}

/// `command` limited to `files` (repository relative) with the suite's file flag.
fn limited(suite: &Suite, files: &[String]) -> String {
    let prefix = if suite.root == "." { String::new() } else { format!("{}/", suite.root.trim_end_matches('/')) };
    let mut command = suite.mutants_command.clone();
    for f in files {
        let rel = f.strip_prefix(&prefix).unwrap_or(f);
        command.push_str(&format!(" {} '{}'", suite.mutants_file_flag, rel.replace('\'', "'\\''")));
    }
    command
}

fn measure_suite(
    repo: &Path,
    files: &[String],
    suite: &Suite,
    prior: Option<Prior>,
    spending: Option<&mut Spending>,
) -> Result<SuiteMeasurement> {
    let root = repo.join(&suite.root);
    let out = repo.join(OUT).join(&suite.name);
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out)?;

    let classifier = Classifier::new(suite)?;
    let totals = classifier.count(files, |f| std::fs::read_to_string(repo.join(f)).ok());
    let resolver = Resolver::new(repo, &totals);

    let started = Instant::now();
    let (run, slowdown) = load::slowdown_during(|| shell(&root, &out, &suite.test_command, "test"));
    let (log, _) = run?;
    let command_seconds = started.elapsed().as_secs_f64();
    let tests = formats::read_tests(suite.tests_report.format, &read_report(&root, &out, &suite.tests_report, &log)?)?;
    let coverage =
        formats::read_coverage(suite.coverage_report.format, &read_report(&root, &out, &suite.coverage_report, &log)?)?;

    let mutation = match prior {
        None => None,
        Some(prior) => Some(match partial(suite, &totals, prior, spending.is_some()) {
            Err(why) => {
                if !prior.all {
                    eprintln!("slimfit: {}: mutating every file: {why}", suite.name);
                }
                Mutation { run: mutate(&root, &out, suite, &resolver, &suite.mutants_command, false)?, partial: None }
            }
            Ok(mut p) => {
                eprintln!("slimfit: {}: mutating {} changed file(s)", suite.name, p.targets.len());
                let mut run = if p.targets.is_empty() {
                    MutationRun::default()
                } else {
                    mutate(&root, &out, suite, &resolver, &limited(suite, &p.targets), true)?
                };
                if let Some(sp) = spending {
                    let mut filled = 0;
                    while filled < p.pending.len() && sp.more() {
                        let f = p.pending[filled].clone();
                        eprintln!("slimfit: {}: mutating {f}, which has no count yet", suite.name);
                        let t = Instant::now();
                        let one =
                            mutate(&root, &out, suite, &resolver, &limited(suite, std::slice::from_ref(&f)), true)?;
                        sp.longest = sp.longest.max(t.elapsed().as_secs_f64());
                        sp.filled += 1;
                        filled += 1;
                        run.mutants.extend(one.mutants);
                        p.targets.push(f);
                    }
                    p.pending.drain(..filled);
                }
                if !p.pending.is_empty() {
                    eprintln!(
                        "slimfit: {}: {} file(s) have no mutant count yet, so their mutants are not held: \
                         `slimfit ratchet --pending-seconds <s>` measures some",
                        suite.name,
                        p.pending.len()
                    );
                }
                Mutation { run, partial: Some(p) }
            }
        }),
    };
    Ok(reduce(suite, &totals, &resolver, tests, coverage, mutation, Timing { command_seconds, slowdown }))
}

/// One run of the mutation tool, its paths resolved to the suite's files.
/// `may_find_none`: a run limited to files may have no mutant to test, and
/// a tool may then write no report; a clean exit then reads as none.
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
    let reports = match read_reports(root, out, &suite.mutants_report, &log) {
        Err(_) if may_find_none && ok => vec![],
        r => r?,
    };
    let mut run = formats::read_mutants(suite.mutants_report.format, &reports)?;
    for m in &mut run.mutants {
        m.file = resolver.resolve(&m.file).unwrap_or_else(|| m.file.clone());
    }
    Ok(run)
}

/// A mutation run, and what it builds on when it mutated only some files.
pub(crate) struct Mutation {
    pub run: MutationRun,
    partial: Option<Partial>,
}

impl From<MutationRun> for Mutation {
    fn from(run: MutationRun) -> Self {
        Self { run, partial: None }
    }
}

/// Runs `command` with `sh -c` in the suite root. A failing exit status is
/// not an error: failing tests still write reports. The log is kept; the
/// flag says whether the command exited 0.
fn shell(root: &Path, out: &Path, command: &str, label: &str) -> Result<(PathBuf, bool)> {
    if command.trim().is_empty() {
        bail!("no {label} command configured");
    }
    let log = out.join(format!("{label}.log"));
    let file = std::fs::File::create(&log)?;
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .env("SLIMFIT_OUT", out)
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()
        .with_context(|| format!("running {label} command: {command}"))?;
    eprintln!("slimfit: {label} command exited {status}; log {}", log.display());
    Ok((log, status.success()))
}

fn report_pattern<F>(root: &Path, out: &Path, report: &Report<F>) -> String {
    let pattern = report.path.replace("$SLIMFIT_OUT", &out.to_string_lossy());
    if Path::new(&pattern).is_absolute() { pattern } else { root.join(&pattern).to_string_lossy().into_owned() }
}

fn report_paths<F>(root: &Path, out: &Path, report: &Report<F>) -> Result<Vec<PathBuf>> {
    expand(&report_pattern(root, out, report))
}

fn read_report<F>(root: &Path, out: &Path, report: &Report<F>, log: &Path) -> Result<Vec<String>> {
    Ok(read_reports(root, out, report, log)?.into_iter().map(|(_, text)| text).collect())
}

/// The report's files, each with its path.
fn read_reports<F>(root: &Path, out: &Path, report: &Report<F>, log: &Path) -> Result<Vec<(PathBuf, String)>> {
    let pattern = report_pattern(root, out, report);
    let paths = expand(&pattern)?;
    if paths.is_empty() {
        let tail = std::fs::read_to_string(log).unwrap_or_default();
        let tail: Vec<&str> = tail.lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("no report at {pattern}; the command's log ends:\n{}", tail.join("\n"));
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

    let (mutants, mutation_tests, partial) = match mutation {
        Some(m) => (m.run.mutants, m.run.tests, m.partial),
        None => (Vec::new(), None, None),
    };
    // Every file mutated: the counts are this run's. Some: the baseline's
    // for the files not mutated, this run's for those that were.
    let (mutant_counts, mutant_files, mutated, mutants_pending) = match partial {
        None => {
            ((!mutants.is_empty()).then(|| MutantCounts::of(&mutants)), MutantCounts::by_file(&mutants), None, vec![])
        }
        Some(Partial { targets, mut kept, pending }) => {
            kept.extend(MutantCounts::by_file(&mutants));
            (Some(MutantCounts::sum(kept.values())), kept, Some(targets), pending)
        }
    };
    let metrics = Metrics {
        prod_lines: totals.prod,
        test_lines: totals.test,
        tests: tests.len() as u64,
        failures: tests.iter().filter(|t| !t.passed).count() as u64,
        test_seconds: (tests.iter().map(|t| t.seconds).sum::<f64>() * 1000.0).round() / 1000.0,
        covered_lines: covered,
        uncovered_lines: uncovered_total,
        mutants: mutant_counts,
        mutant_files,
        mutants_pending,
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
mod t {
    use super::*;
    use crate::config::CheckConfig;
    use crate::ratchet::Bound;

    fn git(dir: &Path, args: &[&str]) {
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
    fn sample_repo() -> (tempfile::TempDir, Config) {
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

    #[test]
    fn mutants_of_changed_files_only_merge_with_the_baselines_counts() {
        let (dir, cfg) = sample_repo();
        let repo = dir.path();
        // No commit recorded: every file is mutated, with no file arguments.
        let (full, args) = run(repo, &cfg, &Baseline::default(), Mutants::Changed);
        assert_eq!(args.as_deref(), Some("\n"));
        let s = &full.suites[0];
        assert_eq!(s.mutated, None);
        assert_eq!(s.metrics.tests, 4);
        let lib = s.metrics.mutant_files["src/lib.rs"];
        assert_eq!(s.metrics.mutants, Some(lib));
        assert!(lib.survived > 0 && lib.killed > 0);

        // A file the baseline counted, unchanged since: kept as counted.
        let kept = MutantCounts { killed: 2, survived: 1, unviable: 0 };
        let mut base = crate::ratchet::ratchet(&Baseline::default(), &full, &Default::default(), &[], None).unwrap().0;
        let b = base.suites.get_mut("rust").unwrap();
        b.mutant_files.insert("src/kept.rs".into(), kept);
        b.mutants = Some(MutantCounts::sum(b.mutant_files.values()));
        let total = b.mutants;

        // Nothing changed: nothing is mutated, the baseline's counts stand.
        let (m, args) = run(repo, &cfg, &base, Mutants::Changed);
        assert_eq!((args, m.suites[0].mutated.clone(), m.suites[0].metrics.mutants), (None, Some(vec![]), total));

        // lib.rs changed: only it is mutated; kept.rs keeps its count.
        std::fs::write(repo.join("src/lib.rs"), std::fs::read_to_string(repo.join("src/lib.rs")).unwrap() + "// x\n")
            .unwrap();
        let (m, args) = run(repo, &cfg, &base, Mutants::Changed);
        assert_eq!(args.as_deref(), Some("--file src/lib.rs\n"));
        let s = &m.suites[0];
        assert_eq!(s.mutated, Some(vec!["src/lib.rs".to_string()]));
        assert_eq!(s.metrics.mutant_files, [("src/kept.rs".to_string(), kept), ("src/lib.rs".to_string(), lib)].into());
        assert_eq!(s.metrics.mutants, total);
        // Its killers are read from the run's own logs.
        let killed: Vec<_> = s.mutants.iter().filter(|m| m.status == MutantStatus::Killed).collect();
        assert!(!killed.is_empty() && killed.iter().all(|m| m.killed_by.as_ref().is_some_and(|k| !k.is_empty())));
        assert!(s.mutation_tests.as_ref().is_some_and(|t| t.len() == 4));
        // The merged counts describe this tree: ratcheting records it.
        let next = crate::ratchet::ratchet(&base, &m, &Default::default(), &[], None).unwrap().0;
        assert_eq!(next.mutants_at.unwrap().dirty, vec!["src/lib.rs".to_string()]);

        // --all-mutants, or a baseline without per-file counts: every file.
        assert_eq!(run(repo, &cfg, &base, Mutants::All).0.suites[0].mutated, None);
        // A commit this repository lacks (squashed away): every file.
        let mut gone = base.clone();
        gone.mutants_at.as_mut().unwrap().commit = "0".repeat(40);
        assert_eq!(run(repo, &cfg, &gone, Mutants::Changed).0.suites[0].mutated, None);
        base.suites.get_mut("rust").unwrap().mutant_files.clear();
        assert_eq!(run(repo, &cfg, &base, Mutants::Changed).0.suites[0].mutated, None);
    }

    /// Five files, each a copy of the sample's lib.rs except `e.rs`, which
    /// has no mutants. The mutation "tool" writes, per file it is limited
    /// to (every file when not limited), the real report renamed to it.
    fn five_file_repo() -> (tempfile::TempDir, Config) {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rust");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::copy(sample.join("Cargo.toml"), dir.path().join("Cargo.toml")).unwrap();
        for f in ["a", "b", "c", "d"] {
            std::fs::copy(sample.join("src/lib.rs"), dir.path().join(format!("src/{f}.rs"))).unwrap();
        }
        std::fs::write(dir.path().join("src/e.rs"), "pub const E: u8 = 1;\n").unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "sample"]);
        let r = sample.join("reports");
        let r = r.to_string_lossy();
        let cfg: Config = toml::from_str(&format!(
            r#"
            [[suite]]
            name = "rust"
            preset = "rust"
            test_command = "cp {r}/junit.xml {r}/lcov.info \"$SLIMFIT_OUT\""
            tests_report = {{ format = "junit", path = "$SLIMFIT_OUT/junit.xml" }}
            mutants_command = "f() {{ fs=; while [ $# -gt 0 ]; do [ \"$1\" = --file ] && {{ fs=\"$fs $2\"; shift; }}; shift; done; [ -n \"$fs\" ] || fs='src/a.rs src/b.rs src/c.rs src/d.rs src/e.rs'; mkdir -p \"$SLIMFIT_OUT/m\"; echo \"$fs\" >> \"$SLIMFIT_OUT/runs\"; for x in $fs; do [ $x = src/e.rs ] || sed \"s#src/lib.rs#$x#g\" {r}/outcomes.json > \"$SLIMFIT_OUT/m/${{x#src/}}.json\"; done; }}; f"
            mutants_report = {{ format = "cargo-mutants", path = "$SLIMFIT_OUT/m/*.json" }}
            "#
        ))
        .unwrap();
        (dir, cfg)
    }

    #[test]
    fn a_baseline_built_in_pieces_counts_as_one_full_run() {
        let (dir, cfg) = five_file_repo();
        let repo = dir.path();
        let fill = |n| Mutants::Fill(Budget { files: Some(n), seconds: None });
        let piece = |base: &Baseline, mutants| {
            // Not fresh: a piece never takes a cached measurement for its own.
            let m = current(repo, &cfg, base, &Options { mutants, fresh: false }).unwrap();
            let runs = std::fs::read_to_string(repo.join(OUT).join("rust/runs")).unwrap_or_default();
            let next = crate::ratchet::ratchet(base, &m, &Default::default(), &[], None).unwrap().0;
            (m, next, runs.lines().map(|l| l.trim().to_string()).collect::<Vec<_>>())
        };

        // Piece 1, on a baseline with no mutants: every file is pending; two are measured.
        let (_, b1, runs) = piece(&Baseline::default(), fill(2));
        assert_eq!(runs, ["src/a.rs", "src/b.rs"]);
        let s = &b1.suites["rust"];
        assert_eq!(s.mutants_pending, ["src/c.rs", "src/d.rs", "src/e.rs"]);
        assert_eq!(s.mutant_files.keys().collect::<Vec<_>>(), ["src/a.rs", "src/b.rs"]);

        // check --mutants holds only the counted files and names the rest.
        let (m, _) = run(repo, &cfg, &b1, Mutants::Changed);
        let v = crate::ratchet::check(&b1, &m, &Default::default());
        // The same report again: only test_seconds may move, with the host's load.
        assert!(v.ok && v.improved.iter().all(|c| c.metric == crate::ratchet::Bound::TestSeconds), "{v:?}");
        assert_eq!(v.mutants_pending["rust"], ["src/c.rs", "src/d.rs", "src/e.rs"]);
        let (m, _) = run(repo, &cfg, &b1, Mutants::None);
        assert!(crate::ratchet::check(&b1, &m, &Default::default()).mutants_pending.is_empty());

        // A counted file changes: piece 2 re-mutates it, so moving mutants_at
        // stays sound. A changed pending file stays pending.
        std::fs::write(repo.join("src/a.rs"), std::fs::read_to_string(repo.join("src/a.rs")).unwrap() + "// x\n")
            .unwrap();
        std::fs::write(repo.join("src/e.rs"), "pub const E: u8 = 2;\n").unwrap();
        let (_, b2, runs) = piece(&b1, fill(2));
        assert_eq!(runs, ["src/a.rs", "src/c.rs", "src/d.rs"]);
        assert_eq!(b2.suites["rust"].mutants_pending, ["src/e.rs"]);
        assert_eq!(b2.mutants_at.as_ref().unwrap().dirty, ["src/a.rs", "src/e.rs"]);

        // Piece 3, on a budget larger than what is left: the last file.
        let (_, b3, runs) = piece(&b2, Mutants::Fill(Budget { files: None, seconds: Some(3600) }));
        assert_eq!(runs, ["src/a.rs", "src/e.rs"]);

        let (full, ..) = piece(&Baseline::default(), Mutants::All);
        let (s, f) = (&b3.suites["rust"], &full.suites[0].metrics);
        assert!(s.mutants_pending.is_empty());
        assert_eq!((&s.mutant_files, s.mutants), (&f.mutant_files, f.mutants));
        assert_eq!(f.mutant_files.len(), 4);
    }

    #[test]
    fn a_piece_records_its_mutants_when_test_seconds_is_over_but_not_when_a_test_fails() {
        let (dir, cfg) = five_file_repo();
        let repo = dir.path();
        let strict = CheckConfig { time_tolerance: 0.0, time_slack: 0.0, coverage_slack: 0 };
        // A suite with no mutants yet, its test_seconds taken on an idle host.
        let idle = Metrics { test_seconds: 0.1, uncovered_lines: 1000, ..Default::default() };
        let b0 = Baseline { suites: [("rust".to_string(), idle)].into(), ..Default::default() };

        // A loaded host: the piece's test_seconds is over its bound. The piece
        // still records its counts, and test_seconds stays at the old value.
        let fill = Mutants::Fill(Budget { files: Some(2), seconds: None });
        let m = current(repo, &cfg, &b0, &Options { mutants: fill, fresh: true }).unwrap();
        assert!(m.piece && m.suites[0].metrics.test_seconds > 0.1);
        let (b1, over) = crate::ratchet::ratchet(&b0, &m, &strict, &[], None).unwrap();
        assert_eq!(over.iter().map(|v| v.metric).collect::<Vec<_>>(), [Bound::TestSeconds]);
        let s = &b1.suites["rust"];
        assert_eq!(s.test_seconds, 0.1);
        assert_eq!(s.mutants_pending, ["src/c.rs", "src/d.rs", "src/e.rs"]);
        assert_eq!(s.mutant_files.keys().collect::<Vec<_>>(), ["src/a.rs", "src/b.rs"]);

        // Not a piece: the same measurement is refused.
        assert!(crate::ratchet::ratchet(&b0, &Measurement { piece: false, ..m.clone() }, &strict, &[], None).is_err());
        // A failing test refuses a piece all the same.
        let mut failing = m;
        failing.suites[0].metrics.failures = 1;
        assert!(crate::ratchet::ratchet(&b0, &failing, &strict, &[], None).is_err());
    }

    #[test]
    fn a_budget_starts_no_file_that_would_end_past_it_but_always_one() {
        let at = |filled, longest, files, seconds| {
            Spending { budget: Budget { files, seconds }, started: Instant::now(), filled, longest }.more()
        };
        assert!(at(0, 0.0, None, Some(0)));
        assert!(at(1, 2.0, None, Some(3)));
        assert!(!at(1, 2.0, None, Some(1)));
        assert!(!at(2, 0.0, Some(2), None));
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
