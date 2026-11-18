//! Stand-ins for the tools slimfit drives, for slimfit's own tests: one
//! binary, linked under each tool's name (`cargo`, `go`, `gremlins`,
//! `swift`, `xcrun`, `muter`, `npx`, `gradlew`) into a directory placed
//! first on PATH. It reads the arguments as the real tool does, refuses
//! (exit 64, no report) what the real tool would not take, and writes the
//! real reports of `testdata/<language>/reports` where the real tool
//! writes its own. Its state is one JSON file (see `state.rs`): every call
//! is recorded there, and faults are injected from it.

mod state;
#[allow(dead_code)]
#[path = "../../src/tools.rs"]
mod tools;

use state::{Call, ENV, Fault, State};
use std::path::{Path, PathBuf};
use tools::*;

/// A tool's work: the reports to write, each a source under the state's
/// `reports` and a destination; what to print on stdout; the exit status
/// the real tool would give.
#[derive(Default)]
struct Work {
    copies: Vec<(String, PathBuf)>,
    stdout: Option<String>,
    exit: i32,
    /// Done before the reports: e.g. the build directory a later step reads.
    prepare: Vec<Box<dyn FnOnce()>>,
}

type Refusal = String;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tool = Path::new(&args[0]).file_name().unwrap().to_string_lossy().into_owned();
    let args = &args[1..];
    let path = PathBuf::from(std::env::var(ENV).unwrap_or_else(|_| die(&tool, &format!("{ENV} is not set"))));
    let mut state = State::load(&path);
    let dir = std::env::current_dir().unwrap();
    let sub = args.iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_default();
    let step = match tool.as_str() {
        "gradlew" if args.iter().any(|a| a == "pitest") => "gradlew pitest".to_string(),
        "gradlew" => "gradlew test".to_string(),
        _ => format!("{tool} {sub}"),
    };
    state.calls.push(Call { step: step.clone(), args: args.to_vec(), dir: dir.clone() });
    state.save(&path);
    let fault = state.faults.get(&step).copied().unwrap_or_default();

    let work = match tool.as_str() {
        "cargo" => cargo(&sub, args, &dir, &state.reports),
        "go" => go(&sub, args),
        "gremlins" => gremlins(&sub, args),
        "swift" => swift(&sub, args, &dir),
        "xcrun" => xcrun(args, &dir),
        "muter" => muter(&sub, args),
        "gradlew" => gradlew(args),
        "npx" => npx(&sub, args),
        _ => Err(format!("no stand-in for {tool}")),
    };
    let work = work.unwrap_or_else(|why| die(&step, &why));
    run(work, &state.reports, &dir, fault, &step);
}

fn die(step: &str, why: &str) -> ! {
    eprintln!("standin {step}: {why}");
    std::process::exit(64);
}

fn run(work: Work, reports: &Path, dir: &Path, fault: Fault, step: &str) {
    let root = dir.to_string_lossy();
    for p in work.prepare {
        p();
    }
    if fault.no_report {
        eprintln!("standin {step}: writing no report (injected)");
    } else {
        for (src, dst) in &work.copies {
            copy(&reports.join(src), &dir.join(dst), &root);
        }
        if let Some(s) = &work.stdout {
            print!("{}", read(&reports.join(s), &root));
        }
    }
    let exit = if fault.exit != 0 { fault.exit } else { work.exit };
    if exit != 0 {
        eprintln!("standin {step}: exiting {exit}");
    }
    std::process::exit(exit);
}

/// A report's text with the project root put in for `/REPO`.
fn read(src: &Path, root: &str) -> String {
    std::fs::read_to_string(src).unwrap_or_else(|e| panic!("{}: {e}", src.display())).replace("/REPO", root)
}

/// Copies a report file, or a directory of them, rooting it at `root`.
fn copy(src: &Path, dst: &Path, root: &str) {
    if src.is_dir() {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap().flatten() {
            copy(&e.path(), &dst.join(e.file_name()), root);
        }
    } else {
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(dst, read(src, root)).unwrap();
    }
}

/// The value of `name`, given as `name value` or `name=value`.
fn value(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name);
    at.and_then(|i| args.get(i + 1).cloned())
        .or_else(|| args.iter().find_map(|a| a.strip_prefix(&format!("{name}=")).map(String::from)))
}

fn need(args: &[String], name: &str) -> Result<String, Refusal> {
    value(args, name).ok_or_else(|| format!("no {name}"))
}

fn has(args: &[String], flag: &str) -> Result<(), Refusal> {
    if args.iter().any(|a| a == flag) { Ok(()) } else { Err(format!("no {flag}")) }
}

fn sub_is(sub: &str, want: &str) -> Result<(), Refusal> {
    if sub == want { Ok(()) } else { Err(format!("no subcommand {sub:?}")) }
}

/// `cargo llvm-cov nextest` and `cargo mutants`.
fn cargo(sub: &str, args: &[String], dir: &Path, reports: &Path) -> Result<Work, Refusal> {
    match sub {
        "llvm-cov" => {
            if args.get(1).map(String::as_str) != Some("nextest") {
                return Err("only `llvm-cov nextest` stands in".into());
            }
            has(args, "--lcov")?;
            let lcov = need(args, "--output-path")?;
            // nextest takes a tool's config file as `tool:path`.
            let config = need(args, "--tool-config-file")?;
            let (_, config) = config.split_once(':').ok_or("--tool-config-file is not tool:path")?;
            let text = std::fs::read_to_string(config).map_err(|e| format!("{config}: {e}"))?;
            let toml: toml::Table = text.parse().map_err(|e| format!("{config}: {e}"))?;
            let junit = toml
                .get("profile")
                .and_then(|p| p.get("default"))
                .and_then(|p| p.get("junit"))
                .and_then(|j| j.get("path"))
                .and_then(|p| p.as_str())
                .ok_or("the nextest config sets no profile.default.junit.path")?;
            Ok(Work {
                copies: vec![
                    ("junit.xml".into(), dir.join(NEXTEST_DIR).join(junit)),
                    ("lcov.info".into(), lcov.into()),
                ],
                ..Work::default()
            })
        }
        "mutants" => {
            let out = PathBuf::from(need(args, "--output")?);
            let outcomes = out.join(CARGO_MUTANTS_OUTCOMES);
            let logs = outcomes.parent().unwrap().join("log");
            // cargo-mutants exits 2 when a mutant was missed.
            let missed =
                std::fs::read_to_string(reports.join("outcomes.json")).is_ok_and(|t| t.contains("\"MissedMutant\""));
            Ok(Work {
                copies: vec![("outcomes.json".into(), outcomes), ("log".into(), logs)],
                exit: if missed { 2 } else { 0 },
                ..Work::default()
            })
        }
        _ => Err(format!("no subcommand {sub:?}")),
    }
}

/// `go test -json -coverprofile=<file>`: the JSON on stdout.
fn go(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    sub_is(sub, "test")?;
    has(args, "-json")?;
    let cover = need(args, "-coverprofile")?;
    Ok(Work { copies: vec![("cover.out".into(), cover.into())], stdout: Some("test.json".into()), ..Work::default() })
}

fn gremlins(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    sub_is(sub, "unleash")?;
    Ok(Work { copies: vec![("gremlins.json".into(), need(args, "--output")?.into())], ..Work::default() })
}

/// `swift test`: XCTest's xunit at the path given, swift-testing's beside
/// it, and the build a coverage export reads: the test bundle and the
/// profile, under `.build/debug`, a symlink as SwiftPM makes it.
fn swift(sub: &str, args: &[String], dir: &Path) -> Result<Work, Refusal> {
    sub_is(sub, "test")?;
    has(args, "--enable-code-coverage")?;
    has(args, "--parallel")?;
    let xunit = PathBuf::from(need(args, "--xunit-output")?);
    let testing = xunit.with_file_name(format!("{}-swift-testing.xml", xunit.file_stem().unwrap().to_string_lossy()));
    let build = dir.join(".build");
    let prepare: Box<dyn FnOnce()> = Box::new(move || {
        let real = build.join("arm64-apple-macosx/debug");
        let bundle = real.join("StandinPackageTests.xctest/Contents/MacOS");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("StandinPackageTests"), "").unwrap();
        std::fs::create_dir_all(real.join("codecov")).unwrap();
        std::fs::write(real.join("codecov/default.profdata"), "").unwrap();
        let link = build.join("debug");
        if !link.exists() {
            std::os::unix::fs::symlink("arm64-apple-macosx/debug", link).unwrap();
        }
    });
    Ok(Work {
        copies: vec![("xunit.xml".into(), xunit), ("xunit-swift-testing.xml".into(), testing)],
        prepare: vec![prepare],
        ..Work::default()
    })
}

/// `xcrun llvm-cov export -format=lcov -instr-profile=<p> <binary>`: the
/// LCOV on stdout, when the profile and the binary exist.
fn xcrun(args: &[String], dir: &Path) -> Result<Work, Refusal> {
    if args.first().map(String::as_str) != Some("llvm-cov") || args.get(1).map(String::as_str) != Some("export") {
        return Err("only `xcrun llvm-cov export` stands in".into());
    }
    has(args, "-format=lcov")?;
    let profile = need(args, "-instr-profile")?;
    let binary = args.last().filter(|a| !a.starts_with('-')).ok_or("no binary")?;
    for p in [&profile, binary] {
        if !dir.join(p).is_file() {
            return Err(format!("{p}: no such file"));
        }
    }
    Ok(Work { stdout: Some("lcov.info".into()), ..Work::default() })
}

fn muter(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    match sub {
        "init" => Ok(Work { copies: vec![("../muter.conf.yml".into(), "muter.conf.yml".into())], ..Work::default() }),
        "run" => {
            if value(args, "--format").as_deref() != Some("json") {
                return Err("no --format json".into());
            }
            Ok(Work { copies: vec![("muter-survived.json".into(), need(args, "--output")?.into())], ..Work::default() })
        }
        _ => Err(format!("no subcommand {sub:?}")),
    }
}

/// `./gradlew <task>...`: each task's report where its plugin writes it.
fn gradlew(args: &[String]) -> Result<Work, Refusal> {
    let mut copies = Vec::new();
    for task in args.iter().filter(|a| !a.starts_with('-')) {
        match task.as_str() {
            "test" => copies.push((
                "junit-com.example.CalcTest.xml".into(),
                PathBuf::from(GRADLE_TEST_RESULTS).join("TEST-com.example.CalcTest.xml"),
            )),
            "koverXmlReport" => copies.push(("kover.xml".into(), KOVER_XML.into())),
            "pitest" => copies.push(("mutations.xml".into(), PITEST_XML.into())),
            _ => return Err(format!("Task '{task}' not found in root project")),
        }
    }
    Ok(Work { copies, ..Work::default() })
}

/// `npx vitest run ...` and `npx stryker run ...`.
fn npx(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    let run = args.get(1).map(String::as_str) == Some("run");
    match sub {
        "vitest" if run => {
            has(args, "--coverage.enabled")?;
            if value(args, "--coverage.reporter").as_deref() != Some("lcov") {
                return Err("no --coverage.reporter=lcov".into());
            }
            if value(args, "--reporter").as_deref() != Some("junit") {
                return Err("no --reporter=junit".into());
            }
            let cov = PathBuf::from(need(args, "--coverage.reportsDirectory")?);
            Ok(Work {
                copies: vec![
                    ("junit.xml".into(), need(args, "--outputFile")?.into()),
                    ("lcov.info".into(), cov.join("lcov.info")),
                ],
                ..Work::default()
            })
        }
        "stryker" if run => {
            if value(args, "--reporters").as_deref() != Some("json") {
                return Err("no --reporters json".into());
            }
            Ok(Work { copies: vec![("mutation.json".into(), STRYKER_JSON.into())], ..Work::default() })
        }
        _ => Err(format!("no `{sub} run`")),
    }
}
