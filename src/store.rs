use crate::{git, lockfile::LockedSkill, paths};
use anyhow::{bail, Result};
use std::path::PathBuf;

/// Outcome of ensuring a skill is in the store.
pub struct Ensured {
    /// Absolute path to the skill content (repo root, or subdir if `path` is set).
    pub path: PathBuf,
    /// True if the repo was fetched now; false if already present in the store.
    pub fetched: bool,
}

/// Ensure the repo@commit for `locked` is present in the global store, fetching if needed.
pub fn ensure(locked: &LockedSkill) -> Result<Ensured> {
    let repo_dir = paths::store_dir()?.join(&locked.store);

    let fetched = !git::is_at_commit(&repo_dir, &locked.commit);
    if fetched {
        // Stale or missing: wipe and re-fetch to keep the store immutable-per-key.
        if repo_dir.exists() {
            std::fs::remove_dir_all(&repo_dir)?;
        }
        std::fs::create_dir_all(repo_dir.parent().unwrap())?;
        git::fetch_commit(&locked.git, &locked.commit, &repo_dir)?;
    }

    let content = match &locked.path {
        Some(sub) => repo_dir.join(sub),
        None => repo_dir,
    };
    if !content.exists() {
        bail!(
            "path `{}` not found in {}@{}",
            locked.path.as_deref().unwrap_or("."),
            locked.git,
            &locked.commit[..locked.commit.len().min(8)]
        );
    }
    Ok(Ensured {
        path: content,
        fetched,
    })
}
