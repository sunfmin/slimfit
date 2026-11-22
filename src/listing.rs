//! The change's candidates as the picker reads them (slimfit#99): each
//! removed test named once, in a table per suite the candidates refer to
//! by index; and, over [`BUDGET`], grouped per function, with a few
//! candidates of each spelled out and the way to list one in full.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::Candidate;

/// The most bytes a listing answers in full; past it, it is grouped per
/// function. A picker reads the listing with the diff in one prompt: 2.2 MB
/// of guild#88's listing was about a million tokens, past what it reads.
pub const BUDGET: usize = 200_000;

/// The candidates a grouped listing spells out per function.
pub const EXAMPLES: usize = 3;

/// How the listing of one function's mutants in full is asked for.
pub const EXPAND: &str = "slimfit check --mutants --function '<file>: <function>'";

/// What a listing answers, per suite.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Listing<'a> {
    /// Each removed test that ran a candidate's line, once (`file:line:
    /// name`, where the change starts): what `removed` indexes.
    pub removed_tests: BTreeMap<String, Vec<String>>,
    /// The candidates: all of them, or, grouped, up to [`EXAMPLES`] per
    /// function, as many as fit the budget.
    pub candidates: BTreeMap<String, Vec<Entry<'a>>>,
    /// Grouped only: every function holding candidates.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub functions: BTreeMap<String, Vec<Function<'a>>>,
}

/// A candidate as listed.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry<'a> {
    #[serde(flatten)]
    pub candidate: &'a Candidate,
    /// The removed tests that ran its lines, as indexes into its suite's
    /// `removed_tests`; none for a mutant of added lines alone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<usize>,
}

/// The candidates of one function, in a grouped listing.
#[derive(Debug, Serialize, PartialEq)]
pub struct Function<'a> {
    pub file: &'a str,
    pub function: &'a str,
    /// How many candidates it holds.
    pub mutants: usize,
    /// The removed tests that ran any of them, as `removed` indexes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<usize>,
}

/// What a listing's candidate prints, as the command pages name it.
pub const ENTRY_HELP: &str = "each with `name` (what a pick names), `file`, `line`, `function`, \
     `replacement` (the code it puts in) and, on lines a test the change removes ran, `removed` (those tests, as \
     indexes into the suite's table of them)";

/// What a grouped listing's function prints.
pub const FUNCTION_HELP: &str = "each with `file`, `function`, `mutants` (how many it holds) and `removed` (the \
     removed tests that ran any of them, as indexes)";

/// The listing of `candidates` per suite; `removed`, the tests the change
/// removes per suite, lead the table, in their order. With `functions`
/// (each `<file>: <function>`), only theirs, in full; else grouped when the
/// full listing passes [`BUDGET`].
pub fn of<'a>(
    candidates: &'a BTreeMap<String, Vec<Candidate>>,
    removed: &BTreeMap<String, Vec<String>>,
    functions: &[String],
) -> Listing<'a> {
    let mut listing = Listing::default();
    let wanted = |c: &&Candidate| functions.is_empty() || functions.iter().any(|f| within(c, f));
    for (suite, listed) in candidates {
        let table = listing.removed_tests.entry(suite.clone()).or_default();
        table.extend(removed.get(suite).into_iter().flatten().cloned());
        let entries =
            listed.iter().filter(wanted).map(|c| Entry { candidate: c, removed: indexes(table, &c.removed_tests) });
        listing.candidates.insert(suite.clone(), entries.collect());
    }
    if !functions.is_empty() || size(&listing) <= BUDGET {
        return listing;
    }
    let mut groups: BTreeMap<String, Vec<Vec<Entry>>> = BTreeMap::new();
    for (suite, entries) in listing.candidates.iter_mut() {
        let (fs, of_each) =
            (listing.functions.entry(suite.clone()).or_default(), groups.entry(suite.clone()).or_default());
        for e in std::mem::take(entries) {
            let c = e.candidate;
            let at = fs.iter().position(|f| f.file == c.file && f.function == c.function).unwrap_or_else(|| {
                fs.push(Function { file: &c.file, function: &c.function, mutants: 0, removed: vec![] });
                of_each.push(vec![]);
                fs.len() - 1
            });
            fs[at].mutants += 1;
            fs[at].removed.extend(&e.removed);
            of_each[at].push(e);
        }
        for f in fs.iter_mut() {
            f.removed.sort_unstable();
            f.removed.dedup();
        }
    }
    // What is left of the budget goes to examples, a round of one per function
    // at a time, until one does not fit; each suite spells out one at least,
    // so a grouped listing always names a candidate to pick.
    let mut full = false;
    for round in 0..EXAMPLES {
        for (suite, of_each) in &groups {
            for e in of_each.iter().filter_map(|entries| entries.get(round)) {
                let first = listing.candidates[suite].is_empty();
                if full && !first {
                    continue;
                }
                listing.candidates.get_mut(suite).unwrap().push(e.clone());
                if !first && size(&listing) > BUDGET - NEXT {
                    listing.candidates.get_mut(suite).unwrap().pop();
                    full = true;
                }
            }
        }
    }
    for (suite, spelled) in listing.candidates.iter_mut() {
        let fs = &listing.functions[suite];
        spelled
            .sort_by_key(|e| fs.iter().position(|f| f.file == e.candidate.file && f.function == e.candidate.function));
    }
    listing
}

impl Listing<'_> {
    /// The answer's `next`: `then` as given, after how to read the listing
    /// when it is grouped.
    pub fn next(&self, then: &str) -> String {
        match self.functions.is_empty() {
            true => then.to_string(),
            false => format!(
                "the listing passes {BUDGET} bytes, so it is grouped per function (`functions`), up to {EXAMPLES} \
                 candidates of each spelled out as the budget allows: `{EXPAND}` lists one function's in full, and \
                 any of them may be picked. Then {then}"
            ),
        }
    }

    /// `suite`'s part of the listing, as a pick-mutants task's evidence
    /// holds it: `removed_tests`, `candidates` and, grouped, `functions`.
    pub fn of_suite(mut self, suite: &str) -> serde_json::Value {
        let mut part = serde_json::json!({
            "removed_tests": self.removed_tests.remove(suite).unwrap_or_default(),
            "candidates": self.candidates.remove(suite).unwrap_or_default(),
        });
        if let Some(functions) = self.functions.remove(suite) {
            part["functions"] = serde_json::json!(functions);
        }
        part
    }
}

/// The bytes left of [`BUDGET`] for the rest of the answer (`next`).
const NEXT: usize = 2_000;

/// Whether `c` is in the function `key` names (`<file>: <function>`).
fn within(c: &Candidate, key: &str) -> bool {
    key.split_once(": ").is_some_and(|(file, function)| c.file == file && c.function == function)
}

/// `tests`' places in `table`, each added to it at its end when not there.
fn indexes(table: &mut Vec<String>, tests: &[String]) -> Vec<usize> {
    let at = |table: &mut Vec<String>, t: &String| match table.iter().position(|x| x == t) {
        Some(i) => i,
        None => {
            table.push(t.clone());
            table.len() - 1
        }
    };
    tests.iter().map(|t| at(table, t)).collect()
}

/// The bytes `listing` answers in, printed as slimfit prints its answers.
pub fn size(listing: &Listing) -> usize {
    serde_json::to_string_pretty(listing).map_or(0, |t| t.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` candidates of one function of `file`, each on a line all of
    /// `tests` ran.
    fn many(file: &str, function: &str, n: usize, tests: &[String]) -> Vec<Candidate> {
        let c = |i: usize| Candidate {
            name: format!("{file}:{i}:5: replace a long expression of the kind a tool names with another {i}"),
            file: file.into(),
            line: i as u32,
            function: function.into(),
            replacement: "Default::default()".into(),
            removed_tests: tests.to_vec(),
            ..Default::default()
        };
        (1..=n).map(c).collect()
    }

    /// guild#88's shape: thousands of mutants on lines dozens of removed
    /// tests ran. Each removed test appears once; over the budget the answer
    /// is grouped per function, and names how to list one in full.
    #[test]
    fn a_large_change_names_each_removed_test_once_and_groups_past_the_budget() {
        let tests: Vec<String> = (0..60).map(|i| format!("src/sched/tests.rs:{i}: removed_test_{i}")).collect();
        let mut listed: Vec<Candidate> = Vec::new();
        for f in 0..120 {
            listed.extend(many(&format!("src/m{f}.rs"), &format!("Thing::f{f}"), 20, &tests));
        }
        // Namesakes: one function's name in two files, two functions in one file.
        listed.extend(many("src/m0.rs", "Thing::other", 2, &[]));
        listed.extend(many("src/n.rs", "Thing::f0", 4, &[]));
        // One function past the budget alone.
        listed.extend(many("src/big.rs", "big", 2_000, &tests));
        let candidates = BTreeMap::from([("rust".to_string(), listed)]);
        let removed = BTreeMap::from([("rust".to_string(), vec![tests[3].clone()])]);
        let listing = of(&candidates, &removed, &[]);
        let text = serde_json::to_string_pretty(&listing).unwrap();
        assert!(text.len() <= BUDGET - NEXT, "{}", text.len());
        for t in &tests {
            assert_eq!(text.matches(&format!("\"{t}\"")).count(), 1, "{t}");
        }
        assert_eq!(listing.removed_tests["rust"][0], tests[3], "the change's removed tests lead the table");
        let functions = &listing.functions["rust"];
        let counts: Vec<(&str, &str, usize)> = functions.iter().map(|f| (f.file, f.function, f.mutants)).collect();
        assert_eq!(counts.len(), 123);
        assert_eq!(counts[..1], [("src/m0.rs", "Thing::f0", 20)]);
        assert_eq!(
            counts[120..],
            [("src/m0.rs", "Thing::other", 2), ("src/n.rs", "Thing::f0", 4), ("src/big.rs", "big", 2_000)]
        );
        assert_eq!((functions[0].removed.len(), &functions[121].removed[..]), (60, &[][..]));
        // The budget left is spent a round at a time, in order, until an example does not fit.
        let spelled = |f: &Function| {
            let of_it = |e: &&Entry| e.candidate.file == f.file && e.candidate.function == f.function;
            listing.candidates["rust"].iter().filter(of_it).count()
        };
        let per: Vec<usize> = functions.iter().map(spelled).collect();
        let firsts = per.iter().take_while(|&&n| n == 1).count();
        assert!(firsts > 1 && per[firsts..].iter().all(|&n| n == 0), "{per:?}");
        // As full as the budget holds: the next example would not fit.
        let next = candidates["rust"].iter().find(|c| c.file == functions[firsts].file).unwrap();
        let mut more = listing;
        more.candidates.get_mut("rust").unwrap().push(Entry { candidate: next, removed: (0..60).collect() });
        assert!(size(&more) > BUDGET - NEXT, "{}", size(&more));
        // Few functions, many mutants each: room for every round.
        let deep: Vec<Candidate> = (0..3).flat_map(|f| many(&format!("src/d{f}.rs"), "f", 2_000, &[])).collect();
        let deep = BTreeMap::from([("rust".to_string(), deep)]);
        let listing = of(&deep, &BTreeMap::new(), &[]);
        assert_eq!((listing.functions["rust"].len(), listing.candidates["rust"].len()), (3, 3 * EXAMPLES));
        let then = listing.next("pick");
        assert!(then.starts_with(&format!("the listing passes {BUDGET} bytes, so it is grouped")), "{then}");
        assert!(then.contains(EXPAND) && then.ends_with("Then pick"), "{then}");
        let part = listing.of_suite("rust");
        assert_eq!(
            (part["functions"].as_array().map(Vec::len), part["candidates"].as_array().map(Vec::len)),
            (Some(3), Some(9))
        );
        // Grouped past the budget with the functions alone: each suite still spells out its first.
        let mut wide: Vec<Candidate> = Vec::new();
        for f in 0..3_000 {
            wide.extend(many(&format!("src/w{f}.rs"), "f", 2, &[]));
        }
        let wide = BTreeMap::from([("go".to_string(), wide.clone()), ("rust".to_string(), wide)]);
        let listing = of(&wide, &BTreeMap::new(), &[]);
        for suite in ["go", "rust"] {
            let spelled: Vec<&str> = listing.candidates[suite].iter().map(|e| e.candidate.name.as_str()).collect();
            assert_eq!((listing.functions[suite].len(), &spelled[..]), (3_000, &[wide[suite][0].name.as_str()][..]));
        }
        // One function's whole listing, as `--function` asks it: every candidate, in full, past the budget too.
        let full = of(&candidates, &BTreeMap::new(), &["src/big.rs: big".to_string()]);
        assert_eq!((full.candidates["rust"].len(), full.functions.is_empty()), (2_000, true));
        assert!(size(&full) > BUDGET);
        assert_eq!(full.candidates["rust"][19].removed, (0..60).collect::<Vec<_>>());
        let named = |key: &str| {
            let one = of(&candidates, &BTreeMap::new(), &[key.to_string()]);
            one.candidates["rust"]
                .iter()
                .map(|e| (e.candidate.file.clone(), e.candidate.function.clone()))
                .collect::<std::collections::BTreeSet<_>>()
        };
        let at = |f: &str, g: &str| std::collections::BTreeSet::from([(f.to_string(), g.to_string())]);
        assert_eq!(named("src/m0.rs: Thing::f0"), at("src/m0.rs", "Thing::f0"));
        assert_eq!(named("src/n.rs: Thing::f0"), at("src/n.rs", "Thing::f0"));
        assert!(named("src/m0.rs: nothing").is_empty());
    }

    /// A listing within the budget is answered whole, each candidate with
    /// the indexes of the removed tests that ran it.
    #[test]
    fn a_small_listing_is_whole() {
        let tests = vec!["t.rs:1: a".to_string(), "t.rs:9: b".to_string()];
        let mut listed = many("src/a.rs", "f", 2, &tests[1..]);
        listed.extend(many("src/b.rs", "g", 1, &[]));
        let candidates = BTreeMap::from([("rust".to_string(), listed)]);
        let listing = of(&candidates, &BTreeMap::from([("rust".to_string(), tests.clone())]), &[]);
        assert!(listing.functions.is_empty());
        assert_eq!(listing.next("pick"), "pick");
        let removed: Vec<&[usize]> = listing.candidates["rust"].iter().map(|e| &e.removed[..]).collect();
        assert_eq!(removed, [&[1][..], &[1], &[]]);
        assert_eq!(listing.removed_tests["rust"], tests);
        let part = listing.of_suite("rust");
        assert_eq!((&part["removed_tests"], part.get("functions")), (&serde_json::json!(tests), None));
    }
}
