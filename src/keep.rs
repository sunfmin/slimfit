//! `slimfit keep`: a kept verdict recorded in the baseline, on a test, a
//! source file, an uncovered span or a sampled survivor, each held to the
//! text (or size and churn) it was kept against, in place of an earlier
//! verdict on it; and each verdict's type, which says when it holds.

use crate::model::{ReachTest, Span};
use crate::session::Session;
use crate::{fault, measure, model, tasks};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Records in `s`'s baseline a verdict on each of `test` (tests, files or
/// spans, one of `reasons` each, in order), or on `mutant`; answers which
/// measurement it read: `cached`, `measured` or `none`. The baseline is
/// changed in `s` only; the command writes it.
pub fn record(
    s: &mut Session,
    test: Vec<String>,
    mutant: Option<String>,
    kind: KeepKind,
    reasons: Vec<String>,
) -> Result<&'static str> {
    if reasons.is_empty() || reasons.iter().any(|r| r.trim().is_empty()) {
        return Err(fault::fault(
            "keep without --reason",
            "add `--reason \"<what the test checks that the other does not, why the module is deep enough, what the span runs against, or why no input tells the mutant from the original>\"`: \
             it is kept in the baseline",
        ));
    }
    let named = if mutant.is_some() { 1 } else { test.len() };
    if reasons.len() != named {
        return Err(fault::fault(
            format!("keep names {named} to keep and gives {} `--reason`", reasons.len()),
            "give one `--reason \"<why>\"` per test, file or span, in the same order: \
             `slimfit keep <a> <b> --reason \"<why a>\" --reason \"<why b>\"`",
        ));
    }
    let mut reasons = reasons.into_iter();
    let mut measured = None;
    match mutant {
        Some(mutant) => keep_mutant(s, mutant, reasons.next().unwrap_or_default())?,
        None => {
            for (at, reason) in test.into_iter().zip(reasons) {
                keep(s, &mut measured, at, kind, reason)?;
            }
        }
    }
    Ok(measured.as_ref().map_or("none", |(_, from)| *from))
}

/// Records in `s`'s baseline that mutant `name` is equivalent, for
/// `reason`, against its function's text now, in place of an earlier
/// verdict on it. Reads its file, and measures nothing.
fn keep_mutant(s: &mut Session, name: String, reason: String) -> Result<()> {
    let Some((file, line)) = model::placed(&name) else {
        return Err(fault::fault(
            format!("`{name}` is not a mutant's name: `<file>:<line>:<column>: <change>`"),
            "name it as the surviving-mutant task's survivor `name` does",
        ));
    };
    let Some((suite, _)) = tasks::source_suite(&s.tree, &s.cfg, file)? else {
        return Err(fault::fault(
            format!("no suite counts {file}, the file of `{name}`, as a source file"),
            "name it as the surviving-mutant task's survivor `name` does, on the tree it was sampled from",
        ));
    };
    let st = s.tree.suite(&s.cfg, &suite)?;
    let text = st.resolver.read(file).unwrap_or_default();
    let hash = measure::function_hash(st.suite.preset, &text, line);
    s.base.kept_mutants.retain(|k| !k.names(&suite, &name));
    let mutant = model::unplaced(&name);
    s.base.kept_mutants.push(KeptMutant { suite, mutant, hash, reason });
    Ok(())
}

/// The tree's measurement for `keep`, taken once for every test and span
/// it names: read from the cache (`cached`), or else measured (`measured`).
fn keep_measurement<'a>(
    s: &Session,
    m: &'a mut Option<(model::Measurement, &'static str)>,
) -> Result<&'a model::Measurement> {
    if m.is_none() {
        *m = Some(match measure::cached(&s.repo, &s.tree, &s.cfg) {
            Some(cached) => (cached, "cached"),
            None => (s.measure(Default::default())?, "measured"),
        });
    }
    Ok(&m.as_ref().expect("set above").0)
}

/// Records in `s`'s baseline that `at` (a span, a source file or a test)
/// is kept, for `reason`, in place of an earlier verdict on it (a test's
/// of the same `kind`); a test or a span from `measured`, taken once.
fn keep(
    s: &mut Session,
    measured: &mut Option<(model::Measurement, &'static str)>,
    at: String,
    kind: KeepKind,
    reason: String,
) -> Result<()> {
    let test_only = || {
        let fix = "drop `--kind slow-test`: it keeps a test, by a slow-test task's `test`";
        Err(fault::fault(format!("--kind slow-test keeps a test, and {at} is a span or a source file"), fix))
    };
    let slow = kind == KeepKind::SlowTest;
    if let Some((file, start, end)) = span_at(&at) {
        if slow {
            return test_only();
        }
        let m = keep_measurement(s, measured)?;
        let found = m.suites.iter().find_map(|m| {
            let sp = m.uncovered.iter().find(|sp| (sp.file.as_str(), sp.start, sp.end) == (file, start, end))?;
            Some((m.suite.clone(), sp.clone(), sp.hash.clone()?))
        });
        let Some((suite, sp, hash)) = found else {
            return Err(fault::fault(
                format!("no suite's coverage has an uncovered span {at}"),
                "name it as the uncovered task's summary does, `<file>:<start>-<end>`: \
                 `slimfit next --all --kind uncovered` lists them (after `slimfit check --fresh`, when the \
                 measurement is from before spans were hashed)",
            ));
        };
        s.base.kept_spans.retain(|k| !(k.suite == suite && (k.file.as_str(), k.start, k.end) == (file, start, end)));
        let function = sp.function;
        s.base.kept_spans.push(KeptSpan { suite, file: sp.file, start, end, function, hash, reason });
        return Ok(());
    }
    if let Some((suite, prod_lines)) = tasks::source_suite(&s.tree, &s.cfg, &at)? {
        if slow {
            return test_only();
        }
        let (commits_90d, commits_total) = tasks::churn(&s.repo, &at);
        s.base.kept_files.retain(|k| !(k.suite == suite && k.file == at));
        s.base.kept_files.push(KeptFile { suite, file: at, prod_lines, commits_90d, commits_total, reason });
        return Ok(());
    }
    let m = keep_measurement(s, measured)?;
    let found = m.suites.iter().find_map(|s| {
        let t = s.reach.as_ref()?.tests.iter().find(|t| t.id() == at)?;
        let cpu = s.tests.iter().find(|c| c.id == at).map_or(0.0, |c| c.cpu);
        Some((s.suite.clone(), t.source.clone(), cpu, s.metrics.test_seconds))
    });
    let Some((suite, source, cpu, total)) = found else {
        return Err(fault::fault(
            format!("no suite's test run recorded a test {at}, and no suite counts it as a source file"),
            "name it as the redundant-test or slow-test task's `test` or the deepen task's `file` does: \
             `slimfit next --all --kind redundant-test` (or `--kind deepen`) lists them",
        ));
    };
    let Some(source) = source else {
        return Err(fault::fault(
            format!("{at}'s own function was not found, so there is no text to hold the verdict to"),
            "delete or merge it instead, as the task's steps say",
        ));
    };
    s.base.kept.retain(|k| !(k.suite == suite && k.test == at && k.kind == kind));
    let (seconds, suite_seconds) = (slow.then_some(cpu), slow.then_some(total));
    s.base.kept.push(Kept { suite, test: at, kind, hash: source.hash, seconds, suite_seconds, reason });
    Ok(())
}

/// The span `keep` names as `<file>:<start>-<end>`, as an uncovered task's
/// summary does; `None` for a test or a file.
fn span_at(at: &str) -> Option<(&str, u32, u32)> {
    let (file, lines) = at.rsplit_once(':')?;
    let (start, end) = lines.split_once('-')?;
    Some((file, start.parse().ok()?, end.parse().ok()?))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Kept {
    pub suite: String,
    pub test: String,
    /// The kind of task it answers; a baseline from before kinds holds
    /// only redundant-test verdicts.
    #[serde(default, skip_serializing_if = "KeepKind::is_redundant")]
    pub kind: KeepKind,
    /// The hash of the test's text when it was kept (`TestSource::hash`).
    pub hash: String,
    /// Its CPU seconds when kept: a slow-test verdict's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    /// Its suite's CPU seconds (`test_seconds`) when kept: a slow-test
    /// verdict's, absent before verdicts kept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_seconds: Option<f64>,
    pub reason: String,
}

/// The kinds of task a test's keep answers.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum KeepKind {
    #[default]
    RedundantTest,
    SlowTest,
}

impl KeepKind {
    fn is_redundant(&self) -> bool {
        *self == KeepKind::RedundantTest
    }
}

/// How much a slow test kept may grow, as a share of its suite's CPU
/// seconds, before it is proposed again: half as much again. Load moves
/// every test's seconds together, so the share holds where seconds swing.
pub const KEPT_SLOWDOWN: f64 = 0.5;

impl Kept {
    /// Whether this verdict's test is `test` of `suite`, its text unchanged.
    pub fn holds(&self, suite: &str, test: &ReachTest) -> bool {
        self.suite == suite && self.test == test.id() && test.source.as_ref().is_some_and(|s| s.hash == self.hash)
    }

    /// Whether it keeps `test` of `suite`, now taking `cpu` of the suite's
    /// `total` CPU seconds, from `kind`'s tasks: by its share of the suite,
    /// or by its seconds alone when kept without the suite's.
    pub fn keeps(&self, kind: KeepKind, suite: &str, test: &ReachTest, cpu: f64, total: f64) -> bool {
        let within = |now: f64, kept: f64| now <= kept * (1.0 + KEPT_SLOWDOWN);
        let time = match (self.seconds, self.suite_seconds) {
            (Some(kept), Some(of)) if of > 0.0 && total > 0.0 => within(cpu / total, kept / of),
            (Some(kept), _) => within(cpu, kept),
            (None, _) => true,
        };
        self.kind == kind && self.holds(suite, test) && time
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeptFile {
    pub suite: String,
    pub file: String,
    /// Its prod lines when it was kept.
    pub prod_lines: u64,
    /// The commits that changed it in the `DEEPEN_DAYS` before it was kept.
    pub commits_90d: u64,
    /// The commits that ever changed it, when it was kept.
    pub commits_total: u64,
    pub reason: String,
}

/// How much a kept file's prod lines may grow before it is proposed again:
/// half as many again.
pub const KEPT_GROWTH: f64 = 0.5;
/// How many commits may change a kept file before it is proposed again, as
/// a share of the commits that made it a candidate: as many again.
pub const KEPT_CHURN: f64 = 1.0;

impl KeptFile {
    /// Whether this verdict still holds for `file` of `suite`, now
    /// `prod_lines` long and changed by `commits_total` commits in all.
    pub fn holds(&self, suite: &str, file: &str, prod_lines: u64, commits_total: u64) -> bool {
        let since = commits_total.saturating_sub(self.commits_total) as f64;
        self.suite == suite
            && self.file == file
            && (prod_lines as f64) < self.prod_lines as f64 * (1.0 + KEPT_GROWTH)
            && since < self.commits_90d as f64 * KEPT_CHURN
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeptSpan {
    pub suite: String,
    pub file: String,
    pub start: u32,
    pub end: u32,
    /// The function it starts in, when the coverage report names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// The hash of its lines' text when it was kept (`Span::hash`).
    pub hash: String,
    pub reason: String,
}

impl KeptSpan {
    /// Whether this verdict still holds for uncovered span `sp` of `suite`:
    /// the same lines of the same function, their text unchanged.
    pub fn holds(&self, suite: &str, sp: &Span) -> bool {
        self.suite == suite
            && (self.file.as_str(), self.start, self.end, &self.function)
                == (sp.file.as_str(), sp.start, sp.end, &sp.function)
            && sp.hash.as_ref() == Some(&self.hash)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeptMutant {
    pub suite: String,
    /// Its name without its line and column (`model::unplaced`): an edit
    /// above it moves it, and the verdict stands.
    pub mutant: String,
    /// The hash of its function's text when it was kept
    /// (`measure::function_hash`).
    pub hash: String,
    pub reason: String,
}

impl KeptMutant {
    /// Whether it names mutant `name` of `suite`, wherever it is now.
    pub fn names(&self, suite: &str, name: &str) -> bool {
        self.suite == suite && self.mutant == crate::model::unplaced(name)
    }

    /// Whether it still keeps mutant `name` of `suite`, whose function's
    /// text now hashes to `hash`.
    pub fn holds(&self, suite: &str, name: &str, hash: &str) -> bool {
        self.names(suite, name) && self.hash == hash
    }
}

/// What a [`KeptMutant`] prints, as the command pages name it.
pub const KEPT_MUTANT_HELP: &str = "each with `suite`, `mutant` (its name without its line and column), `hash` (of its function's text when kept) and `reason`";

/// What a [`KeptSpan`] prints, as the command pages name it.
pub const KEPT_SPAN_HELP: &str = "each with `suite`, `file`, `start`, `end`, `function` (when known), `hash` (of its lines' text when kept) and `reason`";

/// What a [`KeptFile`] prints, as the command pages name it.
pub const KEPT_FILE_HELP: &str = "each with `suite`, `file`, its `prod_lines`, `commits_90d` and `commits_total` (every commit that changed it) when kept, and `reason`";

/// What a [`Kept`] prints, as the command pages name it.
pub const KEPT_HELP: &str = "each with `suite`, `test`, `kind` (`slow-test`; absent for `redundant-test`), `hash` (of the test's text when kept), `seconds` and `suite_seconds` (its and its suite's CPU seconds when kept, for `slow-test`) and `reason`";

#[cfg(test)]
mod t {
    use super::*;
    use crate::model::Span;

    /// A kept file holds up to its thresholds and not at them: 100 prod
    /// lines hold at 149, not 150; 4 commits since its 4 in 90 days
    /// hold at 3, not 4.
    #[test]
    fn a_kept_file_holds_below_its_thresholds_only() {
        let k = KeptFile {
            suite: "s".into(),
            file: "f.rs".into(),
            prod_lines: 100,
            commits_90d: 4,
            commits_total: 10,
            reason: "r".into(),
        };
        let holds = |lines, total| k.holds("s", "f.rs", lines, total);
        assert_eq!((holds(149, 13), holds(150, 13), holds(149, 14)), (true, false, false));
    }

    /// A kept span holds for the same lines of the same function, their text
    /// unchanged, in its own suite: grown past its lines, it does not.
    #[test]
    fn a_kept_span_holds_for_its_lines_as_kept_only() {
        let k = KeptSpan {
            suite: "s".into(),
            file: "a.rs".into(),
            start: 3,
            end: 6,
            function: Some("f".into()),
            hash: "h".into(),
            reason: "r".into(),
        };
        let sp = Span {
            file: "a.rs".into(),
            start: 3,
            end: 6,
            lines: 4,
            function: Some("f".into()),
            hash: Some("h".into()),
        };
        assert!(k.holds("s", &sp) && !k.holds("t", &sp));
        let grown = Span { end: 7, lines: 5, ..sp.clone() };
        let renamed = Span { function: Some("g".into()), ..sp.clone() };
        let unhashed = Span { hash: None, ..sp.clone() };
        assert_eq!([&grown, &renamed, &unhashed].map(|s| k.holds("s", s)), [false; 3]);
    }
}
