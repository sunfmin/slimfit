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
            current = Some((TestCase { id, seconds, passed: true, cpu: 0.0 }, false));
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
        seen.insert(id.clone(), TestCase { id, seconds, passed, cpu: 0.0 });
    }
    out.extend(seen.into_values());
    Ok(())
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
                TestCase { id: "a::ok".into(), seconds: 0.5, passed: true, cpu: 0.0 },
                TestCase { id: "a::bad".into(), seconds: 1.25, passed: false, cpu: 0.0 },
            ]
        );
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
