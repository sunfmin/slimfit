//! The tests a change removes (slimfit#87): a deleted or merged test may
//! have been the only one to notice a fault on lines others run too, so
//! the change owes picks over the lines it ran, as it does over the lines
//! it adds. Found from Rust syntax alone, as cheaply as the gate needs:
//! one read of the touched files where the change starts.

use super::{Landing, Tree};
use crate::config::Config;
use crate::git;
use crate::rust_source;
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A test function where the change starts that it no longer has.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Removed {
    pub file: String,
    /// The line of its `fn` where the change starts.
    pub line: u32,
    pub name: String,
}

impl Removed {
    /// How a record and a task name it: `file:line: name`, its line where
    /// the change starts.
    pub fn id(&self) -> String {
        format!("{}:{}: {}", self.file, self.line, self.name)
    }
}

/// Per suite whose tool can pick (no other runs mutants), the test
/// functions in the Rust files the change touches that are gone from them
/// all: by name, unless the same body is there under a name new to them (a
/// rename or a move). A twin left under its old name renames nothing: it
/// is the redundant test that stays. A test kept by its name in another
/// module of a touched file is taken as kept.
pub fn of(repo: &Path, cfg: &Config, tree: &Tree, l: &Landing) -> Result<BTreeMap<String, Vec<Removed>>> {
    let files: Vec<String> = l.touched.iter().filter(|f| f.ends_with(".rs")).cloned().collect();
    let before = git::files_at(repo, &l.from, &files)?;
    let now: Vec<Option<String>> = files.iter().map(|f| std::fs::read_to_string(repo.join(f)).ok()).collect();
    let mut out = BTreeMap::new();
    for sc in &cfg.suites {
        let s = tree.suite(cfg, &sc.name)?;
        if !s.suite.mutates() {
            continue;
        }
        let counted = |i: &usize| s.kind(&files[*i]).is_some();
        let tests = |texts: &[Option<String>]| -> Vec<(usize, u32, String, String)> {
            let mut out = Vec::new();
            for i in (0..files.len()).filter(counted) {
                let Some(text) = &texts[i] else { continue };
                for (line, name) in rust_source::test_fns(text) {
                    out.push((i, line, name.to_string(), body(text, line, name)));
                }
            }
            out
        };
        let (was, kept) = (tests(&before), tests(&now));
        let old: BTreeSet<&str> = was.iter().map(|t| t.2.as_str()).collect();
        let names: BTreeSet<&str> = kept.iter().map(|t| t.2.as_str()).collect();
        let bodies: BTreeSet<&str> =
            kept.iter().filter(|t| !old.contains(t.2.as_str())).map(|t| t.3.as_str()).collect();
        let gone: Vec<Removed> = was
            .iter()
            .filter(|(_, _, name, body)| !names.contains(name.as_str()) && !bodies.contains(body.as_str()))
            .map(|(i, line, name, _)| Removed { file: files[*i].clone(), line: *line, name: name.clone() })
            .collect();
        if !gone.is_empty() {
            out.insert(sc.name.clone(), gone);
        }
    }
    Ok(out)
}

/// The text of the test at `line` without its name or indentation: the
/// same in a test renamed or moved.
fn body(text: &str, line: u32, name: &str) -> String {
    let item = rust_source::item_text(text, line).unwrap_or_default();
    let item = item.replacen(&format!("fn {name}"), "fn _", 1);
    item.lines().map(str::trim).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::measure::t::git;

    /// A test gone from every touched file is removed, a deleted file's
    /// too; one renamed or moved with the same body, or kept by its name,
    /// is not. Files the suite does not count, and other code, are not read
    /// for tests.
    #[test]
    fn a_test_is_removed_when_its_name_and_body_are_gone() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let test = |name: &str, body: &str| format!("#[test]\nfn {name}() {{\n    {body}\n}}\n");
        let files = [
            ("Cargo.toml", "[package]\nname = \"x\"\n".to_string()),
            (
                "src/lib.rs",
                format!(
                    "pub fn f() {{}}\n#[cfg(test)]\nmod t {{\n{}{}{}}}\n",
                    test("a", "f();"),
                    test("b", "g();"),
                    test("y", "g();")
                ),
            ),
            ("tests/one.rs", test("c", "h();") + &test("d", "i();") + &test("e", "j();")),
            ("tests/gone.rs", test("z", "k();")),
            ("notes/x.rs", test("n", "l();")),
        ];
        crate::workspace::t::write(repo, &files.iter().map(|(f, t)| (*f, t.as_str())).collect::<Vec<_>>());
        git(repo, &["init", "-q"]);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "start"]);
        git(repo, &["branch", "start"]);
        // `a` deleted, `y` (the twin of `b`) deleted, `c` renamed `c2`, `d` moved to lib.rs, `e` changed in place.
        let lib = format!("pub fn f() {{}}\n#[cfg(test)]\nmod t {{\n{}{}}}\n", test("b", "g();"), test("d", "i();"));
        let one = test("c2", "h();") + &test("e", "j2();");
        crate::workspace::t::write(repo, &[("src/lib.rs", &lib), ("tests/one.rs", &one), ("notes/x.rs", "")]);
        std::fs::remove_file(repo.join("tests/gone.rs")).unwrap();
        let cfg: Config = toml::from_str(
            "[check]\ntarget = \"start\"\n[[suite]]\nname = \"rust\"\npreset = \"rust\"\nexcludes = [\"notes/**\"]\n",
        )
        .unwrap();
        let tree = crate::measure::t::tree(repo);
        let l = tree.landing(repo, &cfg.check).unwrap().unwrap();
        let got = of(repo, &cfg, &tree, l).unwrap();
        let ids: Vec<String> = got["rust"].iter().map(Removed::id).collect();
        assert_eq!(ids, ["src/lib.rs:5: a", "src/lib.rs:13: y", "tests/gone.rs:2: z"]);
    }
}
