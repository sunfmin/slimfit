//! Which files of a suite are source and which are test, and how many
//! non-blank lines each side has. Rust's `#[cfg(test)] mod` blocks count as
//! test lines even though they sit in source files, and so does every file
//! of a workspace crate that other members reach only through
//! `[dev-dependencies]` (`workspace::test_crates`).

use crate::config::{Preset, Suite};
use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Classifier {
    root: String,
    sources: GlobSet,
    tests: GlobSet,
    excludes: GlobSet,
    inline_tests: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Source,
    Test,
}

/// The lines of one source file: how many are prod, how many test, and
/// which line numbers (1-based) are test lines.
#[derive(Debug, Default, Clone)]
pub struct FileLines {
    pub prod: u64,
    pub test: u64,
    pub test_ranges: Vec<(u32, u32)>,
}

impl FileLines {
    pub fn is_test_line(&self, n: u32) -> bool {
        self.test_ranges.iter().any(|(a, b)| (*a..=*b).contains(&n))
    }
}

fn set(globs: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g)?);
    }
    Ok(b.build()?)
}

impl Classifier {
    /// `repo` is where a Rust suite's Cargo manifests are read.
    pub fn new(suite: &Suite, repo: &Path) -> Result<Self> {
        let mut tests = suite.tests.clone();
        if suite.preset == Preset::Rust {
            tests.extend(crate::workspace::test_crates(&repo.join(&suite.root)).iter().map(|d| format!("{d}/**")));
        }
        Ok(Self {
            root: suite.root.trim_end_matches('/').to_string(),
            sources: set(&suite.sources)?,
            tests: set(&tests)?,
            excludes: set(&suite.excludes)?,
            inline_tests: suite.inline_tests,
        })
    }

    /// `path` is relative to the repository root.
    pub fn kind(&self, path: &str) -> Option<Kind> {
        let rel = if self.root == "." || self.root.is_empty() {
            path
        } else {
            path.strip_prefix(&self.root)?.strip_prefix('/')?
        };
        if self.excludes.is_match(rel) || !self.sources.is_match(rel) && !self.tests.is_match(rel) {
            return None;
        }
        Some(if self.tests.is_match(rel) { Kind::Test } else { Kind::Source })
    }

    pub fn lines(&self, kind: Kind, text: &str) -> FileLines {
        let non_blank =
            |a: usize, b: usize| text.lines().skip(a).take(b - a).filter(|l| !l.trim().is_empty()).count() as u64;
        let total = text.lines().count();
        let all = non_blank(0, total);
        if kind == Kind::Test {
            return FileLines { prod: 0, test: all, test_ranges: vec![(1, total as u32)] };
        }
        let ranges = if self.inline_tests { rust_test_blocks(text) } else { Vec::new() };
        let test: u64 = ranges.iter().map(|(a, b)| non_blank(*a as usize - 1, *b as usize)).sum();
        FileLines { prod: all - test, test, test_ranges: ranges }
    }

    /// Per-file line counts for `files` read through `read`.
    pub fn count(&self, files: &[String], read: impl Fn(&str) -> Option<String>) -> Totals {
        let texts: Vec<(&String, Kind, String)> =
            files.iter().filter_map(|f| Some((f, self.kind(f)?, read(f)?))).collect();
        // A file declared `#[cfg(test)] mod x;` is test code as a whole.
        let test_modules: Vec<String> = if self.inline_tests {
            texts.iter().flat_map(|(f, _, text)| rust_test_module_files(f, text)).collect()
        } else {
            Vec::new()
        };
        let mut t = Totals::default();
        for (f, kind, text) in texts {
            let kind = if test_modules.contains(f) { Kind::Test } else { kind };
            let l = self.lines(kind, &text);
            t.prod += l.prod;
            t.test += l.test;
            t.files.insert(f.clone(), (kind, l));
        }
        t
    }
}

/// The files of modules `path` declares as `#[cfg(test)] mod name;`.
fn rust_test_module_files(path: &str, text: &str) -> Vec<String> {
    let (dir, file) = path.rsplit_once('/').map(|(d, f)| (format!("{d}/"), f)).unwrap_or((String::new(), path));
    let base = match file {
        "main.rs" | "lib.rs" | "mod.rs" => dir,
        _ => format!("{dir}{}/", file.trim_end_matches(".rs")),
    };
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if !l.starts_with("#[cfg(test)]") {
            continue;
        }
        let rest = l.trim_start_matches("#[cfg(test)]").trim();
        let decl = if rest.is_empty() { lines.get(i + 1).copied().unwrap_or("") } else { rest };
        let decl = decl.trim_start_matches("pub(crate) ").trim_start_matches("pub ");
        if let Some(name) = decl.strip_prefix("mod ").and_then(|d| d.strip_suffix(';')) {
            out.push(format!("{base}{}.rs", name.trim()));
            out.push(format!("{base}{}/mod.rs", name.trim()));
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct Totals {
    pub prod: u64,
    pub test: u64,
    pub files: BTreeMap<String, (Kind, FileLines)>,
}

/// Line ranges (1-based, inclusive) of `#[cfg(test)]` items: from the
/// attribute to the brace that closes the item it is on.
fn rust_test_blocks(text: &str) -> Vec<(u32, u32)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            let start = i;
            let mut depth = 0i32;
            let mut opened = false;
            let mut j = i;
            'scan: while j < lines.len() {
                for ch in strip_strings(lines[j]).chars() {
                    match ch {
                        '{' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' => depth -= 1,
                        ';' if !opened && j > start => break 'scan,
                        _ => {}
                    }
                    if opened && depth == 0 {
                        break 'scan;
                    }
                }
                j += 1;
            }
            let end = j.min(lines.len() - 1);
            out.push((start as u32 + 1, end as u32 + 1));
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// The line without string/char literals and `//` comments, so braces in
/// them don't count.
fn strip_strings(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
        } else {
            match c {
                '"' => in_str = true,
                '\'' => {
                    // a char literal like '{' or '\''; a lifetime has no closing quote nearby
                    let rest: String = chars.clone().take(3).collect();
                    if rest.starts_with('\\') {
                        chars.next();
                        chars.next();
                        chars.next();
                    } else if rest.chars().nth(1) == Some('\'') {
                        chars.next();
                        chars.next();
                    }
                }
                '/' if chars.peek() == Some(&'/') => break,
                _ => out.push(c),
            }
        }
    }
    out
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::config::{Preset, preset};

    fn rust() -> Classifier {
        Classifier::new(&preset(Preset::Rust), Path::new(".")).unwrap()
    }

    #[test]
    fn classifies_by_globs_and_root() {
        let mut s = preset(Preset::Go);
        s.root = "svc".into();
        let c = Classifier::new(&s, Path::new(".")).unwrap();
        assert_eq!(c.kind("svc/a.go"), Some(Kind::Source));
        assert_eq!(c.kind("svc/a_test.go"), Some(Kind::Test));
        assert_eq!(c.kind("svc/vendor/x/a.go"), None);
        assert_eq!(c.kind("other/a.go"), None);
        assert_eq!(c.kind("svc/README.md"), None);
    }

    /// A crate other members reach only through `[dev-dependencies]` is test
    /// code, as `tests/**` is; one that is also a build-dependency stays prod.
    #[test]
    fn a_dev_only_workspace_crate_is_test_code() {
        let d = tempfile::tempdir().unwrap();
        crate::workspace::t::workspace(&d.path().join("ws"));
        let mut s = preset(Preset::Rust);
        s.root = "ws".into();
        let c = Classifier::new(&s, d.path()).unwrap();
        let files: Vec<String> =
            ["ws/app/src/lib.rs", "ws/crates/support/src/lib.rs", "ws/crates/both/src/lib.rs"].map(String::from).into();
        let t = c.count(&files, |_| Some("fn a() {}\n\nfn b() {}\n".into()));
        let kind = |f: &str| t.files[f].0;
        assert_eq!(files.iter().map(|f| kind(f)).collect::<Vec<_>>(), [Kind::Source, Kind::Test, Kind::Source]);
        assert_eq!((t.prod, t.test), (4, 2));
    }

    #[test]
    fn rust_inline_tests_are_test_lines() {
        let src = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    fn s() { let _ = \"}\"; let _ = '{'; }\n}\nfn b() {}\n";
        let l = rust().lines(Kind::Source, src);
        assert_eq!(l.test_ranges, vec![(3, 6)]);
        assert_eq!((l.prod, l.test), (2, 4));
        assert!(l.is_test_line(5) && !l.is_test_line(7));
    }

    #[test]
    fn a_cfg_test_module_file_is_test_code() {
        let files: Vec<String> = ["src/main.rs", "src/fixtures.rs", "src/a.rs", "src/a/t.rs"].map(String::from).into();
        let text = |f: &str| {
            Some(
                match f {
                    "src/main.rs" => "mod a;\n#[cfg(test)]\nmod fixtures;\n",
                    "src/a.rs" => "#[cfg(test)] mod t;\nfn x() {}\n",
                    _ => "fn t() {}\n",
                }
                .to_string(),
            )
        };
        let t = rust().count(&files, text);
        let kind = |f: &str| t.files[f].0;
        assert_eq!(
            [kind("src/main.rs"), kind("src/fixtures.rs"), kind("src/a.rs"), kind("src/a/t.rs")],
            [Kind::Source, Kind::Test, Kind::Source, Kind::Test]
        );
    }

    #[test]
    fn cfg_test_on_a_use_line_ends_at_semicolon() {
        let src = "#[cfg(test)]\nuse x::y;\nfn a() {}\n";
        assert_eq!(rust().lines(Kind::Source, src).test_ranges, vec![(1, 2)]);
    }
}
