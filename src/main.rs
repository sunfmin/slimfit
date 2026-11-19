mod cli;
mod config;
mod doctor;
mod fault;
#[cfg(test)]
mod fixtures;
mod formats;
mod git;
mod landing;
mod loc;
mod measure;
mod model;
mod ratchet;
mod report;
mod select;
mod tasks;
mod tools;
mod workspace;
// build.rs runs it to stamp the version; compiled here too, for its tests and mutants.
#[cfg_attr(not(test), allow(dead_code))]
mod version;

use anyhow::Result;
use cli::{Cmd, EXIT_ERROR, EXIT_OK, EXIT_VIOLATION};
use serde_json::json;

agent_help::binary!("slimfit");

fn main() {
    // Run as `cargo-nextest` (first on PATH during a mutants run): the shim
    // that runs only a mutant's tests (ADR 0007).
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.first().and_then(|a| std::path::Path::new(a).file_name()).is_some_and(|n| n == select::SHIM) {
        select::shim(args);
    }
    let (answer, code) = match run(args) {
        Ok(done) => done,
        Err(e) => (json!({ "error": fault::of(e) }), EXIT_ERROR),
    };
    println!("{}", serde_json::to_string_pretty(&answer).expect("json"));
    std::process::exit(code);
}

/// The command line's answer and exit code (help and version print and exit
/// here, as clap does).
fn run(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<(serde_json::Value, i32)> {
    let (repo, command) = cli::read(args)?;
    let start = repo.unwrap_or(std::env::current_dir()?);
    let repo = git::toplevel(&start)?;
    match command {
        Cmd::Init { dry_run, .. } => {
            let mut writes = Writes { repo: &repo, dry_run, would: Default::default() };
            let created = !repo.join(config::FILE).exists();
            let cfg = if created {
                let suites = config::detect(&git::worktree(&repo)?.files);
                if suites.is_empty() {
                    return Err(fault::fault(
                        format!("no {} project found in {}", cli::languages("or"), repo.display()),
                        format!(
                            "write {} by hand: a `[[suite]]` with name, preset (the language nearest yours) and \
                             root, overriding test_command, tests_report and coverage_report; then `slimfit init`",
                            config::FILE
                        ),
                    ));
                }
                let text = toml::to_string_pretty(&config::Config { suites, check: Default::default() })?;
                writes.put(config::FILE, &text)?;
                config::parse(&repo, &text)
            } else {
                config::load(&repo)
            };
            writes
                .put(".gitignore", &gitignore(&std::fs::read_to_string(repo.join(".gitignore")).unwrap_or_default()))?;
            let (cfg, old) = both(cfg, ratchet::load(&repo))?;
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh: true, dry_run, timed: false },
            )?;
            let base = ratchet::Baseline {
                suites: m.suites.iter().map(|s| (s.suite.clone(), s.metrics.clone())).collect(),
                loosened: old.loosened,
                kept: old.kept,
                kept_files: old.kept_files,
                mutants_from: old.mutants_from,
            };
            writes.put(ratchet::FILE, &ratchet::render(&base)?)?;
            Ok((
                writes.answer(json!({
                    "config": if created { "written" } else { "kept" },
                    "suites": cfg.suites.iter().map(|s| s.resolve()).collect::<Vec<_>>(),
                    "baseline": base.suites,
                    "next": "commit slimfit.toml, .slimfit/baseline.json and .gitignore; then `slimfit next`",
                })),
                EXIT_OK,
            ))
        }
        Cmd::Next { all, kind, fresh, .. } => {
            let (cfg, base) = loaded(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh, dry_run: false, timed: false },
            )?;
            let mut list = tasks::all(&repo, &cfg, &m, &base)?;
            if let Some(k) = kind {
                list.retain(|t| t.kind == k);
            }
            let mut counts = std::collections::BTreeMap::new();
            for t in &list {
                *counts.entry(serde_json::to_value(t.kind)?.as_str().unwrap_or("").to_string()).or_insert(0) += 1;
            }
            let list = tasks::cap(list);
            let mut notes = Vec::new();
            if !m.with_mutants {
                notes.push(
                    "mutants not measured: surviving-mutant tasks need `slimfit check --mutants <name>...` of this \
                     tree first (your picks, at background priority); `slimfit next` then reads its survivors"
                        .to_string(),
                );
            }
            for s in m.suites.iter().filter(|s| s.reach.as_ref().is_none_or(|r| r.lines.is_none())) {
                notes.push(format!(
                    "{}: its test run records no coverage per test, so no redundant test can be proposed",
                    s.suite
                ));
            }
            let next = if list.is_empty() {
                if m.with_mutants {
                    "nothing left to do: the codebase is at its fittest by these measures"
                } else {
                    "nothing left to do but what mutants would show: they run on a change, picked (`slimfit check --mutants`)"
                }
            } else {
                "do the task, then run its accept command, then `slimfit ratchet` and commit"
            };
            Ok((
                if all {
                    json!({ "tasks": list, "counts": counts, "notes": notes, "next": next })
                } else {
                    json!({ "task": list.first(), "remaining": counts, "notes": notes, "next": next })
                },
                EXIT_OK,
            ))
        }
        Cmd::Check { mutants: Some(names), why, .. } if names.is_empty() => {
            let (cfg, _) = loaded(&repo)?;
            if !why.is_empty() {
                return Err(fault::fault(
                    "--why without a mutant's name",
                    "give each pick's name to `--mutants`, in the order of `--why`",
                ));
            }
            measure::refuse_unpickable(&cfg)?;
            let candidates = measure::candidates(&repo, &cfg)?;
            let next = if candidates.values().all(Vec::is_empty) {
                "no mutant of the change to pick: when it adds source lines, `slimfit ratchet --no-mutants \"<why>\"`, \
                 then commit .slimfit/"
            } else {
                "read your change and pick at most 5 of `candidates` on the decisions it makes (comparisons, boolean \
                 logic, match guards, a return value that matters), none that cannot build; run them at background \
                 priority: `factorium worker exclusive --background -- slimfit check --mutants <name>... --why \
                 <why>...` (outside factorium, `nice -n 19 slimfit check ...`), then `slimfit ratchet`; or, when none \
                 is worth running, `slimfit ratchet --no-mutants \"<why>\"`"
            };
            Ok((json!({ "candidates": candidates, "next": next }), EXIT_OK))
        }
        Cmd::Check { mutants, why, fresh, .. } => {
            let (cfg, base) = loaded(&repo)?;
            let picks = picks(mutants.unwrap_or_default(), why)?;
            let with_mutants = !picks.is_empty();
            let scope = if with_mutants { measure::Mutants::Picks(picks) } else { measure::Mutants::None };
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: scope, fresh, dry_run: false, timed: !with_mutants },
            )?;
            let mut v = ratchet::check(&base, &m, &cfg.check);
            let (landing, unrecorded) = landing::hold(&repo, &cfg, m.with_mutants)?;
            v.violations.extend(unrecorded);
            v.ok = v.violations.is_empty();
            let next = if !v.ok {
                "fix each violation as its `fix` says, then `slimfit check` again"
            } else if !v.improved.is_empty() || !v.new_suites.is_empty() || with_mutants {
                "slimfit ratchet, then commit"
            } else {
                "slimfit next"
            };
            let code = if v.ok { EXIT_OK } else { EXIT_VIOLATION };
            Ok((
                json!({ "verdict": v, "landing": landing, "metrics": metrics_of(&m), "mutated": mutated_of(&m), "next": next }),
                code,
            ))
        }
        Cmd::Ratchet { loosen, reason, fresh, dry_run, no_mutants, .. } => {
            let (cfg, old) = loaded(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh, dry_run, timed: false },
            )?;
            let new = ratchet::ratchet(&old, &m, &cfg.check, &loosen, reason.as_deref())?;
            let record = record(&m, no_mutants)?.map(|r| r.file()).transpose()?;
            let mut writes = Writes { repo: &repo, dry_run, would: Default::default() };
            let mut changed = false;
            // Nothing to ratchet is an answer, not a failure.
            if new != old {
                writes.put(ratchet::FILE, &ratchet::render(&new)?)?;
                changed = true;
            }
            if let Some((path, text)) = &record
                && !repo.join(path).exists()
            {
                writes.put(path, text)?;
                changed = true;
            }
            let next = if changed {
                "commit .slimfit/ with the change it records; then `slimfit next`"
            } else {
                "nothing to ratchet: the baseline already holds this measurement; `slimfit next`"
            };
            Ok((
                writes.answer(json!({
                    "changed": changed,
                    "baseline": new.suites,
                    "loosened": new.loosened,
                    "record": record.map(|(path, _)| path),
                    "next": next,
                })),
                EXIT_OK,
            ))
        }
        Cmd::Keep { test, reason, .. } => {
            let (cfg, mut base) = loaded(&repo)?;
            let Some(reason) = reason.filter(|r| !r.trim().is_empty()) else {
                return Err(fault::fault(
                    "keep without --reason",
                    "add `--reason \"<what the test checks that the other does not, or why the module is deep enough>\"`: \
                     it is kept in the baseline",
                ));
            };
            let answer = |base: &ratchet::Baseline| {
                json!({
                    "kept": base.kept,
                    "kept_files": base.kept_files,
                    "next": "commit .slimfit/baseline.json; then `slimfit next`",
                })
            };
            if let Some((suite, prod_lines)) = tasks::source_suite(&repo, &cfg, &test)? {
                let (commits_90d, commits_total) = tasks::churn(&repo, &test);
                base.kept_files.retain(|k| !(k.suite == suite && k.file == test));
                base.kept_files.push(ratchet::KeptFile {
                    suite,
                    file: test,
                    prod_lines,
                    commits_90d,
                    commits_total,
                    reason,
                });
                std::fs::write(repo.join(ratchet::FILE), ratchet::render(&base)?)?;
                return Ok((answer(&base), EXIT_OK));
            }
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh: false, dry_run: false, timed: false },
            )?;
            let found = m.suites.iter().find_map(|s| {
                let t = s.reach.as_ref()?.tests.iter().find(|t| t.id() == test)?;
                Some((s.suite.clone(), t.source.clone()))
            });
            let Some((suite, source)) = found else {
                return Err(fault::fault(
                    format!("no suite's test run recorded a test {test}, and no suite counts it as a source file"),
                    "name it as the redundant-test task's `test` or the deepen task's `file` does: \
                     `slimfit next --all --kind redundant-test` (or `--kind deepen`) lists them",
                ));
            };
            let Some(source) = source else {
                return Err(fault::fault(
                    format!("{test}'s own function was not found, so there is no text to hold the verdict to"),
                    "delete or merge it instead, as the task's steps say",
                ));
            };
            base.kept.retain(|k| !(k.suite == suite && k.test == test));
            base.kept.push(ratchet::Kept { suite, test, hash: source.hash, reason });
            std::fs::write(repo.join(ratchet::FILE), ratchet::render(&base)?)?;
            Ok((answer(&base), EXIT_OK))
        }
        Cmd::Doctor { .. } => {
            let host = doctor::Host::current();
            let findings = doctor::diagnose(&repo, &host);
            let next = findings.first().map_or("slimfit next".to_string(), |f| f.fix.clone());
            Ok((json!({ "findings": findings, "host": host, "next": next }), EXIT_OK))
        }
        Cmd::Help { .. } => unreachable!("`cli::read` answers it"),
        Cmd::Report { commits, points, fresh, .. } => {
            let (cfg, base) = loaded(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh, dry_run: false, timed: false },
            )?;
            let history = report::history(&repo, &cfg, commits, points)?;
            let records = landing::records(&repo)?;
            Ok((
                json!({
                    "current": metrics_of(&m),
                    "baseline": base.suites,
                    "loosened": base.loosened,
                    "history": history,
                    "mutants_records": records,
                    "next": "slimfit next",
                }),
                EXIT_OK,
            ))
        }
    }
}

/// slimfit.toml and the baseline, or the faults of both.
fn loaded(repo: &std::path::Path) -> Result<(config::Config, ratchet::Baseline)> {
    both(config::load(repo), ratchet::load(repo))
}

/// Both, or the faults of both.
fn both(c: Result<config::Config>, b: Result<ratchet::Baseline>) -> Result<(config::Config, ratchet::Baseline)> {
    match (c, b) {
        (Ok(c), Ok(b)) => Ok((c, b)),
        (c, b) => Err(fault::Faults(c.err().into_iter().chain(b.err()).flat_map(fault::of).collect()).into()),
    }
}

/// The picks `check --mutants <name>... --why <why>...` names: at most
/// `PICKS_MAX`, each with its own one-line reason.
fn picks(names: Vec<String>, why: Vec<String>) -> Result<Vec<model::Pick>> {
    let mut faults = Vec::new();
    if names.len() > measure::PICKS_MAX {
        faults.push(fault::Fault {
            fault: format!("{} mutants picked, more than {}", names.len(), measure::PICKS_MAX),
            fix: format!(
                "pick at most {}: those on the decisions the change makes (comparisons, boolean logic, match \
                 guards, a return value that matters)",
                measure::PICKS_MAX
            ),
        });
    }
    if names.len() != why.len() || why.iter().any(|w| w.trim().is_empty() || w.contains('\n')) {
        faults.push(fault::Fault {
            fault: format!("{} mutant(s) picked, with {} `--why`", names.len(), why.len()),
            fix: "give each pick one `--why` of one line, in the order of the names".into(),
        });
    }
    if !faults.is_empty() {
        return Err(fault::Faults(faults).into());
    }
    Ok(names.into_iter().zip(why).map(|(name, why)| model::Pick { name, why, outcome: None }).collect())
}

/// The record `ratchet` writes of `m`: its picks, or, with `--no-mutants`,
/// why none was picked; `None` when there is neither.
fn record(m: &model::Measurement, no_mutants: Option<String>) -> Result<Option<landing::Record>> {
    let picks: std::collections::BTreeMap<String, Vec<model::Pick>> = m
        .suites
        .iter()
        .filter_map(|s| Some((s.suite.clone(), s.mutated.as_ref()?.picks.clone())))
        .filter(|(_, p)| !p.is_empty())
        .collect();
    let none = match no_mutants {
        Some(_) if !picks.is_empty() => {
            return Err(fault::fault(
                "--no-mutants, but this tree's measurement ran picked mutants",
                "leave out `--no-mutants`: the record is of those picks (or `--fresh` to measure without them)",
            ));
        }
        Some(why) if why.trim().is_empty() || why.contains('\n') => {
            return Err(fault::fault(
                "--no-mutants without a reason",
                "say in one line why nothing in the change is worth a mutant",
            ));
        }
        none => none,
    };
    if picks.is_empty() && none.is_none() {
        return Ok(None);
    }
    Ok(Some(landing::Record { tree: m.tree.clone(), picks, none }))
}

fn metrics_of(m: &model::Measurement) -> serde_json::Value {
    m.suites.iter().map(|s| (s.suite.clone(), json!(s.metrics))).collect::<serde_json::Map<_, _>>().into()
}

/// Per suite, what the mutation run covered and found in the change.
fn mutated_of(m: &model::Measurement) -> serde_json::Value {
    if !m.with_mutants {
        return serde_json::Value::Null;
    }
    m.suites.iter().map(|s| (s.suite.clone(), json!(s.mutated))).collect::<serde_json::Map<_, _>>().into()
}

/// The files a command writes, by path in the repo: written at once, or in
/// a dry run only collected, those that would change, for its answer.
struct Writes<'a> {
    repo: &'a std::path::Path,
    dry_run: bool,
    would: serde_json::Map<String, serde_json::Value>,
}

impl Writes<'_> {
    fn put(&mut self, path: &str, text: &str) -> Result<()> {
        let file = self.repo.join(path);
        if std::fs::read_to_string(&file).is_ok_and(|old| old == text) {
            return Ok(());
        }
        if self.dry_run {
            self.would.insert(path.into(), text.into());
        } else {
            std::fs::create_dir_all(file.parent().expect("a path in the repo"))?;
            std::fs::write(file, text)?;
        }
        Ok(())
    }

    /// The command's answer; in a dry run with `would_write`, and `next`
    /// says to run it for real.
    fn answer(self, mut v: serde_json::Value) -> serde_json::Value {
        if self.dry_run {
            v["would_write"] = self.would.into();
            v["next"] = "if `would_write` is what you want, run the same command without --dry-run".into();
        }
        v
    }
}

/// `.gitignore`'s text with the output directory ignored: added once, on a
/// line of its own; an existing `/.../` entry counts.
fn gitignore(text: &str) -> String {
    if text.lines().any(|l| l.trim() == measure::OUT || l.trim() == format!("/{}/", measure::OUT)) {
        return text.into();
    }
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    format!("{text}{sep}{}\n", measure::OUT)
}

#[cfg(test)]
mod gitignore_tests {
    use super::{gitignore, measure::OUT};

    /// The output directory is ignored once: added after any text (on its own
    /// line), never twice, and an existing `/.../` entry counts.
    #[test]
    fn gitignore_adds_the_output_dir_once() {
        assert_eq!(gitignore(""), format!("{OUT}\n"));
        assert_eq!(gitignore("target"), format!("target\n{OUT}\n"));
        assert_eq!(gitignore("target\n"), format!("target\n{OUT}\n"));
        for kept in [format!("a\n {OUT} \nb"), format!("/{OUT}/\nb\n"), format!("b\n/{OUT}/")] {
            assert_eq!(gitignore(&kept), kept);
        }
    }
}

#[cfg(test)]
mod answer_tests {
    use super::{fault::Fault, run};
    use crate::measure::t::{git, sample_repo};
    use serde_json::Value;
    use std::path::Path;

    fn slimfit(repo: &Path, args: &str) -> anyhow::Result<(Value, i32)> {
        let mut all: Vec<std::ffi::OsString> = vec!["slimfit".into()];
        all.extend(args.split_whitespace().map(Into::into));
        all.extend(["--repo".into(), repo.into()]);
        run(all)
    }

    fn faults(repo: &Path, args: &str) -> Vec<Fault> {
        crate::fault::of(slimfit(repo, args).unwrap_err())
    }

    /// Each fault of a command line, a place and a repo carries its fix;
    /// slimfit.toml's and the baseline's are named together.
    #[test]
    fn a_failure_names_every_fault_with_its_fix() {
        let gone = faults(Path::new("/nonexistent"), "next");
        assert_eq!(gone[0].fault, "/nonexistent is not a directory");
        assert!(gone[0].fix.starts_with("run slimfit inside the repository, or pass `--repo <its path>`"));
        let dir = tempfile::tempdir().unwrap();
        let f = faults(dir.path(), "next");
        assert!(f[0].fault.contains("is not inside a git repository") && f[0].fix == gone[0].fix, "{f:?}");
        let f = faults(dir.path(), "nxt");
        assert_eq!(f[0].fault, "unrecognized subcommand 'nxt'");
        assert!(f[0].fix.starts_with("correct the command line (tip: some similar subcommands exist: 'init', 'next')"));

        git(dir.path(), &["init", "-q"]);
        let f = faults(dir.path(), "init");
        assert!(f[0].fault.starts_with("no Rust, Go, Swift, Kotlin or TypeScript project found"), "{f:?}");
        assert!(f[0].fix.starts_with("write slimfit.toml by hand: a `[[suite]]` with name, preset"), "{f:?}");
        std::fs::create_dir(dir.path().join(".slimfit")).unwrap();
        std::fs::write(dir.path().join(".slimfit/baseline.json"), "[]").unwrap();
        let f: Vec<String> = faults(dir.path(), "check").into_iter().map(|f| f.fix).collect();
        assert_eq!(f.len(), 2, "{f:?}");
        assert!(f[0].starts_with("`slimfit init` writes it") && f[1].starts_with("restore it: `git checkout"));
    }

    /// `slimfit` with `args` as given, spaces in them kept.
    fn slimfit_args(repo: &Path, args: &[&str]) -> anyhow::Result<(Value, i32)> {
        let mut all: Vec<std::ffi::OsString> = vec!["slimfit".into()];
        all.extend(args.iter().map(Into::into));
        all.extend(["--repo".into(), repo.into()]);
        run(all)
    }

    /// The gate's plain `check` holds a landing that adds source lines to a
    /// record of its mutants (ADR 0008): it fails while the change adds
    /// none, and passes once it adds one, of picks or of a reason none is
    /// worth one, whatever lines it covers. `next` asks for the picks first.
    /// Two branches that each recorded their own change merge without a
    /// conflict, and `report` counts their records.
    #[test]
    fn a_landing_passes_only_with_a_record_of_its_mutants() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        cfg.suites[0].overrides.mutants_tests = Some(crate::config::MutantTests::All);
        cfg.check.target = Some("target".into());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(repo.join(".gitignore"), ".slimfit/out\n").unwrap();
        slimfit(repo, "ratchet").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "adopt"]);
        git(repo, &["branch", "target"]);
        let held = |want: i32| {
            let (out, code) = slimfit(repo, "check").unwrap();
            assert_eq!(code, want, "{out:#}");
            out
        };
        let add = |file: &str, text: &str| {
            let p = repo.join(file);
            let old = std::fs::read_to_string(&p).unwrap_or_default();
            std::fs::write(p, old + text).unwrap();
        };
        let commit = |branch: &str| {
            git(repo, &["add", "-A"]);
            git(repo, &["commit", "-qm", branch]);
        };
        let killed = "src/lib.rs:12:9: replace >= with < in is_adult";

        git(repo, &["checkout", "-qb", "a"]);
        // Nothing added: nothing to pick, and no record due.
        let (listed, _) = slimfit(repo, "check --mutants").unwrap();
        assert_eq!(listed["candidates"]["rust"], serde_json::json!([]), "{listed:#}");
        assert!(listed["next"].as_str().unwrap().starts_with("no mutant of the change to pick"), "{listed:#}");
        add("src/lib.rs", "\npub fn a_one() -> u8 {\n    1\n}\n");
        let out = held(crate::EXIT_VIOLATION);
        let v = &out["verdict"]["violations"][0];
        assert_eq!((v["metric"].as_str(), v["current"].as_f64()), (Some("unrecorded_mutants"), Some(3.0)), "{out:#}");
        assert!(v["fix"].as_str().unwrap().contains("slimfit check --mutants <name>... --why <why>..."), "{v:#}");
        assert_eq!((out["landing"]["lines"].as_u64(), &out["landing"]["records"]), (Some(3), &serde_json::json!([])));
        // `next` asks for the picks first, with the candidates to pick from.
        let (next, _) = slimfit(repo, "next").unwrap();
        assert_eq!(next["task"]["kind"], "pick-mutants", "{next:#}");
        assert_eq!(next["task"]["evidence"]["candidates"].as_array().unwrap().len(), 17, "{next:#}");
        let (listed, code) = slimfit(repo, "check --mutants").unwrap();
        assert_eq!((listed["candidates"]["rust"][16]["name"].as_str(), code), (Some(killed), 0));
        // Committing it changes nothing: the change is the branch's, not the working tree's.
        git(repo, &["commit", "-qam", "a, unrecorded"]);
        held(crate::EXIT_VIOLATION);
        // Too many picks, or a pick without its reason: refused.
        let six = format!("check --mutants {} --why {}", ["n"; 6].join(" "), ["w"; 6].join(" "));
        assert!(faults(repo, &six)[0].fault.contains("6 mutants picked, more than 5"));
        assert!(faults(repo, "check --mutants n m --why w")[0].fault.contains("2 mutant(s) picked, with 1 `--why`"));
        assert_eq!(faults(repo, "check --mutants --why w")[0].fault, "--why without a mutant's name");
        let (out, code) = slimfit_args(repo, &["check", "--mutants", killed, "--why", "the age bound"]).unwrap();
        assert_eq!((code, out["mutated"]["rust"]["picks"][0]["outcome"].as_str()), (0, Some("killed")), "{out:#}");
        let no = crate::fault::of(slimfit_args(repo, &["ratchet", "--no-mutants", "nothing"]).unwrap_err());
        assert!(no[0].fault.contains("--no-mutants, but this tree's measurement ran picked mutants"), "{no:?}");
        let (out, _) = slimfit(repo, "ratchet").unwrap();
        let record = out["record"].as_str().unwrap().to_string();
        assert!(record.starts_with(".slimfit/mutants/"), "{out:#}");
        let text = std::fs::read_to_string(repo.join(&record)).unwrap();
        assert!(text.contains("\"why\": \"the age bound\"") && text.contains("\"outcome\": \"killed\""), "{text}");
        commit("a");
        assert_eq!(held(crate::EXIT_OK)["landing"]["records"], serde_json::json!([record]));
        // Which lines a record covers is not held: an edit after it passes.
        let text = std::fs::read_to_string(repo.join("src/lib.rs")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), text.replace("    1\n}\n", "    2\n}\n")).unwrap();
        held(crate::EXIT_OK);
        git(repo, &["checkout", "--", "src/lib.rs"]);

        // Another branch records that nothing in its change is worth a mutant.
        git(repo, &["checkout", "-qb", "b", "target"]);
        add("src/b.rs", "pub fn b_two() -> u8 {\n    2\n}\n");
        held(crate::EXIT_VIOLATION);
        assert!(faults(repo, "ratchet --no-mutants")[0].fault.contains("a value is required"));
        let blank = crate::fault::of(slimfit_args(repo, &["ratchet", "--no-mutants", " "]).unwrap_err());
        assert_eq!(blank[0].fault, "--no-mutants without a reason");
        let (out, _) = slimfit_args(repo, &["ratchet", "--no-mutants", "a constant: no decision to mutate"]).unwrap();
        let none = std::fs::read_to_string(repo.join(out["record"].as_str().unwrap())).unwrap();
        assert!(none.contains("\"none\": \"a constant: no decision to mutate\"") && !none.contains("picks"), "{none}");
        commit("b");
        held(crate::EXIT_OK);
        git(repo, &["checkout", "-q", "a"]);
        git(repo, &["merge", "-q", "--no-edit", "b"]);
        let out = held(crate::EXIT_OK);
        assert_eq!(
            (out["landing"]["lines"].as_u64(), out["landing"]["records"].as_array().unwrap().len()),
            (Some(6), 2)
        );
        // A per-line record from before picks counts as older.
        std::fs::write(repo.join(crate::landing::DIR).join("old.json"), r#"{"lines": {}}"#).unwrap();
        let (report, _) = slimfit(repo, "report --points 1").unwrap();
        let counts = serde_json::json!({ "with_picks": 1, "without_picks": 1, "older": 1 });
        assert_eq!(report["mutants_records"], counts, "{report:#}");
        // A record that does not parse, and a target that is not there, are refused.
        std::fs::write(repo.join(crate::landing::DIR).join("x.json"), "{").unwrap();
        let f = faults(repo, "report");
        assert!(f[0].fault.contains("x.json is not a record of picked mutants"), "{f:?}");
        assert_eq!(f[0].fix, "restore it from git: `git checkout -- .slimfit/mutants`");
        std::fs::remove_file(repo.join(crate::landing::DIR).join("x.json")).unwrap();
        std::fs::remove_file(repo.join(crate::landing::DIR).join("old.json")).unwrap();
        cfg.check.target = Some("gone".into());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        let f = faults(repo, "check");
        assert!(f[0].fault.starts_with("slimfit.toml's [check] target gone shares no commit with HEAD"), "{f:?}");
    }

    /// Code from before the rule is not charged (slimfit#37): the baseline's
    /// `mutants_from` moves where a landing starts while the target is
    /// behind it, and a line added after it is held as ever. A cut that is
    /// not on HEAD's history counts for nothing.
    #[test]
    fn code_from_before_the_rule_is_not_charged() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        cfg.check.target = Some("target".into());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        slimfit(repo, "ratchet").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "adopt"]);
        git(repo, &["branch", "target"]);
        let add = |file: &str, text: &str| {
            let p = repo.join(file);
            std::fs::write(&p, std::fs::read_to_string(&p).unwrap() + text).unwrap();
        };
        let cut = |at: &str| {
            let p = repo.join(crate::ratchet::FILE);
            let mut b: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            b["mutants_from"] = at.into();
            std::fs::write(p, b.to_string()).unwrap();
        };
        let out = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(["-c", "user.email=t@slimfit", "-c", "user.name=t", "-C"])
                .arg(repo)
                .args(args)
                .output()
                .unwrap();
            String::from_utf8(o.stdout).unwrap().trim().to_string()
        };
        let held = |want: i32| {
            let (out, code) = slimfit(repo, "check").unwrap();
            assert_eq!(code, want, "{out:#}");
            (out["landing"]["from"].as_str().unwrap().to_string(), out["landing"]["lines"].as_u64().unwrap())
        };
        git(repo, &["checkout", "-qb", "a"]);
        add("src/lib.rs", "\npub fn before_the_rule() -> u8 {\n    1\n}\n");
        git(repo, &["commit", "-qam", "before the rule"]);
        let before = out(&["rev-parse", "HEAD"]);
        cut(&before);
        let (from, n) = held(crate::EXIT_OK);
        assert_eq!((from.as_str(), n), (before.as_str(), 0));
        // A ratchet keeps the cut.
        slimfit(repo, "ratchet").unwrap();
        assert_eq!(crate::ratchet::load(repo).unwrap().mutants_from.as_deref(), Some(before.as_str()));
        // A line added after the cut is the change's, with no record: check fails.
        add("src/lib.rs", "\npub fn after_the_rule() -> u8 {\n    2\n}\n");
        assert_eq!(held(crate::EXIT_VIOLATION), (before.clone(), 3));
        // A cut not on HEAD's history (or no commit at all) moves nothing.
        let target = out(&["rev-parse", "target"]);
        for elsewhere in [out(&["commit-tree", "-m", "x", "-p", "target", "target^{tree}"]), "0".repeat(40)] {
            cut(&elsewhere);
            assert_eq!(held(crate::EXIT_VIOLATION), (target.clone(), 6));
        }
    }

    /// No task left of a kind, and nothing to ratchet, are answers: exit 0.
    #[test]
    fn nothing_to_do_is_an_answer() {
        let (dir, cfg) = sample_repo();
        std::fs::write(dir.path().join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        let (first, code) = slimfit(dir.path(), "ratchet").unwrap();
        assert_eq!((first["changed"].as_bool(), code), (Some(true), 0));
        // No mutants ran, and none is said to be worth one: no record.
        assert_eq!(first["record"], Value::Null, "{first}");
        let (again, code) = slimfit(dir.path(), "ratchet").unwrap();
        assert_eq!((again["changed"].as_bool(), code), (Some(false), 0));
        assert!(again["next"].as_str().unwrap().starts_with("nothing to ratchet"));
        let (all, _) = slimfit(dir.path(), "next --all").unwrap();
        assert_eq!(all["counts"], serde_json::json!({ "slow-test": 1 }));
        assert!(all["notes"][0].as_str().unwrap().starts_with("mutants not measured"), "{all}");
        let (held, code) = slimfit(dir.path(), "check").unwrap();
        assert_eq!((held["next"].as_str(), code), (Some("slimfit next"), 0));
        let (none, code) = slimfit(dir.path(), "next --kind deepen").unwrap();
        assert_eq!((none["task"].clone(), code), (Value::Null, 0));
    }
}
