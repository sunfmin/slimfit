//! Which files of a suite are source and which are test, and which lines
//! of each are source lines: the one oracle every caller asks.
//!
//! A [`Classifier`] is built for a whole tree, because some rules need it:
//! a file declared `#[cfg(test)] mod x;` is test code, and so is every file
//! of a workspace crate that other members reach only through
//! `[dev-dependencies]` (`workspace::test_crates`). Rust's `#[cfg(test)]`
//! items are test lines even though they sit in source files.
//!
//! A source line is a line of a source file, outside its tests, that is
//! neither blank nor only a `//` comment: what `prod_lines` counts, what
//! coverage is kept for, and what a change owes mutants for.

use crate::config::{Preset, Suite};
use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub struct Classifier {
    root: String,
    sources: GlobSet,
    tests: GlobSet,
    excludes: GlobSet,
    inline_tests: bool,
    /// Files some file of the tree declares `#[cfg(test)] mod x;`.
    test_modules: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Source,
    Test,
}

/// The lines of one file: how many are source lines (`prod`), how many
/// test lines, and which line numbers (1-based) are source lines.
#[derive(Debug, Default, Clone)]
pub struct FileLines {
    pub prod: u64,
    pub test: u64,
    source: BTreeSet<u32>,
}

impl FileLines {
    pub fn is_source_line(&self, n: u32) -> bool {
        self.source.contains(&n)
    }
}

fn set(globs: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g)?);
    }
    Ok(b.build()?)
}

/// Whether a line counts at all: neither blank nor only a `//` comment.
fn counts(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with("//")
}

impl Classifier {
    /// The suite's classifier for the tree of `files` (relative to `repo`),
    /// read through `read`. A Rust suite's Cargo manifests are read in `repo`.
    pub fn new(suite: &Suite, repo: &Path, files: &[String], read: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let mut tests = suite.tests.clone();
        if suite.preset == Preset::Rust {
            tests.extend(crate::workspace::test_crates(&repo.join(&suite.root)).iter().map(|d| format!("{d}/**")));
        }
        let mut c = Self {
            root: suite.root.trim_end_matches('/').to_string(),
            sources: set(&suite.sources)?,
            tests: set(&tests)?,
            excludes: set(&suite.excludes)?,
            inline_tests: suite.inline_tests,
            test_modules: BTreeSet::new(),
        };
        if c.inline_tests {
            let declaring = files.iter().filter(|f| c.globbed(f) == Some(Kind::Source));
            c.test_modules =
                declaring.filter_map(|f| Some(crate::rust_source::test_module_files(f, &read(f)?))).flatten().collect();
        }
        Ok(c)
    }

    /// The kind the suite's globs alone give `path` (relative to the repo).
    fn globbed(&self, path: &str) -> Option<Kind> {
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

    /// `path`'s kind in this tree (relative to the repository root); `None`
    /// when the suite does not count it.
    pub fn kind(&self, path: &str) -> Option<Kind> {
        match self.globbed(path)? {
            Kind::Source if self.test_modules.contains(path) => Some(Kind::Test),
            k => Some(k),
        }
    }

    /// `path`'s kind and lines, its text being `text`.
    pub fn file(&self, path: &str, text: &str) -> Option<(Kind, FileLines)> {
        let kind = self.kind(path)?;
        let tests = match kind {
            Kind::Test => vec![(1, u32::MAX)],
            Kind::Source if self.inline_tests => crate::rust_source::test_blocks(text),
            Kind::Source => vec![],
        };
        let mut l = FileLines::default();
        for (n, _) in (1..).zip(text.lines()).filter(|(_, line)| counts(line)) {
            if tests.iter().any(|(a, b)| (*a..=*b).contains(&n)) {
                l.test += 1;
            } else {
                l.prod += 1;
                l.source.insert(n);
            }
        }
        Some((kind, l))
    }

    /// Per-file line counts for `files` read through `read`.
    pub fn count(&self, files: &[String], read: impl Fn(&str) -> Option<String>) -> Totals {
        let mut t = Totals::default();
        for f in files {
            let Some((kind, l)) = self.kind(f).and_then(|_| self.file(f, &read(f)?)) else { continue };
            t.prod += l.prod;
            t.test += l.test;
            t.files.insert(f.clone(), (kind, l));
        }
        t
    }
}

#[derive(Debug, Default)]
pub struct Totals {
    pub prod: u64,
    pub test: u64,
    pub files: BTreeMap<String, (Kind, FileLines)>,
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::config::{Preset, preset};

    /// A Rust classifier for a tree of no files.
    fn rust() -> Classifier {
        Classifier::new(&preset(Preset::Rust), Path::new("."), &[], |_| None).unwrap()
    }

    #[test]
    fn classifies_by_globs_and_root() {
        let mut s = preset(Preset::Go);
        s.root = "svc".into();
        let c = Classifier::new(&s, Path::new("."), &[], |_| None).unwrap();
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
        let c = Classifier::new(&s, d.path(), &[], |_| None).unwrap();
        let files: Vec<String> =
            ["ws/app/src/lib.rs", "ws/crates/support/src/lib.rs", "ws/crates/both/src/lib.rs"].map(String::from).into();
        let t = c.count(&files, |_| Some("fn a() {}\n\nfn b() {}\n".into()));
        let kind = |f: &str| t.files[f].0;
        assert_eq!(files.iter().map(|f| kind(f)).collect::<Vec<_>>(), [Kind::Source, Kind::Test, Kind::Source]);
        assert_eq!((t.prod, t.test), (4, 2));
    }

    /// Inline tests are test lines; blank lines and `//` comments are
    /// neither source nor test lines.
    #[test]
    fn source_lines_are_outside_tests_blanks_and_comments() {
        let src = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    // c\n    fn s() { let _ = \"}\"; let _ = '{'; }\n}\n/// doc\nfn b() {}\n";
        let (_, l) = rust().file("src/a.rs", src).unwrap();
        assert_eq!((l.prod, l.test), (2, 4));
        assert_eq!(l.source.iter().copied().collect::<Vec<_>>(), [1, 9]);
    }

    /// A file declared `#[cfg(test)] mod x;` is test code, asked alone: the
    /// declaring file need not be among the files asked about.
    #[test]
    fn a_cfg_test_module_file_is_test_code() {
        let tree: Vec<String> = ["src/main.rs", "src/fixtures.rs", "src/a.rs", "src/a/t.rs"].map(String::from).into();
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
        let c = Classifier::new(&preset(Preset::Rust), Path::new("."), &tree, text).unwrap();
        let kinds = tree.iter().map(|f| c.kind(f)).collect::<Vec<_>>();
        assert_eq!(kinds, [Some(Kind::Source), Some(Kind::Test), Some(Kind::Source), Some(Kind::Test)]);
        let t = c.count(&tree[1..2], text);
        assert_eq!((t.prod, t.test), (0, 1));
    }
}
