use super::CoverageFormat;
use super::xml::{Node, walk};
use crate::model::{Coverage, Function};
use anyhow::{Context, Result};

pub fn read(format: CoverageFormat, texts: &[String]) -> Result<Coverage> {
    let mut c = Coverage::default();
    for t in texts {
        match format {
            CoverageFormat::Lcov => lcov(t, &mut c)?,
            CoverageFormat::GoCover => go_cover(t, &mut c)?,
            CoverageFormat::Jacoco => jacoco(t, &mut c)?,
        }
    }
    Ok(c)
}

fn hit(c: &mut Coverage, file: &str, line: u32, hits: u64) {
    let h = c.lines.entry(file.to_string()).or_default().entry(line).or_insert(0);
    *h = (*h).max(hits);
}

fn lcov(text: &str, c: &mut Coverage) -> Result<()> {
    let mut file = String::new();
    // FN records give a function's line; FNDA its hits, by name.
    let mut fn_lines: Vec<(String, u32)> = Vec::new();
    let mut fn_hits: std::collections::HashMap<String, u64> = Default::default();
    let flush = |file: &str,
                 fn_lines: &mut Vec<(String, u32)>,
                 fn_hits: &mut std::collections::HashMap<String, u64>,
                 c: &mut Coverage| {
        for (symbol, line) in fn_lines.drain(..) {
            let name = demangle(&symbol);
            let hits = fn_hits.get(&name).copied().unwrap_or(0);
            let symbol = if symbol == name { String::new() } else { symbol };
            c.functions.push(Function { file: file.to_string(), name, line, hits, symbol });
        }
        fn_hits.clear();
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(p) = line.strip_prefix("SF:") {
            file = p.to_string();
        } else if let Some(r) = line.strip_prefix("DA:") {
            let mut parts = r.split(',');
            let n: u32 = parts.next().unwrap_or("").parse().with_context(|| format!("lcov DA: {line}"))?;
            let h: u64 = parts.next().unwrap_or("0").parse::<f64>().unwrap_or(0.0) as u64;
            hit(c, &file, n, h);
        } else if let Some(r) = line.strip_prefix("FN:") {
            // FN:<line>,[<end line>,]<name>; names may hold commas.
            let (start, rest) = r.split_once(',').unwrap_or((r, ""));
            let rest = match rest.split_once(',') {
                Some((end, name)) if end.parse::<u32>().is_ok() => name,
                _ => rest,
            };
            if let Ok(n) = start.parse() {
                fn_lines.push((rest.to_string(), n));
            }
        } else if let Some(r) = line.strip_prefix("FNDA:") {
            if let Some((h, name)) = r.split_once(',') {
                let h = h.parse::<f64>().unwrap_or(0.0) as u64;
                *fn_hits.entry(demangle(name)).or_insert(0) += h;
            }
        } else if line == "end_of_record" {
            flush(&file, &mut fn_lines, &mut fn_hits, c);
        }
    }
    flush(&file, &mut fn_lines, &mut fn_hits, c);
    Ok(())
}

/// `file:startLine.startCol,endLine.endCol statements count`; a line's hits
/// are the most any block over it got.
/// Rust symbols come mangled in llvm-cov output; anything else passes through.
fn demangle(name: &str) -> String {
    match rustc_demangle::try_demangle(name) {
        Ok(d) => rust_name(&format!("{d:#}")),
        Err(_) => name.to_string(),
    }
}

/// A demangled Rust path as a reader of the file names it: no crate path,
/// no generic arguments, closures folded into the function they are in.
/// `<a::Grok as a::Kind>::call::<T>` is `Grok::call`;
/// `a::run::{closure#0}::{closure#0}` is `run (closure)`.
fn rust_name(path: &str) -> String {
    let segments = split_top(path, "::");
    let closure = segments.iter().any(|s| s.starts_with("{closure"));
    let mut names = segments.iter().filter(|s| !s.starts_with('<') && !s.starts_with('{')).map(|s| strip_generics(s));
    let last = names.next_back();
    let name = match (segments.first().filter(|s| s.starts_with('<')), last) {
        (Some(qself), Some(f)) => format!("{}::{f}", self_type(qself)),
        (Some(qself), None) => self_type(qself),
        (None, Some(f)) => f,
        (None, None) => path.to_string(),
    };
    if closure { format!("{name} (closure)") } else { name }
}

/// The type an `impl` block is for: `<a::B<T> as c::D>` is `B`.
fn self_type(qself: &str) -> String {
    let inner = qself.strip_prefix('<').and_then(|s| s.strip_suffix('>')).unwrap_or(qself);
    let ty = split_top(inner, " as ").into_iter().next().unwrap_or(inner);
    let ty = strip_generics(ty.trim_start_matches('&').trim_start_matches("mut "));
    split_top(&ty, "::").last().map(|s| s.to_string()).unwrap_or(ty)
}

/// `s` split at each `sep` outside angle brackets.
fn split_top<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let (mut out, mut depth, mut start, mut prev) = (Vec::new(), 0i32, 0, ' ');
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' if prev != '-' => depth -= 1,
            _ if depth == 0 && i >= start && s[i..].starts_with(sep) => {
                out.push(&s[start..i]);
                start = i + sep.len();
            }
            _ => {}
        }
        prev = ch;
    }
    out.push(&s[start..]);
    out
}

fn strip_generics(s: &str) -> String {
    let (mut out, mut depth, mut prev) = (String::new(), 0i32, ' ');
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' if prev != '-' => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
        prev = ch;
    }
    out
}

fn go_cover(text: &str, c: &mut Coverage) -> Result<()> {
    for line in text.lines().filter(|l| !l.starts_with("mode:") && !l.trim().is_empty()) {
        let (file, start, end, count) = go_block(line)?;
        for n in start..=end {
            hit(c, file, n, count);
        }
    }
    Ok(())
}

/// One block of a Go coverprofile, `<file>:<line>.<col>,<line>.<col>
/// <statements> <count>`: its file, first and last line, and count.
pub(super) fn go_block(line: &str) -> Result<(&str, u32, u32, u64)> {
    let parse = || -> Option<(&str, u32, u32, u64)> {
        let (file, rest) = line.rsplit_once(':')?;
        let mut parts = rest.split(' ');
        let range = parts.next()?;
        let count = parts.nth(1)?.parse().ok()?;
        let (start, end) = range.split_once(',')?;
        let start = start.split('.').next()?.parse().ok()?;
        let end = end.split('.').next()?.parse().ok()?;
        Some((file, start, end, count))
    };
    parse().with_context(|| format!("coverprofile line: {line}"))
}

/// Lines from `<sourcefile><line nr ci/>`; functions from `<method>` with its
/// METHOD counter. Paths are `package/sourcefile`.
fn jacoco(text: &str, c: &mut Coverage) -> Result<()> {
    let mut package = String::new();
    let mut source = String::new();
    let mut class_source = String::new();
    let mut method: Option<(String, u32)> = None;
    walk(text, |node| match node {
        Node::Open { name, attrs } => match name.as_str() {
            "package" => package = attrs.get("name").cloned().unwrap_or_default(),
            "class" => class_source = attrs.get("sourcefilename").cloned().unwrap_or_default(),
            "method" => {
                let line = attrs.get("line").and_then(|l| l.parse().ok()).unwrap_or(0);
                method = Some((attrs.get("name").cloned().unwrap_or_default(), line));
            }
            "counter" if attrs.get("type").map(String::as_str) == Some("METHOD") => {
                if let Some((name, line)) = method.take() {
                    let covered: u64 = attrs.get("covered").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let file = join(&package, &class_source);
                    if line > 0 {
                        c.functions.push(Function { file, name, line, hits: covered, symbol: String::new() });
                    }
                }
            }
            "sourcefile" => source = attrs.get("name").cloned().unwrap_or_default(),
            "line" => {
                let nr = attrs.get("nr").and_then(|v| v.parse().ok());
                let ci: u64 = attrs.get("ci").and_then(|v| v.parse().ok()).unwrap_or(0);
                if let Some(nr) = nr {
                    hit(c, &join(&package, &source), nr, ci);
                }
            }
            _ => {}
        },
        Node::Close(name) if name == "method" => method = None,
        _ => {}
    })
}

fn join(package: &str, file: &str) -> String {
    if package.is_empty() { file.to_string() } else { format!("{package}/{file}") }
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn lcov_lines_and_functions() {
        let l = "SF:/r/src/a.rs\nFN:3,5,f<a, b>\nFN:9,g\nFNDA:2,f<a, b>\nFNDA:0,g\nDA:3,2\nDA:4,0\nend_of_record\n";
        let c = read(CoverageFormat::Lcov, &[l.into()]).unwrap();
        assert_eq!(c.lines["/r/src/a.rs"][&4], 0);
        assert_eq!(
            c.functions,
            vec![
                Function {
                    file: "/r/src/a.rs".into(),
                    name: "f<a, b>".into(),
                    line: 3,
                    hits: 2,
                    symbol: String::new()
                },
                Function { file: "/r/src/a.rs".into(), name: "g".into(), line: 9, hits: 0, symbol: String::new() },
            ]
        );
    }

    #[test]
    fn rust_names_drop_generics_crate_paths_and_closures() {
        let cases = [
            ("calc::tests::adds", "adds"),
            (
                "<factorium_harness::harness::grok::Grok as factorium_harness::harness::Kind>::call::<&mut serde_json::de::Deserializer<serde_json::read::StrRead>>",
                "Grok::call",
            ),
            ("factorium::run::{closure#0}::{closure#0}", "run (closure)"),
            ("<a::B<fn() -> u8>>::new", "B::new"),
            ("<&mut a::W as core::fmt::Write>::write_str::{closure#0}", "W::write_str (closure)"),
        ];
        for (path, name) in cases {
            assert_eq!(rust_name(path), name, "{path}");
        }
    }

    #[test]
    fn go_cover_takes_max_over_blocks() {
        let p = "mode: set\nm/p/a.go:3.10,5.2 2 1\nm/p/a.go:5.2,7.3 1 0\n";
        let c = read(CoverageFormat::GoCover, &[p.into()]).unwrap();
        let f = &c.lines["m/p/a.go"];
        assert_eq!((f[&3], f[&5], f[&7]), (1, 1, 0));
    }

    #[test]
    fn jacoco_paths_lines_methods() {
        let x = r#"<report name="r"><package name="com/x">
          <class name="com/x/A" sourcefilename="A.kt">
            <method name="f" desc="()V" line="4"><counter type="INSTRUCTION" missed="3" covered="0"/><counter type="METHOD" missed="1" covered="0"/></method>
          </class>
          <sourcefile name="A.kt"><line nr="4" mi="3" ci="0" mb="0" cb="0"/><line nr="5" mi="0" ci="2" mb="0" cb="0"/></sourcefile>
        </package></report>"#;
        let c = read(CoverageFormat::Jacoco, &[x.into()]).unwrap();
        assert_eq!(c.lines["com/x/A.kt"][&5], 2);
        assert_eq!(
            c.functions,
            vec![Function { file: "com/x/A.kt".into(), name: "f".into(), line: 4, hits: 0, symbol: String::new() }]
        );
    }
}
