//! What every measuring command opens first: the repository, its
//! `slimfit.toml` and baseline, and HEAD's working tree, read once; then
//! the tree's measurement, from the cache when it holds one.

use crate::config::Config;
use crate::model::Measurement;
use crate::ratchet::Baseline;
use crate::{config, fault, git, landing, measure, ratchet};
use anyhow::Result;
use std::path::PathBuf;

pub struct Session {
    pub repo: PathBuf,
    pub cfg: Config,
    pub base: Baseline,
    pub tree: landing::Tree,
}

impl Session {
    /// `head`'s repository, with its `slimfit.toml` and baseline, or the
    /// faults of both.
    pub fn open(head: &git::Head) -> Result<Session> {
        Session::of(head, config::load(&head.top))
    }

    /// `head`'s repository with `cfg`, which `init` gives before it is
    /// written; the tree is read now, after any write before it.
    pub fn of(head: &git::Head, cfg: Result<Config>) -> Result<Session> {
        let (cfg, base) = match (cfg, ratchet::load(&head.top)) {
            (Ok(c), Ok(b)) => (c, b),
            (c, b) => {
                return Err(fault::Faults(c.err().into_iter().chain(b.err()).flat_map(fault::of).collect()).into());
            }
        };
        Ok(Session { repo: head.top.clone(), cfg, base, tree: landing::Tree::read(head)? })
    }

    /// The session with its change taken against `target` (`--target`), in
    /// place of `[check] target`, when given.
    pub fn against(mut self, target: Option<String>) -> Session {
        if target.is_some() {
            self.cfg.check.target = target;
        }
        self
    }

    /// The tree's measurement, as `opts` asks for it.
    pub fn measure(&self, opts: measure::Options) -> Result<Measurement> {
        measure::current(&self.repo, &self.tree, &self.cfg, &opts)
    }
}
