use super::ReachFormat;
use anyhow::{Result, bail};
use std::collections::BTreeSet;

/// One test and the functions it ran, by the names the coverage report's
/// functions have.
#[derive(Debug, Clone, PartialEq)]
pub struct TestReach {
    /// nextest's binary id: the JUnit report's classname.
    pub binary: String,
    pub name: String,
    /// `None` when a profile of the test could not be read: what it ran is
    /// unknown, so it counts as running every function.
    pub functions: Option<BTreeSet<String>>,
}

pub fn read(format: ReachFormat, texts: &[String]) -> Result<Vec<TestReach>> {
    let mut out: Vec<TestReach> = Vec::new();
    for t in texts {
        match format {
            ReachFormat::LlvmProfdata => llvm_profdata(t, &mut out)?,
        }
    }
    Ok(out)
}

/// `test <binary>\t<name>` lines, each followed by `llvm-profdata show
/// --all-functions --counts` of every profile the test wrote (or
/// `unreadable <file>`). A function ran when any of its counters is above 0.
/// A test named twice (nextest retried it) ran what either run ran.
fn llvm_profdata(text: &str, out: &mut Vec<TestReach>) -> Result<()> {
    let mut at: Option<usize> = None;
    let mut function = String::new();
    for line in text.lines() {
        if let Some(t) = line.strip_prefix("test ") {
            let Some((binary, name)) = t.split_once('\t') else { bail!("a test line without a tab: {line:?}") };
            at = Some(match out.iter().position(|r| r.binary == binary && r.name == name) {
                Some(i) => i,
                None => {
                    out.push(TestReach { binary: binary.into(), name: name.into(), functions: Some(BTreeSet::new()) });
                    out.len() - 1
                }
            });
            continue;
        }
        let Some(i) = at else { bail!("profile data before any test line: {line:?}") };
        if line.starts_with("unreadable ") {
            out[i].functions = None;
        } else if let Some(name) =
            line.strip_prefix("  ").and_then(|l| l.strip_suffix(':')).filter(|n| !n.starts_with(' '))
        {
            function = name.to_string();
        } else if let Some(counts) = line
            .strip_prefix("    Function count: ")
            .map(|c| c.to_string())
            .or_else(|| line.strip_prefix("    Block counts: ").map(|c| c.trim_matches(['[', ']']).to_string()))
        {
            let ran = counts.split(',').any(|c| c.trim().parse::<u64>().is_ok_and(|n| n > 0));
            if let (true, Some(f)) = (ran, out[i].functions.as_mut()) {
                f.insert(function.clone());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A function ran when its entry or any block counted; a retried test
    /// keeps what both runs ran; an unreadable profile makes the test's
    /// reach unknown.
    #[test]
    fn reads_what_each_test_ran() {
        let text = "test calc\ttests::adds\nCounters:\n  f:\n    Hash: 0x1\n    Counters: 2\n    Function count: 1\n    Block counts: [0]\n  \
                    g:\n    Hash: 0x2\n    Counters: 3\n    Function count: 0\n    Block counts: [0, 4]\n  h:\n    Hash: 0x3\n    \
                    Counters: 1\n    Function count: 0\n    Block counts: []\nInstrumentation level: Front-end\nTotal functions: 3\n\
                    test calc::it\ttop\ntest calc\ttests::adds\nCounters:\n  h:\n    Function count: 2\n\
                    test calc\tgone\nunreadable /t/slimfit-1-2-3.profraw\n";
        let got = read(ReachFormat::LlvmProfdata, &[text.into()]).unwrap();
        let r = |b: &str, n: &str, f: Option<&[&str]>| TestReach {
            binary: b.into(),
            name: n.into(),
            functions: f.map(|f| f.iter().map(|s| s.to_string()).collect()),
        };
        assert_eq!(
            got,
            [
                r("calc", "tests::adds", Some(&["f", "g", "h"])),
                r("calc::it", "top", Some(&[])),
                r("calc", "gone", None)
            ]
        );
        assert!(read(ReachFormat::LlvmProfdata, &["Counters:\n".into()]).is_err());
    }
}
