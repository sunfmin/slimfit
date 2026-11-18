use super::TestsFormat;
use super::xml::{Node, walk};
use crate::model::TestCase;
use anyhow::{Context, Result};
use std::collections::BTreeMap;

pub fn read(format: TestsFormat, texts: &[String]) -> Result<Vec<TestCase>> {
    let mut out = Vec::new();
    for t in texts {
        match format {
            TestsFormat::Junit => junit(t, &mut out)?,
            TestsFormat::GoTestJson => go_test_json(t, &mut out)?,
        }
    }
    Ok(out)
}

/// Skipped test cases are left out: they neither cost time nor test anything.
fn junit(text: &str, out: &mut Vec<TestCase>) -> Result<()> {
    let mut current: Option<(TestCase, bool)> = None;
    walk(text, |node| match node {
        Node::Open { name, attrs } if name == "testcase" => {
            let n = attrs.get("name").cloned().unwrap_or_default();
            let id = match attrs.get("classname").filter(|c| !c.is_empty()) {
                Some(c) => format!("{c}::{n}"),
                None => n,
            };
            let seconds = attrs.get("time").and_then(|t| t.replace(',', "").parse().ok()).unwrap_or(0.0);
            let start = attrs.get("timestamp").and_then(|t| epoch_seconds(t));
            current = Some((TestCase { id, seconds, passed: true, start }, false));
        }
        Node::Open { name, .. } if name == "failure" || name == "error" => {
            if let Some((t, _)) = current.as_mut() {
                t.passed = false;
            }
        }
        Node::Open { name, .. } if name == "skipped" => {
            if let Some((_, skipped)) = current.as_mut() {
                *skipped = true;
            }
        }
        Node::Close(name) if name == "testcase" => {
            if let Some((t, skipped)) = current.take()
                && !skipped
            {
                out.push(t);
            }
        }
        _ => {}
    })
}

/// Top-level tests only: a subtest's time is already inside its parent's.
fn go_test_json(text: &str, out: &mut Vec<TestCase>) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Event {
        #[serde(rename = "Action")]
        action: String,
        #[serde(rename = "Package", default)]
        package: String,
        #[serde(rename = "Test")]
        test: Option<String>,
        #[serde(rename = "Elapsed")]
        elapsed: Option<f64>,
        #[serde(rename = "Time")]
        time: Option<String>,
    }
    let mut seen = BTreeMap::new();
    for line in text.lines().filter(|l| l.starts_with('{')) {
        let e: Event = serde_json::from_str(line).with_context(|| format!("go test -json line: {line}"))?;
        let Some(test) = e.test.filter(|t| !t.contains('/')) else { continue };
        let passed = match e.action.as_str() {
            "pass" => true,
            "fail" => false,
            _ => continue,
        };
        let id = format!("{}::{}", e.package, test);
        let seconds = e.elapsed.unwrap_or(0.0);
        // The event is the test's end.
        let start = e.time.as_deref().and_then(epoch_seconds).map(|end| end - seconds);
        seen.insert(id.clone(), TestCase { id, seconds, passed, start });
    }
    out.extend(seen.into_values());
    Ok(())
}

/// An RFC 3339 time (`2026-11-18T09:47:49.639+08:00`, or with `Z`) in
/// seconds since the Unix epoch.
fn epoch_seconds(s: &str) -> Option<f64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (clock, offset) = match time.strip_suffix('Z') {
        Some(c) => (c, 0),
        None => {
            let i = time.rfind(['+', '-'])?;
            let (h, mi) = time[i + 1..].split_once(':')?;
            let off = h.parse::<i64>().ok()? * 3600 + mi.parse::<i64>().ok()? * 60;
            (&time[..i], if time[i..].starts_with('-') { -off } else { off })
        }
    };
    let mut c = clock.splitn(3, ':');
    let (hh, mm) = (c.next()?.parse::<i64>().ok()?, c.next()?.parse::<i64>().ok()?);
    let ss: f64 = c.next()?.parse().ok()?;
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hh * 3600 + mm * 60 - offset) as f64 + ss)
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn junit_reads_outcomes_and_skips() {
        let x = r#"<testsuites><testsuite name="s">
            <testcase classname="a" name="ok" time="0.5"/>
            <testcase classname="a" name="bad" time="1.25"><failure message="x">boom</failure></testcase>
            <testcase name="skip" time="0"><skipped/></testcase>
        </testsuite></testsuites>"#;
        let t = read(TestsFormat::Junit, &[x.into()]).unwrap();
        assert_eq!(
            t,
            vec![
                TestCase { id: "a::ok".into(), seconds: 0.5, passed: true, start: None },
                TestCase { id: "a::bad".into(), seconds: 1.25, passed: false, start: None },
            ]
        );
    }

    /// The reports' times, as nextest, Gradle and go test write them.
    #[test]
    fn times_read_as_seconds_since_the_epoch() {
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(epoch_seconds("2026-11-18T09:47:49.5+08:00"), Some(1_794_966_469.5));
        assert_eq!(epoch_seconds("2000-02-29T07:00:00-05:00"), Some(951_825_600.0));
        assert_eq!(epoch_seconds("yesterday"), None);
        let j = r#"<testcase name="n" time="0.25" timestamp="2026-11-18T01:47:49Z"/>"#;
        assert_eq!(read(TestsFormat::Junit, &[j.into()]).unwrap()[0].start, Some(1_794_966_469.0));
        let g = r#"{"Time":"2026-11-18T01:47:50Z","Action":"pass","Package":"p","Test":"T","Elapsed":0.5}"#;
        assert_eq!(read(TestsFormat::GoTestJson, &[g.into()]).unwrap()[0].start, Some(1_794_966_469.5));
    }

    #[test]
    fn go_json_keeps_top_level_tests() {
        let j = r#"{"Action":"run","Package":"m/p","Test":"TestA"}
{"Action":"pass","Package":"m/p","Test":"TestA/sub","Elapsed":0.1}
{"Action":"pass","Package":"m/p","Test":"TestA","Elapsed":0.2}
{"Action":"fail","Package":"m/p","Test":"TestB","Elapsed":0}
{"Action":"pass","Package":"m/p","Elapsed":0.3}"#;
        let t = read(TestsFormat::GoTestJson, &[j.into()]).unwrap();
        assert_eq!(t.len(), 2);
        assert!(t[0].passed && t[0].id == "m/p::TestA" && t[0].seconds == 0.2);
        assert!(!t[1].passed);
    }
}
