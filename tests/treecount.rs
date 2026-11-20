//! slimfit#66's counter (`examples/treecount`) counts the instructions of a
//! command's whole process tree, not only of the process it starts: macOS
//! adds no child's instructions to its parent.

use std::path::{Path, PathBuf};
use std::process::Command;

fn treecount() -> PathBuf {
    let p = Path::new(env!("CARGO_BIN_EXE_slimfit")).parent().unwrap().join("examples/treecount");
    assert!(p.exists(), "no counter at {}: `cargo build --examples`", p.display());
    p
}

/// The counter's row for `sh -c script`: instructions, counted CPU, tree CPU, processes.
fn count(script: &str) -> (u64, u64, u64, u64) {
    let out = tempfile::tempdir().unwrap();
    let tsv = out.path().join("out.tsv");
    let st = Command::new(treecount()).env("TREECOUNT_ID", "t").arg(&tsv).args(["sh", "-c", script]).status().unwrap();
    assert!(st.success());
    let row = std::fs::read_to_string(&tsv).unwrap();
    let f: Vec<&str> = row.trim_end().split('\t').collect();
    assert_eq!(f[0], "t", "{row}");
    let n = |i: usize| f[i].parse::<u64>().unwrap();
    (n(1), n(3), n(4), n(5))
}

#[test]
fn a_childs_work_is_in_its_ancestors_count() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work.sh");
    std::fs::write(&work, "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done").unwrap();
    let (alone, ..) = count(&format!("sh {}", work.display()));
    // The work two shells down, one of them run with an empty environment
    // (as a test that clears it would): still counted. A shell with no
    // environment does less of its own, so not quite as much; uncounted,
    // it would be the shells' few tens of millions.
    let (nested, counted, tree, procs) = count(&format!("sh -c 'env -i /bin/sh {}; true'; true", work.display()));
    assert!(alone > 100_000_000, "{alone}");
    assert!(nested > alone / 2, "nested {nested} against {alone} alone");
    assert!(procs >= 2, "{procs} processes");
    // Every process the kernel added to the tree's CPU time was counted.
    assert!(counted as f64 > tree as f64 * 0.95, "counted {counted} ns of {tree}");
}
