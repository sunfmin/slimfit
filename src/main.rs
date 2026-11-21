mod cli;
mod config;
mod doctor;
mod fault;
#[cfg(test)]
mod fixtures;
mod formats;
mod git;
mod go_source;
mod growth;
mod landing;
mod loc;
mod measure;
mod model;
mod ratchet;
mod report;
mod rust_source;
mod select;
mod session;
mod tasks;
mod tools;
mod workspace;
// build.rs runs it to stamp the version; compiled here too, for its tests and mutants.
#[cfg_attr(not(test), allow(dead_code))]
mod version;

use anyhow::Result;
use cli::{Cmd, EXIT_ERROR, EXIT_OK, EXIT_VIOLATION};
use serde_json::json;
use session::Session;

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
    let head = git::open(&start)?;
    let repo = head.top.clone();
    match command {
        Cmd::Init { dry_run, .. } => {
            let mut writes = Writes { repo: &repo, dry_run, would: Default::default() };
            let created = !repo.join(config::FILE).exists();
            let cfg = if created {
                let suites = config::detect(&git::worktree(&head)?.files);
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
            // Opened after the writes above: the tree is measured as it is now.
            let s = Session::of(&head, cfg)?;
            let m = s.measure(measure::Options { fresh: true, dry_run, ..Default::default() })?;
            let suites = m.suites.iter().map(|s| (s.suite.clone(), s.metrics.clone())).collect();
            let base = ratchet::Baseline::next(&s.base, suites, &m);
            writes.put(ratchet::FILE, &ratchet::render(&base)?)?;
            Ok((
                writes.answer(json!({
                    "config": if created { "written" } else { "kept" },
                    "suites": s.cfg.suites.iter().map(|s| s.resolve()).collect::<Vec<_>>(),
                    "baseline": base.suites,
                    "next": "commit slimfit.toml, .slimfit/baseline.json and .gitignore; then `slimfit next`",
                })),
                EXIT_OK,
            ))
        }
        Cmd::Next { all, kind, fresh, sample, .. } => {
            let s = Session::open(&repo)?;
            let (cfg, tree) = (&s.cfg, &s.tree);
            let m = s.measure(measure::Options { fresh, ..Default::default() })?;
            let mut list = tasks::all(&repo, tree, cfg, &m, &s.base)?;
            let mut notes = Vec::new();
            if let Some(n) = sample {
                measure::refuse_unpickable(cfg)?;
                let s = measure::sample::sample(&repo, tree, cfg, &m, n)?;
                let c = s.sample.counts();
                notes.push(format!(
                    "sampled at {} from the changes since {}: {} killed, {} survived, {} unviable, of {} mutants no \
                     record picked; out of the loop: no `check` reads it",
                    s.sample.tip, s.sample.from, c.killed, c.survived, c.unviable, s.sample.pool
                ));
                list.extend(tasks::sampled(&s)?);
                list.sort_by_key(|t| t.kind);
            }
            if let Some(k) = kind {
                list.retain(|t| t.kind == k);
            }
            let mut counts = std::collections::BTreeMap::new();
            for t in &list {
                *counts.entry(serde_json::to_value(t.kind)?.as_str().unwrap_or("").to_string()).or_insert(0) += 1;
            }
            let list = tasks::cap(list);
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
            let s = Session::open(&repo)?;
            if !why.is_empty() {
                return Err(fault::fault(
                    "--why without a mutant's name",
                    "give each pick's name to `--mutants`, in the order of `--why`",
                ));
            }
            measure::refuse_unpickable(&s.cfg)?;
            let candidates = measure::candidates(&repo, &s.cfg, &s.tree)?;
            let next = if candidates.values().all(Vec::is_empty) {
                "no mutant of the change to pick: when it adds source lines, a second agent still says why none is \
                 worth one: `slimfit ratchet --no-mutants \"<its why>\" --picker <its id> --author <yours>`, then \
                 commit .slimfit/"
            } else {
                "ask a second agent to pick, never yourself: give it the goal (`slimfit help picker`), the change's diff and \
                 `candidates`, nothing else; run its picks at background priority: `factorium worker exclusive \
                 --background -- slimfit check --mutants <name>... --why <why>...` (outside factorium, `nice -n 19 \
                 slimfit check ...`), then `slimfit ratchet --picker <its id> --author <yours>`; or, when it finds \
                 none worth running, `slimfit ratchet --no-mutants \"<its why>\" --picker <its id> --author <yours>`"
            };
            Ok((json!({ "candidates": candidates, "next": next }), EXIT_OK))
        }
        Cmd::Check { mutants, why, fresh, .. } => {
            let s = Session::open(&repo)?;
            let scope = measure::Mutants::of(mutants.unwrap_or_default(), why)?;
            let with_mutants = scope != measure::Mutants::None;
            let m =
                s.measure(measure::Options { mutants: scope, fresh, timed: !with_mutants, ..Default::default() })?;
            if m.with_mutants {
                landing::remember_firsts(&repo, &m)?;
            }
            let landing = landing::hold(&repo, &s.cfg, &s.tree, m.with_mutants)?;
            let v = ratchet::check(&s.base, &m, &s.cfg.check, landing.as_ref());
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
        Cmd::Ratchet { loosen, reason, fresh, dry_run, no_mutants, picker, author, growth, .. } => {
            let s = Session::open(&repo)?;
            let old = &s.base;
            // Timed as a gate times it: a test run inside `--mutants` takes the
            // time of one of the same files, or runs again (slimfit#74).
            let m = s.measure(measure::Options { fresh, dry_run, timed: true, ..Default::default() })?;
            let new = ratchet::ratchet(old, &m, &s.cfg.check, &loosen, reason.as_deref())?;
            let grown = if growth.serves.is_some() {
                landing::hold(&repo, &s.cfg, &s.tree, false)?.map_or(0, |h| h.growth)
            } else {
                0
            };
            let growth_record = growth.record(&m.tree, grown, author.as_deref())?;
            let growth_record = growth_record.map(|r| landing::file_in(growth::DIR, &r)).transpose()?;
            let author = author.filter(|_| growth_record.is_none() || picker.is_some() || no_mutants.is_some());
            let record = landing::Record::of(&m, no_mutants, picker, author)?;
            let record = record.map(|r| landing::file_in(landing::DIR, &r.with_firsts(&repo))).transpose()?;
            let mut writes = Writes { repo: &repo, dry_run, would: Default::default() };
            let mut changed = false;
            // Nothing to ratchet is an answer, not a failure.
            if new != *old {
                writes.put(ratchet::FILE, &ratchet::render(&new)?)?;
                changed = true;
            }
            if let Some((path, text)) = &record
                && !repo.join(path).exists()
            {
                writes.put(path, text)?;
                if !dry_run {
                    landing::forget_firsts(&repo)?;
                }
                changed = true;
            }
            if let Some((path, text)) = &growth_record
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
                    "growth_record": growth_record.map(|(path, _)| path),
                    "next": next,
                })),
                EXIT_OK,
            ))
        }
        Cmd::Keep { test, reason, .. } => {
            let mut s = Session::open(&repo)?;
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
            if let Some((suite, prod_lines)) = tasks::source_suite(&s.tree, &s.cfg, &test)? {
                let (commits_90d, commits_total) = tasks::churn(&repo, &test);
                s.base.kept_files.retain(|k| !(k.suite == suite && k.file == test));
                s.base.kept_files.push(ratchet::KeptFile {
                    suite,
                    file: test,
                    prod_lines,
                    commits_90d,
                    commits_total,
                    reason,
                });
                std::fs::write(repo.join(ratchet::FILE), ratchet::render(&s.base)?)?;
                return Ok((answer(&s.base), EXIT_OK));
            }
            let m = s.measure(Default::default())?;
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
            s.base.kept.retain(|k| !(k.suite == suite && k.test == test));
            s.base.kept.push(ratchet::Kept { suite, test, hash: source.hash, reason });
            std::fs::write(repo.join(ratchet::FILE), ratchet::render(&s.base)?)?;
            Ok((answer(&s.base), EXIT_OK))
        }
        Cmd::Doctor { .. } => {
            let host = doctor::Host::current();
            let findings = doctor::diagnose(&repo, &host);
            let next = findings.first().map_or("slimfit next".to_string(), |f| f.fix.clone());
            Ok((json!({ "findings": findings, "host": host, "next": next }), EXIT_OK))
        }
        Cmd::Help { .. } => unreachable!("`cli::read` answers it"),
        Cmd::Report { commits, points, fresh, by, lines_only, .. } => {
            let s = Session::open(&repo)?;
            let globs = report::By::new(&by)?;
            let (lines, by_dir) = report::lines(&repo, &s.cfg, &globs)?;
            let current = match lines_only {
                true => json!(lines),
                false => metrics_of(&s.measure(measure::Options { fresh, ..Default::default() })?),
            };
            let history = report::history(&repo, &s.cfg, commits, points, &globs)?;
            let records = landing::records(&repo)?;
            let growth_records = growth::Records::read(&repo)?;
            let mut answer = json!({
                "current": current,
                "baseline": s.base.suites,
                "loosened": s.base.loosened,
                "history": history,
                "mutants_records": records,
                "growth_records": growth_records,
                "next": "slimfit next",
            });
            if !by.is_empty() {
                answer["by"] = json!(by_dir);
            }
            Ok((answer, EXIT_OK))
        }
    }
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
    use crate::growth;
    use crate::measure::t::{git, sample_repo};
    use crate::model::MutantStatus;
    use serde_json::{Value, json};
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
        // `init` records no mutants: it takes no `--no-mutants`.
        let f = faults(dir.path(), "init --no-mutants why");
        assert!(f[0].fault.contains("unexpected argument '--no-mutants'"), "{f:?}");
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
        // A pick that survives fails as `survived_mutants`, whoever picked.
        let survivor = "src/lib.rs:12:5: replace is_adult -> bool with true";
        let (out, code) =
            slimfit_args(repo, &["check", "--mutants", killed, survivor, "--why", "the age bound", "w"]).unwrap();
        let outcomes: Vec<&str> =
            out["mutated"]["rust"]["picks"].as_array().unwrap().iter().filter_map(|p| p["outcome"].as_str()).collect();
        let metrics: Vec<&str> =
            out["verdict"]["violations"].as_array().unwrap().iter().filter_map(|v| v["metric"].as_str()).collect();
        assert_eq!((code, outcomes, metrics), (1, vec!["killed", "survived"], vec!["survived_mutants"]), "{out:#}");
        let loose = "ratchet --loosen survived_mutants --reason r";
        let no = faults(repo, &format!("{loose} --no-mutants nothing"));
        assert!(no[0].fault.contains("--no-mutants, but this tree's measurement ran picked mutants"), "{no:?}");
        // The author does not pick (slimfit#58): a record without a picker, or with the author as its picker, is refused.
        for (args, fault) in [
            ("", "without `--picker` and `--author`"),
            (" --picker me --author me", "--picker is the change's author"),
            (" --picker p", "without `--picker` and `--author`"),
        ] {
            let f = faults(repo, &format!("{loose}{args}"));
            assert!(f[0].fault.contains(fault) && f[0].fix.contains("`slimfit help picker`"), "{args}: {f:?}");
        }
        assert!(!repo.join(crate::landing::DIR).exists(), "a refused ratchet writes no record");
        let (out, _) = slimfit(repo, &format!("{loose} --picker p --author a")).unwrap();
        let record = out["record"].as_str().unwrap().to_string();
        assert!(record.starts_with(".slimfit/mutants/"), "{out:#}");
        let text = std::fs::read_to_string(repo.join(&record)).unwrap();
        assert!(text.contains("\"why\": \"the age bound\"") && text.contains("\"outcome\": \"killed\""), "{text}");
        assert!(text.contains("\"picker\": \"p\"") && text.contains("\"author\": \"a\""), "{text}");
        // A record with a picker other than its author passes the gate.
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
        // Nor does a record that names no picker other than its author (as those from before slimfit#58).
        std::fs::create_dir_all(repo.join(crate::landing::DIR)).unwrap();
        let unnamed = repo.join(crate::landing::DIR).join("unnamed.json");
        for text in [r#"{"none":"mine"}"#, r#"{"none":"mine","picker":"a","author":"a"}"#, "{"] {
            std::fs::write(&unnamed, text).unwrap();
            git(repo, &["add", "-A"]);
            let fix = held(crate::EXIT_VIOLATION)["verdict"]["violations"][0]["fix"].clone();
            assert!(fix.as_str().unwrap().contains("no record of its mutants that names a picker other"), "{fix}");
        }
        git(repo, &["rm", "-qf", &unnamed.to_string_lossy()]);
        // Either flag with no record to write is refused.
        assert!(faults(repo, "ratchet --picker p --author a")[0].fault.contains("no record to name them in"));
        assert!(faults(repo, "ratchet --no-mutants")[0].fault.contains("a value is required"));
        let blank = crate::fault::of(slimfit_args(repo, &["ratchet", "--no-mutants", " "]).unwrap_err());
        assert_eq!(blank[0].fault, "--no-mutants without a reason");
        let no_mutants =
            ["ratchet", "--no-mutants", "a constant: no decision to mutate", "--picker", "p", "--author", "b"];
        assert!(faults(repo, "ratchet --no-mutants x")[0].fault.contains("without `--picker` and `--author`"));
        let (out, _) = slimfit_args(repo, &no_mutants).unwrap();
        let none = std::fs::read_to_string(repo.join(out["record"].as_str().unwrap())).unwrap();
        assert!(none.contains("\"none\": \"a constant: no decision to mutate\"") && !none.contains("picks"), "{none}");
        assert!(none.contains("\"picker\": \"p\""), "{none}");
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

    /// A change past `growth_allowance` net fails every `check` as
    /// `unrecorded_growth` until it adds a growth record that names a
    /// reviewer other than its author (ADR 0012); one at or under it needs
    /// none, and code it deletes pays for what it adds. `next` asks for the
    /// review; `report` counts the records.
    #[test]
    fn growth_past_the_allowance_passes_only_with_a_reviewed_record() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        cfg.check.target = Some("target".into());
        cfg.check.growth_allowance = 4;
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(repo.join(".gitignore"), ".slimfit/out\n").unwrap();
        slimfit(repo, "ratchet").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "adopt"]);
        git(repo, &["branch", "target"]);
        git(repo, &["checkout", "-qb", "grow"]);
        let lib = repo.join("src/lib.rs");
        let original = std::fs::read_to_string(&lib).unwrap();
        let growth = || {
            let (out, _) = slimfit(repo, "check").unwrap();
            let metrics: Vec<String> = out["verdict"]["violations"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v["metric"].as_str().map(String::from))
                .collect();
            (out["landing"]["growth"].as_i64().unwrap(), metrics.contains(&"unrecorded_growth".into()), out)
        };
        std::fs::write(&lib, original.clone() + "\npub fn one() -> u8 {\n    1\n}\n// c\n").unwrap();
        let (lines, due, _) = growth();
        assert_eq!((lines, due), (3, false), "under the allowance: no record due");
        std::fs::write(&lib, original.clone() + "\npub fn one() -> u8 {\n    1\n}\npub fn two() -> u8 {\n    2\n}\n")
            .unwrap();
        let (lines, due, out) = growth();
        assert!(lines == 6 && due, "{out:#}");
        let v = out["verdict"]["violations"].as_array().unwrap().iter().find(|v| v["metric"] == "unrecorded_growth");
        let v = v.unwrap();
        assert_eq!(
            (v["suite"].as_str(), v["limit"].as_f64(), v["current"].as_f64()),
            (Some("rust"), Some(4.0), Some(6.0))
        );
        assert!(v["fix"].as_str().unwrap().contains("`slimfit help reviewer`"), "{v:#}");
        // Code deleted in the same change pays for what it adds.
        let without_unused = original.replace("pub fn unused(x: i64) -> i64 {\n    let doubled = x * 2;\n", "");
        std::fs::write(
            &lib,
            without_unused.clone() + "pub fn one() -> u8 {\n    1\n}\npub fn two() -> u8 {\n    2\n}\n",
        )
        .unwrap();
        assert_eq!((growth().0, growth().1), (4, false));
        std::fs::write(&lib, original.clone() + "\npub fn one() -> u8 {\n    1\n}\npub fn two() -> u8 {\n    2\n}\n")
            .unwrap();
        let (next, _) = slimfit(repo, "next --kind justify-growth").unwrap();
        assert_eq!(
            (next["task"]["evidence"]["lines"].as_i64(), &next["task"]["files"]),
            (Some(6), &json!(["src/lib.rs"]))
        );
        // The author does not judge its own growth, and a record says what it serves and what was weighed.
        let full = ["--serves", "#75", "--considered", "add() does not do it", "--reviewer", "r", "--author", "a"];
        let refused =
            |args: &[&str]| crate::fault::of(slimfit_args(repo, &[&["ratchet"][..], args].concat()).unwrap_err());
        let f = refused(&["--as-is", "x", "--author", "a"]);
        assert!(f[0].fix.contains("--serves <ISSUE>") && f[0].fix.contains("--reviewer <ID>"), "{f:?}");
        assert!(refused(&[&full[..], &["--cut", "two()"]].concat())[0].fix.contains("--answer <WHAT>"));
        assert!(refused(&[&full[..6], &["--as-is", "x"]].concat())[0].fault.contains("without `--author`"));
        let blank = [&["--serves", " "], &full[2..], &["--as-is", "x"]].concat();
        assert!(refused(&blank)[0].fault.contains("a field that is not one line"));
        let mine = [&full[..6], &["--author", "r", "--as-is", "x"]].concat();
        assert!(refused(&mine)[0].fault.contains("--reviewer is the change's author"));
        assert!(!repo.join(growth::DIR).exists(), "a refused ratchet writes no record");
        let (out, _) = slimfit_args(repo, &[&["ratchet"][..], &full, &["--as-is", "each fn is new"]].concat()).unwrap();
        let record = out["growth_record"].as_str().unwrap();
        let text = std::fs::read_to_string(repo.join(record)).unwrap();
        assert!(text.contains("\"lines\": 6") && text.contains("\"reviewer\": \"r\""), "{text}");
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "grow"]);
        let (_, due, out) = growth();
        assert!(!due && out["landing"]["growth_records"] == json!([record]), "{out:#}");
        let (report, _) = slimfit(repo, "report --points 1").unwrap();
        assert_eq!(report["growth_records"], json!({ "as_is": 1, "cut": 0 }), "{report:#}");
        assert_eq!(report["history"][0]["growth_records"], json!({ "as_is": 1, "cut": 0 }), "{report:#}");
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
        assert_eq!(crate::session::Session::open(repo).unwrap().base.mutants_from.as_deref(), Some(before.as_str()));
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

    /// A change that adds uncovered lines leaves `check` green (slimfit#67):
    /// its answer says how far `uncovered_lines` rose and names the spans
    /// holding a line the change adds, and only those; `ratchet` keeps the
    /// lower count, and refuses to loosen what bounds nothing.
    #[test]
    fn new_uncovered_lines_are_named_not_failed() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        cfg.check.target = Some("target".into());
        // The sample's coverage, its paths in the repository.
        let tests = cfg.suites[0].overrides.test_command.as_mut().unwrap();
        tests.push_str(" && sed 's#/REPO/##' \"$SLIMFIT_OUT/lcov.info\" > \"$SLIMFIT_OUT/l\" && mv \"$SLIMFIT_OUT/l\" \"$SLIMFIT_OUT/lcov.info\"");
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(repo.join(".gitignore"), ".slimfit/out\n").unwrap();
        // The target lacks `unused`'s body (lines 6-8), which the sample's coverage leaves unrun.
        let lib = repo.join("src/lib.rs");
        let text = std::fs::read_to_string(&lib).unwrap();
        let body = "    let doubled = x * 2;\n    let shifted = doubled + 7;\n    shifted / 3\n";
        std::fs::write(&lib, text.replace(body, "")).unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "target"]);
        git(repo, &["branch", "target"]);
        std::fs::write(&lib, &text).unwrap();
        let (out, _) =
            slimfit_args(repo, &["ratchet", "--no-mutants", "arithmetic", "--picker", "p", "--author", "a"]).unwrap();
        assert!(out["record"].is_string(), "{out:#}");
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "a"]);
        let p = repo.join(crate::ratchet::FILE);
        let mut b: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let measured = b["suites"]["rust"]["uncovered_lines"].as_u64().unwrap();
        b["suites"]["rust"]["uncovered_lines"] = 0.into();
        std::fs::write(&p, b.to_string()).unwrap();
        let (out, code) = slimfit(repo, "check").unwrap();
        assert_eq!((code, &out["verdict"]["violations"]), (crate::EXIT_OK, &serde_json::json!([])), "{out:#}");
        let t = &out["verdict"]["uncovered_lines"][0];
        assert_eq!((t["baseline"].as_f64(), t["current"].as_f64()), (Some(0.0), Some(measured as f64)), "{t:#}");
        let span =
            serde_json::json!([{ "file": "src/lib.rs", "start": 5, "end": 9, "lines": 5, "function": "unused" }]);
        assert_eq!(t["new_spans"], span, "{t:#}");
        // Kept low, and not loosened: there is no bound to raise.
        slimfit(repo, "ratchet").unwrap();
        assert_eq!(crate::session::Session::open(repo).unwrap().base.suites["rust"].uncovered_lines, 0);
        let f = faults(repo, "ratchet --loosen uncovered_lines --reason r");
        assert!(f[0].fault.contains("never loosened") && f[0].fix.contains("asserts what the lines decide"), "{f:?}");
    }

    /// A sample of a landed change's mutants, out of the loop: no name the
    /// change's record picked is drawn, a sampled survivor answers as a
    /// `sampled` surviving-mutant task, the sample's record as a task to
    /// commit, and `check` stays green with both outstanding. The same tip
    /// draws the same; `report` tallies the sample beside the picks once
    /// its record is committed, and the next sample starts from it.
    #[test]
    fn a_sampled_survivor_is_a_task_and_never_a_bound() {
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
        let p = repo.join("src/lib.rs");
        std::fs::write(&p, std::fs::read_to_string(&p).unwrap() + "\npub fn a_one() -> u8 {\n    1\n}\n").unwrap();
        let killed = "src/lib.rs:12:9: replace >= with < in is_adult";
        slimfit_args(repo, &["check", "--mutants", killed, "--why", "the age bound"]).unwrap();
        slimfit(repo, "ratchet --picker p --author a").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "a, recorded"]);
        // Landed: the target is where the change now is.
        git(repo, &["branch", "-f", "target"]);
        // Code from before picks is not drawn from: the baseline's cut bounds the window.
        let adopt =
            std::process::Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD~1"]).output().unwrap();
        let adopt = String::from_utf8(adopt.stdout).unwrap().trim().to_string();
        let b = repo.join(crate::ratchet::FILE);
        let mut base: Value = serde_json::from_str(&std::fs::read_to_string(&b).unwrap()).unwrap();
        base["mutants_from"] = adopt.clone().into();
        std::fs::write(&b, base.to_string()).unwrap();
        git(repo, &["commit", "-qam", "cut"]);
        git(repo, &["branch", "-f", "target"]);

        let (out, code) = slimfit(repo, "next --all --sample 20").unwrap();
        assert_eq!(code, 0, "{out:#}");
        let tasks = out["tasks"].as_array().unwrap();
        let of = |kind: &str| tasks.iter().filter(|t| t["kind"] == kind).collect::<Vec<_>>();
        let survivors = of("surviving-mutant");
        assert!(!survivors.is_empty() && survivors.iter().all(|t| t["evidence"]["source"] == "sampled"), "{out:#}");
        let named = |t: &Value| {
            t["evidence"]["survivors"].as_array().unwrap().iter().map(|m| m["name"].clone()).collect::<Vec<_>>()
        };
        assert!(
            survivors.iter().flat_map(|t| named(t)).any(|n| n == "src/lib.rs:12:5: replace is_adult -> bool with true")
        );
        let record = of("record-sample");
        assert_eq!(record.len(), 1, "{out:#}");
        let text = record[0]["evidence"]["text"].as_str().unwrap();
        let sample: crate::measure::sample::Sample = serde_json::from_str(text).unwrap();
        let drawn: Vec<&str> = sample.drawn["rust"].iter().map(|d| d.name.as_str()).collect();
        assert_eq!((drawn.len(), sample.pool), (16, 16), "{text}");
        assert_eq!(sample.from, adopt, "{text}");
        assert!(!drawn.contains(&killed), "{drawn:?}");
        // Every viable one ran in the first round: none is unviable in the fixture.
        assert_eq!(sample.counts().unviable, 0);
        // The same tip draws the same, from the cache or not.
        let (again, _) = slimfit(repo, "next --all --sample 20").unwrap();
        assert_eq!(again["tasks"], out["tasks"]);
        std::fs::remove_file(repo.join(".slimfit/out/sample.json")).unwrap();
        let (fresh, _) = slimfit(repo, "next --all --sample 20").unwrap();
        assert_eq!(fresh["tasks"], out["tasks"]);
        // Never a bound: `check` passes with the sampled survivors outstanding.
        let (held, code) = slimfit(repo, "check").unwrap();
        assert_eq!((code, held["verdict"]["ok"].as_bool()), (0, Some(true)), "{held:#}");

        // The record committed: report sets the sample beside the picks, and
        // the next sample draws from what landed since, here nothing.
        let path = record[0]["evidence"]["path"].as_str().unwrap();
        std::fs::create_dir_all(repo.join(crate::measure::sample::DIR)).unwrap();
        std::fs::write(repo.join(path), text).unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "record the sample"]);
        let (held, code) = slimfit(repo, "check").unwrap();
        assert_eq!(code, 0, "{held:#}");
        let (report, _) = slimfit(repo, "report --points 1").unwrap();
        let survival = &report["history"][0]["mutants_survival"];
        assert_eq!((survival["picker"]["ran"].as_u64(), survival["picker"]["survived"].as_u64()), (Some(1), Some(0)));
        assert_eq!(survival["sampled"]["ran"].as_u64(), Some(16), "{survival:#}");
        let (next, _) = slimfit(repo, "next --all --sample 20").unwrap();
        assert!(next["tasks"].as_array().unwrap().iter().all(|t| t["evidence"]["source"] != "sampled"), "{next:#}");
        assert!(next["notes"][0].as_str().unwrap().contains("of 0 mutants"), "{next:#}");
    }

    /// Three landings whose measured `uncovered_lines` rise (slimfit#68):
    /// the ratcheted bound hides it, `report`'s history shows it in
    /// `measured`, and `next` gives a `rising-trend` task naming the
    /// landings and the uncovered span they added; `check` stays green.
    #[test]
    fn a_run_of_rises_shows_in_report_and_is_a_task() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        // The sample's coverage, its paths in the repository.
        let tests = cfg.suites[0].overrides.test_command.as_mut().unwrap();
        tests.push_str(" && sed 's#/REPO/##' \"$SLIMFIT_OUT/lcov.info\" > \"$SLIMFIT_OUT/l\" && mv \"$SLIMFIT_OUT/l\" \"$SLIMFIT_OUT/lcov.info\"");
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(repo.join(".gitignore"), ".slimfit/out\n").unwrap();
        // The first landing lacks `unused`'s body (lines 6-8), which the sample's coverage leaves unrun.
        let lib = repo.join("src/lib.rs");
        let text = std::fs::read_to_string(&lib).unwrap();
        let body = "    let doubled = x * 2;\n    let shifted = doubled + 7;\n    shifted / 3\n";
        std::fs::write(&lib, text.replace(body, "")).unwrap();
        slimfit(repo, "ratchet").unwrap();
        let measured = crate::session::Session::open(repo).unwrap().base.measured["rust"].uncovered_lines;
        // Each landing as its ratchet would have written it, two lines more uncovered each time.
        let land = |uncovered: u64, message: &str| {
            let mut b = crate::session::Session::open(repo).unwrap().base;
            b.measured.get_mut("rust").unwrap().uncovered_lines = uncovered;
            std::fs::write(repo.join(crate::ratchet::FILE), crate::ratchet::render(&b).unwrap()).unwrap();
            git(repo, &["add", "-A"]);
            git(repo, &["commit", "-qm", message]);
        };
        land(measured - 4, "landing 1");
        std::fs::write(&lib, &text).unwrap();
        land(measured - 2, "landing 2");
        let (out, _) = slimfit(repo, "ratchet").unwrap();
        assert!(out["changed"].as_bool().unwrap(), "{out:#}");
        git(repo, &["commit", "-qam", "landing 3"]);
        let b = crate::session::Session::open(repo).unwrap().base;
        // The bound keeps the first count; what was measured rose past it with `unused` back.
        let last = b.measured["rust"].uncovered_lines;
        assert_eq!(b.suites["rust"].uncovered_lines, measured);
        assert!(last >= measured, "{b:?}");

        let (report, _) = slimfit(repo, "report").unwrap();
        let history = report["history"].as_array().unwrap();
        let seen: Vec<Value> =
            history.iter().map(|p| p["suites"]["rust"]["measured"]["uncovered_lines"].clone()).collect();
        assert_eq!(seen, [Value::Null, (measured - 4).into(), (measured - 2).into(), last.into()], "{report:#}");

        let (next, _) = slimfit(repo, "next --all --kind rising-trend").unwrap();
        let tasks = next["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1, "{next:#}");
        let e = &tasks[0]["evidence"];
        assert_eq!(e["metric"], "uncovered_lines", "{e:#}");
        let landed: Vec<&str> =
            e["landings"].as_array().unwrap().iter().map(|l| l["subject"].as_str().unwrap()).collect();
        assert_eq!(landed, ["landing 1", "landing 2", "landing 3"]);
        let span =
            serde_json::json!([{ "file": "src/lib.rs", "start": 5, "end": 9, "lines": 5, "function": "unused" }]);
        assert_eq!(e["spans"], span, "{e:#}");
        assert!(tasks[0]["steps"].as_str().unwrap().contains("`git show <commit>`"), "{next:#}");
        // A task, never a bound.
        let (held, code) = slimfit(repo, "check").unwrap();
        assert_eq!((code, held["verdict"]["ok"].as_bool()), (crate::EXIT_OK, Some(true)), "{held:#}");
    }

    /// `report --lines-only` runs no test, and answers `by` only when asked;
    /// `report --fresh` measures again where the cache would answer.
    #[test]
    fn report_lines_only_runs_no_test_and_fresh_measures_again() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        let runs = repo.join("runs");
        let tests = cfg.suites[0].overrides.test_command.as_mut().unwrap();
        *tests = format!("echo run >> '{}'; {tests}", runs.display());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(repo.join(".gitignore"), ".slimfit/out\nruns\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "config"]);
        let ran = || std::fs::read_to_string(&runs).map_or(0, |t| t.lines().count());
        let (out, _) = slimfit(repo, "report --lines-only --by src --points 0").unwrap();
        assert_eq!(
            (ran(), out["by"]["src"]["rust"]["prod_lines"].clone()),
            (0, out["current"]["rust"]["prod_lines"].clone())
        );
        assert!(
            out["current"]["rust"]["tests"].is_null() && out["by"]["src"]["rust"]["prod_lines"].as_u64() > Some(0),
            "{out:#}"
        );
        let (out, _) = slimfit(repo, "report --points 0").unwrap();
        assert!(out.get("by").is_none() && ran() == 1, "{out:#}");
        slimfit(repo, "report --fresh --points 0").unwrap();
        assert_eq!(ran(), 2);
    }

    /// A landing whose ratchet followed `--mutants` keeps its time
    /// (slimfit#74). The gate's run before the commit times the files the
    /// mutants run then finds under a new HEAD: its time stands, not the
    /// mutants run's own. With no such run, `ratchet` times the tests
    /// itself. Landings so recorded show their rise to `rising-trend`.
    #[test]
    fn a_landing_recorded_after_mutants_keeps_its_time() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        // The slow test's CPU time, as the test command finds it when it runs.
        let clock = tempfile::tempdir().unwrap();
        let cpu = clock.path().join("cpu");
        let set = |secs: &str| std::fs::write(&cpu, format!("0m{secs}s")).unwrap();
        let tests = cfg.suites[0].overrides.test_command.as_mut().unwrap();
        *tests = tests.replace("0m0.600s", &format!("'\"$(cat {})\"'", cpu.display()));
        cfg.suites[0].overrides.mutants_tests = Some(crate::config::MutantTests::All);
        cfg.check.target = Some("target".into());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(repo.join(".gitignore"), ".slimfit/out\n").unwrap();
        set("1.000");
        slimfit(repo, "ratchet").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "adopt"]);
        git(repo, &["branch", "target"]);
        let lib = repo.join("src/lib.rs");
        let killed = "src/lib.rs:12:9: replace >= with < in is_adult";
        let land = |n: u32, secs: &str, gated: bool| {
            let text = std::fs::read_to_string(&lib).unwrap();
            std::fs::write(&lib, format!("{text}\npub fn f{n}() -> u8 {{\n    {n}\n}}\n")).unwrap();
            set(secs);
            if gated {
                slimfit(repo, "check").unwrap();
            }
            git(repo, &["commit", "-qam", &format!("change {n}")]);
            // A time no gate took: the mutants run's own test run would, and
            // a ratchet timing the tests again would.
            set("9.000");
            let (out, code) = slimfit_args(repo, &["check", "--mutants", killed, "--why", "the age bound"]).unwrap();
            assert_eq!(code, crate::EXIT_OK, "{out:#}");
            if !gated {
                set(secs);
            }
            slimfit(repo, "ratchet --picker p --author a").unwrap();
            git(repo, &["add", "-A"]);
            git(repo, &["commit", "-qm", &format!("landing {n}")]);
            crate::session::Session::open(repo).unwrap().base.measured["rust"].test_seconds
        };
        assert_eq!(land(1, "1.500", true), Some(1.525));
        // No gate's run of these files: the ratchet's own.
        assert_eq!(land(2, "2.000", false), Some(2.025));
        assert_eq!(land(3, "4.000", true), Some(4.025));

        let (next, _) = slimfit(repo, "next --all --kind rising-trend").unwrap();
        let tasks: Vec<&Value> =
            next["tasks"].as_array().unwrap().iter().filter(|t| t["evidence"]["metric"] == "test_seconds").collect();
        assert_eq!(tasks.len(), 1, "{next:#}");
        let landed: Vec<(&str, f64)> = tasks[0]["evidence"]["landings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| (l["subject"].as_str().unwrap(), l["value"].as_f64().unwrap()))
            .collect();
        assert_eq!(landed, [("adopt", 1.025), ("landing 1", 1.525), ("landing 2", 2.025), ("landing 3", 4.025)]);
    }

    /// A pick that survives its first run and is then killed (slimfit#68):
    /// its record keeps `survived` as its first outcome beside the `killed`
    /// it ends with, though the run the record is made of only saw it
    /// killed; `report` counts it as a first-run survivor of a second
    /// agent's picks.
    #[test]
    fn a_pick_killed_after_it_survived_keeps_its_first_outcome() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        // The mutation tool's outcomes, which the author's new test changes.
        let tool_dir = tempfile::tempdir().unwrap();
        let outcomes = tool_dir.path().join("outcomes.json");
        let reports = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rust/reports");
        let real = std::fs::read_to_string(reports.join("outcomes.json")).unwrap();
        std::fs::write(&outcomes, &real).unwrap();
        let tool = cfg.suites[0].overrides.mutants_command.as_mut().unwrap();
        *tool = tool.replace(&format!("{}/outcomes.json", reports.to_string_lossy()), &outcomes.to_string_lossy());
        cfg.suites[0].overrides.mutants_tests = Some(crate::config::MutantTests::All);
        cfg.check.target = Some("target".into());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(repo.join(".gitignore"), ".slimfit/out\n").unwrap();
        slimfit(repo, "ratchet").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "adopt"]);
        git(repo, &["branch", "target"]);
        let p = repo.join("src/lib.rs");
        std::fs::write(&p, std::fs::read_to_string(&p).unwrap() + "\npub fn a_one() -> u8 {\n    1\n}\n").unwrap();
        let pick = "src/lib.rs:12:5: replace is_adult -> bool with true";
        let (out, code) = slimfit_args(repo, &["check", "--mutants", pick, "--why", "the age bound"]).unwrap();
        assert_eq!(
            (code, out["mutated"]["rust"]["picks"][0]["outcome"].as_str()),
            (crate::EXIT_VIOLATION, Some("survived"))
        );
        // The author's test kills it: the tree and the tool's outcome change.
        let mut caught: Value = serde_json::from_str(&real).unwrap();
        for o in caught["outcomes"].as_array_mut().unwrap() {
            if o["scenario"]["Mutant"]["name"] == pick {
                o["summary"] = "CaughtMutant".into();
            }
        }
        std::fs::write(&outcomes, caught.to_string()).unwrap();
        std::fs::write(repo.join("src/minor.rs"), "// a minor is no adult\n").unwrap();
        let (out, code) = slimfit_args(repo, &["check", "--mutants", pick, "--why", "the age bound"]).unwrap();
        assert_eq!(
            (code, out["mutated"]["rust"]["picks"][0]["outcome"].as_str()),
            (crate::EXIT_OK, Some("killed")),
            "{out:#}"
        );
        let (out, _) = slimfit(repo, "ratchet --picker p --author a").unwrap();
        let record: crate::landing::Record =
            serde_json::from_str(&std::fs::read_to_string(repo.join(out["record"].as_str().unwrap())).unwrap())
                .unwrap();
        let p = &record.picks["rust"][0];
        assert_eq!((p.outcome, p.first_outcome), (Some(MutantStatus::Killed), Some(MutantStatus::Survived)));
        // Written into its record, the first outcomes start afresh for the next change.
        assert!(!repo.join(crate::landing::FIRSTS).exists());
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "a, recorded"]);
        let (report, _) = slimfit(repo, "report --points 1").unwrap();
        let picker = &report["history"][0]["mutants_survival"]["picker"];
        let counted = ["survived", "first_survived", "first_unknown"].map(|k| picker[k].as_u64().unwrap());
        assert_eq!(counted, [0, 1, 0], "{picker:#}");
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
