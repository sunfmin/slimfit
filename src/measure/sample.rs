//! An unbiased sample of the mutants of recently landed changes, out of the
//! loop (ADR 0011): the agent that makes a change picks its mutants, so the
//! picks may lean toward what its tests already hold. The sample is drawn at
//! random from the same changes, seeded by the tip it is drawn at, and run
//! as picks run; `report` sets its survival beside the picks'. No `check`
//! or `ratchet` reads it.

use super::mutants::{Listed, Stop, list_suite, mutate_picks, runs_alone, stops};
use super::{OUT, Resolver, Scratch};
use crate::config::Config;
use crate::git;
use crate::landing::{self, Record};
use crate::loc::Classifier;
use crate::model::*;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Where the samples are kept, one file per tip drawn at.
pub const DIR: &str = ".slimfit/samples";

/// How far back a first sample reaches, in days: later ones reach back to
/// the last sample's tip.
pub const SAMPLE_DAYS: u32 = 7;

/// How many mutants a sample of N draws, in N: those after the first N
/// replace, in the draw's order, the ones that do not build, all in one run.
pub const DRAWN: usize = 2;

/// What a sample costs, measured on slimfit itself (evidence: slimfit#60).
pub const COST: &str = "a sample of 5, drawn from 574 mutants of 7 days of changes, took 38.5s wall and 115s CPU (user + sys), its test run included, on a host at load 6-11: one cargo-mutants run of the 10 drawn built 8 (one did not build, 2 more than the sample kept) and skipped 2";

/// git's empty tree: where changes start in a repository younger than the window.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A sample: the tip it was drawn at (its seed), the changes it was drawn
/// from, how many mutants they held that no record picked, and each drawn
/// mutant's outcome, per suite.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    pub tip: String,
    pub from: String,
    pub pool: usize,
    #[serde(default)]
    pub drawn: BTreeMap<String, Vec<Drawn>>,
}

/// What a [`Sample`] prints, as the command pages name it.
pub const SAMPLE_HELP: &str = "`tip` (the commit it was drawn at, which seeds the draw), `from` (the commit the \
     changes it was drawn from start at), `pool` (the mutants of those changes no record picked) and `drawn` (per \
     suite, each drawn mutant's `name` and `outcome`)";

/// A drawn mutant and its outcome (`None`: the run reported none).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Drawn {
    pub name: String,
    pub outcome: Option<MutantStatus>,
}

impl Sample {
    /// Its path, named by its tip, and its text.
    pub fn file(&self) -> Result<(String, String)> {
        Ok((format!("{DIR}/{}.json", self.tip), serde_json::to_string_pretty(self)? + "\n"))
    }

    /// The drawn mutants by outcome.
    pub fn counts(&self) -> MutantCounts {
        let mut c = MutantCounts::default();
        for d in self.drawn.values().flatten() {
            match d.outcome {
                Some(MutantStatus::Killed) => c.killed += 1,
                Some(MutantStatus::Survived) => c.survived += 1,
                Some(MutantStatus::Unviable) | None => c.unviable += 1,
            }
        }
        c
    }
}

/// A sample, and the candidates of the mutants drawn: what the tasks need.
pub struct Sampled {
    pub sample: Sample,
    pub candidates: Vec<(String, Candidate)>,
}

/// A mutant's name without its line and column: what a pick is still
/// called after an edit above it moved it.
fn unplaced(name: &str) -> String {
    let Some((place, rest)) = name.split_once(": ") else { return name.to_string() };
    let file = place.rsplitn(3, ':').last().unwrap_or(place);
    format!("{file}: {rest}")
}

/// SplitMix64: a small generator whose sequence the seed alone fixes.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// The order mutants are drawn in from `pool` (suite, candidate) at `tip`:
/// those `picked` names (wherever an edit since moved them) left out, the
/// rest shuffled by a generator seeded by the tip alone. The same tip
/// always draws the same; nobody can draw again for a better result.
pub fn draw(pool: &[(String, Candidate)], picked: &BTreeSet<String>, tip: &str) -> Vec<(String, Candidate)> {
    let picked: BTreeSet<String> = picked.iter().map(|n| unplaced(n)).collect();
    let mut order: Vec<(String, Candidate)> =
        pool.iter().filter(|(_, c)| !picked.contains(&unplaced(&c.name))).cloned().collect();
    order.sort_by(|a, b| (&a.0, &a.1.name).cmp(&(&b.0, &b.1.name)));
    order.dedup_by(|a, b| a.0 == b.0 && a.1.name == b.1.name);
    let mut rng = SplitMix(u64::from_str_radix(&landing::hash(tip), 16).expect("a hash is hex"));
    for i in (1..order.len()).rev() {
        order.swap(i, (rng.next() % (i as u64 + 1)) as usize);
    }
    order
}

/// Where the changes a sample is drawn from start: the latest sample on
/// HEAD's history, or else the first-parent commit [`SAMPLE_DAYS`] back
/// (the empty tree in a younger repository); never before the baseline's
/// `mutants_from`, code from before picks.
fn from(repo: &Path, head: &str) -> Result<String> {
    let tips = on_disk(repo).into_iter().map(|s| s.tip).filter(|t| git::is_ancestor(repo, t, head));
    let last = tips.reduce(|a, b| if git::is_ancestor(repo, &a, &b) { b } else { a });
    let window = || {
        let before = format!("--before={SAMPLE_DAYS}.days.ago");
        let c = git::run(repo, &["rev-list", "-1", "--first-parent", &before, head]).unwrap_or_default();
        Some(c.trim().to_string()).filter(|c| !c.is_empty())
    };
    let start = last.or_else(window).unwrap_or_else(|| EMPTY_TREE.into());
    let cut = crate::ratchet::load(repo)?.mutants_from.filter(|c| git::is_ancestor(repo, c, head));
    let before_cut = |c: &String| start == EMPTY_TREE || git::is_ancestor(repo, &start, c);
    Ok(cut.filter(before_cut).unwrap_or(start))
}

/// The samples in `.slimfit/samples/`; one that does not read is left out.
fn on_disk(repo: &Path) -> Vec<Sample> {
    let Ok(dir) = std::fs::read_dir(repo.join(DIR)) else { return vec![] };
    dir.filter_map(|e| std::fs::read_to_string(e.ok()?.path()).ok())
        .filter_map(|t| serde_json::from_str(&t).ok())
        .collect()
}

/// The names the records added in `from` → `head` picked.
fn picked_since(repo: &Path, from: &str, head: &str) -> Result<BTreeSet<String>> {
    let added =
        git::run(repo, &["diff", "--name-only", "--diff-filter=A", "--no-renames", from, head, "--", landing::DIR])?;
    let read = |path: &str| git::run(repo, &["show", &format!("{head}:{path}")]).ok();
    let records = added.lines().filter(|p| p.ends_with(".json")).filter_map(read);
    let records = records.filter_map(|t| serde_json::from_str::<Record>(&t).ok());
    Ok(records.flat_map(|r| r.picks.into_values().flatten().map(|p| p.name)).collect())
}

/// Draws `n` viable mutants of what HEAD's history added since the last
/// sample (or [`SAMPLE_DAYS`]), none a record picked, and runs them as
/// picks run, each with its function's tests, on `m`, this tree's test run.
/// A drawn mutant that does not build is replaced by the next in the draw,
/// among `n` × [`DRAWN`] run at once. Cached in `.slimfit/out/` per tip and `n`.
pub fn sample(repo: &Path, cfg: &Config, m: &Measurement, n: usize) -> Result<Sampled> {
    let head = git::run(repo, &["rev-parse", "HEAD"])?.trim().to_string();
    let cache = repo.join(OUT).join("sample.json");
    let scratch = Scratch::new(repo, false)?;
    let from = from(repo, &head)?;
    eprintln!("slimfit: sampling {n} mutant(s) of the changes from {from} to {head}");
    // The tree the tool mutates: the working tree, as a change's listing takes it.
    let to = git::write_tree(repo, &git::head(repo)?)?;
    let added = landing::added_lines(&git::diff(repo, &from, &to, ".", &[])?, "");
    let mut listed: BTreeMap<String, Listed> = BTreeMap::new();
    for (name, lines) in landing::required(repo, cfg, &added)? {
        let out = scratch.0.join("sample");
        listed.insert(name.clone(), list_suite(repo, cfg, (&from, &to), &name, lines, &out)?);
    }
    // A mutant that cannot run without a field deletion beside it is not drawn.
    let alone = |l: &Listed| l.candidates.iter().filter(|c| runs_alone(l, c)).cloned().collect::<Vec<_>>();
    let pool: Vec<(String, Candidate)> =
        listed.iter().flat_map(|(s, l)| alone(l).into_iter().map(move |c| (s.clone(), c))).collect();
    let picked = picked_since(repo, &from, &head)?;
    let order = draw(&pool, &picked, &head);
    let cached: Option<(usize, Sample)> =
        std::fs::read_to_string(&cache).ok().and_then(|t| serde_json::from_str(&t).ok());
    let sample = match cached.filter(|(k, s)| *k == n && s.tip == head && s.from == from) {
        Some((_, s)) => s,
        None => {
            let s = run(
                repo,
                cfg,
                m,
                &listed,
                &order,
                n,
                Sample { tip: head, from, pool: order.len(), ..Default::default() },
            )?;
            std::fs::write(&cache, serde_json::to_string_pretty(&(n, &s))?)?;
            s
        }
    };
    let drawn: BTreeSet<(&str, &str)> =
        sample.drawn.iter().flat_map(|(s, d)| d.iter().map(move |d| (s.as_str(), d.name.as_str()))).collect();
    let candidates = order.iter().filter(|(s, c)| drawn.contains(&(s.as_str(), c.name.as_str()))).cloned().collect();
    Ok(Sampled { sample, candidates })
}

/// Whether a mutant's outcome says it built.
fn built(outcome: &Option<MutantStatus>) -> bool {
    matches!(outcome, Some(MutantStatus::Killed | MutantStatus::Survived))
}

/// Runs the first `n` × [`DRAWN`] of `order`, one run per suite, as picks
/// run, and keeps the first `n` in the order that built, with those before
/// them that did not. A run that can stop builds none once `n` drawn before
/// it have built; one that cannot (every test runs: no shim) holds only the
/// first `n`, none replaced.
fn run(
    repo: &Path,
    cfg: &Config,
    m: &Measurement,
    listed: &BTreeMap<String, Listed>,
    order: &[(String, Candidate)],
    n: usize,
    mut sample: Sample,
) -> Result<Sample> {
    let files = git::files(repo)?;
    let drawn = &order[..order.len().min(n * DRAWN)];
    let suites: BTreeSet<&String> = drawn.iter().map(|(s, _)| s).collect();
    let mut ran: BTreeMap<usize, Option<MutantStatus>> = BTreeMap::new();
    for s in suites {
        let suite = cfg.suites.iter().find(|x| &x.name == s).expect("a configured suite").resolve();
        let tested = m.suites.iter().find(|x| &x.suite == s).expect("a measured suite");
        let out = repo.join(OUT).join("sample").join(s);
        std::fs::create_dir_all(&out)?;
        let stop = stops(&suite, tested, &out);
        let (ranks, picks): (Vec<usize>, Vec<Pick>) = (drawn.iter().enumerate())
            .filter(|(r, (x, _))| x == s && (stop || *r < n))
            .map(|(r, (_, c))| (r, Pick { name: c.name.clone(), why: "sampled".into(), outcome: None }))
            .unzip();
        let known = ran.iter().filter(|(_, o)| built(o)).map(|(r, _)| *r).collect();
        let stop = stop.then(|| Stop { viable: n, ranks: ranks.clone(), known });
        let totals = Classifier::of_worktree(&suite, repo, &files)?
            .count(&files, |f| std::fs::read_to_string(repo.join(f)).ok());
        let root = repo.join(&suite.root);
        let resolver = Resolver::new(repo, &totals);
        let run = mutate_picks((&root, &out), &suite, &resolver, &listed[s], &picks, tested, stop.as_ref())?;
        ran.extend(ranks.into_iter().zip(run.mutated.picks.into_iter().map(|p| p.outcome)));
    }
    let mut viable = 0;
    for (r, outcome) in ran {
        if viable == n {
            break;
        }
        let (s, c) = &drawn[r];
        viable += usize::from(built(&outcome));
        sample.drawn.entry(s.clone()).or_default().push(Drawn { name: c.name.clone(), outcome });
    }
    Ok(sample)
}

#[cfg(test)]
mod t {
    use super::*;

    fn pool(n: usize) -> Vec<(String, Candidate)> {
        (0..n)
            .map(|i| {
                let name = format!("src/a.rs:{i}:5: replace f -> u8 with {i}");
                let c = Candidate {
                    name,
                    file: "src/a.rs".into(),
                    line: i as u32,
                    function: "f".into(),
                    replacement: i.to_string(),
                    ..Candidate::default()
                };
                ("rust".to_string(), c)
            })
            .collect()
    }

    fn names(order: &[(String, Candidate)], n: usize) -> Vec<String> {
        order.iter().take(n).map(|(_, c)| c.name.clone()).collect()
    }

    /// The draw is the tip's: the same tip draws the same mutants, whatever
    /// order they were listed in; another tip draws another set. A name a
    /// record picked is never drawn, though an edit above it has since moved it.
    #[test]
    fn the_tip_seeds_the_draw_and_picked_names_are_never_drawn() {
        let p = pool(40);
        let none = BTreeSet::new();
        let a = names(&draw(&p, &none, "aaaa"), 5);
        let mut reversed = p.clone();
        reversed.reverse();
        assert_eq!(names(&draw(&reversed, &none, "aaaa"), 5), a);
        let b = names(&draw(&p, &none, "bbbb"), 5);
        assert_ne!(a.iter().collect::<BTreeSet<_>>(), b.iter().collect::<BTreeSet<_>>(), "{a:?} {b:?}");
        // Every name drawn at "aaaa", picked: none of them is drawn, moved or not.
        let picked: BTreeSet<String> = a
            .iter()
            .enumerate()
            .map(|(i, n)| if i % 2 == 0 { n.clone() } else { n.replacen(":5:", ":9:", 1) })
            .collect();
        let order = draw(&p, &picked, "aaaa");
        assert_eq!(order.len(), 35);
        assert!(order.iter().all(|(_, c)| !a.contains(&c.name)), "{order:?}");
    }

    /// A sample counts what built apart from what did not, or said nothing.
    #[test]
    fn a_sample_counts_its_outcomes() {
        let d = |outcome| Drawn { name: String::new(), outcome };
        use MutantStatus::*;
        let drawn = vec![d(Some(Killed)), d(Some(Survived)), d(Some(Unviable)), d(None), d(Some(Killed))];
        let s = Sample { drawn: [("rust".to_string(), drawn)].into(), ..Default::default() };
        assert_eq!(s.counts(), MutantCounts { killed: 2, survived: 1, unviable: 2 });
    }

    /// A name without its place keeps its file and the change it makes.
    #[test]
    fn a_name_unplaced_keeps_its_file_and_change() {
        assert_eq!(unplaced("src/x:y.rs:12:9: replace >= with < in f"), "src/x:y.rs: replace >= with < in f");
        assert_eq!(unplaced("odd"), "odd");
    }
}
