//! Measuring the working tree: run each suite's commands, read their
//! reports, and reduce them to `Measurement`. The result is cached under
//! `.slimfit/out/` and reused while the tree is unchanged.

use crate::config::{Config, Report, Suite};
use crate::formats::{self, MutationRun};
use crate::git;
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
    All,
}

pub struct Options {
    pub mutants: Mutants,
    pub fresh: bool,
}

/// The measurement of the tree as it is now: from the cache when it was
/// taken of this exact tree (and with as many mutants as are wanted).
/// `base` is the baseline a `Mutants::Changed` run builds on.
pub fn current(repo: &Path, cfg: &Config, base: &Baseline, opts: &Options) -> Result<Measurement> {
    let tree = git::tree_key(repo)?;
    let cache = repo.join(OUT).join("measurement.json");
    if !opts.fresh
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
        (Some(then), Mutants::Changed) => match git::changed_since(repo, then) {
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
                Mutants::All => " with every mutant",
            }
        );
        let prior = match opts.mutants {
            Mutants::None => None,
            Mutants::Changed => Some(changed.as_ref().map(|c| (c, base.suites.get(&suite.name)))),
            Mutants::All => Some(None),
        };
        suites.push(measure_suite(repo, &files, &suite, prior)?);
    }
    let m = Measurement { tree, with_mutants: opts.mutants != Mutants::None, snapshot, suites };
    std::fs::create_dir_all(repo.join(OUT))?;
    std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
    Ok(m)
}

/// What a run that mutates only the changed files builds on: the files
/// changed since the baseline's mutants were measured, and the suite's
/// baseline. `None` inside: mutate every file.
type Prior<'a> = Option<(&'a BTreeSet<String>, Option<&'a Metrics>)>;

/// The mutants to run, and the per-file counts kept from the baseline for
/// the files not mutated.
struct Partial {
    targets: Vec<String>,
    kept: BTreeMap<String, MutantCounts>,
}

/// Why a suite's mutants cannot be limited to the changed files, if so.
fn partial(suite: &Suite, totals: &Totals, prior: Prior) -> std::result::Result<Partial, String> {
    let Some((changed, base)) = prior else { return Err("asked for every mutant".into()) };
    if suite.mutants_file_flag.is_empty() {
        return Err("its mutation tool cannot be limited to files".into());
    }
    let Some(b) = base else { return Err("no baseline".into()) };
    let Some(total) = b.mutants else { return Err("no mutants in the baseline".into()) };
    if MutantCounts::sum(b.mutant_files.values()) != total {
        return Err("the baseline has no per-file mutant counts (ratchet once with --all-mutants)".into());
    }
    let targets = changed.iter().filter(|f| matches!(totals.files.get(*f), Some((Kind::Source, _)))).cloned().collect();
    let kept = b.mutant_files.iter().filter(|(f, _)| !changed.contains(*f)).map(|(f, c)| (f.clone(), *c)).collect();
    Ok(Partial { targets, kept })
}

fn measure_suite(repo: &Path, files: &[String], suite: &Suite, prior: Option<Prior>) -> Result<SuiteMeasurement> {
    let root = repo.join(&suite.root);
    let out = repo.join(OUT).join(&suite.name);
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out)?;

    let classifier = Classifier::new(suite)?;
    let totals = classifier.count(files, |f| std::fs::read_to_string(repo.join(f)).ok());
    let resolver = Resolver::new(repo, &totals);

    let started = Instant::now();
    let (log, _) = shell(&root, &out, &suite.test_command, "test")?;
    let command_seconds = started.elapsed().as_secs_f64();
    let tests = formats::read_tests(suite.tests_report.format, &read_report(&root, &out, &suite.tests_report, &log)?)?;
    let coverage =
        formats::read_coverage(suite.coverage_report.format, &read_report(&root, &out, &suite.coverage_report, &log)?)?;

    let mutation = match prior {
        None => None,
        Some(prior) => Some(match partial(suite, &totals, prior) {
            Err(why) => {
                if prior.is_some() {
                    eprintln!("slimfit: {}: mutating every file: {why}", suite.name);
                }
                Mutation { run: mutate(&root, &out, suite, &resolver, &suite.mutants_command, false)?, partial: None }
            }
            Ok(p) => {
                eprintln!("slimfit: {}: mutating {} changed file(s)", suite.name, p.targets.len());
                let run = if p.targets.is_empty() {
                    MutationRun::default()
                } else {
                    let prefix = if suite.root == "." {
                        String::new()
                    } else {
                        format!("{}/", suite.root.trim_end_matches('/'))
                    };
                    let mut command = suite.mutants_command.clone();
                    for f in &p.targets {
                        let rel = f.strip_prefix(&prefix).unwrap_or(f);
                        command.push_str(&format!(" {} '{}'", suite.mutants_file_flag, rel.replace('\'', "'\\''")));
                    }
                    mutate(&root, &out, suite, &resolver, &command, true)?
                };
                Mutation { run, partial: Some(p) }
            }
        }),
    };
    Ok(reduce(suite, &totals, &resolver, tests, coverage, mutation, command_seconds))
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
    let (log, ok) = shell(root, out, command, "mutants")?;
    let texts = match read_report(root, out, &suite.mutants_report, &log) {
        Err(_) if may_find_none && ok => vec![],
        r => r?,
    };
    let mut run = formats::read_mutants(suite.mutants_report.format, &texts)?;
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

fn read_report<F>(root: &Path, out: &Path, report: &Report<F>, log: &Path) -> Result<Vec<String>> {
    let pattern = report.path.replace("$SLIMFIT_OUT", &out.to_string_lossy());
    let pattern =
        if Path::new(&pattern).is_absolute() { pattern } else { root.join(&pattern).to_string_lossy().into_owned() };
    let paths = expand(&pattern)?;
    if paths.is_empty() {
        let tail = std::fs::read_to_string(log).unwrap_or_default();
        let tail: Vec<&str> = tail.lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("no report at {pattern}; the command's log ends:\n{}", tail.join("\n"));
    }
    paths.iter().map(|p| std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))).collect()
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

pub(crate) fn reduce(
    suite: &Suite,
    totals: &Totals,
    resolver: &Resolver,
    tests: Vec<TestCase>,
    coverage: Coverage,
    mutation: Option<Mutation>,
    command_seconds: f64,
) -> SuiteMeasurement {
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
    let (mutant_counts, mutant_files, mutated) = match partial {
        None => ((!mutants.is_empty()).then(|| MutantCounts::of(&mutants)), MutantCounts::by_file(&mutants), None),
        Some(Partial { targets, mut kept }) => {
            kept.extend(MutantCounts::by_file(&mutants));
            (Some(MutantCounts::sum(kept.values())), kept, Some(targets))
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
    };
    SuiteMeasurement {
        suite: suite.name.clone(),
        metrics,
        tests,
        uncovered,
        mutants,
        mutation_tests,
        mutated,
        command_seconds: (command_seconds * 1000.0).round() / 1000.0,
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
            mutants_command = "f() {{ echo \"$@\" > \"$SLIMFIT_OUT/args\"; mkdir -p \"$SLIMFIT_OUT/mutants.out\"; cp {r}/outcomes.json \"$SLIMFIT_OUT/mutants.out\"; }}; f"
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
        let lib = s.metrics.mutant_files["src/lib.rs"];
        assert_eq!(s.metrics.mutants, Some(lib));
        assert!(lib.survived > 0 && lib.killed > 0);

        // A file the baseline counted, unchanged since: kept as counted.
        let kept = MutantCounts { killed: 2, survived: 1, unviable: 0 };
        let mut base = crate::ratchet::ratchet(&Baseline::default(), &full, &Default::default(), &[], None).unwrap();
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
        // The merged counts describe this tree: ratcheting records it.
        let next = crate::ratchet::ratchet(&base, &m, &Default::default(), &[], None).unwrap();
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
