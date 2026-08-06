use crate::{git, lockfile::LockedSkill, paths};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

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
        None => repo_dir.clone(),
    };
    if !content.exists() {
        bail!(
            "path `{}` not found in {}@{}",
            locked.path.as_deref().unwrap_or("."),
            locked.git,
            &locked.commit[..locked.commit.len().min(8)]
        );
    }
    // Defense in depth: the subdir is validated lexically upstream, but a
    // symlinked directory inside the repo could still resolve outside it.
    // Canonicalize and require the content to stay within the checkout.
    let repo_canon = repo_dir.canonicalize()?;
    let content_canon = content.canonicalize()?;
    if !content_canon.starts_with(&repo_canon) {
        bail!(
            "path `{}` escapes the repository checkout for {}",
            locked.path.as_deref().unwrap_or("."),
            locked.git
        );
    }
    Ok(Ensured {
        path: content_canon,
        fetched,
    })
}

/// A snapshot of what the global store holds, for `spm prune` to report.
pub struct StoreStats {
    /// Number of cached checkouts (one directory per (repo, commit) key).
    pub entries: usize,
    /// Approximate bytes on disk across all checkouts: the sum of file sizes,
    /// excluding directory metadata and filesystem block/overhead — hence the
    /// `~` the CLI prints in front of it.
    pub bytes: u64,
}

/// Inspect the global store without modifying it. A missing store reads as
/// empty rather than an error, so `prune` on a never-populated home is a no-op.
///
/// Counts only directory entries: the store layout is one directory per
/// (repo, commit) key, so stray non-directory files (e.g. a macOS `.DS_Store`)
/// are ignored rather than inflating the reported checkout count.
pub fn stats() -> Result<StoreStats> {
    let dir = paths::store_dir()?;
    if !dir.exists() {
        return Ok(StoreStats {
            entries: 0,
            bytes: 0,
        });
    }
    let mut entries = 0;
    let mut bytes = 0;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        // `file_type` from a read_dir entry does not follow symlinks, so a
        // symlink is never miscounted as a checkout directory.
        if !entry.file_type()?.is_dir() {
            continue;
        }
        entries += 1;
        bytes += dir_size(&entry.path())?;
    }
    Ok(StoreStats { entries, bytes })
}

/// Remove the entire global store. Safe to run at any time: `ensure` re-creates
/// and re-fetches keys on demand, so the only cost of pruning is re-downloading
/// whatever a later `install` needs.
pub fn remove_all() -> Result<()> {
    let dir = paths::store_dir()?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Recursive on-disk size of `path`. Uses `symlink_metadata` so symlinks are
/// counted as their own (tiny) size and never followed — avoids both double
/// counting and traversal cycles.
fn dir_size(path: &Path) -> Result<u64> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_dir() {
        let mut total = 0;
        for entry in std::fs::read_dir(path)? {
            total += dir_size(&entry?.path())?;
        }
        Ok(total)
    } else {
        Ok(meta.len())
    }
}
