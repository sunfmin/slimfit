//! The change's growth (ADR 0012): the source lines a landing adds net,
//! and the record that justifies growth past `[check] growth_allowance`,
//! judged by a second agent. Each record is a file of its own in
//! `.slimfit/growth/`, named by its contents, as a mutants record is.

use crate::config::Config;
use crate::git;
use crate::landing::{Landing, Tree, hash};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Where the growth records are kept, one file each.
pub const DIR: &str = ".slimfit/growth";

/// A record of why a change's growth could not be smaller: the issue it
/// serves, what existing code was weighed for reuse or deletion, and the
/// verdict of a second agent given only the diff: `as_is` with its why, or
/// `cut`, what to cut, and the author's `answer` to it. Who reviewed and who
/// made the change are the caller's word: the gate holds only that they
/// differ.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Record {
    /// The working tree it was made of: two branches' records differ.
    #[serde(default)]
    pub tree: String,
    /// The source lines the change added net when it was written.
    #[serde(default)]
    pub lines: i64,
    #[serde(default)]
    pub serves: String,
    #[serde(default)]
    pub considered: String,
    #[serde(default)]
    pub reviewer: String,
    #[serde(default)]
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_is: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

impl Record {
    /// Whether it names a reviewer other than the change's author: all the
    /// gate holds of a record, compared as given, never judged.
    pub fn reviewed_by_another(&self) -> bool {
        let (r, a) = (self.reviewer.trim(), self.author.trim());
        !r.is_empty() && !a.is_empty() && r != a
    }

    /// Its path, named by its contents, and its text.
    pub fn file(&self) -> Result<(String, String)> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        Ok((format!("{DIR}/{}.json", hash(&text)), text))
    }
}

/// How many growth records there are, by verdict.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Records {
    pub as_is: usize,
    pub cut: usize,
}

/// What [`Records`] prints, as the command pages name it.
pub const RECORDS_HELP: &str =
    "`as_is` (the reviewer found nothing to cut) and `cut` (it named cuts, which the author did or answered)";

impl Records {
    /// Counts each record among `files` (path, text) under [`DIR`]; `None`
    /// when there is none. One that does not read is left out.
    pub fn of<'a>(files: impl Iterator<Item = (&'a str, &'a str)>) -> Option<Records> {
        let mut n: Option<Records> = None;
        for (_, text) in files.filter(|(p, _)| p.starts_with(&format!("{DIR}/")) && p.ends_with(".json")) {
            let Ok(r) = serde_json::from_str::<Record>(text) else { continue };
            let n = n.get_or_insert_default();
            if r.cut.is_some() { n.cut += 1 } else { n.as_is += 1 }
        }
        n
    }

    /// The records in the working copy's [`DIR`].
    pub fn read(repo: &Path) -> Records {
        let files: Vec<(String, String)> = std::fs::read_dir(repo.join(DIR))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                Some((format!("{DIR}/{}", e.file_name().to_str()?), std::fs::read_to_string(e.path()).ok()?))
            })
            .collect();
        Records::of(files.iter().map(|(p, t)| (p.as_str(), t.as_str()))).unwrap_or_default()
    }
}

/// The source lines a landing adds net, per suite that counts any of the
/// files it touches: by file (only those that changed), and in all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Grown {
    pub lines: i64,
    pub files: BTreeMap<String, i64>,
}

/// Per suite, the source lines `l` adds net: each touched file's source lines
/// now, less its source lines at `l.from`, both read by today's classifier.
/// One `cat-file`, of the touched files a suite counts.
pub fn grown(repo: &Path, cfg: &Config, tree: &Tree, l: &Landing) -> Result<BTreeMap<String, Grown>> {
    let suites = cfg.suites.iter().map(|s| tree.suite(cfg, &s.name)).collect::<Result<Vec<_>>>()?;
    let counted: Vec<String> =
        l.touched.iter().filter(|f| suites.iter().any(|s| s.kind(f).is_some())).cloned().collect();
    let old = git::files_at(repo, &l.from, &counted)?;
    let mut out = BTreeMap::new();
    for (sc, s) in cfg.suites.iter().zip(&suites) {
        let mut g = Grown::default();
        for (f, before) in counted.iter().zip(&old) {
            let now = s.source_lines(f).map_or(0, |l| l.prod) as i64;
            let was = before.as_deref().map_or(0, |t| s.prod(f, t)) as i64;
            if now != was {
                g.files.insert(f.clone(), now - was);
                g.lines += now - was;
            }
        }
        if !g.files.is_empty() {
            out.insert(sc.name.clone(), g);
        }
    }
    Ok(out)
}

/// What clears `unrecorded_growth`: the rule of every change past the allowance.
pub const JUSTIFY_GROWTH: &str = "first cut what you can yourself; then ask a second agent, never yourself, \
     giving it only the goal (`slimfit help reviewer`) and the change's diff (`git diff <landing.from>`); when it \
     answers as-is, `slimfit ratchet --serves \"<the issue>\" --considered \"<the code you weighed reusing or \
     deleting>\" --as-is \"<its why>\" --reviewer <its id> --author <yours>`; when it names cuts, make them (if the \
     change then grows no more than the allowance, no record is due) or answer each, then `slimfit ratchet --serves \
     ... --considered ... --cut \"<what it named>\" --answer \"<what you did, or why not>\" --reviewer <its id> \
     --author <yours>`; then commit .slimfit/growth/ with the change";

impl crate::cli::GrowthArgs {
    /// Whether any flag of a growth record is given.
    pub fn given(&self) -> bool {
        [&self.serves, &self.considered, &self.as_is, &self.cut, &self.answer, &self.reviewer]
            .iter()
            .any(|f| f.is_some())
    }

    /// The growth record these flags ask for, of `tree`, which adds `lines`
    /// net, made by `author`; `None` when none is asked for. Refused unless
    /// every field is one line, the verdict is one of `--as-is` and `--cut`
    /// (with `--answer`), and the reviewer is not the author.
    pub fn record(&self, tree: &str, lines: i64, author: Option<&str>) -> Result<Option<Record>> {
        if !self.given() {
            return Ok(None);
        }
        let line = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty() && !v.contains('\n')).map(String::from);
        let mut missing: Vec<&str> = [
            ("--serves", &self.serves),
            ("--considered", &self.considered),
            ("--reviewer", &self.reviewer),
            ("--author", &author.map(String::from)),
        ]
        .into_iter()
        .filter(|(_, v)| line(v.as_deref()).is_none())
        .map(|(n, _)| n)
        .collect();
        let verdict = (line(self.as_is.as_deref()), line(self.cut.as_deref()), line(self.answer.as_deref()));
        match verdict {
            (Some(_), None, None) | (None, Some(_), Some(_)) => {}
            (None, None, _) => missing.push("--as-is or --cut"),
            _ => missing.push("--answer"),
        }
        if !missing.is_empty() {
            return Err(crate::fault::fault(
                format!("a growth record without {}, each one line", missing.join(", ")),
                format!("{}; `slimfit ratchet --help` lists the flags", JUSTIFY_GROWTH),
            ));
        }
        let (as_is, cut, answer) = verdict;
        let r = Record {
            tree: tree.into(),
            lines,
            serves: line(self.serves.as_deref()).unwrap_or_default(),
            considered: line(self.considered.as_deref()).unwrap_or_default(),
            reviewer: line(self.reviewer.as_deref()).unwrap_or_default(),
            author: line(author).unwrap_or_default(),
            as_is,
            cut,
            answer,
        };
        if !r.reviewed_by_another() {
            return Err(crate::fault::fault(
                "--reviewer is the change's author: a growth record's reviewer is a second agent",
                JUSTIFY_GROWTH.to_string(),
            ));
        }
        Ok(Some(r))
    }
}
