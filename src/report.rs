//! Over git history: how fast each suite's prod and test code has been
//! growing, and what the agents' judgments add up to (ADR 0009, point 4):
//! the bounds the committed baseline held, the verdicts it kept and the
//! records of picked mutants, all read from git, with no test run.

use crate::config::Config;
use crate::git;
use crate::landing::{self, Record, Records};
use crate::loc::Classifier;
use crate::measure::sample;
use crate::model::{MutantStatus, Source};
use crate::ratchet;
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Point {
    pub commit: String,
    pub date: String,
    pub suites: BTreeMap<String, Lines>,
    /// The records `.slimfit/mutants/` held at the commit; absent when it
    /// held none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_records: Option<Records>,
    /// The survival of the mutants that ran, by who chose them, over the
    /// records and samples committed by then; absent when there were none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_survival: Option<BTreeMap<Source, Rate>>,
}

/// How the mutants one source chose came out: `ran` built and ran (killed
/// or survived), `survived` of those, `rate` = survived / ran (`None` when
/// none ran), and `unviable` (did not build, or no outcome).
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Rate {
    pub ran: u64,
    pub survived: u64,
    pub rate: Option<f64>,
    pub unviable: u64,
}

/// What a [`Rate`] prints, as the command pages name it.
pub const RATE_HELP: &str = "`ran` (built and ran: killed or survived), `survived` (of those), `rate` (survived / \
     ran, or null when none ran) and `unviable` (did not build, or reported no outcome)";

impl Rate {
    fn add(&mut self, outcome: Option<MutantStatus>) {
        match outcome {
            Some(MutantStatus::Killed) => self.ran += 1,
            Some(MutantStatus::Survived) => (self.ran, self.survived) = (self.ran + 1, self.survived + 1),
            Some(MutantStatus::Unviable) | None => self.unviable += 1,
        }
        self.rate = (self.ran > 0).then(|| self.survived as f64 / self.ran as f64);
    }
}

/// Survival by source over a commit's records (a second agent's picks, or
/// the author's, in records naming no other picker) and samples: each
/// source with a mutant that ran, or `None` when none did.
fn survival<'a>(files: impl Iterator<Item = (&'a str, &'a str)>) -> Option<BTreeMap<Source, Rate>> {
    let (records, samples) = (format!("{}/", landing::DIR), format!("{}/", sample::DIR));
    let mut out: BTreeMap<Source, Rate> = BTreeMap::new();
    for (p, text) in files.filter(|(p, _)| p.ends_with(".json")) {
        if p.starts_with(&records) {
            let Ok(r) = serde_json::from_str::<Record>(text) else { continue };
            let by = if r.picked_by_another() { Source::Picker } else { Source::Author };
            for pick in r.picks.values().flatten() {
                out.entry(by).or_default().add(pick.outcome);
            }
        } else if p.starts_with(&samples) {
            let Ok(s) = serde_json::from_str::<sample::Sample>(text) else { continue };
            for d in s.drawn.values().flatten() {
                out.entry(Source::Sampled).or_default().add(d.outcome);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A suite at a commit: its lines counted, and what the committed baseline
/// said of it. A number the baseline did not hold then is absent, not zero.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Lines {
    pub prod: u64,
    pub test: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncovered_lines: Option<u64>,
    /// Redundant-test verdicts kept (`slimfit keep`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept: Option<usize>,
    /// Deepen verdicts kept: files judged deep enough (`slimfit keep <file>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_files: Option<usize>,
}

/// What a history point's suite prints, as the command pages name it.
pub const LINES_HELP: &str = "`prod` and `test` lines, and what the baseline committed then held: `tests`, \
     `test_seconds` (its trend) and `uncovered_lines` (its bound) and `kept` (redundant-test verdicts kept) and `kept_files` (deepen verdicts kept)";

/// The baseline's numbers for `suite` in `baseline` (a commit's
/// `.slimfit/baseline.json`), read field by field: a baseline written by an
/// older slimfit lacks some, and those stay absent.
fn judged(baseline: Option<&Value>, suite: &str, lines: &mut Lines) {
    let Some(b) = baseline else { return };
    let m = &b["suites"][suite];
    lines.tests = m["tests"].as_u64();
    lines.test_seconds = m["test_seconds"].as_f64();
    lines.uncovered_lines = m["uncovered_lines"].as_u64();
    let count = |key: &str| b[key].as_array().map(|k| k.iter().filter(|k| k["suite"] == suite).count());
    lines.kept = count("kept");
    lines.kept_files = count("kept_files");
}

/// The records among a commit's files, or `None` when it holds none. One
/// that does not read as a record is left out: history is not the place to
/// refuse it.
fn records<'a>(files: impl Iterator<Item = (&'a str, &'a str)>) -> Option<Records> {
    let prefix = format!("{}/", landing::DIR);
    let mut n: Option<Records> = None;
    for (_, text) in files.filter(|(p, _)| p.starts_with(&prefix) && p.ends_with(".json")) {
        if let Ok(r) = serde_json::from_str::<Record>(text) {
            n.get_or_insert_default().add(&r);
        }
    }
    n
}

/// `points` commits spread evenly over the last `commits` first-parent
/// commits, oldest first.
pub fn history(repo: &Path, cfg: &Config, commits: usize, points: usize) -> Result<Vec<Point>> {
    let log =
        git::run(repo, &["log", "--first-parent", "--format=%H %cs", "-n", &commits.to_string()]).unwrap_or_default();
    let all: Vec<(&str, &str)> = log.lines().filter_map(|l| l.split_once(' ')).collect();
    if all.is_empty() || points == 0 {
        return Ok(vec![]);
    }
    let step = (all.len() as f64 / points as f64).max(1.0);
    let mut picked: Vec<(&str, &str)> =
        (0..points).map(|i| (i as f64 * step) as usize).filter(|i| *i < all.len()).map(|i| all[i]).collect();
    picked.dedup();
    picked.reverse();
    let resolved: Vec<_> = cfg.suites.iter().map(|s| s.resolve()).collect();
    // Which files a suite counts at all does not depend on the tree: these
    // classifiers, of no tree, pick the blobs to read.
    let claims: Vec<Classifier> =
        resolved.iter().map(|s| Classifier::new(s, repo, &[], |_| None)).collect::<Result<_>>()?;
    let mut out = Vec::new();
    for (commit, date) in picked {
        let ours = |p: &str| p == ratchet::FILE || p.starts_with(landing::DIR) || p.starts_with(sample::DIR);
        let blobs = git::tree_blobs(repo, commit, |p| ours(p) || claims.iter().any(|c| c.kind(p).is_some()))?;
        let texts: BTreeMap<&str, &str> = blobs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let baseline: Option<Value> = texts.get(ratchet::FILE).and_then(|t| serde_json::from_str(t).ok());
        let paths: Vec<String> = texts.keys().filter(|p| !ours(p)).map(|p| p.to_string()).collect();
        let mut suites = BTreeMap::new();
        let read = |p: &str| texts.get(p).map(|t| t.to_string());
        for s in &resolved {
            let t = Classifier::new(s, repo, &paths, read)?.count(&paths, read);
            let mut lines = Lines { prod: t.prod, test: t.test, ..Lines::default() };
            judged(baseline.as_ref(), &s.name, &mut lines);
            suites.insert(s.name.clone(), lines);
        }
        let mutants_records = records(texts.iter().map(|(p, t)| (*p, *t)));
        let mutants_survival = survival(texts.iter().map(|(p, t)| (*p, *t)));
        let commit = commit[..12].to_string();
        out.push(Point { commit, date: date.to_string(), suites, mutants_records, mutants_survival });
    }
    Ok(out)
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::measure::t::{git, sample_repo};

    /// Each point reads the baseline and records committed at it: a commit
    /// with no baseline shows none of its numbers, one written before a
    /// field shows that field absent, records count by kind, and the
    /// mutants that ran tally by who chose them.
    #[test]
    fn history_reads_what_each_commit_committed() {
        let (dir, cfg) = sample_repo();
        let repo = dir.path();
        let commit = |files: &[(&str, &str)]| {
            for (p, t) in files {
                let p = repo.join(p);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, t).unwrap();
            }
            git(repo, &["add", "-A"]);
            git(repo, &["commit", "-qm", "c"]);
        };
        commit(&[(ratchet::FILE, r#"{"suites":{"rust":{"tests":3,"uncovered_lines":9}}}"#)]);
        let picked = r#"{"picks":{"rust":[{"name":"m","why":"w","outcome":"killed"}]}}"#;
        let second = r#"{"picker":"p","author":"a","picks":{"rust":[{"name":"n","why":"w","outcome":"survived"}]}}"#;
        commit(&[
            (
                ratchet::FILE,
                r#"{"suites":{"rust":{"tests":2,"test_seconds":1.5,"uncovered_lines":4}},"kept":[{"suite":"rust"},{"suite":"go"}],"kept_files":[{"suite":"rust"},{"suite":"rust"}]}"#,
            ),
            (".slimfit/mutants/a.json", picked),
            (".slimfit/mutants/e.json", second),
            (".slimfit/mutants/b.json", r#"{"none":"docs only"}"#),
            (".slimfit/mutants/c.json", r#"{"tree":"t"}"#),
            (".slimfit/mutants/d.json", "not a record"),
            (
                ".slimfit/samples/t.json",
                r#"{"tip":"t","from":"f","pool":9,"drawn":{"rust":[{"name":"a","outcome":"survived"},{"name":"b","outcome":"killed"},{"name":"c","outcome":"unviable"}]}}"#,
            ),
        ]);
        let h = history(repo, &cfg, 10, 10).unwrap();
        let rust: Vec<&Lines> = h.iter().map(|p| &p.suites["rust"]).collect();
        let judged = |l: &Lines| (l.tests, l.test_seconds, l.uncovered_lines, l.kept, l.kept_files);
        assert_eq!(
            rust.iter().map(|l| judged(l)).collect::<Vec<_>>(),
            [
                (None, None, None, None, None),
                (Some(3), None, Some(9), None, None),
                (Some(2), Some(1.5), Some(4), Some(1), Some(2)),
            ]
        );
        assert!(rust.iter().all(|l| l.prod > 0), "{rust:?}");
        let records: Vec<_> = h.iter().map(|p| p.mutants_records.as_ref()).collect();
        assert_eq!(records, [None, None, Some(&Records { with_picks: 2, without_picks: 1, older: 1 })]);
        // Survival by who chose the mutants, side by side: a second agent's picks, the author's and the sample.
        let survival = serde_json::to_value(&h[2].mutants_survival).unwrap();
        let want = serde_json::json!({
            "picker": { "ran": 1, "survived": 1, "rate": 1.0, "unviable": 0 },
            "author": { "ran": 1, "survived": 0, "rate": 0.0, "unviable": 0 },
            "sampled": { "ran": 2, "survived": 1, "rate": 0.5, "unviable": 1 },
        });
        assert_eq!((survival, h[1].mutants_survival.is_none()), (want, true));
    }
}
