//! The change's candidates as the picker reads them (slimfit#99): each
//! removed test named once, in a table per suite the candidates refer to
//! by index; and, over [`BUDGET`], grouped per function, with a few
//! candidates of each spelled out and the way to list one in full.

use std::collections::{BTreeMap, BTreeSet};

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
    /// The candidates: all of them, or, grouped, [`EXAMPLES`] per function.
    pub candidates: BTreeMap<String, Vec<Entry<'a>>>,
    /// Grouped only: every function holding candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functions: Option<BTreeMap<String, Vec<Function<'a>>>>,
}

/// A candidate as listed.
#[derive(Debug, Serialize, PartialEq)]
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
    let mut functions = BTreeMap::new();
    for (suite, entries) in std::mem::take(&mut listing.candidates) {
        let mut groups: Vec<(Function, Vec<Entry>)> = Vec::new();
        for e in entries {
            let c = e.candidate;
            let at = groups.iter().position(|(f, _)| f.file == c.file && f.function == c.function);
            let (f, examples) = match at {
                Some(i) => &mut groups[i],
                None => {
                    let f = Function { file: &c.file, function: &c.function, mutants: 0, removed: vec![] };
                    groups.push((f, vec![]));
                    groups.last_mut().unwrap()
                }
            };
            f.mutants += 1;
            let union: BTreeSet<usize> = f.removed.iter().chain(&e.removed).copied().collect();
            f.removed = union.into_iter().collect();
            if examples.len() < EXAMPLES {
                examples.push(e);
            }
        }
        let (fs, examples): (Vec<_>, Vec<_>) = groups.into_iter().unzip();
        listing.candidates.insert(suite.clone(), examples.into_iter().flatten().collect());
        functions.insert(suite, fs);
    }
    listing.functions = Some(functions);
    listing
}

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

/// The bytes `listing` answers in.
pub fn size(listing: &Listing) -> usize {
    serde_json::to_string(listing).map_or(0, |t| t.len())
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
        let text = serde_json::to_string(&listing).unwrap();
        assert!(text.len() <= BUDGET, "{}", text.len());
        for t in &tests {
            assert_eq!(text.matches(&format!("\"{t}\"")).count(), 1, "{t}");
        }
        assert_eq!(listing.removed_tests["rust"][0], tests[3], "the change's removed tests lead the table");
        let functions = &listing.functions.as_ref().unwrap()["rust"];
        let counts: Vec<(&str, &str, usize)> = functions.iter().map(|f| (f.file, f.function, f.mutants)).collect();
        assert_eq!(counts.len(), 123);
        assert_eq!(counts[..1], [("src/m0.rs", "Thing::f0", 20)]);
        assert_eq!(
            counts[120..],
            [("src/m0.rs", "Thing::other", 2), ("src/n.rs", "Thing::f0", 4), ("src/big.rs", "big", 2_000)]
        );
        assert_eq!((functions[0].removed.len(), &functions[121].removed[..]), (60, &[][..]));
        assert_eq!(listing.candidates["rust"].len(), 120 * EXAMPLES + 2 + 2 * EXAMPLES);
        // One function's whole listing, as `--function` asks it: every candidate, in full, past the budget too.
        let full = of(&candidates, &BTreeMap::new(), &["src/big.rs: big".to_string()]);
        assert_eq!((full.candidates["rust"].len(), full.functions.is_none()), (2_000, true));
        assert!(size(&full) > BUDGET);
        assert_eq!(full.candidates["rust"][19].removed, (0..60).collect::<Vec<_>>());
        let named = |key: &str| {
            let one = of(&candidates, &BTreeMap::new(), &[key.to_string()]);
            one.candidates["rust"]
                .iter()
                .map(|e| (e.candidate.file.clone(), e.candidate.function.clone()))
                .collect::<BTreeSet<_>>()
        };
        let at = |f: &str, g: &str| BTreeSet::from([(f.to_string(), g.to_string())]);
        assert_eq!(named("src/m0.rs: Thing::f0"), at("src/m0.rs", "Thing::f0"));
        assert_eq!(named("src/n.rs: Thing::f0"), at("src/n.rs", "Thing::f0"));
        assert_eq!(named("src/m0.rs: nothing"), BTreeSet::new());
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
        assert_eq!(listing.functions, None);
        let removed: Vec<&[usize]> = listing.candidates["rust"].iter().map(|e| &e.removed[..]).collect();
        assert_eq!(removed, [&[1][..], &[1], &[]]);
        assert_eq!(listing.removed_tests["rust"], tests);
    }
}
