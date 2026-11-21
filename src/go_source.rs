//! Go source, read as slimfit needs it: where a top-level test function is,
//! and its text. gofmt, which Go code is written through, ends a top-level
//! `func` with a `}` alone at the start of a line (or on its own line, when
//! the whole func is one line): one rule, no parsing.

/// Where top-level test `name` of package `package` (an import path, as
/// `go test -json` names it) is: the `func <name>(` line of the one test
/// file in `files` whose path `resolve` maps the package's path of it to.
pub fn test_function<'a>(
    files: impl IntoIterator<Item = &'a String>,
    read: impl Fn(&str) -> Option<String>,
    resolve: impl Fn(&str) -> Option<String>,
    package: &str,
    name: &str,
) -> Option<(String, u32)> {
    let head = format!("func {name}(");
    let mut found = files.into_iter().filter(|f| f.ends_with("_test.go")).filter_map(|f| {
        let base = f.rsplit('/').next()?;
        (resolve(&format!("{package}/{base}")).as_deref() == Some(f.as_str())).then_some(())?;
        let line = read(f)?.lines().position(|l| l.starts_with(&head))?;
        Some((f.clone(), line as u32 + 1))
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The text of the top-level func whose first line is `line` (1-based) of
/// `text`: to its closing `}`. `None` when there is no such line or the
/// func does not end.
pub fn func_text(text: &str, line: u32) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = line.checked_sub(1)? as usize;
    let first = lines.get(start)?;
    let end = if first.trim_end().ends_with('}') {
        start
    } else {
        start + 1 + lines[start + 1..].iter().position(|l| l.starts_with('}'))?
    };
    Some(lines[start..=end].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test is found in its package's test file only, and its text runs
    /// to the `}` that ends it, not to one inside it.
    #[test]
    fn finds_a_test_and_its_text() {
        let text = "package p\n\nfunc TestA(t *testing.T) {\n\tif x {\n\t}\n}\n\nfunc TestB(t *testing.T) {}\n";
        let files = ["p/a_test.go".to_string(), "q/a_test.go".to_string(), "p/a.go".to_string()];
        let read = |_: &str| Some(text.to_string());
        let resolve = |p: &str| p.strip_prefix("m/").map(String::from);
        assert_eq!(test_function(&files, read, resolve, "m/p", "TestA"), Some(("p/a_test.go".into(), 3)));
        assert_eq!(test_function(&files, read, resolve, "m/p", "TestC"), None);
        assert_eq!(func_text(text, 3).unwrap(), "func TestA(t *testing.T) {\n\tif x {\n\t}\n}");
        assert_eq!(func_text(text, 8).unwrap(), "func TestB(t *testing.T) {}");
        assert_eq!(func_text("func TestA() {\n", 1), None);
    }
}
