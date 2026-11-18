//! Line counts over git history: how fast each suite's prod and test code
//! has been growing.

use crate::config::Config;
use crate::git;
use crate::loc::Classifier;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Point {
    pub commit: String,
    pub date: String,
    pub suites: BTreeMap<String, Lines>,
}

#[derive(Debug, Serialize)]
pub struct Lines {
    pub prod: u64,
    pub test: u64,
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
        cfg.suites.iter().map(|s| Ok((s.name.clone(), Classifier::new(&s.resolve())?))).collect::<Result<_>>()?;
    let mut out = Vec::new();
    for (commit, date) in picked {
        let blobs = git::tree_blobs(repo, commit, |p| classifiers.iter().any(|(_, c)| c.kind(p).is_some()))?;
        let texts: BTreeMap<&str, &str> = blobs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let paths: Vec<String> = texts.keys().map(|p| p.to_string()).collect();
        let mut suites = BTreeMap::new();
        for (name, c) in &classifiers {
            let t = c.count(&paths, |p| texts.get(p).map(|t| t.to_string()));
            suites.insert(name.clone(), Lines { prod: t.prod, test: t.test });
        }
        out.push(Point { commit: commit[..12].to_string(), date: date.to_string(), suites });
    }
    Ok(out)
}
