//! A test's shape, read with tree-sitter (slimfit#88): its body with every
//! literal replaced by a placeholder, the literals in order, and each of
//! its assertions. Two tests of one shape differ only in literals (one
//! table-driven test); a test whose every assertion another holds checks
//! nothing that one does not. A language is data: its grammar, the node
//! kinds of its literals and comments, and what an assertion is.
//!
//! Only the languages whose test runs record each test's lines have
//! redundant-test candidates: Rust and Go have a grammar here; Python's
//! candidates, without one, are all `read`.

use crate::config::Preset;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

/// What [`shape`] reads of a test function.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shape {
    /// The hash of its body's tokens, each literal a placeholder.
    pub body: String,
    /// Its body's literals, in order: the placeholders' values.
    pub literals: Vec<String>,
    /// The hash of each assertion's tokens, literals kept.
    pub asserts: Vec<String>,
    /// The hash of each statement of its body's, literals kept: its
    /// assertions and the setup they check.
    pub statements: Vec<String>,
}

struct Grammar {
    language: fn() -> tree_sitter::Language,
    /// What makes a lone function a whole file.
    prefix: &'static str,
    function: &'static str,
    literals: &'static [&'static str],
    comments: &'static [&'static str],
    is_assert: fn(Node, &str) -> bool,
}

/// Whether a grammar here reads `preset`'s tests.
pub fn reads(preset: Preset) -> bool {
    grammar(preset).is_some()
}

fn grammar(preset: Preset) -> Option<Grammar> {
    match preset {
        Preset::Rust => Some(Grammar {
            language: || tree_sitter_rust::LANGUAGE.into(),
            prefix: "",
            function: "function_item",
            literals: &[
                "string_literal",
                "raw_string_literal",
                "char_literal",
                "integer_literal",
                "float_literal",
                "boolean_literal",
            ],
            comments: &["line_comment", "block_comment"],
            is_assert: |n, src| {
                let name = n.child_by_field_name("macro").map(|m| text(m, src).rsplit("::").next().unwrap_or(""));
                // Only a macro invocation has a `macro`.
                name.is_some_and(|m| m.starts_with("assert") || m.starts_with("debug_assert"))
            },
        }),
        Preset::Go => Some(Grammar {
            language: || tree_sitter_go::LANGUAGE.into(),
            prefix: "package p\n",
            function: "function_declaration",
            literals: &[
                "interpreted_string_literal",
                "raw_string_literal",
                "rune_literal",
                "int_literal",
                "float_literal",
                "imaginary_literal",
                "true",
                "false",
            ],
            comments: &["comment"],
            is_assert: |n, src| match n.kind() {
                // `if <failed> { t.Errorf(…) }`, or testify's `assert.X(…)` / `require.X(…)`.
                "if_statement" => n.child_by_field_name("consequence").is_some_and(|c| fails(c, src)),
                "call_expression" => n
                    .child_by_field_name("function")
                    .and_then(|f| f.child_by_field_name("operand"))
                    .is_some_and(|o| ["assert", "require"].contains(&text(o, src))),
                _ => false,
            },
        }),
        _ => None,
    }
}

/// Whether `n` calls one of `testing.T`'s failing methods.
fn fails(n: Node, src: &str) -> bool {
    let failing = ["Error", "Errorf", "Fatal", "Fatalf", "Fail", "FailNow"];
    let here = n.kind() == "call_expression"
        && n.child_by_field_name("function")
            .filter(|f| f.kind() == "selector_expression")
            .and_then(|f| f.child_by_field_name("field"))
            .is_some_and(|f| failing.contains(&text(f, src)));
    here || children(n).any(|c| fails(c, src))
}

fn text<'a>(n: Node, src: &'a str) -> &'a str {
    &src[n.byte_range()]
}

fn children(n: Node) -> impl Iterator<Item = Node> {
    (0..n.child_count()).filter_map(move |i| n.child(i))
}

/// The shape of the test function `text` (its first line to its end, as
/// the language's reader cuts it), in the language of `preset`. `None` for
/// a language without a grammar here, or text holding no function.
pub fn shape(preset: Preset, text: &str) -> Option<Shape> {
    let g = grammar(preset)?;
    let src = format!("{}{text}", g.prefix);
    let mut parser = Parser::new();
    parser.set_language(&(g.language)()).ok()?;
    let tree = parser.parse(&src, None)?;
    let body = children(tree.root_node()).find(|n| n.kind() == g.function)?.child_by_field_name("body")?;
    let mut shaped = Vec::new();
    let mut literals = Vec::new();
    tokens(&g, body, &src, &mut |t, literal| {
        if literal {
            literals.push(t.to_string());
        }
        shaped.push(if literal { "$" } else { t });
    });
    let mut asserts = Vec::new();
    find_asserts(&g, body, &src, &mut asserts);
    let mut statements = Vec::new();
    top_statements(&g, body, &src, &mut statements);
    Some(Shape { body: fnv(&shaped.join(" ")), literals, asserts, statements })
}

/// Each token of `n`, comments left out, a literal whole (and flagged).
fn tokens<'a>(g: &Grammar, n: Node, src: &'a str, out: &mut impl FnMut(&'a str, bool)) {
    if g.comments.contains(&n.kind()) {
        return;
    }
    let literal = g.literals.contains(&n.kind());
    if literal || n.child_count() == 0 {
        return out(text(n, src), literal);
    }
    children(n).for_each(|c| tokens(g, c, src, out));
}

/// The hash of each statement of block `n`, comments left out (a Go
/// block holds its statements in a list).
fn top_statements(g: &Grammar, n: Node, src: &str, out: &mut Vec<String>) {
    for c in children(n).filter(|c| c.is_named() && !g.comments.contains(&c.kind())) {
        if c.kind() == "statement_list" {
            top_statements(g, c, src, out);
        } else {
            let mut ts = Vec::new();
            tokens(g, c, src, &mut |t, _| ts.push(t));
            out.push(fnv(&ts.join(" ")));
        }
    }
}

/// The hash of each outermost assertion's tokens under `n`.
fn find_asserts(g: &Grammar, n: Node, src: &str, out: &mut Vec<String>) {
    if (g.is_assert)(n, src) {
        let mut ts = Vec::new();
        tokens(g, n, src, &mut |t, _| ts.push(t));
        return out.push(fnv(&ts.join(" ")));
    }
    children(n).for_each(|c| find_asserts(g, c, src, out));
}

/// FNV-1a 64 of `text`, in hex.
pub fn fnv(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(p: Preset, text: &str) -> Shape {
        shape(p, text).unwrap()
    }

    /// Bodies that differ only in literals (and layout, and comments) share
    /// a shape, each with its literals; a different call does not. An
    /// assertion is the same only with the same literals.
    #[test]
    fn rust_tests_of_one_shape_differ_only_in_literals() {
        let a = of(Preset::Rust, "fn a() {\n    let x = start(\"gmail\", 2);\n    assert_eq!(x.n, 2);\n}");
        let b = of(
            Preset::Rust,
            "fn b() {\n    // gh's token\n    let x = start(\"gh\",\n 3);\n    assert_eq!(x.n, 3);\n}",
        );
        let c = of(Preset::Rust, "fn c() {\n    let x = stop(\"gh\", 3);\n    assert_eq!(x.n, 3);\n}");
        assert_eq!(a.body, b.body);
        assert_ne!(a.body, c.body);
        assert_eq!(
            (a.literals, b.literals.clone()),
            (vec!["\"gmail\"".into(), "2".into(), "2".into()], vec!["\"gh\"".into(), "3".into(), "3".into()])
        );
        assert_eq!(b.asserts, c.asserts);
        assert_ne!(a.asserts, b.asserts);
        let many = of(
            Preset::Rust,
            "fn m() {\n    assert!(ok());\n    std::assert_ne!(f(1), 2);\n    debug_assert!(g());\n    println!(\"x\");\n}",
        );
        assert_eq!(many.asserts.len(), 3);
    }

    /// Go's assertion is an `if` that fails the test, or testify's call; its
    /// shape, as Rust's, leaves out literals.
    #[test]
    fn go_assertions_are_failing_ifs_and_testify_calls() {
        let a = of(
            Preset::Go,
            "func TestA(t *testing.T) {\n\tif Add(2, 3) != 5 {\n\t\tt.Fatal(\"2+3\")\n\t}\n\tassert.Equal(t, 1, f())\n}",
        );
        let b = of(
            Preset::Go,
            "func TestB(t *testing.T) {\n\tif Add(1, 1) != 2 {\n\t\tt.Fatal(\"1+1\")\n\t}\n\tassert.Equal(t, 1, f())\n}",
        );
        let c = of(Preset::Go, "func TestC(t *testing.T) {\n\tif Add(1, 1) != 2 {\n\t\tlog.Print(\"no\")\n\t}\n}");
        assert_eq!(a.body, b.body);
        assert_eq!((a.asserts.len(), a.asserts[1] == b.asserts[1], a.asserts[0] == b.asserts[0]), (2, true, false));
        assert!(c.asserts.is_empty());
        assert_eq!(shape(Preset::Kotlin, "fun a() {}"), None);
        assert!(reads(Preset::Rust) && reads(Preset::Go) && !reads(Preset::Kotlin));
        // FNV-1a 64, as kept verdicts were hashed before shapes.
        assert_eq!((fnv(""), fnv("a")), ("cbf29ce484222325".into(), "af63dc4c8601ec8c".into()));
        assert_eq!(shape(Preset::Rust, "struct S;"), None);
    }
}
