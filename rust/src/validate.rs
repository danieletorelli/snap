//! Repository validation (SPEC §4.5).
//!
//! AGENTS.md lists "repository validation and replay" as its own
//! responsibility, separate from commands and CLI dispatch. `replay` holds the
//! replay half; this module holds the checks that run before it, plus the
//! entry points that turn stored bytes into a repository value.

use crate::error::{self, Result};
use crate::model::{Repository, Tree};
use crate::replay;
use crate::version::Version;

/// Parse and validate a repository value (SPEC §4.5).
pub fn load(text: &str) -> Result<Repository> {
    let repository = Repository::from_json_str(text)?;
    validate(&repository)?;
    Ok(repository)
}

/// SPEC §4.5's validation passes beyond what parsing already enforces:
/// contiguous contributor revisions, `revision = base[author] + 1`, complete
/// base closure, acyclicity, and a deterministic replay of the frontier.
pub fn validate(repository: &Repository) -> Result<()> {
    for patch in &repository.patches {
        let expected = patch
            .base
            .get(&patch.author)
            .checked_add(1)
            .ok_or_else(|| error::invalid_json("revision overflow"))?;
        if patch.revision != expected {
            return Err(error::invalid_json("revision must be base[author] + 1"));
        }
        for (id, revision) in patch.base.iter() {
            if repository.find(id, revision).is_none() {
                return Err(error::missing_base(&format!("{id}@{revision}")));
            }
        }
    }
    // Contiguity: revision n must follow n-1 for each contributor (SPEC §3.5).
    for window in repository.patches.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        if a.author == b.author {
            let next = a
                .revision
                .checked_add(1)
                .ok_or_else(|| error::invalid_json("revision overflow"))?;
            if b.revision != next {
                return Err(error::cyclic_history());
            }
        }
    }
    for patch in &repository.patches {
        if patch.revision == 1 {
            continue;
        }
        if repository.find(&patch.author, patch.revision - 1).is_none() {
            return Err(error::cyclic_history());
        }
    }
    // `patches` must be exactly the causal closure of `frontier`, with nothing
    // unreachable (SPEC §4.1).
    for patch in &repository.patches {
        if patch.revision > repository.frontier.get(&patch.author) {
            // SPEC §4.1: `patches` is exactly the causal closure of
            // `frontier`, "with no unreachable patches".
            return Err(error::unreachable_patch(&patch.author, patch.revision));
        }
    }
    for (id, revision) in repository.frontier.iter() {
        if repository.find(id, revision).is_none() {
            return Err(error::missing_base(&format!("{id}@{revision}")));
        }
    }
    // Replay proves acyclicity and that every change applies to its base.
    replay::materialize(repository, &repository.frontier)?;
    Ok(())
}

pub fn current_tree(repository: &Repository) -> Result<Tree> {
    replay::materialize_tree(repository, &repository.frontier)
}

/// Materialize a version, rejecting one the repository does not know
/// (SPEC §4.1).
pub fn known_tree(repository: &Repository, version: &Version) -> Result<Tree> {
    for (id, revision) in version.iter() {
        if repository.find(id, revision).is_none() {
            return Err(error::unknown_version(&version.to_string()));
        }
    }
    replay::materialize_tree(repository, version)
        .map_err(|_| error::unknown_version(&version.to_string()))
}
