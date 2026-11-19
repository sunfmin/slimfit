//! Which crates of a Cargo workspace are test code: a member that other
//! members reach only through `[dev-dependencies]` (slimfit#48). Cargo's
//! manifests already say so; slimfit reads them rather than ask for a
//! second declaration in `slimfit.toml`.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use toml::{Table, Value};

/// The directories (relative to `root`, `/`-separated) of the workspace
/// members at `root` that some member lists in `[dev-dependencies]` and none
/// in `[dependencies]` or `[build-dependencies]`. Empty when `root/Cargo.toml`
/// is not a workspace, or cannot be read.
pub fn test_crates(root: &Path) -> Vec<String> {
    let Some(manifest) = read(&root.join("Cargo.toml")) else { return vec![] };
    let Some(ws) = manifest.get("workspace").and_then(Value::as_table) else { return vec![] };
    let shared = ws.get("dependencies").and_then(Value::as_table);
    let mut members = members(root, ws);
    if manifest.contains_key("package") {
        members.insert(PathBuf::new());
    }
    let (mut prod, mut dev) = (BTreeSet::new(), BTreeSet::new());
    for m in &members {
        let Some(t) = read(&root.join(m).join("Cargo.toml")) else { continue };
        for (kind, deps) in dep_tables(&t) {
            for (name, dep) in deps {
                let Some(path) = dep_path(m, name, dep, shared) else { continue };
                if members.contains(&path) {
                    if kind == "dev" { &mut dev } else { &mut prod }.insert(path);
                }
            }
        }
    }
    dev.difference(&prod)
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
        .collect()
}

fn read(path: &Path) -> Option<Table> {
    std::fs::read_to_string(path).ok()?.parse().ok()
}

/// `[workspace] members`, globs expanded, less `exclude`: directories with a
/// `Cargo.toml`, relative to `root`.
fn members(root: &Path, ws: &Table) -> BTreeSet<PathBuf> {
    let list = |key: &str| -> Vec<String> {
        ws.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(normal).collect()
    };
    let exclude = list("exclude");
    let mut out = BTreeSet::new();
    for pattern in list("members") {
        let mut dirs = vec![PathBuf::new()];
        for part in pattern.split('/').filter(|p| !p.is_empty()) {
            let Ok(glob) = globset::Glob::new(part) else { return out };
            let glob = glob.compile_matcher();
            dirs = dirs
                .iter()
                .flat_map(|d| {
                    let entries = std::fs::read_dir(root.join(d)).into_iter().flatten().flatten();
                    entries
                        .filter(|e| e.path().is_dir() && glob.is_match(e.file_name()))
                        .map(|e| d.join(e.file_name()))
                        .collect::<Vec<_>>()
                })
                .collect();
        }
        out.extend(dirs.into_iter().filter(|d| root.join(d).join("Cargo.toml").is_file()));
    }
    out.retain(|d| !exclude.iter().any(|e| Path::new(e) == d));
    out
}

/// Each dependency table of a manifest, `[target.<cfg>.*]` ones too, with
/// its kind: "dev", or "prod" for `[dependencies]` and `[build-dependencies]`.
fn dep_tables(t: &Table) -> Vec<(&'static str, &Table)> {
    fn of(t: &Table) -> Vec<(&'static str, &Table)> {
        [
            ("dependencies", "prod"),
            ("build-dependencies", "prod"),
            ("build_dependencies", "prod"),
            ("dev-dependencies", "dev"),
            ("dev_dependencies", "dev"),
        ]
        .iter()
        .filter_map(|(key, kind)| Some((*kind, t.get(*key)?.as_table()?)))
        .collect()
    }
    let mut out = of(t);
    let targets = t.get("target").and_then(Value::as_table);
    for target in targets.into_iter().flat_map(|ts| ts.values()).filter_map(Value::as_table) {
        out.extend(of(target));
    }
    out
}

/// The directory (relative to the workspace root) a dependency of `member`
/// points at with `path`, directly or through `workspace = true`.
fn dep_path(member: &Path, name: &str, dep: &Value, shared: Option<&Table>) -> Option<PathBuf> {
    let dep = dep.as_table()?;
    if let Some(p) = dep.get("path").and_then(Value::as_str) {
        return Some(PathBuf::from(normal(&member.join(p).to_string_lossy())));
    }
    if dep.get("workspace").and_then(Value::as_bool) == Some(true) {
        let p = shared?.get(name)?.as_table()?.get("path")?.as_str()?;
        return Some(PathBuf::from(normal(p)));
    }
    None
}

/// `path` without `.` and with each `x/..` folded away.
fn normal(path: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for c in Path::new(path).components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s.to_string_lossy().into()),
            _ => {}
        }
    }
    out.join("/")
}

#[cfg(test)]
pub mod t {
    use super::*;

    pub fn write(dir: &Path, files: &[(&str, &str)]) {
        for (f, text) in files {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    /// A workspace with a prod crate, a dev-only test-support crate, and a
    /// crate that is both a dev- and a build-dependency.
    pub fn workspace(dir: &Path) {
        write(
            dir,
            &[
                (
                    "Cargo.toml",
                    "[workspace]\nmembers = [\"crates/*\", \"app\"]\nexclude = [\"crates/old\"]\n\
                     [workspace.dependencies]\nsupport = { path = \"crates/support\" }\n\
                     both = { path = \"crates/both\" }\n",
                ),
                (
                    "app/Cargo.toml",
                    "[package]\nname = \"app\"\n[dependencies]\ncore = { path = \"../crates/core\" }\n\
                     serde = \"1\"\n[dev-dependencies]\nsupport = { workspace = true }\n\
                     [target.'cfg(unix)'.dev-dependencies]\nboth = { workspace = true }\n",
                ),
                // `both` is a dev-dependency of app and, through `../`, a build-dependency of core: prod.
                (
                    "crates/core/Cargo.toml",
                    "[package]\nname = \"core\"\n[build-dependencies]\nboth = { path = \"../both\" }\n",
                ),
                (
                    "crates/support/Cargo.toml",
                    "[package]\nname = \"support\"\n[dependencies]\ncore = { path = \"../core\" }\n",
                ),
                ("crates/both/Cargo.toml", "[package]\nname = \"both\"\n"),
                // Excluded: its dependency on support counts for nothing.
                (
                    "crates/old/Cargo.toml",
                    "[package]\nname = \"old\"\n[dependencies]\nsupport = { path = \"../support\" }\n",
                ),
            ],
        );
    }

    #[test]
    fn a_member_reached_only_through_dev_dependencies_is_a_test_crate() {
        let d = tempfile::tempdir().unwrap();
        workspace(d.path());
        assert_eq!(test_crates(d.path()), ["crates/support"]);
    }

    #[test]
    fn a_single_crate_has_no_test_crates() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), &[("Cargo.toml", "[package]\nname = \"a\"\n[dev-dependencies]\nb = { path = \"b\" }\n")]);
        assert!(test_crates(d.path()).is_empty());
        assert!(test_crates(&d.path().join("missing")).is_empty());
    }
}
