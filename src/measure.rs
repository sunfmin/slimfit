//! Measuring the working tree: run each suite's commands, read their
//! reports, and reduce them to `Measurement`. The result is cached under
//! `.slimfit/out/` and reused while the tree is unchanged.

use crate::config::{Config, Report, Suite};
use crate::formats::{self, MutationRun};
use crate::git;
use crate::loc::{Classifier, Kind, Totals};
use crate::model::*;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub const OUT: &str = ".slimfit/out";

pub struct Options {
    pub mutants: bool,
    pub fresh: bool,
}

/// The measurement of the tree as it is now: from the cache when it was
/// taken of this exact tree (and with mutants if they are wanted).
pub fn current(repo: &Path, cfg: &Config, opts: &Options) -> Result<Measurement> {
    let tree = git::tree_key(repo)?;
    let cache = repo.join(OUT).join("measurement.json");
    if !opts.fresh
        && let Ok(text) = std::fs::read_to_string(&cache)
        && let Ok(m) = serde_json::from_str::<Measurement>(&text)
        && m.tree == tree
        && (m.with_mutants || !opts.mutants)
        && m.suites.iter().map(|s| &s.suite).eq(cfg.suites.iter().map(|s| &s.name))
    {
        return Ok(m);
    }
    let files = git::files(repo)?;
    let mut suites = Vec::new();
    for s in &cfg.suites {
        let suite = s.resolve();
        eprintln!(
            "slimfit: measuring {} ({}){}",
            suite.name,
            suite.root,
            if opts.mutants { " with mutants" } else { "" }
        );
        suites.push(measure_suite(repo, &files, &suite, opts.mutants)?);
    }
    let m = Measurement { tree, with_mutants: opts.mutants, suites };
    std::fs::create_dir_all(repo.join(OUT))?;
    std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
    Ok(m)
}

fn measure_suite(repo: &Path, files: &[String], suite: &Suite, with_mutants: bool) -> Result<SuiteMeasurement> {
    let root = repo.join(&suite.root);
    let out = repo.join(OUT).join(&suite.name);
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out)?;

    let classifier = Classifier::new(suite)?;
    let totals = classifier.count(files, |f| std::fs::read_to_string(repo.join(f)).ok());
    let resolver = Resolver::new(repo, &totals);

    let started = Instant::now();
    let log = shell(&root, &out, &suite.test_command, "test")?;
    let command_seconds = started.elapsed().as_secs_f64();
    let tests = formats::read_tests(suite.tests_report.format, &read_report(&root, &out, &suite.tests_report, &log)?)?;
    let coverage =
        formats::read_coverage(suite.coverage_report.format, &read_report(&root, &out, &suite.coverage_report, &log)?)?;

    let mutation = if with_mutants {
        let log = shell(&root, &out, &suite.mutants_command, "mutants")?;
        let mut run = formats::read_mutants(
            suite.mutants_report.format,
            &read_report(&root, &out, &suite.mutants_report, &log)?,
        )?;
        for m in &mut run.mutants {
            m.file = resolver.resolve(&m.file).unwrap_or_else(|| m.file.clone());
        }
        Some(run)
    } else {
        None
    };
    Ok(reduce(suite, &totals, &resolver, tests, coverage, mutation, command_seconds))
}

/// Runs `command` with `sh -c` in the suite root. A failing exit status is
/// not an error: failing tests still write reports. The log is kept.
fn shell(root: &Path, out: &Path, command: &str, label: &str) -> Result<PathBuf> {
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
    Ok(log)
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
    mutation: Option<MutationRun>,
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

    let (mutants, mutation_tests) = match mutation {
        Some(r) => (r.mutants, r.tests),
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
        mutants: (!mutants.is_empty()).then(|| MutantCounts::of(&mutants)),
    };
    SuiteMeasurement {
        suite: suite.name.clone(),
        metrics,
        tests,
        uncovered,
        mutants,
        mutation_tests,
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
