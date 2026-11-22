//! Rust source, read as slimfit needs it: items and where they end, `fn`s
//! and their names, `#[cfg(test)]` items and modules, and const-ness. The
//! one place that reads Rust syntax; the others ask it.
//!
//! An item ends at the brace that closes its first `{`, or at a `;` before
//! any `{` (`use x;`, `mod t;`), braces in strings, char literals and `//`
//! comments not counting: one rule, whatever shape the item is written in.

use std::collections::BTreeMap;

/// The 0-based index of the line item `lines[start..]` ends on: the brace
/// that closes its first `{`, or a `;` before any `{`. `None` when it does
/// not end.
pub fn item_end(lines: &[&str], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut opened = false;
    for (j, line) in lines.iter().enumerate().skip(start) {
        for ch in strip_strings(line).chars() {
            match ch {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                ';' if !opened => return Some(j),
                _ => {}
            }
            if opened && depth == 0 {
                return Some(j);
            }
        }
    }
    None
}

/// Line ranges (1-based, inclusive) of `#[cfg(test)]` items: from the
/// attribute to where the item it is on ends (or the last line).
pub fn test_blocks(text: &str) -> Vec<(u32, u32)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            let end = item_end(&lines, i).unwrap_or(lines.len() - 1);
            out.push((i as u32 + 1, end as u32 + 1));
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// The files of modules `path` declares as `#[cfg(test)] mod name;`.
pub fn test_module_files(path: &str, text: &str) -> Vec<String> {
    let (dir, file) = path.rsplit_once('/').map(|(d, f)| (format!("{d}/"), f)).unwrap_or((String::new(), path));
    let base = match file {
        "main.rs" | "lib.rs" | "mod.rs" => dir,
        _ => format!("{dir}{}/", file.trim_end_matches(".rs")),
    };
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if !l.starts_with("#[cfg(test)]") {
            continue;
        }
        let rest = l.trim_start_matches("#[cfg(test)]").trim();
        let decl = if rest.is_empty() { lines.get(i + 1).copied().unwrap_or("") } else { rest };
        let decl = decl.trim_start_matches("pub(crate) ").trim_start_matches("pub ");
        if let Some(name) = decl.strip_prefix("mod ").and_then(|d| d.strip_suffix(';')) {
            out.push(format!("{base}{}.rs", name.trim()));
            out.push(format!("{base}{}/mod.rs", name.trim()));
        }
    }
    out
}

/// The name a line declares a function of: `fn x(`, after any of `pub`,
/// `async`, `unsafe`, `const`, `extern`.
pub fn fn_name(line: &str) -> Option<&str> {
    let mut words = line.split_whitespace();
    let rest = loop {
        match words.next()? {
            "fn" => break words.next()?,
            w if w.starts_with("pub") || ["async", "unsafe", "const", "extern"].contains(&w) => {}
            _ => return None,
        }
    };
    let name = &rest[..rest.find(['(', '<']).unwrap_or(rest.len())];
    (!name.is_empty()).then_some(name)
}

/// Every `fn` of `files`, read through `read`, by name: where each is
/// (file, 1-based line).
pub fn functions<'a>(
    files: impl IntoIterator<Item = &'a String>,
    read: impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, Vec<(String, u32)>> {
    let mut out: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
    for file in files {
        let Some(text) = read(file) else { continue };
        for (i, line) in text.lines().enumerate() {
            if let Some(name) = fn_name(line) {
                out.entry(name.to_string()).or_default().push((file.clone(), i as u32 + 1));
            }
        }
    }
    out
}

/// The test functions of `text`: each `fn` under an attribute named `test`
/// (`#[test]`, `#[tokio::test(...)]`) or `rstest`, other attributes and
/// comments between them allowed; by its 1-based line and name.
pub fn test_fns(text: &str) -> Vec<(u32, &str)> {
    let mut out = Vec::new();
    let mut test = false;
    for (i, line) in text.lines().enumerate() {
        let l = line.trim_start();
        if let Some(attr) = l.strip_prefix("#[") {
            let path = &attr[..attr.find(['(', ']']).unwrap_or(attr.len())];
            test |= matches!(path.trim().rsplit("::").next(), Some("test" | "rstest"));
        } else if let Some(name) = fn_name(l).filter(|_| test) {
            out.push((i as u32 + 1, name));
            test = false;
        } else if !l.starts_with("//") && !l.is_empty() {
            test = false;
        }
    }
    out
}

/// Where test `name` (nextest's: `module::path::function`) is: the one `fn`
/// of its last segment; of several, the one whose file's path names the
/// most of its modules (`queue::x` in `tests/scenarios/queue.rs`).
pub fn test_function<'a>(fns: &'a BTreeMap<String, Vec<(String, u32)>>, name: &str) -> Option<(&'a str, u32)> {
    let mut segments: Vec<&str> = name.split("::").collect();
    let found = fns.get(segments.pop()?)?;
    let named = |file: &str| {
        let parts: Vec<&str> = file.trim_end_matches(".rs").split('/').collect();
        segments.iter().filter(|m| parts.contains(m)).count()
    };
    let best = found.iter().map(|(f, _)| named(f)).max()?;
    match found.iter().filter(|(f, _)| named(f) == best).collect::<Vec<_>>().as_slice() {
        [(file, line)] => Some((file, *line)),
        _ => None,
    }
}

/// The text of the item whose first line is `line` (1-based) of `text`: to
/// where it ends ([`item_end`]). `None` when there is no such line or the
/// item does not end.
pub fn item_text(text: &str, line: u32) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = line.checked_sub(1)? as usize;
    lines.get(start)?;
    Some(lines[start..=item_end(&lines, start)?].join("\n"))
}

/// Whether the function in lines `start..=end` is a `const fn`: one
/// evaluated while compiling, which no test's profile shows running.
pub fn is_const(source: &str, start: u32, end: u32) -> bool {
    let lines = source.lines().skip(start.saturating_sub(1) as usize).take((end + 1).saturating_sub(start) as usize);
    let code: String = lines
        .map(str::trim_start)
        .filter(|l| !l.starts_with("//") && !l.starts_with("#["))
        .collect::<Vec<_>>()
        .join(" ");
    let signature = code.split('{').next().unwrap_or("");
    let words: Vec<&str> = signature.split_whitespace().collect();
    words.windows(2).any(|w| w[0] == "const" && ["fn", "unsafe", "extern", "async"].contains(&w[1]))
}

/// The line without string/char literals and `//` comments, so braces in
/// them don't count.
fn strip_strings(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
        } else {
            match c {
                '"' => in_str = true,
                '\'' => {
                    // a char literal like '{' or '\''; a lifetime has no closing quote nearby
                    let rest: String = chars.clone().take(3).collect();
                    if rest.starts_with('\\') {
                        chars.next();
                        chars.next();
                        chars.next();
                    } else if rest.chars().nth(1) == Some('\'') {
                        chars.next();
                        chars.next();
                    }
                }
                '/' if chars.peek() == Some(&'/') => break,
                _ => out.push(c),
            }
        }
    }
    out
}

#[cfg(test)]
mod t {
    use super::*;

    /// An item ends where its braces close, braces in strings, chars and
    /// comments not counting; a `#[cfg(test)]` item without a body ends at
    /// its `;`, on its own line or the attribute's.
    #[test]
    fn an_item_ends_where_its_braces_close_or_at_its_semicolon() {
        let src = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    // c\n    fn s() { let _ = \"}\"; let _ = '{'; let _ = '\\\"'; }\n}\n/// doc\nfn b() {}\n";
        assert_eq!(test_blocks(src), vec![(3, 7)]);
        assert_eq!(test_blocks("#[cfg(test)]\nuse x::y;\nfn a() {}\n"), vec![(1, 2)]);
        assert_eq!(test_blocks("#[cfg(test)] mod t;\nfn x() {}\n"), vec![(1, 1)]);
        assert_eq!(test_blocks("#[cfg(test)]\nmod t {\n"), vec![(1, 2)], "unclosed: to the last line");
    }

    /// A test is found by its function's name; of two of that name, by the
    /// modules its path names; when that does not tell them apart, not at all.
    #[test]
    fn a_test_is_found_by_its_name_and_modules() {
        assert_eq!(fn_name("    pub(crate) async fn x<T>(t: T) {"), Some("x"));
        assert_eq!(fn_name("    let fn_x = 1;"), None);
        let at = |f: &str, l| (f.to_string(), l);
        let fns: BTreeMap<String, Vec<(String, u32)>> = [
            ("a".to_string(), vec![at("src/lib.rs", 3)]),
            ("b".to_string(), vec![at("tests/s/queue.rs", 5), at("tests/s/line.rs", 9)]),
        ]
        .into();
        assert_eq!(test_function(&fns, "tests::a"), Some(("src/lib.rs", 3)));
        assert_eq!(test_function(&fns, "line::b"), Some(("tests/s/line.rs", 9)));
        assert_eq!(test_function(&fns, "b"), None);
        assert_eq!(test_function(&fns, "c"), None);
    }

    /// A test is a `fn` under `#[test]`, a path ending in `test`, or
    /// `rstest`, through other attributes and comments; not a `fn` after
    /// other code, nor a `#[cfg(test)]` module.
    #[test]
    fn a_test_fn_is_one_under_a_test_attribute() {
        let src = "#[cfg(test)]\nmod t {\n    #[test]\n    fn a() {}\n    #[tokio::test(flavor = \"x\")]\n    // c\n    \
                   #[ignore]\n    async fn b() {}\n    #[test]\n    struct S;\n    fn c() {}\n    #[rstest]\n    fn d() {}\n    \
                   #[testing]\n    fn e() {}\n}\n";
        assert_eq!(test_fns(src), [(4, "a"), (8, "b"), (13, "d")]);
    }

    #[test]
    fn a_const_fn_is_known_by_its_signature() {
        let src = "/// a const fn\n#[inline]\npub const fn a() -> u8 {\n    1\n}\nfn b() { const fn c() {} }\n";
        assert!(is_const(src, 1, 5));
        assert!(!is_const(src, 6, 6));
    }
}
