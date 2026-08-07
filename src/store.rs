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
/// `entries` counts only directory entries — the store layout is one directory
/// per (repo, commit) key, so stray non-directory files (e.g. a macOS
/// `.DS_Store`) must not inflate the checkout count. `bytes` sizes *every*
/// entry though, strays included: `prune` removes the whole store, so the
/// reported freed size must account for what it actually deletes.
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
        if entry.file_type()?.is_dir() {
            entries += 1;
        }
        bytes += dir_size(&entry.path())?;
    }
    Ok(StoreStats { entries, bytes })
}

/// Whether the store holds nothing to prune: the directory is absent or has no
/// entries at all. Deliberately distinct from `stats().entries == 0`, which
/// ignores stray non-checkout files (e.g. a macOS `.DS_Store`) — `prune` still
/// wants to remove those to honor its "everything under the store" contract, so
/// the "nothing to prune" gate must consider a stray-only store non-empty.
pub fn is_empty() -> Result<bool> {
    let dir = paths::store_dir()?;
    match std::fs::read_dir(&dir) {
        Ok(mut rd) => Ok(rd.next().is_none()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e.into()),
    }
}

/// Cheap count of cached checkouts: a single shallow read of the store root,
/// with no recursive size walk. Lets `prune`'s confirmation prompt show useful
/// context without paying the full `stats()` cost when the user may abort.
pub fn checkout_count() -> Result<usize> {
    let dir = paths::store_dir()?;
    let mut n = 0;
    match std::fs::read_dir(&dir) {
        Ok(rd) => {
            for entry in rd {
                if entry?.file_type()?.is_dir() {
                    n += 1;
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(n)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ENV_LOCK;
    use std::process::Command as StdCommand;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "spm-store-test-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ))
    }

    /// Point `SPM_HOME` at `home` for the duration of `f`, restoring whatever
    /// was there before. Must be called with `ENV_LOCK` held.
    fn with_spm_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let saved = std::env::var("SPM_HOME").ok();
        std::env::set_var("SPM_HOME", home);
        let result = f();
        match saved {
            Some(v) => std::env::set_var("SPM_HOME", v),
            None => std::env::remove_var("SPM_HOME"),
        }
        result
    }

    /// A never-populated store (no `store/` dir at all) reads as empty across
    /// every inspection function, rather than erroring.
    #[test]
    fn absent_store_reads_as_empty_everywhere() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = scratch("absent");
        std::fs::create_dir_all(&home).unwrap();

        with_spm_home(&home, || {
            assert!(is_empty().unwrap());
            assert_eq!(checkout_count().unwrap(), 0);
            let s = stats().unwrap();
            assert_eq!(s.entries, 0);
            assert_eq!(s.bytes, 0);
            // remove_all on an absent store is a no-op, not an error.
            remove_all().unwrap();
        });

        std::fs::remove_dir_all(&home).unwrap();
    }

    /// A non-`NotFound` I/O error reading the store dir (here: a plain file
    /// sitting where a directory is expected) must be surfaced, not swallowed
    /// as if the store were merely empty.
    #[test]
    fn blocked_store_path_surfaces_a_real_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = scratch("blocked");
        std::fs::create_dir_all(&home).unwrap();
        // A regular file named "store" instead of a directory.
        std::fs::write(home.join("store"), b"not a directory").unwrap();

        with_spm_home(&home, || {
            assert!(is_empty().is_err(), "expected a real error, not `Ok(true)`");
            assert!(checkout_count().is_err());
        });

        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Build a throwaway one-commit git repo, returning its HEAD sha.
    fn make_skill_repo(root: &Path) -> String {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        let run = |args: &[&str]| {
            assert!(StdCommand::new("git")
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["add", "-A"]);
        run(&["commit", "-qm", "initial"]);
        let out = StdCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `ensure` re-fetches when the cached checkout exists but is stale (not at
    /// the pinned commit) — e.g. a prior fetch was interrupted or the checkout
    /// was externally reset — rather than trusting a mismatched directory.
    #[test]
    fn ensure_refetches_a_stale_cached_checkout() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = scratch("stale-refetch");
        std::fs::create_dir_all(&home).unwrap();
        let src = scratch("stale-refetch-src");
        let sha = make_skill_repo(&src);

        let locked = LockedSkill {
            git: format!("file://{}", src.display()),
            reference: "branch:main".into(),
            commit: sha.clone(),
            path: None,
            store: crate::lockfile::store_key(&format!("file://{}", src.display()), &sha),
        };

        with_spm_home(&home, || {
            let first = ensure(&locked).unwrap();
            assert!(first.fetched);

            // Corrupt the cached checkout so it's no longer recognized as
            // being at the pinned commit, while the directory still exists.
            std::fs::remove_dir_all(first.path.join(".git")).unwrap();

            let second = ensure(&locked).unwrap();
            assert!(second.fetched, "a stale checkout must be re-fetched");
            assert!(second.path.join(".git").exists());
        });

        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&src).unwrap();
    }

    /// `ensure` must reject a locked `path` that doesn't exist in the fetched
    /// repo (e.g. a stale manifest entry after the upstream repo restructured).
    #[test]
    fn ensure_errors_when_locked_path_is_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = scratch("missing-path");
        std::fs::create_dir_all(&home).unwrap();
        let src = scratch("missing-path-src");
        let sha = make_skill_repo(&src);
        let git_url = format!("file://{}", src.display());

        let locked = LockedSkill {
            git: git_url.clone(),
            reference: "branch:main".into(),
            commit: sha.clone(),
            path: Some("does/not/exist".into()),
            store: crate::lockfile::store_key(&git_url, &sha),
        };

        with_spm_home(&home, || {
            let Err(err) = ensure(&locked) else {
                panic!("expected an error");
            };
            assert!(format!("{err}").contains("not found in"), "{err}");
        });

        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&src).unwrap();
    }

    /// Defense in depth: a symlink inside the checkout that resolves outside
    /// the repo root must be rejected even though the lexical path passed
    /// schema validation upstream.
    #[cfg(unix)]
    #[test]
    fn ensure_rejects_symlink_escaping_the_checkout() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = scratch("symlink-escape");
        std::fs::create_dir_all(&home).unwrap();
        let src = scratch("symlink-escape-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        // A symlink that, once checked out into the store, resolves one level
        // above the repo checkout (i.e. escapes to the store root itself).
        std::os::unix::fs::symlink("..", src.join("escape")).unwrap();
        let run = |args: &[&str]| {
            assert!(StdCommand::new("git")
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&src)
                .status()
                .unwrap()
                .success());
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["add", "-A"]);
        run(&["commit", "-qm", "initial"]);
        let out = StdCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&src)
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let git_url = format!("file://{}", src.display());

        let locked = LockedSkill {
            git: git_url.clone(),
            reference: "branch:main".into(),
            commit: sha.clone(),
            path: Some("escape".into()),
            store: crate::lockfile::store_key(&git_url, &sha),
        };

        with_spm_home(&home, || {
            let Err(err) = ensure(&locked) else {
                panic!("expected an error");
            };
            assert!(
                format!("{err}").contains("escapes the repository checkout"),
                "{err}"
            );
        });

        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&src).unwrap();
    }
}
