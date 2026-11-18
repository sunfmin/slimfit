//! Stamps `SLIMFIT_VERSION`: see src/version.rs.

#[path = "src/version.rs"]
mod version;

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=SLIMFIT_RELEASE");
    // A new commit, tag or checkout, or an edit, changes the development form.
    for path in ["src", "build.rs", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for dir in [git(&["rev-parse", "--git-dir"]), git(&["rev-parse", "--git-common-dir"])].into_iter().flatten() {
        for file in ["HEAD", "index", "packed-refs", "refs"] {
            println!("cargo:rerun-if-changed={dir}/{file}");
        }
    }
    let release = std::env::var("SLIMFIT_RELEASE").ok();
    let describe = git(&["describe", "--tags", "--long", "--dirty", "--match", "v[0-9]*"]);
    let fallback = std::env::var("CARGO_PKG_VERSION").unwrap();
    println!("cargo:rustc-env=SLIMFIT_VERSION={}", version::stamp(release.as_deref(), describe.as_deref(), &fallback));
}
