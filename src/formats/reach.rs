use super::ReachFormat;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};

/// One test and what it ran: functions by the names the coverage report's
/// functions have, lines by the report's file paths.
#[derive(Debug, Clone, PartialEq)]
pub struct TestReach {
    /// nextest's binary id: the JUnit report's classname.
    pub binary: String,
    pub name: String,
    /// `None` when a profile of the test could not be read: what it ran is
    /// unknown, so it counts as running every function (and no line).
    pub functions: Option<BTreeSet<String>>,
    pub lines: BTreeMap<String, BTreeSet<u32>>,
}

/// What each test ran; refused when the run left tests out (slimfit#79): its
/// coverage and reach are then not the tree's, and a mutant planned from
/// them would run too few tests.
pub fn read(format: ReachFormat, texts: &[String]) -> Result<Vec<TestReach>> {
    let mut out: Vec<TestReach> = Vec::new();
    let mut not_run = Vec::new();
    for t in texts {
        tests(format, t, &mut out, &mut not_run)?;
    }
    if !not_run.is_empty() {
        return Err(crate::fault::fault(
            format!(
                "the test run stopped before every test ran, so its coverage and reach are not the tree's; not run: {}",
                not_run.join(", ")
            ),
            "let every test run: take `--fail-fast` and `--max-fail` out of the suite's `test_args` (the rust preset \
             runs every test whatever fails), then run the same slimfit command again",
        ));
    }
    Ok(out)
}

/// `test <binary>\t<name>` lines, each followed by what the test ran, in
/// `format`, or by `unreadable <what>`. A test named twice (nextest retried
/// it) ran what either run ran. A `not run <what>` line, anywhere, names a
/// test the run left out (or nextest's words on how many).
fn tests(format: ReachFormat, text: &str, out: &mut Vec<TestReach>, not_run: &mut Vec<String>) -> Result<()> {
    let mut at: Option<usize> = None;
    let mut file = String::new();
    for line in text.lines() {
        if let Some(what) = line.strip_prefix("not run ") {
            not_run.push(what.replace('\t', " "));
            continue;
        }
        if let Some(t) = line.strip_prefix("test ") {
            let Some((binary, name)) = t.split_once('\t') else { bail!("a test line without a tab: {line:?}") };
            at = Some(match out.iter().position(|r| r.binary == binary && r.name == name) {
                Some(i) => i,
                None => {
                    out.push(TestReach {
                        binary: binary.into(),
                        name: name.into(),
                        functions: Some(BTreeSet::new()),
                        lines: BTreeMap::new(),
                    });
                    out.len() - 1
                }
            });
            continue;
        }
        let Some(i) = at else { bail!("coverage before any test line: {line:?}") };
        let t = &mut out[i];
        if line.starts_with("unreadable ") {
            t.functions = None;
            continue;
        }
        match format {
            ReachFormat::Lcov => lcov(line, &mut file, t)?,
            ReachFormat::GoCover => go_cover(line, t)?,
        }
    }
    Ok(())
}

/// A record of the LCOV of what the test's profiles ran (`SF`, `FNDA` and
/// `DA`; a count of 0 is not run).
fn lcov(line: &str, file: &mut String, t: &mut TestReach) -> Result<()> {
    if let Some(f) = line.strip_prefix("SF:") {
        *file = f.to_string();
    } else if let Some((count, name)) = line.strip_prefix("FNDA:").and_then(|r| r.split_once(',')) {
        if let (true, Some(fs)) = (count != "0", t.functions.as_mut()) {
            fs.insert(name.to_string());
        }
    } else if let Some((n, count)) = line.strip_prefix("DA:").and_then(|r| r.split_once(',')) {
        let n: u32 = n.parse().with_context(|| format!("lcov DA: {line:?}"))?;
        if count.split(',').next() != Some("0") {
            t.lines.entry(file.clone()).or_default().insert(n);
        }
    }
    Ok(())
}

/// A block of the test's coverprofile: its lines ran when its count is
/// above 0. A coverprofile names no functions.
fn go_cover(line: &str, t: &mut TestReach) -> Result<()> {
    if line.starts_with("mode:") || line.trim().is_empty() {
        return Ok(());
    }
    let (file, start, end, count) = super::coverage::go_block(line)?;
    if count > 0 {
        t.lines.entry(file.to_string()).or_default().extend(start..=end);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What ran is what counted above 0; a retried test keeps what both
    /// runs ran; an unreadable profile makes the test's functions unknown.
    #[test]
    fn reads_what_each_test_ran() {
        let text = "test calc\ttests::adds\nSF:/r/a.rs\nFNDA:1,f\nFNDA:0,g\nDA:1,1\nDA:2,0\nend_of_record\n\
                    test calc::it\ttop\n\
                    test calc\ttests::adds\nSF:/r/b.rs\nFNDA:2,h\nDA:7,3\nend_of_record\n\
                    test calc\tgone\nunreadable /t/slimfit-1\n";
        let got = read(ReachFormat::Lcov, &[text.into()]).unwrap();
        let r = |b: &str, n: &str, f: Option<&[&str]>, l: &[(&str, &[u32])]| TestReach {
            binary: b.into(),
            name: n.into(),
            functions: f.map(|f| f.iter().map(|s| s.to_string()).collect()),
            lines: l.iter().map(|(f, ls)| (f.to_string(), ls.iter().copied().collect())).collect(),
        };
        assert_eq!(
            got,
            [
                r("calc", "tests::adds", Some(&["f", "h"]), &[("/r/a.rs", &[1]), ("/r/b.rs", &[7])]),
                r("calc::it", "top", Some(&[]), &[]),
                r("calc", "gone", None, &[])
            ]
        );
        assert!(read(ReachFormat::Lcov, &["SF:/r/a.rs\n".into()]).is_err());
        let cut = read(ReachFormat::Lcov, &[format!("{text}not run calc\tlater\n")]).unwrap_err();
        assert!(crate::fault::of(cut)[0].fault.ends_with("not run: calc later"));
        assert!(read(ReachFormat::Lcov, &["test a\tb\nDA:x,1\n".into()]).is_err());
    }

    /// Of each test's coverprofile, the lines of the blocks that ran.
    #[test]
    fn reads_the_lines_of_each_tests_coverprofile() {
        let text = "test m/p\tTestA\nmode: set\nm/p/a.go:3.10,5.2 2 1\nm/p/a.go:7.2,7.9 1 0\n\
                    test m/p\tTestB\nunreadable /o/each/1-TestB.cover\n";
        let got = read(ReachFormat::GoCover, &[text.into()]).unwrap();
        assert_eq!(got[0].lines, [("m/p/a.go".to_string(), [3, 4, 5].into())].into());
        assert_eq!((got[0].functions.as_ref().map(BTreeSet::len), got[1].functions.is_none()), (Some(0), true));
        assert!(read(ReachFormat::GoCover, &["test m/p\tTestA\nm/p/a.go:x 1 1\n".into()]).is_err());
    }
}
