//! Python source, read as slimfit needs it: where a pytest test function
//! is, and its text. A `def` ends before the next line, not blank, that is
//! indented no deeper than it: one rule, no parsing.

/// Where test `name` (`[…]` parameters dropped) of JUnit classname
/// `class` is: the `def <name>(` line of the one file in `files` that is
/// the module the classname starts with (pytest's classname is the
/// module's path, dotted, then its classes), when the file holds one.
pub fn test_function<'a>(
    files: impl IntoIterator<Item = &'a String>,
    read: impl Fn(&str) -> Option<String>,
    class: &str,
    name: &str,
) -> Option<(String, u32)> {
    let name = name.split('[').next()?;
    let parts: Vec<&str> = class.split('.').collect();
    // The module: the file whose path, dotted, ends with the longest leading run of the classname's parts.
    let run = |f: &&String| {
        let m = format!(".{}", f.strip_suffix(".py")?.replace('/', "."));
        (1..=parts.len()).rev().find(|n| m.ends_with(&format!(".{}", parts[..*n].join("."))))
    };
    let file = files.into_iter().filter_map(|f| Some((run(&f)?, f))).max_by_key(|(n, _)| *n)?.1;
    let text = read(file)?;
    let mut found = text.lines().enumerate().filter(|(_, l)| {
        let l = l.trim_start();
        let l = l.strip_prefix("async ").unwrap_or(l);
        l.strip_prefix("def ").and_then(|l| l.strip_prefix(name)).is_some_and(|l| l.starts_with('('))
    });
    let (line, _) = found.next()?;
    found.next().is_none().then(|| (file.clone(), line as u32 + 1))
}

/// The text of the `def` whose first line is `line` (1-based) of `text`:
/// to its last line indented deeper than it. `None` when there is no such line.
pub fn def_text(text: &str, line: u32) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = line.checked_sub(1)? as usize;
    let indent = |l: &str| l.len() - l.trim_start().len();
    let depth = indent(lines.get(start)?);
    let body = lines[start + 1..].iter().take_while(|l| l.trim().is_empty() || indent(l) > depth).count();
    let end =
        start + body - lines[start + 1..start + 1 + body].iter().rev().take_while(|l| l.trim().is_empty()).count();
    Some(lines[start..=end].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test is found in the module its classname names, under a suite
    /// root or not, parameters dropped; its text ends at its last line
    /// indented deeper than its `def`.
    #[test]
    fn finds_a_test_and_its_text() {
        let text = "import x\n\nclass TestA:\n    def test_a(self):\n        assert x\n\n    def test_b(self):\n        pass\n\n\ndef test_c():\n    assert y\n";
        let files = ["py/tests/test_m.py".to_string(), "py/src/m.py".to_string()];
        let read = |_: &str| Some(text.to_string());
        let at = |class: &str, name: &str| test_function(&files, read, class, name);
        assert_eq!(at("tests.test_m.TestA", "test_a[1-2]"), Some(("py/tests/test_m.py".into(), 4)));
        assert_eq!(at("tests.test_m", "test_c"), Some(("py/tests/test_m.py".into(), 11)));
        assert_eq!(at("tests.test_n", "test_c"), None);
        assert_eq!(at("tests.test_m", "test_d"), None);
        assert_eq!(def_text(text, 4).unwrap(), "    def test_a(self):\n        assert x");
        assert_eq!(def_text(text, 11).unwrap(), "def test_c():\n    assert y");
    }
}
