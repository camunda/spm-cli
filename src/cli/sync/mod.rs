//! The sync pipeline shared by add/remove/update/install/link: [`plan`]
//! resolves, fetches and checks everything without writing outside the global
//! store, and [`Plan::apply`] writes the result into the targets. Each phase
//! lives in its own module:
//!
//! - [`validate`]: the checks before (policy, targets, links) and after
//!   (collisions, overlap guard, `--locked` backstop) resolution;
//! - [`resolve`]: pinning direct dependencies, fetching plugins and skills,
//!   and walking transitive ones;
//! - [`guard`]: the overlap guard protecting linked working copies and spm's
//!   own state files;
//! - [`apply`]: materializing a plan and persisting `ai.lock`.

use super::session::{Access, ScopeSession, ScopeSnapshot};
use super::verdict::{Enforcer, LockFindings, LockVerdict};
use crate::links::Links;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::owned::Ownership;
use crate::scope::Scope;
use crate::vendor::{MaterializedPlugin, MaterializedSkill, Vendor};
use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;

/// `println!` unless `$quiet` (a read-only [`LockMode::Check`] prints nothing).
macro_rules! say {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet {
            println!($($arg)*);
        }
    };
}

mod apply;
mod guard;
mod resolve;
mod validate;

pub(super) use guard::{guard_working_copies, Writes};
pub(super) use validate::denied_sources;

/// What a sync writes once everything is resolved and checked.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Apply {
    /// Materialize into every target, then write `ai.lock`.
    Install,
    /// `--lock-only`: write `ai.lock` only. Only the entries this run
    /// (re)resolves are fetched and scanned; every entry reused from `ai.lock`
    /// is carried over as is, without touching its remote or the store (with
    /// `resolveTransitive` on, skills are still walked to read their nested
    /// `ai.json`). Nothing is materialized.
    LockOnly,
}

impl Apply {
    pub(super) fn from_lock_only(lock_only: bool) -> Self {
        if lock_only {
            Apply::LockOnly
        } else {
            Apply::Install
        }
    }
}

/// Core pipeline shared by add/remove/update/install:
/// resolve manifest -> ai.lock -> populate store -> materialize vendor config.
///
/// `force_refresh` re-resolves refs to their latest commit; `only` limits the
/// refresh to a single skill (used by `update <name>`).
///
/// `locked` (`spm install --locked`) forbids any re-resolution: every direct
/// and transitive dependency must already be pinned in `ai.lock` for exactly
/// the request in `ai.json`, `ai.lock` must not carry stale entries, and it is
/// never written. Every mismatch is collected and reported in one error before
/// anything is materialized. Fetching into the store is still allowed.
pub(super) fn sync(
    scope: &Scope,
    force_refresh: bool,
    only: Option<&str>,
    locked: bool,
    apply: Apply,
) -> Result<usize> {
    let session = ScopeSession::open(scope, Access::Write)?;
    let manifest = session.manifest()?;
    let links = session.links()?;
    let mode = if locked {
        LockMode::Locked
    } else {
        LockMode::Write(apply)
    };
    plan(&session, &manifest, links, None, force_refresh, only, mode)?.apply(scope)
}

/// Sync with an edited set of `spm link` overrides, persisting them only once
/// the sync has been planned successfully (an unreadable or blocked linked
/// directory leaves the links file untouched). `ai.json` and `ai.lock` are not
/// affected by links: the lock still pins every dependency's declared source,
/// so this runs in [`LockMode::Links`] — it never re-resolves a pin nor writes
/// `ai.lock`, and fails instead when `ai.lock` is out of sync with `ai.json`.
/// `links` must have been read and edited under `session`, opened for
/// [`Access::Links`].
pub(super) fn sync_with_links(session: ScopeSession, links: Links) -> Result<usize> {
    let manifest = session.manifest()?;
    plan(
        &session,
        &manifest,
        links,
        None,
        false,
        None,
        LockMode::Links,
    )?
    .apply(session.scope())
}

/// How a sync may treat `ai.lock`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LockMode {
    /// Re-resolve what changed or is unpinned, then write `ai.lock` — and,
    /// unless [`Apply::LockOnly`], materialize into the targets.
    Write(Apply),
    /// `spm install --locked`: install exactly `ai.lock`, never write it.
    Locked,
    /// `spm link`/`spm unlink`: like [`LockMode::Locked`] — they promise to
    /// leave `ai.lock` byte-identical — but with their own guidance on failure.
    Links,
    /// `spm doctor`: judge `ai.lock` by the exact [`LockMode::Locked`] rules,
    /// read-only and offline. Nothing is fetched, scanned, printed or written,
    /// links are ignored, and every mismatch is returned instead of raised (see
    /// [`check_lock`]).
    Check,
}

impl LockMode {
    /// Who rejects a missing or out-of-sync `ai.lock` in this mode.
    fn enforcer(self) -> Enforcer {
        match self {
            LockMode::Links => Enforcer::Links,
            _ => Enforcer::Locked,
        }
    }
}

/// Write an edited `manifest` to `ai.json` and [`sync`] it, without ever leaving
/// a failed change half-recorded.
///
/// Every command that edits `ai.json` and then syncs (`add`, `add --all`,
/// `remove`, `target add`) must go through here. The edited manifest is
/// validated and [`plan`]ned *in memory* first; `ai.json` is only written once
/// resolution, fetching, the content scan and collision checks have all passed.
/// So any of those failures (an unreachable source, a bad ref, a blocked scan,
/// ...) leaves `ai.json` byte-for-byte untouched and the same command can simply
/// be retried.
///
/// If [`Plan::apply`] then fails, some targets may already hold the new output,
/// so `ai.json` deliberately *keeps* the change: it is what records that spm
/// owns that output (shared skill dirs are cleaned by declared/locked name
/// only), and the next `spm install` finishes the job.
///
/// `manifest` must have been read from `session` (opened for
/// [`Access::Write`]), so a command queued behind another edits what that one
/// left.
pub(super) fn save_and_sync(
    session: ScopeSession,
    manifest: &Manifest,
    apply: Apply,
) -> Result<usize> {
    let scope = session.scope();
    let path = Manifest::path_in(session.dir());
    let text = manifest.to_text()?;
    // Validate exactly as the next `Manifest::load` will (schema, name
    // collisions across skills/plugins, ...), so an invalid edit is never saved.
    let validated = Manifest::parse(&text, &path)?;
    let plan = plan(
        &session,
        &validated,
        session.links()?,
        None,
        false,
        None,
        LockMode::Write(apply),
    )?;
    // Writes exactly `text`: `save` renders through the same `to_text`.
    manifest.save(session.dir())?;
    plan.apply(scope).map_err(|err| {
        anyhow!(
            "{err:#}\nhint: {} keeps this change because some targets may already have been \
             updated; fix the cause and run `{}` to finish applying it",
            path.display(),
            install_command(scope)
        )
    })
}

/// The outcome of [`plan`]: everything resolved, fetched into the store and
/// checked, ready to be written into the targets by [`Plan::apply`].
struct Plan {
    dir: PathBuf,
    targets: Vec<String>,
    /// Whether any skill or plugin is declared (else nothing is announced).
    declares_any: bool,
    vendors: Vec<Box<dyn Vendor>>,
    lock: Lockfile,
    materialized: Vec<MaterializedSkill>,
    plugins: Vec<MaterializedPlugin>,
    previously_owned: Ownership,
    locked: bool,
    /// The `spm link` overrides to persist after applying (with `managed`
    /// refreshed), or `None` when the links file is already up to date.
    links: Option<Links>,
    _snapshots: Option<apply::Snapshots>,
    apply: Apply,
    /// [`LockMode::Check`] only: the verdict on `ai.lock`.
    verdict: LockFindings,
}

/// How one [`plan`] runs, as every phase needs it.
#[derive(Clone, Copy)]
struct Run {
    /// Install exactly `ai.lock`: never re-resolve a pin (see [`LockMode`]).
    locked: bool,
    /// [`LockMode::Check`]: read-only and offline, printing nothing.
    check: bool,
    /// [`Apply::LockOnly`].
    lock_only: bool,
    /// Column width for aligned per-entry output (skills + plugins).
    width: usize,
}

impl Run {
    fn quiet(self) -> bool {
        self.check
    }
}

/// Resolve `manifest` -> lock, populate the store, and run every check (source
/// policy, content scan, collisions, `--locked` drift). Writes nothing outside
/// the global store: no target output, no `ai.json`, no `ai.lock`.
///
/// `links` must have been read under `session` — opened for [`Access::Links`]
/// when they were edited (`LockMode::Links`) — and the caller holds it until
/// the plan has been applied (it is only borrowed here, so no failure can
/// release it early), so no other command changes link state in between.
///
/// `loaded` is the scope's state already read under `session` (or why it
/// couldn't be), used instead of reading `ai.lock` and ownership again; a
/// load error fails the plan just where reading them would have.
fn plan(
    session: &ScopeSession,
    manifest: &Manifest,
    mut links: Links,
    loaded: Option<Result<&ScopeSnapshot, &anyhow::Error>>,
    force_refresh: bool,
    only: Option<&str>,
    mode: LockMode,
) -> Result<Plan> {
    let scope = session.scope();
    let (locked, apply) = match mode {
        LockMode::Write(apply) => (false, apply),
        LockMode::Locked | LockMode::Links | LockMode::Check => (true, Apply::Install),
    };
    let run = Run {
        locked,
        check: mode == LockMode::Check,
        lock_only: apply == Apply::LockOnly,
        width: manifest
            .dependencies()
            .map(|(_, n, _)| n.len())
            .max()
            .unwrap_or(0),
    };
    let validate::Preflight {
        policy,
        mut linked,
        vendors,
    } = validate::preflight(session, manifest, &links, mode, run)?;
    let dir = session.dir().to_path_buf();
    let (prev, previously_owned) = previous(session, manifest, &links, loaded)?;
    // `--locked` only: every way `ai.lock` disagrees with `ai.json` — and,
    // for `LockMode::Check` only, the entries whose store checkout is missing,
    // so what they contribute to `ai.lock` could not be verified offline.
    let mut findings = LockFindings::default();
    let refresh = resolve::Refresh {
        force: force_refresh,
        only,
    };
    let mut res = resolve::direct(manifest, prev, &dir, refresh, run, &linked, &mut findings)?;
    // Scan and materialize linked content from one snapshot, so a later edit
    // to a working copy can't reach a target unscanned (or only some targets).
    let snapshots = apply::Snapshots::take(&mut linked)?;
    let fetched = resolve::plugins(manifest, &mut res, &linked, run, &mut findings)?;
    let mut expansion = resolve::skills(manifest, &mut res, refresh, run, &policy, &linked)?;
    if locked {
        let resolve_transitive = manifest.resolve_transitive;
        resolve::judge_transitive(&mut res, &mut expansion, resolve_transitive, &mut findings);
    }
    // Only skill subtrees are walked: a plugin missing from the store keeps
    // its locked entry, so it never makes the built lock partial.
    let unwalked_skills = expansion.unverified.clone();
    findings.unverified.extend(expansion.unverified);
    let mut materialized = expansion.materialized;
    let mut lock = res.lock;
    resolve::fold_transitive(&mut lock, expansion.transitive, run)?;
    validate::collisions(&mut materialized, fetched.skills, &lock)?;
    // A check writes nothing, so it has nothing to guard.
    if !run.check {
        let out = validate::Output {
            vendors: &vendors,
            previously_owned: &previously_owned,
            materialized: &materialized,
            plugins: &fetched.plugins,
            linked: &linked,
            snapshot: snapshots.as_ref().map(|s| s.root.as_path()),
        };
        validate::guard(scope, &dir, manifest, out, run)?;
    }
    if locked && findings.in_sync() {
        let resolve_transitive = manifest.resolve_transitive;
        validate::backstop(
            &dir,
            resolve_transitive,
            &lock,
            &unwalked_skills,
            &mut findings,
        )?;
    }
    // A check returns its verdict; enforcing modes fail on it.
    let verdict = if run.check {
        findings
    } else {
        if locked {
            LockVerdict::Checked(findings).enforce(mode.enforcer(), scope)?;
        }
        LockFindings::default()
    };
    let mut linked_plugin_skills = fetched.linked_skills;
    linked_plugin_skills.sort();
    linked_plugin_skills.dedup();
    links.managed = linked_plugin_skills;
    // Only rewrite the links file when it changes (a `link`/`unlink`, or a
    // linked plugin's bundled skills moving), never on a plain sync.
    let links = (!run.check && session.links().ok().as_ref() != Some(&links)).then_some(links);
    if let Some(links) = &links {
        links.document(scope)?;
    }
    Ok(Plan {
        dir,
        targets: manifest.targets.clone(),
        declares_any: manifest.declares_any(),
        vendors,
        lock,
        materialized,
        plugins: fetched.plugins,
        previously_owned,
        locked,
        links,
        _snapshots: snapshots,
        apply,
        verdict,
    })
}

/// The previous `ai.lock` and ownership record of `session`'s scope: `loaded`
/// when given (or why it couldn't be), else read now.
fn previous(
    session: &ScopeSession,
    manifest: &Manifest,
    links: &Links,
    loaded: Option<Result<&ScopeSnapshot, &anyhow::Error>>,
) -> Result<(Lockfile, Ownership)> {
    match loaded {
        Some(Ok(state)) => Ok((state.lock.clone(), state.ownership.clone())),
        Some(Err(e)) => bail!("{e:#}"),
        None => {
            let prev = session.lockfile()?;
            // What spm owns in each target before this sync (see
            // `crate::owned`): global/shared-dir vendors share a directory
            // with the user's own skills, so they purge only entries spm put
            // there that are no longer wanted.
            let owned = session.ownership(&prev, links, &manifest.targets)?;
            Ok((prev, owned))
        }
    }
}

/// Judge `ai.lock` against `manifest` by the exact `spm install --locked`
/// rules, without fetching, scanning, printing or writing anything (`spm
/// doctor`). `manifest` and `state` (or the error loading it) must have been
/// read from `session`, which the caller keeps holding for its further reads;
/// `links_error` is why its links file couldn't be read, if it couldn't. Like
/// `install --locked`, a missing `ai.lock` is judged before anything else, and
/// links are read before `ai.lock`.
pub(super) fn check_lock(
    session: &ScopeSession,
    manifest: &Manifest,
    links_error: Option<&anyhow::Error>,
    state: Result<&ScopeSnapshot, &anyhow::Error>,
) -> LockVerdict {
    let path = Lockfile::path_in(session.dir());
    if !path.exists() {
        return LockVerdict::Missing {
            path,
            declared: manifest.declares_any(),
        };
    }
    if let Some(e) = links_error {
        return LockVerdict::Unchecked(anyhow!("{e:#}"));
    }
    // A check never resolves links, so none are needed until `state` is.
    let links = state.map_or_else(|_| Links::default(), |s| s.links.clone());
    match plan(
        session,
        manifest,
        links,
        Some(state),
        false,
        None,
        LockMode::Check,
    ) {
        Ok(plan) => LockVerdict::Checked(plan.verdict),
        Err(e) => LockVerdict::Unchecked(e),
    }
}

/// `spm unlink <name>`, spelled for the scope in use.
pub(super) fn unlink_command(scope: &Scope, name: &str) -> String {
    let g = if scope.is_global() { " -g" } else { "" };
    format!("spm unlink{g} {}", crate::shell::operands([name]))
}

/// `spm install`, spelled for the scope in use.
pub(super) fn install_command(scope: &Scope) -> &'static str {
    if matches!(scope, Scope::Global) {
        "spm install -g"
    } else {
        "spm install"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh source dir under the system temp dir holding `files`.
    pub(super) fn source(label: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spm-snapshot-src-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, body) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        std::fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn plan_requires_the_lock_its_mode_needs() {
        let root = source("plan-access", &[("project/.keep", "")]);
        let _home = crate::paths::SpmHomeGuard::set(&root.join("home"));
        let scope = Scope::Project {
            root: root.join("project"),
        };
        let manifest =
            Manifest::parse(r#"{"targets": ["claude"]}"#, &root.join("ai.json")).unwrap();
        let err = |access, mode| {
            let session = ScopeSession::open(&scope, access).unwrap();
            let err = plan(
                &session,
                &manifest,
                Links::default(),
                None,
                false,
                None,
                mode,
            )
            .err()
            .unwrap();
            format!("{err:#}")
        };
        for mode in [
            LockMode::Write(Apply::Install),
            LockMode::Locked,
            LockMode::Check,
        ] {
            assert!(err(Access::Read, mode).contains("requires the state lock"));
        }
        for access in [Access::Read, Access::Write] {
            assert!(err(access, LockMode::Links).contains("exclusive links lock"));
        }
        // Past the guards with the right lock (failing later: there is no ai.lock).
        assert!(err(Access::Links, LockMode::Links).contains("ai.lock not found"));
        assert!(err(Access::Write, LockMode::Check).contains("ai.lock not found"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
