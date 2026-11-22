//! Over git history: how fast each suite's prod and test code has been
//! growing, and what the agents' judgments add up to (ADR 0009, point 4):
//! the bounds the committed baseline held, the verdicts it kept and the
//! records of picked mutants, all read from git, with no test run.

use crate::config::{Config, Suite};
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
    /// Per directory `--by` matches, per suite, its lines at the commit;
    /// absent without `--by`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by: ByDir,
    /// The records `.slimfit/mutants/` held at the commit; absent when it
    /// held none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_records: Option<Records>,
    /// The growth records `.slimfit/growth/` held at the commit, by verdict;
    /// absent when it held none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub growth_records: Option<crate::growth::Records>,
    /// The survival of the mutants that ran, by who chose them, over the
    /// records and samples committed by then; absent when there were none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_survival: Option<BTreeMap<Source, Rate>>,
}

/// How the mutants one source chose came out: `ran` built and ran (killed
/// or survived), `survived` of those, `rate` = survived / ran (`None` when
/// none ran), and `unviable` (did not build, or no outcome); and on their
/// first run, before the change's author could answer a survivor:
/// `first_survived`, and `first_unknown` (picks in records from before the
/// first was kept).
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Rate {
    pub ran: u64,
    pub survived: u64,
    pub rate: Option<f64>,
    pub unviable: u64,
    pub first_survived: u64,
    pub first_unknown: u64,
    /// Sampled survivors the baseline then kept as equivalent: in no other count.
    pub equivalent: u64,
}

/// What a [`Rate`] prints, as the command pages name it.
pub const RATE_HELP: &str = "`ran` (built and ran: killed or survived), `survived` (of those), `rate` (survived / \
     ran, or null when none ran), `unviable` (did not build, or reported no outcome), `first_survived` (survived \
     their first run, before the change's author could answer them: one killed since counts here too, so it counts \
     the gaps the picks found) and `first_unknown` (picks in records from before the first run's outcome was kept; \
     a sample runs once, so its one run is its first) and `equivalent` (sampled survivors the baseline then kept as \
     equivalent with slimfit keep --mutant: counted in none of the above, so a kept survivor leaves the survivors and still shows)";

impl Rate {
    /// Counts a mutant by its last `outcome`, and by its `first` (`None`:
    /// not known).
    fn add(&mut self, outcome: Option<MutantStatus>, first: Option<MutantStatus>) {
        match first {
            Some(MutantStatus::Survived) => self.first_survived += 1,
            None => self.first_unknown += 1,
            Some(_) => {}
        }
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
/// source with a mutant that ran, or `None` when none did. A sampled
/// survivor `kept` (the baseline's, then) names is equivalent, counted apart.
fn survival<'a>(
    files: impl Iterator<Item = (&'a str, &'a str)>,
    kept: &[ratchet::KeptMutant],
) -> Option<BTreeMap<Source, Rate>> {
    let (records, samples) = (format!("{}/", landing::DIR), format!("{}/", sample::DIR));
    let mut out: BTreeMap<Source, Rate> = BTreeMap::new();
    for (p, text) in files.filter(|(p, _)| p.ends_with(".json")) {
        if p.starts_with(&records) {
            let Ok(r) = serde_json::from_str::<Record>(text) else { continue };
            let by = if r.picked_by_another() { Source::Picker } else { Source::Author };
            for pick in r.picks.values().flatten() {
                out.entry(by).or_default().add(pick.outcome, pick.first_outcome);
            }
        } else if p.starts_with(&samples) {
            let Ok(s) = serde_json::from_str::<sample::Sample>(text) else { continue };
            for (suite, d) in s.drawn.iter().flat_map(|(s, ds)| ds.iter().map(move |d| (s, d))) {
                let rate = out.entry(Source::Sampled).or_default();
                if d.outcome == Some(MutantStatus::Survived) && kept.iter().any(|k| k.names(suite, &d.name)) {
                    rate.equivalent += 1;
                    continue;
                }
                // A sample runs once: its one outcome is its first (none: not a survivor).
                rate.add(d.outcome, Some(d.outcome.unwrap_or(MutantStatus::Unviable)));
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
    /// Uncovered verdicts kept: spans judged worth no fast test (`slimfit keep <file>:<start>-<end>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_spans: Option<usize>,
    /// Surviving-mutant verdicts kept: sampled survivors judged equivalent (`slimfit keep --mutant`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_mutants: Option<usize>,
    /// What the measurement the baseline was written from measured: a rise
    /// the bounds above do not keep shows here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured: Option<ratchet::Measured>,
}

/// What a history point's suite prints, as the command pages name it.
pub const LINES_HELP: &str = "`prod` and `test` lines, and what the baseline committed then held: `tests`, \
     `test_seconds` and `uncovered_lines` (trends, ratcheted down) and `kept` (redundant-test verdicts kept), `kept_files` (deepen verdicts kept), `kept_spans` (uncovered verdicts kept), `kept_mutants` (equivalent mutants kept) and `measured` (what was measured when the baseline was written: `uncovered_lines` and `test_seconds`, which only it shows rising, or in place of `test_seconds` why none was measured, `untimed`; absent before baselines kept it)";

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
    lines.kept_spans = count("kept_spans");
    lines.kept_mutants = count("kept_mutants");
    lines.measured = serde_json::from_value(b["measured"][suite].clone()).ok();
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

/// A suite's lines as `prod_lines` and `test_lines` count them.
#[derive(Debug, Default, Clone, Copy, Serialize, PartialEq)]
pub struct DirLines {
    pub prod_lines: u64,
    pub test_lines: u64,
}

/// Per directory, per suite, its lines.
pub type ByDir = BTreeMap<String, BTreeMap<String, DirLines>>;

/// The directories `--by`'s globs name: each matched against a directory
/// of the repo, relative to its root, a `*` not crossing a `/`.
pub struct By(Vec<globset::GlobMatcher>);

impl By {
    pub fn new(globs: &[String]) -> Result<By> {
        let one = |g: &String| {
            let glob = globset::GlobBuilder::new(g.trim_end_matches('/')).literal_separator(true).build();
            glob.map(|g| g.compile_matcher()).map_err(|e| {
                crate::fault::fault(
                    format!("--by {g} is not a glob: {e}"),
                    "give a glob of directories relative to the repo's root, such as `--by 'crates/*'`",
                )
            })
        };
        Ok(By(globs.iter().map(one).collect::<Result<_>>()?))
    }
}

/// What counts a commit's lines: the suites resolved, and the files each
/// counts at all, which does not depend on the tree.
struct Counter {
    resolved: Vec<Suite>,
    claims: Vec<Classifier>,
}

/// slimfit's own files a commit holds, read for what it judged then.
fn ours(p: &str) -> bool {
    p == ratchet::FILE || p.starts_with(landing::DIR) || p.starts_with(sample::DIR) || p.starts_with(crate::growth::DIR)
}

impl Counter {
    fn new(repo: &Path, cfg: &Config) -> Result<Counter> {
        let resolved: Vec<Suite> = cfg.suites.iter().map(|s| s.resolve()).collect();
        // Classifiers of no tree: they pick the blobs to read.
        let claims = resolved.iter().map(|s| Classifier::new(s, repo, &[], |_| None)).collect::<Result<_>>()?;
        Ok(Counter { resolved, claims })
    }

    /// `commit`'s blobs a suite counts, and slimfit's own.
    fn blobs(&self, repo: &Path, commit: &str) -> Result<Vec<(String, String)>> {
        git::tree_blobs(repo, commit, |p| ours(p) || self.claims.iter().any(|c| c.kind(p).is_some()))
    }

    /// Each suite's lines in the tree `texts`, counted as `prod_lines` and
    /// `test_lines` are, and summed per directory `by` matches that holds
    /// the file: a file in nested matches counts in each.
    fn count(&self, repo: &Path, texts: &BTreeMap<&str, &str>, by: &By) -> Result<(BTreeMap<String, DirLines>, ByDir)> {
        let paths: Vec<String> = texts.keys().filter(|p| !ours(p)).map(|p| p.to_string()).collect();
        let read = |p: &str| texts.get(p).map(|t| t.to_string());
        let (mut suites, mut dirs) = (BTreeMap::new(), ByDir::new());
        for s in &self.resolved {
            let t = Classifier::new(s, repo, &paths, read)?.count(&paths, read);
            suites.insert(s.name.clone(), DirLines { prod_lines: t.prod, test_lines: t.test });
            for (file, (_, l)) in t.files.iter().filter(|_| !by.0.is_empty()) {
                let all = file.match_indices('/').map(|(i, _)| &file[..i]);
                for dir in all.filter(|d| by.0.iter().any(|g| g.is_match(d))) {
                    let e = dirs.entry(dir.to_string()).or_default().entry(s.name.clone()).or_default();
                    (e.prod_lines, e.test_lines) = (e.prod_lines + l.prod, e.test_lines + l.test);
                }
            }
        }
        Ok((suites, dirs))
    }
}

/// HEAD's lines, from git alone: per suite, and per directory `by` names.
pub fn lines(repo: &Path, cfg: &Config, by: &By) -> Result<(BTreeMap<String, DirLines>, ByDir)> {
    let counter = Counter::new(repo, cfg)?;
    let blobs = counter.blobs(repo, "HEAD")?;
    counter.count(repo, &blobs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect(), by)
}

/// `points` commits spread evenly over the last `commits` first-parent
/// commits, oldest first, with each suite's lines per directory `by` names.
pub fn history(repo: &Path, cfg: &Config, commits: usize, points: usize, by: &By) -> Result<Vec<Point>> {
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
    let counter = Counter::new(repo, cfg)?;
    let mut out = Vec::new();
    for (commit, date) in picked {
        let blobs = counter.blobs(repo, commit)?;
        let texts: BTreeMap<&str, &str> = blobs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let baseline: Option<Value> = texts.get(ratchet::FILE).and_then(|t| serde_json::from_str(t).ok());
        let (counted, by) = counter.count(repo, &texts, by)?;
        let mut suites = BTreeMap::new();
        for (name, t) in counted {
            let mut lines = Lines { prod: t.prod_lines, test: t.test_lines, ..Lines::default() };
            judged(baseline.as_ref(), &name, &mut lines);
            suites.insert(name, lines);
        }
        let mutants_records = records(texts.iter().map(|(p, t)| (*p, *t)));
        let kept: Vec<ratchet::KeptMutant> =
            baseline.as_ref().and_then(|b| serde_json::from_value(b["kept_mutants"].clone()).ok()).unwrap_or_default();
        let mutants_survival = survival(texts.iter().map(|(p, t)| (*p, *t)), &kept);
        let growth_records = crate::growth::Records::of(texts.iter().map(|(p, t)| (*p, *t)));
        let commit = commit[..12].to_string();
        out.push(Point {
            commit,
            date: date.to_string(),
            suites,
            by,
            mutants_records,
            growth_records,
            mutants_survival,
        });
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
        // A second agent's: one from before first outcomes were kept, one that survived its first run and was then killed.
        let second = r#"{"picker":"p","author":"a","picks":{"rust":[{"name":"n","why":"w","outcome":"survived"},{"name":"k","why":"w","outcome":"killed","first_outcome":"survived"}]}}"#;
        commit(&[
            (
                ratchet::FILE,
                r#"{"suites":{"rust":{"tests":2,"test_seconds":1.5,"uncovered_lines":4}},"kept":[{"suite":"rust"},{"suite":"go"}],"kept_files":[{"suite":"rust"},{"suite":"rust"}],"measured":{"rust":{"uncovered_lines":6,"test_seconds":2.0}}}"#,
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
        let h = history(repo, &cfg, 10, 10, &By::new(&[]).unwrap()).unwrap();
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
        // Beside the ratcheted numbers, what was measured: a rise shows there.
        let measured = Some(ratchet::Measured { uncovered_lines: 6, test_seconds: Some(2.0), untimed: None });
        assert_eq!(rust.iter().map(|l| l.measured.clone()).collect::<Vec<_>>(), [None, None, measured]);
        let records: Vec<_> = h.iter().map(|p| p.mutants_records.as_ref()).collect();
        assert_eq!(records, [None, None, Some(&Records { with_picks: 2, without_picks: 1, older: 1 })]);
        // Survival by who chose the mutants, side by side: a second agent's picks, the author's and the sample;
        // and on the first run, a survivor since killed counted, unknown in records from before it was kept.
        let survival = serde_json::to_value(&h[2].mutants_survival).unwrap();
        let want = serde_json::json!({
            "picker": { "ran": 2, "survived": 1, "rate": 0.5, "unviable": 0, "first_survived": 1, "first_unknown": 1, "equivalent": 0 },
            "author": { "ran": 1, "survived": 0, "rate": 0.0, "unviable": 0, "first_survived": 0, "first_unknown": 1, "equivalent": 0 },
            "sampled": { "ran": 2, "survived": 1, "rate": 0.5, "unviable": 1, "first_survived": 1, "first_unknown": 0, "equivalent": 0 },
        });
        assert_eq!((survival, h[1].mutants_survival.is_none()), (want, true));
        // The sample's survivor kept as equivalent, wherever it is now: counted apart, in no other count.
        let kept = r#"{"suites":{},"kept_mutants":[{"suite":"rust","mutant":"src/a.rs: replace < with <=","hash":"h","reason":"r"}]}"#;
        let drawn = r#"{"tip":"t","from":"f","pool":9,"drawn":{"rust":[{"name":"src/a.rs:9:5: replace < with <=","outcome":"survived"},{"name":"go.go:1:1: x","outcome":"survived"}]}}"#;
        commit(&[(ratchet::FILE, kept), (".slimfit/samples/t.json", drawn)]);
        let h = history(repo, &cfg, 10, 10, &By::new(&[]).unwrap()).unwrap();
        let sampled = serde_json::to_value(&h[3].mutants_survival.as_ref().unwrap()[&Source::Sampled]).unwrap();
        assert_eq!((&sampled["ran"], &sampled["survived"], &sampled["equivalent"]), (&1.into(), &1.into(), &1.into()));
        assert_eq!(h[3].suites["rust"].kept_mutants, Some(1));
    }
}

#[cfg(test)]
mod by {
    use super::*;
    use crate::measure::t::git;

    /// Lines per directory, from HEAD alone: one crate's counted by hand
    /// (blanks and `//` comments out, inline tests and `tests/` as test),
    /// the crates summing to the suite's, an uncommitted file not counted,
    /// and each history point split the same way.
    #[test]
    fn by_counts_each_directory_as_the_suite_does() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let files = [
            // 2 prod (`///`, `//` and the blank do not count), 4 test (the `#[cfg(test)]` block).
            (
                "crates/a/src/lib.rs",
                "/// a\npub fn a() {}\n\n// note\nfn b() {}\n#[cfg(test)]\nmod t {\n    fn c() {}\n}\n",
            ),
            // 2 test.
            ("crates/a/tests/it.rs", "#[test]\nfn it() {}\n"),
            ("crates/b/src/lib.rs", "fn b() {}\n"),
            ("crates/b/README.md", "not counted\n"),
        ];
        for (p, t) in files {
            std::fs::create_dir_all(repo.join(p).parent().unwrap()).unwrap();
            std::fs::write(repo.join(p), t).unwrap();
        }
        git(repo, &["init", "-q"]);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "c"]);
        std::fs::write(repo.join("crates/b/src/new.rs"), "fn uncommitted() {}\n").unwrap();
        let cfg: Config = toml::from_str("[[suite]]\nname = \"rust\"\npreset = \"rust\"\n").unwrap();
        let by = By::new(&["crates/*".to_string()]).unwrap();
        let (suites, dirs) = lines(repo, &cfg, &by).unwrap();
        let of = |d: &str| dirs[d]["rust"];
        assert_eq!(of("crates/a"), DirLines { prod_lines: 2, test_lines: 6 });
        assert_eq!(of("crates/b"), DirLines { prod_lines: 1, test_lines: 0 });
        // `*` does not cross a `/`: no key for a crate's subdirectories.
        assert_eq!(dirs.keys().collect::<Vec<_>>(), ["crates/a", "crates/b"]);
        assert_eq!(suites["rust"], DirLines { prod_lines: 3, test_lines: 6 });
        let h = history(repo, &cfg, 10, 10, &by).unwrap();
        assert_eq!(h[0].by, dirs);
        assert!(By::new(&["crates/[".to_string()]).is_err());
    }
}
