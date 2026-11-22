//! `slimfit keep`: a kept verdict recorded in the baseline, on a test, a
//! source file, an uncovered span or a sampled survivor, each held to the
//! text (or size and churn) it was kept against, in place of an earlier
//! verdict on it.

use crate::ratchet::{self, KeepKind};
use crate::session::Session;
use crate::{fault, measure, model, tasks};
use anyhow::Result;

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
    s.base.kept_mutants.push(ratchet::KeptMutant { suite, mutant, hash, reason });
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
        s.base.kept_spans.push(ratchet::KeptSpan { suite, file: sp.file, start, end, function, hash, reason });
        return Ok(());
    }
    if let Some((suite, prod_lines)) = tasks::source_suite(&s.tree, &s.cfg, &at)? {
        if slow {
            return test_only();
        }
        let (commits_90d, commits_total) = tasks::churn(&s.repo, &at);
        s.base.kept_files.retain(|k| !(k.suite == suite && k.file == at));
        s.base.kept_files.push(ratchet::KeptFile { suite, file: at, prod_lines, commits_90d, commits_total, reason });
        return Ok(());
    }
    let m = keep_measurement(s, measured)?;
    let found = m.suites.iter().find_map(|s| {
        let t = s.reach.as_ref()?.tests.iter().find(|t| t.id() == at)?;
        let cpu = s.tests.iter().find(|c| c.id == at).map_or(0.0, |c| c.cpu);
        Some((s.suite.clone(), t.source.clone(), cpu))
    });
    let Some((suite, source, cpu)) = found else {
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
    let seconds = slow.then_some(cpu);
    s.base.kept.push(ratchet::Kept { suite, test: at, kind, hash: source.hash, seconds, reason });
    Ok(())
}

/// The span `keep` names as `<file>:<start>-<end>`, as an uncovered task's
/// summary does; `None` for a test or a file.
fn span_at(at: &str) -> Option<(&str, u32, u32)> {
    let (file, lines) = at.rsplit_once(':')?;
    let (start, end) = lines.split_once('-')?;
    Some((file, start.parse().ok()?, end.parse().ok()?))
}
