//! The version `slimfit --version` says. build.rs includes this file and
//! stamps its result into the binary as `SLIMFIT_VERSION`.

/// The version to stamp: the release the release script names
/// (`SLIMFIT_RELEASE`), as is; otherwise the development form
/// `1.2.3+<commits>.g<commit>[.dirty]`, read from
/// `git describe --tags --long --dirty` (`v1.2.3-4-gabc1234-dirty`); otherwise,
/// outside a repository, `fallback` (Cargo.toml's version).
pub fn stamp(release: Option<&str>, describe: Option<&str>, fallback: &str) -> String {
    if let Some(release) = release {
        return release.to_string();
    }
    describe.and_then(dev_form).unwrap_or_else(|| fallback.to_string())
}

fn dev_form(describe: &str) -> Option<String> {
    let (rest, dirty) = match describe.strip_suffix("-dirty") {
        Some(rest) => (rest, ".dirty"),
        None => (describe, ""),
    };
    let mut parts = rest.rsplitn(3, '-');
    let (commit, commits, tag) = (parts.next()?, parts.next()?, parts.next()?);
    Some(format!("{}+{commits}.{commit}{dirty}", tag.strip_prefix('v')?))
}

#[cfg(test)]
mod tests {
    use super::stamp;

    #[test]
    fn a_release_says_its_version_and_anything_else_says_where_it_was_built() {
        let describe = Some("v1.2.3-4-gabc1234");
        assert_eq!(stamp(Some("1.2.4"), describe, "0.1.0"), "1.2.4");
        assert_eq!(stamp(None, describe, "0.1.0"), "1.2.3+4.gabc1234");
        let dirty = Some("v1.2.3-0-gabc1234-dirty");
        assert_eq!(stamp(None, dirty, "0.1.0"), "1.2.3+0.gabc1234.dirty");
        assert_eq!(stamp(None, None, "0.1.0"), "0.1.0");
        assert_eq!(stamp(None, Some("gabc1234"), "0.1.0"), "0.1.0");
        assert_eq!(stamp(None, Some("x-4-gabc1234"), "0.1.0"), "0.1.0");
    }
}
