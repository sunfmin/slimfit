//! Over git history: how fast each suite's prod and test code has been
//! growing, and what the agents' judgments add up to (ADR 0009, point 4):
//! the bounds the committed baseline held, the verdicts it kept and the
//! records of picked mutants, all read from git, with no test run.

use crate::config::Config;
use crate::git;
use crate::landing::{self, Record, Records};
use crate::loc::Classifier;
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
     `test_seconds` and `uncovered_lines` (its bounds) and `kept` (redundant-test verdicts kept) and `kept_files` (deepen verdicts kept)";

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
    let classifiers: Vec<(String, Classifier)> =
        cfg.suites.iter().map(|s| Ok((s.name.clone(), Classifier::new(&s.resolve(), repo)?))).collect::<Result<_>>()?;
    let mut out = Vec::new();
    for (commit, date) in picked {
        let ours = |p: &str| p == ratchet::FILE || p.starts_with(landing::DIR);
        let blobs = git::tree_blobs(repo, commit, |p| ours(p) || classifiers.iter().any(|(_, c)| c.kind(p).is_some()))?;
        let texts: BTreeMap<&str, &str> = blobs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let baseline: Option<Value> = texts.get(ratchet::FILE).and_then(|t| serde_json::from_str(t).ok());
        let paths: Vec<String> = texts.keys().filter(|p| !ours(p)).map(|p| p.to_string()).collect();
        let mut suites = BTreeMap::new();
        for (name, c) in &classifiers {
            let t = c.count(&paths, |p| texts.get(p).map(|t| t.to_string()));
            let mut lines = Lines { prod: t.prod, test: t.test, ..Lines::default() };
            judged(baseline.as_ref(), name, &mut lines);
            suites.insert(name.clone(), lines);
        }
        let mutants_records = records(texts.iter().map(|(p, t)| (*p, *t)));
        out.push(Point { commit: commit[..12].to_string(), date: date.to_string(), suites, mutants_records });
    }
    Ok(out)
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::measure::t::{git, sample_repo};

    /// Each point reads the baseline and records committed at it: a commit
    /// with no baseline shows none of its numbers, one written before a
    /// field shows that field absent, and records count by kind.
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
        commit(&[
            (
                ratchet::FILE,
                r#"{"suites":{"rust":{"tests":2,"test_seconds":1.5,"uncovered_lines":4}},"kept":[{"suite":"rust"},{"suite":"go"}],"kept_files":[{"suite":"rust"},{"suite":"rust"}]}"#,
            ),
            (".slimfit/mutants/a.json", picked),
            (".slimfit/mutants/b.json", r#"{"none":"docs only"}"#),
            (".slimfit/mutants/c.json", r#"{"tree":"t"}"#),
            (".slimfit/mutants/d.json", "not a record"),
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
        assert_eq!(records, [None, None, Some(&Records { with_picks: 1, without_picks: 1, older: 1 })]);
    }
}
