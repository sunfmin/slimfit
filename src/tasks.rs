//! Turning a measurement into work: one task at a time, cheapest and
//! safest kinds first, each with its evidence and the command that accepts it.

use crate::config::Config;
use crate::git;
use crate::landing::{SuiteTree, Tree};
use crate::loc::Kind;
use crate::model::{Measurement, MutantStatus, ReachTest, Source, SuiteMeasurement, TestSource};
use crate::ratchet::{Baseline, Kept, KeptFile, KeptSpan, Measured};
use crate::test_shape::Shape;
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, agent_help::Help)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    /// A test fails. Fix it first; nothing else is measured honestly.
    #[value(verbatim_doc_comment)]
    FailingTest,
    /// The change adds source lines or removes tests and no record of its mutants: the gate's `check` fails until it does (see [what the gate holds](help:mutants-scope)). Ask a second agent to pick at most {const:PICKS_MAX} of the listed mutants, never yourself (see [the picker](help:picker)), run its picks, and ratchet with who picked; or record its why when none is worth one.
    #[value(verbatim_doc_comment)]
    PickMutants,
    /// The change adds more than `growth_allowance` source lines net and no growth record: the gate's `check` fails until it does. Cut what you can, then ask a second agent, never yourself, whether it could be smaller (see [the reviewer](help:reviewer)), do or answer what it names, and ratchet with its verdict.
    #[value(verbatim_doc_comment)]
    JustifyGrowth,
    /// Lines no test runs. No caller: delete them. A user path needs them: test it through its module's interface, with a fast test that asserts what the lines decide; a test that only runs them is no fix. A feature nobody uses: don't delete it, escalate: that is a product decision. Lines that run only against the real host or the real program, where a fast test could only run them: keep them (`slimfit keep <file>:<start>-<end>`). `uncovered_lines` is a trend, not a bound: these tasks, not `check`, are how it falls.
    #[value(verbatim_doc_comment)]
    Uncovered,
    /// Coverage proposes a test whose every source line another test runs too, some past both binaries' shared setup (a test of setup alone, such as one reading a shipped file, gives coverage no evidence), or whose lines mostly overlap another's. Overlap leaves out the lines more than {const:COMMON_LINES} of a test binary's tests run (its shared setup; a package or test file where tests have no binary), so two tests overlap by what they run past it. A task is one test file with every candidate whose test is in it, files with the most CPU seconds to save first: read the file once and decide each candidate as its `class` says: delete it (`asserts-contained`), merge its group into one table-driven test (`same-shape`), or by both tests' assertions delete it, merge the two, or keep it (`slimfit keep`) (`read`). Only where the test run records coverage per test ({const:PER_TEST}); runs no mutant, but a delete or a merge then owes picks over the lines the removed test ran (a `pick-mutants` task).
    #[value(verbatim_doc_comment)]
    RedundantTest,
    /// One test holds a large share of the suite's `test_seconds`, by its CPU time. Make it cheaper without testing less, or merge it into faster ones.
    #[value(verbatim_doc_comment)]
    SlowTest,
    /// A trend rose over the suite's last landings (first-parent commits that wrote the baseline, at most {const:TREND_LANDINGS} back, at least {const:TREND_MIN} of them): `uncovered_lines` as measured, from the first to the last, by more than `coverage_slack`, or `test_seconds` by more than {const:TIME_NOISE} (load moves CPU time nearly that far on an unchanged tree). The baseline keeps the lowest, so no `check` sees a run of rises: this task is how it shows, and no bound holds it. `evidence.landings` names them; `evidence.spans` the uncovered spans holding a line added since the first, or `evidence.tests` the tests in files changed since, slowest first. Lower it as an `uncovered` or `slow-test` task says, or judge the rise worth what it bought and leave it: the task goes once the landings no longer rise.
    #[value(verbatim_doc_comment)]
    RisingTrend,
    /// A mutant no test notices. Assert the behaviour through the interface with a fast test, or simplify the code away. Its `evidence.source` says who chose it: `picker`, a second agent's pick of the change, from a `slimfit check --mutants <name>...` of this tree; `sampled`, drawn by `slimfit next --sample` from changes already landed (see [the sample](help:sample)).
    #[value(verbatim_doc_comment)]
    SurvivingMutant,
    /// A sample's record to commit: `slimfit next --sample` ran it where nothing is committed (idle work), and `slimfit report` reads it only from git. Write `evidence.text` to the file in `files` and commit it, nothing else (see [the sample](help:sample)).
    #[value(verbatim_doc_comment)]
    RecordSample,
    /// A big file that keeps changing: a proposal from size and churn, not a verdict. Load the codebase-design and improve-codebase-architecture skills and decide: design the deeper interface first, then move to it one passing step at a time; or keep it as it is (`slimfit keep <file>`).
    #[value(verbatim_doc_comment)]
    Deepen,
}

impl TaskKind {
    /// The command that must pass after a task of this kind is done.
    pub fn accept(self) -> &'static str {
        // Every kind's: the change it makes then picks its own mutants, as
        // any change that adds source lines does (a pick-mutants task).
        "slimfit check"
    }

    /// Every accept command, each once, in the order of the kinds.
    pub fn accepts() -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for k in <TaskKind as clap::ValueEnum>::value_variants() {
            if !out.contains(&k.accept()) {
                out.push(k.accept());
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub kind: TaskKind,
    pub suite: String,
    pub summary: String,
    pub files: Vec<String>,
    pub evidence: Value,
    pub steps: String,
    pub accept: String,
}

#[cfg(test)]
impl Task {
    /// A task of no substance.
    pub fn example() -> Self {
        let kind = TaskKind::FailingTest;
        Task {
            kind,
            suite: String::new(),
            summary: String::new(),
            files: vec![],
            evidence: Value::Null,
            steps: String::new(),
            accept: kind.accept().into(),
        }
    }
}

/// How many tasks of one kind in one suite `next --all` lists; the rest are
/// only counted.
pub const LISTED_PER_KIND: usize = 5;
/// As [`LISTED_PER_KIND`], for deepen tasks.
pub const LISTED_DEEPEN: usize = 3;
/// How far back in git history `deepen` counts a file's changes, in days.
pub const DEEPEN_DAYS: u32 = 90;

fn per_kind(kind: TaskKind) -> usize {
    if kind == TaskKind::Deepen { LISTED_DEEPEN } else { LISTED_PER_KIND }
}

/// The tasks worth listing: the first few of each kind in each suite, in
/// order. Count before capping: what is left is every task, not the list.
pub fn cap(tasks: Vec<Task>) -> Vec<Task> {
    let mut seen: BTreeMap<(String, TaskKind), usize> = BTreeMap::new();
    tasks
        .into_iter()
        .filter(|t| {
            let n = seen.entry((t.suite.clone(), t.kind)).or_default();
            *n += 1;
            *n <= per_kind(t.kind)
        })
        .collect()
}

pub fn all(repo: &Path, tree: &Tree, cfg: &Config, m: &Measurement, base: &Baseline) -> Result<Vec<Task>> {
    let held = crate::landing::hold(repo, cfg, tree, m.with_mutants)?;
    let mut out = pick_mutants(repo, tree, cfg, held.as_ref())?;
    out.extend(held.as_ref().and_then(|h| justify_growth(h, cfg.check.growth_allowance)));
    let landed = landed(repo);
    for (sc, s) in cfg.suites.iter().zip(&m.suites) {
        out.extend(of_measurement(s, base));
        out.extend(rising(repo, &cfg.check, s, &landed));
        out.extend(deepen(repo, tree.suite(cfg, &sc.name)?, &base.kept_files));
    }
    out.sort_by_key(|t| t.kind);
    Ok(out)
}

/// The tasks a suite's measurement alone shows (everything but `deepen`,
/// which reads git history), less the uncovered spans and redundant tests
/// `base` keeps.
pub fn of_measurement(s: &SuiteMeasurement, base: &Baseline) -> Vec<Task> {
    [failing(s), uncovered(s, &base.kept_spans), redundant(s, &base.kept), slow(s), surviving(s)].concat()
}

fn failing(s: &SuiteMeasurement) -> Vec<Task> {
    s.tests
        .iter()
        .filter(|t| !t.passed)
        .map(|t| Task {
            kind: TaskKind::FailingTest,
            suite: s.suite.clone(),
            summary: format!("{} fails", t.id),
            files: vec![],
            evidence: json!({ "test": t.id, "log": format!(".slimfit/out/{}/test.log", s.suite) }),
            steps: "Read the log, find why it fails, fix the code (or the test if the test is wrong). Nothing else is measured honestly until every test passes.".into(),
            accept: TaskKind::FailingTest.accept().into(),
        })
        .collect()
}

/// A task per suite whose change adds source lines or removes tests and no
/// record of its mutants (none due once `picked` ran: `ratchet` records
/// them), with what its tool lists of the change to pick from.
fn pick_mutants(repo: &Path, tree: &Tree, cfg: &Config, held: Option<&crate::landing::Held>) -> Result<Vec<Task>> {
    let unrecorded = held.map(|h| h.unrecorded.clone()).unwrap_or_default();
    if unrecorded.is_empty() {
        return Ok(vec![]);
    }
    let mut candidates = crate::measure::candidates(repo, cfg, tree)?;
    let removed = held.map(|h| h.removed_tests.clone()).unwrap_or_default();
    Ok(unrecorded
        .into_iter()
        .map(|(suite, u)| {
            let lines = u.lines as f64;
            let listed = candidates.remove(&suite).unwrap_or_default();
            let files: BTreeSet<String> = listed.iter().map(|c| c.file.clone()).collect();
            let removed = removed.get(&suite).cloned().unwrap_or_default();
            let what = match u.tests {
                0 => format!("adds {lines} source line(s)"),
                t => format!("adds {lines} source line(s) and removes {t} test(s)"),
            };
            Task {
                kind: TaskKind::PickMutants,
                summary: format!(
                    "the change {what} and no record of its mutants: have a second agent pick the few worth running, of {}",
                    listed.len()
                ),
                suite,
                files: files.into_iter().collect(),
                evidence: json!({ "lines": lines, "removed_tests": removed, "candidates": listed }),
                steps: format!(
                    "Do not pick yourself: you wrote the change, so you would pick what your tests already hold. \
                     Ask a second agent, giving it only the goal (`slimfit help picker`), the change's diff and \
                     `candidates` (what `slimfit check --mutants` lists); in factorium, as `slimfit help factorium` \
                     says. It answers at most {} picks, each with a why, or why none is worth one. Run its picks at \
                     background priority: `factorium worker exclusive --background -- slimfit check --mutants \
                     <name>... --why <why>...` (outside factorium, `nice -n 19 slimfit check ...`). A survivor: assert \
                     what it changes with a fast test through the interface, or simplify the code; if it changes \
                     nothing observable, `slimfit ratchet --loosen survived_mutants --reason \"<why>\"`; one on a removed \
                     test's line (`removed_tests`) is a test removed too many: put it back, or assert what it \
                     changes in the test it was merged into. Then \
                     `slimfit ratchet --picker <its id> --author <yours>` writes the record; when it picked none, \
                     `slimfit ratchet --no-mutants \"<its why>\" --picker <its id> --author <yours>`. Commit \
                     .slimfit/ with the change.",
                    crate::measure::PICKS_MAX
                ),
                accept: TaskKind::PickMutants.accept().into(),
            }
        })
        .collect())
}

/// A task when the landing `h` adds more than `allowance` source lines net
/// and no growth record that names a reviewer other than its author, with
/// the files that grew most first.
fn justify_growth(h: &crate::landing::Held, allowance: u64) -> Option<Task> {
    if !h.growth_due(allowance) {
        return None;
    }
    let mut files: Vec<(&String, i64)> = h.grown.values().flatten().map(|(f, n)| (f, *n)).collect();
    files.sort_by_key(|(f, n)| (-n, f.to_string()));
    let suites: Vec<&str> = h.grown.keys().map(String::as_str).collect();
    Some(Task {
        kind: TaskKind::JustifyGrowth,
        suite: suites.join(", "),
        summary: format!(
            "the change adds {} source line(s) net, more than growth_allowance ({allowance}), and no growth record: \
             have a second agent judge whether it could be smaller",
            h.growth
        ),
        files: files.iter().filter(|(_, n)| *n > 0).map(|(f, _)| f.to_string()).collect(),
        evidence: json!({
            "lines": h.growth,
            "allowance": allowance,
            "from": h.from,
            "grown": files.iter().map(|(f, n)| json!({ "file": f, "lines": n })).collect::<Vec<_>>(),
        }),
        steps: format!(
            "Do not judge it yourself: you wrote the change, so you would find it all needed. So {}.",
            crate::growth::JUSTIFY_GROWTH
        ),
        accept: TaskKind::JustifyGrowth.accept().into(),
    })
}

/// A task per uncovered span of two lines or more, less those `kept`,
/// unchanged since.
fn uncovered(s: &SuiteMeasurement, kept: &[KeptSpan]) -> Vec<Task> {
    s.uncovered
        .iter()
        .filter(|sp| sp.lines >= 2 && !kept.iter().any(|k| k.holds(&s.suite, sp)))
        .map(|sp| Task {
            kind: TaskKind::Uncovered,
            suite: s.suite.clone(),
            summary: format!(
                "{}:{}-{} ({} lines{}) runs under no test",
                sp.file,
                sp.start,
                sp.end,
                sp.lines,
                sp.function.as_ref().map(|f| format!(", in {f}")).unwrap_or_default()
            ),
            files: vec![sp.file.clone()],
            evidence: json!(sp),
            steps: "Find who calls this code. No caller on any user path: delete it, and whatever only it used. \
                    A user path needs it: add a test through the module's interface that asserts what these lines decide: \
                    the result or effect that would differ were they wrong (never test past the interface). \
                    A test that only runs the lines, asserting nothing they decide, is no fix: coverage would count it, \
                    and no bound holds you to coverage, so nothing is gained by it. \
                    The new test must be fast (milliseconds, no sleeps, network or process spawns): if only a slow test can reach the line, delete the code or escalate, never add the slow test. \
                    It is a product feature nobody seems to use: do not delete it; removing features is a product decision, so escalate with this evidence. \
                    KEEP it when it runs only against the real host or the real program (a binary's `main` under the tests that spawn it, \
                    which coverage does not record), so a fast test could only run it without asserting what it decides: record why with \
                    `slimfit keep <file>:<start>-<end> --reason \"<why>\"` (this task's span) and commit `.slimfit/baseline.json`; \
                    it is proposed again once a line of it changes or the span grows past it."
                .into(),
            accept: TaskKind::Uncovered.accept().into(),
        })
        .collect()
}

/// Two tests overlap enough to be merge candidates when the lines both run
/// are at least this share of the lines either runs (ADR 0006).
pub const MERGE_OVERLAP: f64 = 0.8;

/// A line more than this share of a test binary's tests run is its shared
/// setup (a scenario's world, a daemon's tick): overlap is computed without
/// it (ADR 0006).
pub const COMMON_LINES: f64 = 0.5;

/// Redundant-test candidates from per-test coverage (ADR 0006): a test whose
/// lines are all run by another test too (when both run the same lines, the
/// slower of the two is proposed), or that shares at least [`MERGE_OVERLAP`]
/// of the lines either runs with another (the slower of the two is
/// proposed). Overlap is computed without the lines more than
/// [`COMMON_LINES`] of the test's binary's tests run, which shared setup makes, not what a
/// test checks; a subset is of all lines, but only of a test whose own lines (those past its binary's shared setup) the other test runs as its own too: a test of shared setup alone, such as reading a shipped file, is a subset of every test that reads it, and its lines say nothing of what it checks. Each is named once, with the other test it most overlaps.
/// A task is one test file with every candidate in it, slowest first; the
/// files most seconds to save first. Coverage only proposes:
/// the steps decide by reading the assertions. A test kept, unchanged since,
/// is not proposed.
fn redundant(s: &SuiteMeasurement, kept: &[Kept]) -> Vec<Task> {
    let Some((reach, per_test)) = s.reach.as_ref().and_then(|r| Some((r, r.lines.as_ref()?))) else { return vec![] };
    let seconds = |id: &str| s.tests.iter().find(|t| t.id == id).map_or(0.0, |t| t.cpu);
    let ids: Vec<String> = reach.tests.iter().map(ReachTest::id).collect();
    // Each test, its binary, the (file, line)s it runs, and its CPU seconds.
    let lines: Vec<_> = ids
        .iter()
        .zip(&reach.tests)
        .zip(per_test)
        .map(|((t, rt), files)| {
            let ls: BTreeSet<(&str, u32)> =
                files.iter().flat_map(|(f, ls)| ls.iter().map(move |l| (f.as_str(), *l))).collect();
            (t, rt.binary.as_str(), ls, seconds(t))
        })
        .filter(|(_, _, ls, _)| !ls.is_empty())
        .map(|(t, bin, ls, secs)| ((t, ls, secs), bin))
        .collect();
    // Each test's own lines: those COMMON_LINES of its binary's tests or fewer run.
    let mut runs: HashMap<(&str, (&str, u32)), usize> = HashMap::new();
    let mut tests_of: HashMap<&str, usize> = HashMap::new();
    for ((_, ls, _), bin) in &lines {
        *tests_of.entry(*bin).or_default() += 1;
        for l in ls {
            *runs.entry((*bin, *l)).or_default() += 1;
        }
    }
    let own: Vec<BTreeSet<_>> = lines
        .iter()
        .map(|((_, ls, _), bin)| {
            let shared = COMMON_LINES * tests_of[bin] as f64;
            ls.iter().filter(|l| runs[&(*bin, **l)] as f64 <= shared).copied().collect()
        })
        .collect();
    let lines: Vec<_> = lines.into_iter().map(|(l, _)| l).collect();
    let at = |id: &str| reach.tests.iter().find(|t| t.id() == id);
    // Per test file: the seconds its candidates would save, the other tests' files, and the candidates.
    type File = (f64, BTreeSet<String>, Vec<(f64, Value)>);
    let mut by_file: BTreeMap<String, File> = BTreeMap::new();
    for ((t, mine, secs), mine_own) in lines.iter().zip(&own) {
        if at(t).is_some_and(|rt| kept.iter().any(|k| k.holds(&s.suite, rt))) {
            continue;
        }
        // The other test with the most overlap; its share and whether it runs all of mine.
        let best = lines
            .iter()
            .zip(&own)
            .filter(|((o, _, _), _)| o != t)
            .map(|((o, theirs, o_secs), theirs_own)| {
                let slower = (secs, t) > (o_secs, o);
                // A subset runs own lines of its own in common: one of only shared setup gives no evidence (slimfit#91).
                let both = mine_own.intersection(theirs_own).count();
                let within = both > 0 && mine.is_subset(theirs) && (mine.len() < theirs.len() || slower);
                // Own lines that only overlap: the pair is named once, by its slower test.
                let overlap = both < mine_own.len() && both < theirs_own.len() && slower;
                let union = mine_own.union(theirs_own).count();
                (o, if union == 0 { 0.0 } else { both as f64 / union as f64 }, within, overlap)
            })
            .filter(|(_, share, within, overlap)| *within || (*overlap && *share >= MERGE_OVERLAP))
            .map(|(o, share, within, _)| (o, share, within))
            .max_by(|a, b| (a.2, a.1).partial_cmp(&(b.2, b.1)).unwrap_or(std::cmp::Ordering::Equal).then(b.0.cmp(a.0)));
        let Some((other, share, within)) = best else { continue };
        let file = at(t).map(|rt| rt.source.as_ref().map_or(rt.binary.clone(), |src| src.file.clone()));
        let other_file = at(other).and_then(|rt| Some(rt.source.as_ref()?.file.clone()));
        let candidate = json!({
            "test": t,
            "other_test": other,
            "kind": if within { "subset" } else { "overlap" },
            "share": (share * 100.0).round() / 100.0,
            "seconds": secs,
            "lines": mine.len(),
        });
        let (total, others, candidates) = by_file.entry(file.unwrap_or_default()).or_default();
        *total += *secs;
        others.extend(other_file);
        candidates.push((*secs, candidate));
    }
    let mut files: Vec<_> = by_file.into_iter().collect();
    files.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
    files
        .into_iter()
        .map(|(file, (total, others, mut candidates))| {
            candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut candidates: Vec<Value> = candidates.into_iter().map(|(_, c)| c).collect();
            let groups = classify(&file, &mut candidates, |id| at(id)?.source.as_ref());
            let files = std::iter::once(file.clone()).chain(others.into_iter().filter(|f| *f != file)).collect();
            Task {
                kind: TaskKind::RedundantTest,
                suite: s.suite.clone(),
                summary: format!(
                    "{file}: {} test(s) another test runs over, {total:.2}s of CPU to save",
                    candidates.len()
                ),
                files,
                evidence: json!({
                    "file": file,
                    "seconds": (total * 1000.0).round() / 1000.0,
                    "candidates": candidates,
                    "groups": groups,
                }),
                steps: "Coverage only proposes; each candidate's `class` says how it is decided. Read the file once and decide every candidate in it. \
                        `asserts-contained`: DELETE `test`: every statement it makes, its assertions and their setup, is in its `other_test` as it is written, which runs every line it runs. \
                        `same-shape`: MERGE its group (`evidence.groups[group]`) into one table-driven test: the tests differ only in the literals each row names, \
                        so the table's rows are those literals and its body the tests' body with each row's literal in their place; each row fails with its own name; rows of the same literals are one case, kept once. \
                        `read`: the assertions decide. For \
                        each: for every assertion of its `test`, find the assertion in its `other_test` (or another test you name) that \
                        checks the same outcome on the same setup. Then do one: \
                        DELETE `test` when every assertion maps (put the mapping in the commit message). \
                        MERGE when some assertion does not map but the two build compatible setups: fold the unmapped assertions, and the setup they need, \
                        into one test, or turn both into one test over a table of cases, so the setup is paid once; the merged test keeps every \
                        assertion of both, each failing with its own message, and the commit message lists where each assertion went. \
                        KEEP both when the setups conflict (folding one changes what the other checks) or a merge would hide which behaviour failed: \
                        record it with `slimfit keep <test> --reason \"<why>\"` and commit `.slimfit/baseline.json`, so `test` is not proposed again \
                        until its text changes. \
                        The accept command holds that no test fails, and answers, as trends, whether a line became \
                        uncovered (`new_spans`) and how test time moved: neither may rise, and after a merge `test_seconds` must fall. \
                        A delete or a merge removes a test, so the change owes picks over the lines it ran, run against the tests left \
                        (a `pick-mutants` task): a survivor is a test removed too many: put it back, or assert what it changes in the merged test."
                    .into(),
                accept: TaskKind::RedundantTest.accept().into(),
            }
        })
        .collect()
}

/// Sets the `class` of each of `file`'s `candidates` (slimfit#88), from the
/// tests' sources (`source`): `asserts-contained`, a subset that asserts and
/// whose every statement (its assertions, and the setup they check) its
/// `other_test` makes too, as written; else `same-shape`, its body that of
/// another test of the file but for literals (another candidate, or one's
/// `other_test`), with `group`, its group's index in the answer: each group
/// the rows of one table-driven test, a test and the literals that differ
/// between its rows; else `read`.
fn classify<'a>(file: &str, candidates: &mut [Value], source: impl Fn(&str) -> Option<&'a TestSource>) -> Vec<Value> {
    let shape = |id: &Value| source(id.as_str().unwrap_or_default())?.shape.as_ref();
    let contained = |c: &Value| {
        let (Some(mine), Some(theirs)) = (shape(&c["test"]), shape(&c["other_test"])) else { return false };
        // An assertion alone may hold on another setup: guild's doctor asserts no `behind_hub` of a hub, and of a machine.
        let within = mine.statements.iter().all(|s| theirs.statements.contains(s));
        c["kind"] == "subset" && !mine.asserts.is_empty() && within
    };
    for c in candidates.iter_mut() {
        c["class"] = json!(if contained(c) { "asserts-contained" } else { "read" });
    }
    // The file's tests by body, but those deleted: each candidate's, then its other test's.
    let deleted: Vec<&Value> = candidates.iter().filter(|c| c["class"] != "read").map(|c| &c["test"]).collect();
    let mut groups: Vec<(&str, Vec<(&Value, &Shape)>)> = Vec::new();
    for c in candidates.iter().filter(|c| c["class"] == "read") {
        for id in [&c["test"], &c["other_test"]] {
            let here = source(id.as_str().unwrap_or_default()).filter(|s| s.file == file);
            let Some(s) = here.and_then(|s| s.shape.as_ref()).filter(|_| !deleted.contains(&id)) else { continue };
            match groups.iter_mut().find(|(body, _)| *body == s.body) {
                Some((_, rows)) if rows.iter().any(|(t, _)| *t == id) => {}
                Some((_, rows)) => rows.push((id, s)),
                None => groups.push((&s.body, vec![(id, s)])),
            }
        }
    }
    groups.retain(|(_, rows)| rows.len() > 1);
    let tables: Vec<(Vec<Value>, Value)> = groups
        .into_iter()
        .map(|(_, rows)| {
            let first = &rows[0].1.literals;
            let differ: Vec<usize> =
                (0..first.len()).filter(|&k| rows.iter().any(|(_, s)| s.literals[k] != first[k])).collect();
            let ids = rows.iter().map(|(t, _)| (*t).clone()).collect();
            let rows = rows.iter().map(
                |(t, s)| json!({ "test": t, "literals": differ.iter().map(|k| &s.literals[*k]).collect::<Vec<_>>() }),
            );
            (ids, Value::Array(rows.collect()))
        })
        .collect();
    for c in candidates.iter_mut() {
        if let Some(g) = tables.iter().position(|(ids, _)| ids.contains(&c["test"])) {
            c["class"] = json!("same-shape");
            c["group"] = json!(g);
        }
    }
    tables.into_iter().map(|(_, rows)| rows).collect()
}

/// How many redundant-test candidates `tasks` hold, over all their files.
pub fn candidates(tasks: &[Task]) -> usize {
    let of = |t: &Task| t.evidence["candidates"].as_array().map_or(0, Vec::len);
    tasks.iter().filter(|t| t.kind == TaskKind::RedundantTest).map(of).sum()
}

fn slow(s: &SuiteMeasurement) -> Vec<Task> {
    let total = s.metrics.test_seconds;
    let mut tests: Vec<_> = s.tests.iter().filter(|t| t.cpu >= 0.5 && t.cpu >= total * 0.05).collect();
    tests.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
    tests
        .into_iter()
        .map(|t| Task {
            kind: TaskKind::SlowTest,
            suite: s.suite.clone(),
            summary: format!("{} takes {:.2}s of CPU of {:.2}s", t.id, t.cpu, total),
            files: vec![],
            evidence: json!({ "test": t.id, "seconds": t.cpu, "suite_seconds": total }),
            steps: "Find where the time goes (sleeps, real network or disk, process spawns, large fixtures, repeated setup). \
                    Make it cheaper without testing less: shrink the input, share setup, replace a slow dependency with an in-memory adapter at an existing seam. \
                    If it overlaps faster tests, merge or delete it."
                .into(),
            accept: TaskKind::SlowTest.accept().into(),
        })
        .collect()
}

/// How many landings back a rising trend is looked for.
pub const TREND_LANDINGS: usize = 5;
/// The fewest landings a rising trend is read from: a rise over fewer is
/// the change's own, which its `check` answers.
pub const TREND_MIN: usize = 3;
/// How far `test_seconds` may rise over the landings, as a share of the
/// first, before it is a task: past what load moved the CPU time of an
/// unchanged tree (to 51% over its baseline, the most seen).
pub const TIME_NOISE: f64 = 0.6;

/// A landing: a first-parent commit that wrote the baseline, and what the
/// measurement it was written from measured of each suite.
struct Landed {
    commit: String,
    date: String,
    subject: String,
    measured: BTreeMap<String, Measured>,
}

/// The last [`TREND_LANDINGS`] landings of HEAD that kept what they
/// measured, oldest first.
fn landed(repo: &Path) -> Vec<Landed> {
    let n = TREND_LANDINGS.to_string();
    let args = ["log", "--first-parent", "-n", &n, "--format=%H%x09%cs%x09%s", "--", crate::ratchet::FILE];
    let log = git::run(repo, &args).unwrap_or_default();
    let mut out: Vec<Landed> = log
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (commit, date, subject) = (f.next()?, f.next()?, f.next().unwrap_or_default());
            let text = git::run(repo, &["show", &format!("{commit}:{}", crate::ratchet::FILE)]).ok()?;
            let b: Value = serde_json::from_str(&text).ok()?;
            let measured = serde_json::from_value(b.get("measured")?.clone()).ok()?;
            Some(Landed { commit: commit[..12].into(), date: date.into(), subject: subject.into(), measured })
        })
        .collect();
    out.reverse();
    out
}

/// The suite's trends that rose over `landed` past their noise: a task each.
fn rising(repo: &Path, check: &crate::config::CheckConfig, s: &SuiteMeasurement, landed: &[Landed]) -> Vec<Task> {
    let series = |of: fn(&Measured) -> Option<f64>| -> Vec<(&Landed, f64)> {
        landed.iter().filter_map(|l| Some((l, of(l.measured.get(&s.suite)?)?))).collect()
    };
    let slack = check.coverage_slack as f64;
    let mut out = Vec::new();
    for time in [false, true] {
        let metric = if time { "test_seconds" } else { "uncovered_lines" };
        let values = if time { series(|m| m.test_seconds) } else { series(|m| Some(m.uncovered_lines as f64)) };
        let (Some((first, from)), Some((last, to))) = (values.first(), values.last()) else { continue };
        let past = if time { from * (1.0 + TIME_NOISE) } else { from + slack };
        if values.len() < TREND_MIN || *to <= past + 1e-9 {
            continue;
        }
        // What grew: what the landings after the first added.
        let patch = git::diff(repo, &first.commit, "HEAD", ".", &[]).unwrap_or_default();
        let added = crate::landing::added_lines(&patch, "");
        let (grew, files) = if time { grown_tests(s, &added) } else { grown_spans(s, &added) };
        let landings: Vec<Value> = values
            .iter()
            .map(|(l, v)| json!({ "commit": l.commit, "date": l.date, "subject": l.subject, "value": v }))
            .collect();
        let (key, kind) = if time { ("tests", "slow-test") } else { ("spans", "uncovered") };
        out.push(Task {
            kind: TaskKind::RisingTrend,
            suite: s.suite.clone(),
            summary: format!(
                "{metric} rose from {from} to {to} over the last {} landings, {} to {}",
                values.len(),
                first.commit,
                last.commit
            ),
            files,
            evidence: json!({ "metric": metric, "from": from, "to": to, "landings": landings, key: grew }),
            steps: format!(
                "No bound holds {metric}, so this is how a run of rises shows. Read the landings in \
                 `evidence.landings` (`git show <commit>`): which brought the rise, and what it bought. Then lower it \
                 where `evidence.{key}` points, as a `{kind}` task's steps say ({}), or judge the rise worth what it \
                 bought and leave it: no check fails on it, and the task goes once the landings no longer rise.",
                if time {
                    "make each test cheaper without testing less, or merge it into faster ones"
                } else {
                    "a fast test through the module's interface that asserts what the lines decide, or delete the \
                     lines nothing needs; a test that only runs them is no fix"
                }
            ),
            accept: TaskKind::RisingTrend.accept().into(),
        });
    }
    out
}

/// The uncovered spans holding a line of `added`, and their files.
fn grown_spans(s: &SuiteMeasurement, added: &crate::landing::Lines) -> (Value, Vec<String>) {
    let spans: Vec<_> = s.uncovered.iter().filter(|sp| sp.holds_any(added)).collect();
    let files: BTreeSet<String> = spans.iter().map(|sp| sp.file.clone()).collect();
    (json!(spans), files.into_iter().collect())
}

/// The tests in the files `added` changes, slowest first (every test when
/// the test run records no test's file), the first [`LISTED_PER_KIND`].
fn grown_tests(s: &SuiteMeasurement, added: &crate::landing::Lines) -> (Value, Vec<String>) {
    let file = |id: &str| {
        let t = s.reach.as_ref()?.tests.iter().find(|t| t.id() == id)?;
        Some(t.source.as_ref()?.file.clone())
    };
    let known = s.reach.is_some();
    let mut tests: Vec<_> =
        s.tests.iter().filter(|t| !known || file(&t.id).is_some_and(|f| added.contains_key(&f))).collect();
    tests.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
    tests.truncate(LISTED_PER_KIND);
    let files: BTreeSet<String> = tests.iter().filter_map(|t| file(&t.id)).collect();
    let tests: Vec<Value> = tests.iter().map(|t| json!({ "test": t.id, "seconds": t.cpu })).collect();
    (json!(tests), files.into_iter().collect())
}

fn surviving(s: &SuiteMeasurement) -> Vec<Task> {
    let mut by_file: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for m in s.mutants.iter().filter(|m| m.status == MutantStatus::Survived) {
        by_file.entry(&m.file).or_default().push(json!({ "line": m.line, "mutant": m.description }));
    }
    survivors(&s.suite, by_file, Source::Picker)
}

/// The surviving-mutant tasks of `by_file`'s survivors (each with its
/// `line` and `mutant`) chosen by `source`: one per file, most first.
fn survivors(suite: &str, by_file: BTreeMap<&str, Vec<Value>>, source: Source) -> Vec<Task> {
    let mut files: Vec<_> = by_file.into_iter().collect();
    files.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    files
        .into_iter()
        .map(|(file, ms)| Task {
            kind: TaskKind::SurvivingMutant,
            suite: suite.to_string(),
            summary: format!("{} mutants survive in {file} ({})", ms.len(), json!(source).as_str().unwrap_or_default()),
            files: vec![file.to_string()],
            evidence: json!({ "source": source, "survivors": ms }),
            steps: "For each survivor: the change it makes is behaviour no test checks. If the behaviour matters, add an assertion through the module's interface that the mutant breaks. \
                    If it does not matter, the code is more than it needs to be: simplify it until the mutant cannot exist. \
                    The new assertion must be fast: add it to a fast test, never a slow one; if only a slow test could catch the mutant, simplify the code instead."
                .into(),
            accept: TaskKind::SurvivingMutant.accept().into(),
        })
        .collect()
}

/// A sample's tasks: its survivors, as `sampled` surviving-mutant tasks,
/// and, when it drew any, its record to commit. No `check` reads either.
pub fn sampled(s: &crate::measure::sample::Sampled) -> Result<Vec<Task>> {
    let (path, text) = s.sample.file()?;
    let mut out = Vec::new();
    for (suite, drawn) in &s.sample.drawn {
        let mut by_file: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        for d in drawn.iter().filter(|d| d.outcome == Some(MutantStatus::Survived)) {
            let Some((_, c)) = s.candidates.iter().find(|(x, c)| x == suite && c.name == d.name) else { continue };
            let mutant = c.name.splitn(4, ':').nth(3).map_or(c.name.as_str(), str::trim);
            by_file.entry(&c.file).or_default().push(json!({ "line": c.line, "mutant": mutant, "name": c.name }));
        }
        out.extend(survivors(suite, by_file, Source::Sampled));
    }
    let counts = s.sample.counts();
    if s.sample.drawn.values().any(|d| !d.is_empty()) {
        out.push(Task {
            kind: TaskKind::RecordSample,
            suite: s.sample.drawn.keys().cloned().collect::<Vec<_>>().join(","),
            summary: format!(
                "record the sample drawn at {}: {} killed, {} survived, {} unviable, of {} mutants not picked",
                &s.sample.tip[..12.min(s.sample.tip.len())],
                counts.killed,
                counts.survived,
                counts.unviable,
                s.sample.pool
            ),
            files: vec![path.clone()],
            evidence: json!({ "path": path, "counts": counts, "text": text }),
            steps: "Write `evidence.text` exactly as it is to `evidence.path` and commit it alone. Change nothing                     else: it is a record of what ran, not work on the code; its survivors are tasks of their own.                     The change adds no source line, so it needs no record of mutants."
                .into(),
            accept: TaskKind::RecordSample.accept().into(),
        });
    }
    Ok(out)
}

/// The suite that counts `path` (relative to the repo) as an existing
/// source file, with its prod lines: where a deepen task's file belongs.
pub fn source_suite(tree: &Tree, cfg: &Config, path: &str) -> Result<Option<(String, u64)>> {
    for sc in &cfg.suites {
        if let Some(l) = tree.suite(cfg, &sc.name)?.source_lines(path) {
            return Ok(Some((sc.name.clone(), l.prod)));
        }
    }
    Ok(None)
}

/// How many commits changed `file`: in the last [`DEEPEN_DAYS`], and ever.
pub fn churn(repo: &Path, file: &str) -> (u64, u64) {
    let count = |args: &[&str]| {
        let out = git::run(repo, &[&["log", "--no-merges", "--format=%H"], args, &["--", file]].concat());
        out.unwrap_or_default().lines().count() as u64
    };
    (count(&[&format!("--since={DEEPEN_DAYS}.days")]), count(&[]))
}

/// Source files that keep changing and are big: where a deeper module pays
/// off soonest. A file kept as it is (`kept`) is not proposed until it has
/// grown or churned past the verdict's thresholds.
fn deepen(repo: &Path, st: &SuiteTree, kept: &[KeptFile]) -> Vec<Task> {
    let suite = &st.suite.name;
    let log =
        git::run(repo, &["log", &format!("--since={DEEPEN_DAYS}.days"), "--no-merges", "--format=", "--name-only"])
            .unwrap_or_default();
    let mut changes: BTreeMap<&str, u64> = BTreeMap::new();
    for f in log.lines().filter(|l| !l.is_empty()) {
        if st.kind(f) == Some(Kind::Source) {
            *changes.entry(f).or_default() += 1;
        }
    }
    let mut hot = Vec::new();
    for (f, n) in changes.into_iter().filter(|(_, n)| *n >= 5) {
        let Some(lines) = st.source_lines(f).map(|l| l.prod) else { continue };
        // Only a file with a verdict pays for counting its whole history.
        let held = || kept.iter().filter(|k| k.file == f).any(|k| k.holds(suite, f, lines, churn(repo, f).1));
        if lines >= 100 && !held() {
            hot.push((f.to_string(), n, lines));
        }
    }
    hot.sort_by_key(|(f, n, l)| (std::cmp::Reverse(n * l), f.clone()));
    hot
        .into_iter()
        .map(|(f, n, l)| Task {
            kind: TaskKind::Deepen,
            suite: suite.to_string(),
            summary: format!("{f}: {l} lines, changed in {n} commits in {DEEPEN_DAYS} days"),
            files: vec![f.clone()],
            evidence: json!({ "file": f, "prod_lines": l, "commits_90d": n }),
            steps: "Size and churn only propose; the module decides. Load the codebase-design and improve-codebase-architecture skills. Look at the module this file belongs to: \
                    shallow interfaces, pass-through functions, one concept spread over many small modules, tests that reach past the interface. Then do one: \
                    CHANGE it: write the deeper interface down first (GLOSSARY names, ADR if it settles a trade-off), then move to it one step per commit, each step passing the accept command. \
                    KEEP it when it is already deep enough and nothing is worth changing: record why with `slimfit keep <file> --reason \"<why>\"` and commit \
                    `.slimfit/baseline.json`; `slimfit keep --help` says when it is proposed again."
                .into(),
            accept: TaskKind::Deepen.accept().into(),
        })
        .collect()
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::model::{Metrics, TestCase};

    #[test]
    fn cap_keeps_the_first_few_of_each_kind_in_each_suite() {
        let task = |suite: &str, kind| Task { kind, suite: suite.into(), ..Task::example() };
        let mut all = Vec::new();
        for suite in ["a", "b"] {
            all.extend((0..7).map(|_| task(suite, TaskKind::Uncovered)));
            all.extend((0..4).map(|_| task(suite, TaskKind::Deepen)));
        }
        let kept = cap(all);
        let n = |suite: &str, kind| kept.iter().filter(|t| t.suite == suite && t.kind == kind).count();
        assert_eq!((n("a", TaskKind::Uncovered), n("a", TaskKind::Deepen)), (5, 3));
        assert_eq!((n("b", TaskKind::Uncovered), n("b", TaskKind::Deepen)), (5, 3));
    }

    fn suite(tests: impl IntoIterator<Item = (&'static str, f64, Vec<u32>)>) -> SuiteMeasurement {
        let mut reach = crate::model::Reach { lines: Some(vec![]), ..Default::default() };
        let mut cases = Vec::new();
        for (id, secs, lines) in tests {
            // `binary::name`, or a name alone, in binary `t`.
            let (binary, name) = id.split_once("::").unwrap_or(("t", id));
            let source =
                crate::model::TestSource { file: format!("tests/{name}.rs"), line: 1, hash: "h".into(), shape: None };
            reach.tests.push(ReachTest { binary: binary.into(), name: name.into(), source: Some(source) });
            reach.lines.as_mut().unwrap().push([("a.rs".to_string(), lines.into_iter().collect())].into());
            cases.push(TestCase { id: format!("{binary}::{name}"), seconds: 0.0, passed: true, cpu: secs });
        }
        SuiteMeasurement {
            suite: "s".into(),
            metrics: Metrics::default(),
            tests: cases,
            uncovered: vec![],
            mutants: vec![],
            reach: Some(reach),
            mutated: None,
            command_seconds: 0.0,
            untimed: false,
        }
    }

    /// A big file that keeps changing is not proposed when it is test code:
    /// a file some other file declares `#[cfg(test)] mod x;`.
    #[test]
    fn a_test_module_file_is_no_deepen_candidate() {
        use crate::measure::t::git;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/main.rs"), "#[cfg(test)]\nmod fixtures;\nfn main() {}\n").unwrap();
        for i in 0..5 {
            let body: String = (0..120).map(|n| format!("pub const C{n}: u8 = {i};\n")).collect();
            for f in ["src/fixtures.rs", "src/big.rs"] {
                std::fs::write(repo.join(f), &body).unwrap();
            }
            git(repo, &["add", "."]);
            git(repo, &["commit", "-qm", &format!("c {i}")]);
        }
        let (tree, cfg) = (crate::measure::t::tree(repo), Config::one("s", crate::config::Preset::Rust));
        let proposed: Vec<_> = deepen(repo, tree.suite(&cfg, "s").unwrap(), &[]).into_iter().map(|t| t.files).collect();
        assert_eq!(proposed, [["src/big.rs"]]);
    }

    /// A big file that keeps changing is proposed; kept, it is not, until
    /// its prod lines grow by KEPT_GROWTH or as many commits change it again
    /// as made it a candidate; a verdict on another suite does not hold.
    #[test]
    fn a_kept_file_returns_once_it_grows_or_churns_past_the_verdict() {
        use crate::measure::t::git;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        let cfg = Config::one("s", crate::config::Preset::Rust);
        let commit = |lines: usize, note: &str| {
            let body: String = (0..lines).map(|i| format!("pub const C{i}: u8 = {};\n", note.len())).collect();
            std::fs::create_dir_all(repo.join("src")).unwrap();
            std::fs::write(repo.join("src/big.rs"), format!("// {note}\n{body}")).unwrap();
            git(repo, &["add", "."]);
            git(repo, &["commit", "-qm", &format!("c {note}")]);
        };
        for i in 0..5 {
            commit(120, &"x".repeat(i));
        }
        // The tree as it is after the last commit.
        let proposed =
            |kept: &[KeptFile]| deepen(repo, crate::measure::t::tree(repo).suite(&cfg, "s").unwrap(), kept).len();
        assert_eq!(proposed(&[]), 1);
        let (commits_90d, commits_total) = churn(repo, "src/big.rs");
        assert_eq!((commits_90d, commits_total), (5, 5));
        let prod_lines = source_suite(&crate::measure::t::tree(repo), &cfg, "src/big.rs").unwrap().unwrap().1;
        let verdict = |suite: &str| KeptFile {
            suite: suite.into(),
            file: "src/big.rs".into(),
            prod_lines,
            commits_90d,
            commits_total,
            reason: "deep enough".into(),
        };
        assert_eq!(proposed(&[verdict("s")]), 0);
        assert_eq!(proposed(&[verdict("other suite")]), 1);
        // Growth just short of the threshold, in one commit: still kept.
        let short = (prod_lines as f64 * (1.0 + crate::ratchet::KEPT_GROWTH)) as usize - 2;
        commit(short, "grown");
        assert_eq!(proposed(&[verdict("s")]), 0);
        // Past it: proposed again.
        commit(short + 2, "grown more");
        assert_eq!(proposed(&[verdict("s")]), 1);
        // Back to its size: the 5 commits since reach the 5 that made it a candidate.
        let back = (0..3).map(|i| "y".repeat(10 + i));
        for note in back {
            commit(120, &note);
        }
        let mut fewer = verdict("s");
        fewer.commits_total += 1;
        assert_eq!(proposed(&[fewer]), 0);
        assert_eq!(proposed(&[verdict("s")]), 1);
    }

    /// A trend is a task only past its noise over at least TREND_MIN
    /// landings: `test_seconds` past TIME_NOISE of the first, with the
    /// tests, slowest first (all of them when the run records no test's
    /// file); `uncovered_lines` past `coverage_slack`.
    #[test]
    fn a_trend_is_a_task_only_past_its_noise_over_enough_landings() {
        let landed = |values: &[(u64, f64)]| -> Vec<Landed> {
            let at = |(i, (u, t)): (usize, &(u64, f64))| Landed {
                commit: format!("c{i}"),
                date: String::new(),
                subject: String::new(),
                measured: [("s".to_string(), Measured { uncovered_lines: *u, test_seconds: Some(*t), untimed: None })]
                    .into(),
            };
            values.iter().enumerate().map(at).collect()
        };
        let s = suite([("fast", 0.1, vec![1]), ("slow", 0.9, vec![2])]);
        let s = SuiteMeasurement { reach: None, ..s };
        let check = crate::config::CheckConfig::default();
        let nowhere = Path::new("/nonexistent");
        let metrics = |values: &[(u64, f64)]| -> Vec<String> {
            let tasks = rising(nowhere, &check, &s, &landed(values));
            tasks.iter().map(|t| t.evidence["metric"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(metrics(&[(10, 1.0), (11, 1.2), (11, 1.6)]), Vec::<String>::new());
        assert_eq!(metrics(&[(10, 1.0), (11, 1.2), (12, 1.7)]), ["uncovered_lines", "test_seconds"]);
        assert_eq!(metrics(&[(10, 1.0), (12, 1.7)]), Vec::<String>::new());
        let time = &rising(nowhere, &check, &s, &landed(&[(1, 1.0), (1, 1.0), (1, 2.0)]))[0];
        assert_eq!(time.evidence["tests"][0]["test"], "t::slow");
        assert_eq!(time.evidence["landings"][2]["commit"], "c2");
        // Where the run records each test's file, only the tests in the files the landings changed.
        let known = suite([("fast", 0.1, vec![1]), ("slow", 0.9, vec![2])]);
        let added: crate::landing::Lines = [("tests/fast.rs".to_string(), [(1, "x".to_string())].into())].into();
        let (tests, files) = grown_tests(&known, &added);
        assert_eq!((tests, files), (json!([{ "test": "t::fast", "seconds": 0.1 }]), vec!["tests/fast.rs".to_string()]));
    }

    /// A failing test is a task, with the log to read; a passing one is not.
    #[test]
    fn a_failing_test_is_a_task_with_its_log() {
        let mut s = suite([("ok", 0.1, vec![1]), ("broken", 0.1, vec![2])]);
        s.tests[1].passed = false;
        let tasks = failing(&s);
        assert_eq!(tasks.len(), 1);
        assert_eq!((tasks[0].kind, tasks[0].summary.as_str()), (TaskKind::FailingTest, "t::broken fails"));
        assert_eq!(tasks[0].evidence["log"], ".slimfit/out/s/test.log");
    }

    /// Each proposed test with the other test it names, task by task.
    fn pairs(s: &SuiteMeasurement, kept: &[Kept]) -> Vec<(String, String)> {
        let tasks = redundant(s, kept);
        let candidates = tasks.iter().flat_map(|t| t.evidence["candidates"].as_array().unwrap().clone());
        let name = |c: &Value, k: &str| c[k].as_str().unwrap().to_string();
        candidates.map(|c| (name(&c, "test"), name(&c, "other_test"))).collect()
    }

    /// A task is a test file with every candidate in it, slowest first, and
    /// the files whose candidates would save most come first: three in one
    /// file and one in another make two tasks, the first holding three.
    #[test]
    fn a_redundant_test_task_is_a_file_of_candidates() {
        let tests = [
            ("one", 0.1, 1..=10),
            ("one_again", 0.3, 1..=10),
            ("two", 0.1, 20..=29),
            ("two_again", 0.4, 21..=30),
            ("three", 0.2, 40..=41),
            ("wide", 0.1, 40..=49),
            ("lone", 0.8, 60..=69),
            ("lone_again", 0.1, 60..=69),
        ];
        let mut s = suite(tests.map(|(n, secs, ls)| (n, secs, ls.collect())));
        for t in &mut s.reach.as_mut().unwrap().tests {
            let file = if t.name.starts_with("lone") || t.name == "wide" { "tests/b.rs" } else { "tests/a.rs" };
            t.source.as_mut().unwrap().file = file.into();
        }
        let tasks = redundant(&s, &[]);
        let files: Vec<_> = tasks.iter().map(|t| (t.evidence["file"].clone(), t.evidence["seconds"].clone())).collect();
        assert_eq!(files, [(json!("tests/a.rs"), json!(0.9)), (json!("tests/b.rs"), json!(0.8))]);
        let first = &tasks[0].evidence["candidates"];
        let got: Vec<_> = first.as_array().unwrap().iter().map(|c| (c["test"].clone(), c["kind"].clone())).collect();
        let want = [("two_again", "overlap"), ("one_again", "subset"), ("three", "subset")];
        assert_eq!(got, want.map(|(t, k)| (json!(format!("t::{t}")), json!(k))));
        assert_eq!((first[0]["share"].clone(), first[0]["other_test"].clone()), (json!(0.82), json!("t::two")));
        assert_eq!(tasks[0].files, ["tests/a.rs", "tests/b.rs"]);
        assert_eq!(candidates(&tasks), 4);
        // A kept test leaves its file's task; a file with none left has none.
        let keep = |test: &str| Kept { suite: "s".into(), test: test.into(), hash: "h".into(), reason: "r".into() };
        let left = redundant(&s, &[keep("t::lone")]);
        assert_eq!((left.len(), candidates(&left)), (1, 3));
        assert_eq!(candidates(&redundant(&s, &[keep("t::two_again")])), 3);
    }

    /// In each language with redundant-test tasks: three tests of one shape,
    /// differing in literals, make one group, the fastest (no candidate, the
    /// others' other test) a row too; a subset whose assertion its other test
    /// makes, on the same setup, is deleted; one whose assertion differs, or
    /// whose assertion is the other's on another setup, or that asserts nothing, is read;
    /// a comment is no statement.
    #[test]
    fn candidates_are_classed_by_their_tests_shapes() {
        let rust = [
            "fn start_gmail() {\n    let d = start(\"gmail\", 7);\n    assert_eq!(d.sources(), 1);\n}",
            "fn start_gh() {\n    let d = start(\"gh\", 7);\n    assert_eq!(d.sources(), 1);\n}",
            "fn start_none() {\n    let d = start(\"\", 7);\n    assert_eq!(d.sources(), 0);\n}",
            "fn copy() {\n    // the sum\n    let x = add(2, 3);\n    assert_eq!(x, 5);\n}",
            "fn orig() {\n    let x = add(2, 3);\n    assert!(x > 0);\n    assert_eq!(x, 5);\n}",
            "fn differ() {\n    let _y = 0;\n    assert_eq!(add(1, 1), 3);\n}",
            "fn base() {\n    assert_eq!(add(1, 1), 2);\n}",
            "fn on_itself() {\n    let h = hub(\"itself\");\n    assert!(!h.behind());\n}",
            "fn on_unheard() {\n    let h = hub(\"unheard\");\n    assert!(!h.behind());\n    assert!(h.silent());\n}",
            "fn quiet() {\n    let x = add(2, 3);\n}",
            "fn loud() {\n    let x = add(2, 3);\n    assert_eq!(x, 5);\n}",
        ];
        let go = [
            "func TestStartGmail(t *testing.T) {\n\tif Start(\"gmail\", 7).Sources() != 1 {\n\t\tt.Fatal(\"gmail\")\n\t}\n}",
            "func TestStartGh(t *testing.T) {\n\tif Start(\"gh\", 7).Sources() != 1 {\n\t\tt.Fatal(\"gh\")\n\t}\n}",
            "func TestStartNone(t *testing.T) {\n\tif Start(\"\", 7).Sources() != 0 {\n\t\tt.Fatal(\"none\")\n\t}\n}",
            "func TestCopy(t *testing.T) {\n\t// the sum\n\tx := Add(2, 3)\n\tif x != 5 {\n\t\tt.Fatal(\"2+3\")\n\t}\n}",
            "func TestOrig(t *testing.T) {\n\tx := Add(2, 3)\n\tif x <= 0 {\n\t\tt.Fatal(\"positive\")\n\t}\n\tif x != 5 {\n\t\tt.Fatal(\"2+3\")\n\t}\n}",
            "func TestDiffer(t *testing.T) {\n\t_ = 0\n\tif Add(1, 1) != 3 {\n\t\tt.Fatal(\"1+1\")\n\t}\n}",
            "func TestBase(t *testing.T) {\n\tif Add(1, 1) != 2 {\n\t\tt.Fatal(\"1+1\")\n\t}\n}",
            "func TestOnItself(t *testing.T) {\n\th := Hub(\"itself\")\n\tif h.Behind() {\n\t\tt.Fatal(\"behind\")\n\t}\n}",
            "func TestOnUnheard(t *testing.T) {\n\th := Hub(\"unheard\")\n\tif h.Behind() {\n\t\tt.Fatal(\"behind\")\n\t}\n\tif !h.Silent() {\n\t\tt.Fatal(\"silent\")\n\t}\n}",
            "func TestQuiet(t *testing.T) {\n\t_ = Add(2, 3)\n}",
            "func TestLoud(t *testing.T) {\n\t_ = Add(2, 3)\n\tif Add(2, 3) != 5 {\n\t\tt.Fatal(\"2+3\")\n\t}\n}",
        ];
        let tests = [
            ("gmail", 0.3, 1..=10),
            ("gh", 0.2, 1..=10),
            ("none", 0.1, 1..=10),
            ("copy", 0.2, 20..=25),
            ("orig", 0.1, 20..=29),
            ("differ", 0.2, 40..=42),
            ("base", 0.1, 40..=49),
            ("on_itself", 0.2, 60..=62),
            ("on_unheard", 0.1, 60..=69),
            ("quiet", 0.2, 80..=82),
            ("loud", 0.1, 80..=89),
        ];
        for (preset, texts) in [(crate::config::Preset::Rust, rust), (crate::config::Preset::Go, go)] {
            let mut s = suite(tests.clone().map(|(n, secs, ls)| (n, secs, ls.collect())));
            for (t, text) in s.reach.as_mut().unwrap().tests.iter_mut().zip(texts) {
                let source = t.source.as_mut().unwrap();
                (source.file, source.shape) = ("tests/a.rs".into(), crate::test_shape::shape(preset, text));
            }
            let task = &redundant(&s, &[])[0];
            let got: Vec<_> = task.evidence["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| (c["test"].as_str().unwrap().to_string(), c["class"].clone(), c["group"].clone()))
                .collect();
            let want = [
                ("gmail", json!("same-shape"), json!(0)),
                ("gh", json!("same-shape"), json!(0)),
                ("copy", json!("asserts-contained"), Value::Null),
                ("differ", json!("read"), Value::Null),
                ("on_itself", json!("read"), Value::Null),
                ("quiet", json!("read"), Value::Null),
            ];
            assert_eq!(got, want.map(|(t, c, g)| (format!("t::{t}"), c, g)), "{preset:?}");
            let rows = &task.evidence["groups"][0];
            let row = |i: usize| (rows[i]["test"].clone(), rows[i]["literals"].clone());
            let tables = (rows.as_array().unwrap().len(), row(0), row(2));
            let none = if preset == crate::config::Preset::Go {
                json!(["\"\"", "0", "\"none\""])
            } else {
                json!(["\"\"", "0"])
            };
            let gmail = if preset == crate::config::Preset::Go {
                json!(["\"gmail\"", "1", "\"gmail\""])
            } else {
                json!(["\"gmail\"", "1"])
            };
            assert_eq!(tables, (3, (json!("t::gmail"), gmail), (json!("t::none"), none)), "{preset:?}");
        }
    }

    /// A test whose lines another runs too is proposed (of two that run the
    /// same lines, the slower); a test overlapping another by at least
    /// MERGE_OVERLAP is proposed for a merge; one apart from the rest, or
    /// overlapping less, is not; without per-test coverage, none is.
    #[test]
    fn redundant_tests_are_proposed_from_per_test_lines() {
        let tests = [("wide", 0.1, 1..=10), ("inside", 0.3, 2..=4), ("twin", 0.2, 1..=10), ("near", 0.1, 20..=29)];
        let tests =
            tests.into_iter().chain([("near2", 0.5, 21..=30), ("apart", 0.1, 40..=45), ("loose", 0.1, 41..=50)]);
        let s = suite(tests.map(|(n, secs, ls)| (n, secs, ls.collect())));
        let got = |kept: &[Kept]| pairs(&s, kept);
        let want = [("near2", "near"), ("inside", "twin"), ("twin", "wide")];
        assert_eq!(got(&[]), want.map(|(a, b)| (format!("t::{a}"), format!("t::{b}"))));
        assert_eq!(redundant(&s, &[])[0].files, ["tests/near2.rs", "tests/near.rs"]);
        // Kept, it is not proposed again until its text changes.
        let keep = |suite: &str, hash: &str| Kept {
            suite: suite.into(),
            test: "t::near2".into(),
            hash: hash.into(),
            reason: "r".into(),
        };
        assert_eq!(got(&[keep("s", "h")]).len(), 2);
        assert_eq!(got(&[keep("s", "changed")]).len(), 3);
        assert_eq!(got(&[keep("other suite", "h")]).len(), 3);
        let functions_only = crate::model::Reach { lines: None, ..s.reach.clone().unwrap() };
        assert!(redundant(&SuiteMeasurement { reach: Some(functions_only), ..s }, &[]).is_empty());
    }

    /// The lines more than COMMON_LINES of the tests run (their shared
    /// setup) make no overlap: tests that share only them are not proposed;
    /// two that run the same lines past the setup are.
    #[test]
    fn lines_most_tests_run_are_left_out_of_overlap() {
        let with_setup = |own: std::ops::RangeInclusive<u32>| (100..=190).chain(own).collect::<Vec<_>>();
        let tests = [
            ("a", 0.1, with_setup(1..=5)),
            ("b", 0.1, with_setup(11..=15)),
            ("c", 0.1, with_setup(21..=25)),
            ("same", 0.2, with_setup(31..=35)),
            ("same2", 0.1, with_setup(31..=35)),
        ];
        let s = suite(tests);
        assert_eq!(pairs(&s, &[]), [("t::same".to_string(), "t::same2".to_string())]);
        assert_eq!(redundant(&s, &[])[0].evidence["candidates"][0]["lines"], 96);
    }

    /// Shared setup is each binary's: lines more than COMMON_LINES of its
    /// own tests run, though under half of the suite's do. A pair inside a
    /// binary that overlaps only in its setup is not proposed; one that
    /// overlaps past it is.
    #[test]
    fn each_binarys_own_setup_is_left_out_of_overlap() {
        let with =
            |setup: std::ops::RangeInclusive<u32>, own: std::ops::RangeInclusive<u32>| setup.chain(own).collect();
        let tests = [
            ("x::a", 0.2, with(100..=190, 1..=5)),
            ("x::b", 0.1, with(100..=190, 11..=15)),
            ("x::c", 0.1, with(100..=190, 21..=25)),
            ("y::near", 0.2, with(200..=290, 31..=40)),
            ("y::near2", 0.1, with(200..=290, 32..=41)),
            ("y::d", 0.1, with(200..=290, 51..=55)),
            ("y::g", 0.1, with(200..=290, 81..=85)),
            ("z::e", 0.1, with(0..=0, 61..=65)),
            ("z::f", 0.1, with(0..=0, 71..=75)),
        ];
        assert_eq!(pairs(&suite(tests), &[]), [("y::near".to_string(), "y::near2".to_string())]);
    }

    /// A test whose lines are all its binary's shared setup (a library both
    /// binaries read, such as a shipped file) is a subset of a test in
    /// another binary that reads it too, but coverage says nothing of what it
    /// checks: it is not proposed. One with own lines the other runs is.
    #[test]
    fn a_subset_of_shared_setup_alone_is_not_proposed() {
        let with = |setup: std::ops::RangeInclusive<u32>, own: std::ops::RangeInclusive<u32>| {
            setup.chain(own).collect::<Vec<u32>>()
        };
        let tests = [
            ("x::shipped", 0.2, (100..=110).collect()),
            ("x::shipped2", 0.1, (100..=110).collect()),
            ("x::own", 0.1, with(100..=110, 1..=5)),
            ("y::reads", 0.1, (200..=210).chain(with(100..=110, 1..=5)).collect()),
            ("y::b", 0.1, with(200..=210, 31..=35)),
            ("y::c", 0.1, with(200..=210, 41..=45)),
        ];
        assert_eq!(pairs(&suite(tests), &[]), [("x::own".to_string(), "y::reads".to_string())]);
    }
}
