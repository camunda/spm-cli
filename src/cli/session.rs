//! The one way a command reaches a scope's state.
//!
//! A [`ScopeSession`] takes the scope's [`StateLock`] (for every [`Access`]
//! but [`Access::Read`]) *before* anything is read, then loads `ai.json`,
//! `ai.lock`, the `spm link` overrides and target ownership on demand —
//! [`ScopeSession::snapshot`] in their canonical order. A command holds its
//! session until its last write (passing it on to `sync`), so whatever it read
//! can't change underneath it. The lock is private to this module, so no
//! command can take it any other way — nor, by [`StateLock::acquire`]'s guard,
//! twice in one process.

use crate::links::Links;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::owned::Ownership;
use crate::paths;
use crate::scope::Scope;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// `$SPM_HOME` file locked machine-wide by [`StateLock`].
const LOCK_FILE: &str = "links.lock";
/// Per-scope lock files, under `$SPM_HOME` (see [`StateLock`]).
const LOCK_DIR: &str = "locks";

/// How a command uses a scope's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Access {
    /// Report only (`list`, `status`, `outdated`): takes no lock, so reading
    /// never waits on (nor delays) another command.
    Read,
    /// Read, then write `ai.json`, `ai.lock`, target output or ownership
    /// records — or just read them all as one consistent view (`spm doctor`):
    /// the scope's lock exclusively, the machine-wide one shared.
    Write,
    /// [`Access::Write`] that may also activate a link (`spm link`, `spm
    /// unlink`): both locks exclusively.
    Links,
}

/// A command's hold on one scope: its lock (see [`Access`]) and the loaders
/// for its state.
pub(super) struct ScopeSession {
    scope: Scope,
    dir: PathBuf,
    access: Access,
    _lock: Option<StateLock>,
}

/// A scope's whole state, loaded in canonical order by
/// [`ScopeSession::snapshot`].
pub(super) struct ScopeSnapshot {
    pub(super) manifest: Manifest,
    pub(super) lock: Lockfile,
    pub(super) links: Links,
    pub(super) ownership: Ownership,
}

impl ScopeSession {
    /// Take the lock `access` requires, waiting (and saying so on stderr)
    /// while another command holds it in a conflicting mode.
    pub(super) fn open(scope: &Scope, access: Access) -> Result<Self> {
        let lock = match access {
            Access::Read => None,
            Access::Write => Some(StateLock::acquire(scope, false)?),
            Access::Links => Some(StateLock::acquire(scope, true)?),
        };
        Ok(Self {
            dir: scope.manifest_dir()?,
            scope: scope.clone(),
            access,
            _lock: lock,
        })
    }

    pub(super) fn scope(&self) -> &Scope {
        &self.scope
    }

    /// The directory holding this scope's `ai.json` and `ai.lock`.
    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn access(&self) -> Access {
        self.access
    }

    /// `ai.json`, validated.
    pub(super) fn manifest(&self) -> Result<Manifest> {
        Manifest::load(&self.dir, &self.scope)
    }

    /// `ai.lock`, or an empty lock when there is none.
    pub(super) fn lockfile(&self) -> Result<Lockfile> {
        Lockfile::load_or_default(&self.dir)
    }

    /// The `spm link` overrides (none when there is no links file).
    pub(super) fn links(&self) -> Result<Links> {
        Links::load(&self.scope)
    }

    /// What spm owns in each of `targets`, given the `lock` and `links` it is
    /// judged against (as on disk, or as a sync is about to change them).
    pub(super) fn ownership(
        &self,
        lock: &Lockfile,
        links: &Links,
        targets: &[String],
    ) -> Result<Ownership> {
        Ownership::load(&self.scope, lock, &links.managed, targets)
    }

    /// Load everything, in the canonical order: `ai.json`, `ai.lock`, links,
    /// then the ownership derived from them.
    pub(super) fn snapshot(&self) -> Result<ScopeSnapshot> {
        self.snapshot_with(self.manifest()?)
    }

    /// [`ScopeSession::snapshot`] for a `manifest` already read from this
    /// session: the single definition of the order the rest is loaded in.
    pub(super) fn snapshot_with(&self, manifest: Manifest) -> Result<ScopeSnapshot> {
        let lock = self.lockfile()?;
        let links = self.links()?;
        let ownership = self.ownership(&lock, &links, &manifest.targets)?;
        Ok(ScopeSnapshot {
            manifest,
            lock,
            links,
            ownership,
        })
    }
}

/// Whether this process holds a [`StateLock`]. The lock files are locked per
/// open file, so a second acquisition in the same process would wait on the
/// first forever.
static HELD: AtomicBool = AtomicBool::new(false);

/// The locks over link state and target ownership, held by a mutating
/// command from before it reads link state until its last write or deletion:
///
/// - the scope's own lock, always exclusively: commands on the same project
///   (or on the global scope) run one at a time, so none plans from ownership
///   records (`managed`) another is about to replace;
/// - the machine-wide lock: exclusively by commands that may activate a link
///   (`spm link`, `spm unlink`), shared by every other one. So no link can be
///   published into another project's output between that project's command
///   checking for links and rewriting the output — while commands on
///   different projects still run side by side.
///
/// Always taken in that order, so two commands can't deadlock. Lock files live
/// in their own directory, never next to the links files (which may be
/// redirected into a target's output).
struct StateLock {
    _scope: std::fs::File,
    _machine: std::fs::File,
}

impl StateLock {
    /// Take both locks; `exclusive` is for the machine-wide lock. Fails,
    /// rather than deadlocks, when this process already holds them.
    fn acquire(scope: &Scope, exclusive: bool) -> Result<Self> {
        if HELD.swap(true, Ordering::SeqCst) {
            bail!("internal error: this spm process already holds the state lock");
        }
        Self::lock(scope, exclusive).inspect_err(|_| HELD.store(false, Ordering::SeqCst))
    }

    fn lock(scope: &Scope, exclusive: bool) -> Result<Self> {
        let home = paths::spm_home()?;
        let stem = Links::path(scope)?
            .file_stem()
            .context("links file has no name")?
            .to_owned();
        let mut scope_lock = home.join(LOCK_DIR).join(stem);
        scope_lock.set_extension("lock");
        Ok(Self {
            _scope: lock(&scope_lock, true)?,
            _machine: lock(&home.join(LOCK_FILE), exclusive)?,
        })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        HELD.store(false, Ordering::SeqCst);
    }
}

/// Open (creating it and its directory) and lock the file `p`.
fn lock(p: &Path, exclusive: bool) -> Result<std::fs::File> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(p)
        .with_context(|| format!("opening {}", p.display()))?;
    let attempt = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    match attempt {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            eprintln!(
                "waiting for another spm command to finish (lock: {})...",
                p.display()
            );
            if exclusive {
                file.lock()
            } else {
                file.lock_shared()
            }
            .with_context(|| format!("locking {}", p.display()))?;
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(e).with_context(|| format!("locking {}", p.display()))
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::SpmHomeGuard;

    /// An isolated `SPM_HOME` (held for the test, which also serializes every
    /// test that takes the process-wide lock) and a project scope.
    fn sandbox(name: &str) -> (PathBuf, Scope, SpmHomeGuard) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("spm-session-{name}-{nanos}"));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let guard = SpmHomeGuard::set(&root.join("home"));
        (root, Scope::Project { root: project }, guard)
    }

    /// Whether another open file description could lock `p` in this mode now.
    fn lockable(p: &Path, exclusive: bool) -> bool {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        if exclusive {
            f.try_lock().is_ok()
        } else {
            f.try_lock_shared().is_ok()
        }
    }

    /// Whether an exclusive lock on `p` is granted (blocking, like `spm`
    /// itself) within a generous bound. A non-blocking probe is not enough
    /// for release: a child spawned concurrently by another test inherits the
    /// open file description, keeping the flock until its `exec` closes it.
    fn released(p: &Path) -> bool {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f.lock().is_ok());
        });
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .unwrap_or(false)
    }

    fn scope_lock(root: &Path) -> PathBuf {
        let dir = root.join("home").join(LOCK_DIR);
        let mut entries = std::fs::read_dir(dir).unwrap();
        let file = entries.next().unwrap().unwrap().path();
        assert!(entries.next().is_none(), "one scope lock");
        file
    }

    #[test]
    fn a_process_never_takes_the_state_lock_twice() {
        let (root, scope, _home) = sandbox("twice");
        for access in [Access::Write, Access::Links] {
            let held = ScopeSession::open(&scope, access).unwrap();
            for again in [Access::Write, Access::Links] {
                let err = ScopeSession::open(&scope, again).err().unwrap();
                assert!(format!("{err:#}").contains("already holds the state lock"));
            }
            // Nor on another scope: the machine-wide lock is shared by all.
            let err = ScopeSession::open(&Scope::Global, Access::Write)
                .err()
                .unwrap();
            assert!(format!("{err:#}").contains("already holds the state lock"));
            drop(held);
            drop(ScopeSession::open(&scope, access).unwrap());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_acquisition_does_not_count_as_held() {
        let (root, scope, _home) = sandbox("failed");
        // `$SPM_HOME` is a file, so no lock file can be created under it.
        std::fs::write(root.join("home"), "").unwrap();
        assert!(ScopeSession::open(&scope, Access::Write).is_err());
        std::fs::remove_file(root.join("home")).unwrap();
        drop(ScopeSession::open(&scope, Access::Write).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn access_selects_the_locks_taken() {
        let (root, scope, _home) = sandbox("access");
        let machine = root.join("home").join(LOCK_FILE);

        let read = ScopeSession::open(&scope, Access::Read).unwrap();
        assert!(!machine.exists(), "reading takes no lock");
        // ...so it never blocks a writer.
        let write = ScopeSession::open(&scope, Access::Write).unwrap();
        drop(read);
        let scope_file = scope_lock(&root);
        assert!(!lockable(&scope_file, false), "scope lock is exclusive");
        assert!(lockable(&machine, false), "machine lock is shared");
        assert!(!lockable(&machine, true));
        drop(write);

        let links = ScopeSession::open(&scope, Access::Links).unwrap();
        assert_eq!(links.access(), Access::Links);
        assert!(!lockable(&scope_file, false), "scope lock is exclusive");
        assert!(!lockable(&machine, false), "machine lock is exclusive");
        drop(links);
        assert!(released(&scope_file) && released(&machine));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_loads_in_canonical_order() {
        let (root, scope, _home) = sandbox("order");
        let session = ScopeSession::open(&scope, Access::Read).unwrap();
        assert_eq!(session.dir(), scope.manifest_dir().unwrap());
        let ai_json = Manifest::path_in(session.dir());
        let ai_lock = Lockfile::path_in(session.dir());
        let links = Links::path(&scope).unwrap();
        std::fs::create_dir_all(links.parent().unwrap()).unwrap();
        for p in [&ai_json, &ai_lock, &links] {
            std::fs::write(p, "not json").unwrap();
        }
        let first_error = || format!("{:#}", session.snapshot().err().unwrap());
        // Each file is only read once the ones before it loaded.
        assert!(first_error().contains("ai.json"), "{}", first_error());
        std::fs::write(&ai_json, r#"{"targets": ["claude"]}"#).unwrap();
        assert!(first_error().contains("ai.lock"), "{}", first_error());
        std::fs::remove_file(&ai_lock).unwrap();
        assert!(
            first_error().contains(&links.display().to_string()),
            "{}",
            first_error()
        );
        std::fs::remove_file(&links).unwrap();
        let snapshot = session.snapshot().unwrap();
        assert_eq!(snapshot.manifest.targets, ["claude"]);
        assert!(snapshot.lock.skills.is_empty() && snapshot.lock.id.is_empty());
        assert_eq!(snapshot.links, Links::default());
        assert_eq!(
            snapshot.ownership,
            Ownership::load(&scope, &snapshot.lock, &[], &["claude".to_string()]).unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
