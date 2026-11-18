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
use crate::fetch::{Fetch, Reporter};
use crate::ids::DepName;
use crate::links::Links;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::materialized::{MaterializedPlugin, MaterializedSkill, Selected};
use crate::owned::Ownership;
use crate::scope::Scope;
use crate::transitive;
use crate::vendor::Vendor;
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeSet;
use std::path::PathBuf;

mod apply;
mod guard;
mod resolve;
mod validate;

pub(super) use guard::{guard_working_copies, Writes};
pub(super) use validate::denied_sources;

/// What a sync writes once everything is resolved and checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
/// resolve manifest -> ai.lock -> populate store -> materialize vendor config,
/// as `mode` allows ([`Mode::Write`] or [`Mode::Locked`]; links and checks
/// have their own entry points, [`sync_with_links`] and [`check_lock`]).
pub(super) fn sync(scope: &Scope, mode: Mode) -> Result<usize> {
    let session = ScopeSession::open(scope, Access::Write)?;
    let manifest = session.manifest()?;
    let links = session.links()?;
    plan(
        &session,
        Request {
            manifest: &manifest,
            links,
            loaded: None,
            mode,
        },
    )?
    .apply(scope)
}

/// Sync with an edited set of `spm link` overrides, persisting them only once
/// the sync has been planned successfully (an unreadable or blocked linked
/// directory leaves the links file untouched). `ai.json` and `ai.lock` are not
/// affected by links: the lock still pins every dependency's declared source,
/// so this runs in [`Mode::Links`] — it never re-resolves a pin nor writes
/// `ai.lock`, and fails instead when `ai.lock` is out of sync with `ai.json`.
/// `links` must have been read and edited under `session`, opened for
/// [`Access::Links`].
pub(super) fn sync_with_links(session: ScopeSession, links: Links) -> Result<usize> {
    let manifest = session.manifest()?;
    plan(
        &session,
        Request {
            manifest: &manifest,
            links,
            loaded: None,
            mode: Mode::Links,
        },
    )?
    .apply(session.scope())
}

/// Which pins a [`Mode::Write`] sync re-resolves although `ai.lock` still
/// matches them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refresh<'a> {
    /// Reuse every pin that still matches `ai.json`.
    None,
    /// `spm update`: re-resolve every ref to its latest commit, the
    /// transitive frontier included.
    All,
    /// `spm update <name>`: re-resolve only this direct dependency.
    Only(&'a str),
}

impl<'a> Refresh<'a> {
    /// `spm update [name]`.
    pub(super) fn update(name: Option<&'a str>) -> Self {
        name.map_or(Refresh::All, Refresh::Only)
    }

    /// Whether the direct dependency `name` is re-resolved.
    fn direct(self, name: &str) -> bool {
        match self {
            Refresh::None => false,
            Refresh::All => true,
            Refresh::Only(only) => only == name,
        }
    }

    /// Only a bare `spm update` (no name) refreshes the transitive frontier.
    /// `spm update <name>` is deliberately surgical: it re-resolves the named
    /// direct root and re-reads its nested manifest, but does NOT cascade a
    /// moving-ref refresh into that root's unchanged transitive children —
    /// because those children are deduplicated across roots, so refreshing one
    /// root's subtree could advance an unrelated root's dependency. See
    /// `resolve_child` and docs/guide/transitive-dependencies.md.
    fn transitive(self) -> bool {
        self == Refresh::All
    }
}

/// How a sync runs: the one place its treatment of `ai.lock`, the store, the
/// links and the targets is decided. Every phase derives what it needs from
/// it, so no combination of those can contradict another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode<'a> {
    /// Re-resolve what changed, is unpinned or `refresh` names, then write
    /// `ai.lock` — and, unless [`Apply::LockOnly`], materialize into the
    /// targets.
    Write { refresh: Refresh<'a>, apply: Apply },
    /// `spm install --locked`: install exactly `ai.lock`, never write it. Every
    /// direct and transitive dependency must already be pinned for exactly
    /// the request in `ai.json`, and `ai.lock` must not carry stale entries;
    /// every mismatch is reported in one error before anything is
    /// materialized. Fetching into the store is still allowed.
    Locked,
    /// `spm link`/`spm unlink`: like [`Mode::Locked`] — they promise to leave
    /// `ai.lock` byte-identical — but with their own guidance on failure.
    Links,
    /// `spm doctor`: judge `ai.lock` by the exact [`Mode::Locked`] rules,
    /// read-only and offline. Nothing is fetched, scanned, printed or written,
    /// links are ignored, and every mismatch is returned instead of raised (see
    /// [`check_lock`]).
    Check,
}

impl<'a> Mode<'a> {
    /// `spm install [--locked]`.
    pub(super) fn install(locked: bool) -> Self {
        if locked {
            Mode::Locked
        } else {
            Mode::Write {
                refresh: Refresh::None,
                apply: Apply::Install,
            }
        }
    }

    /// Whether `ai.lock` is judged rather than (re)written: no pin is ever
    /// re-resolved, and every way it disagrees with `ai.json` is a finding.
    fn locked(self) -> bool {
        !matches!(self, Mode::Write { .. })
    }

    fn check(self) -> bool {
        self == Mode::Check
    }

    fn lock_only(self) -> bool {
        matches!(
            self,
            Mode::Write {
                apply: Apply::LockOnly,
                ..
            }
        )
    }

    /// Which pins are re-resolved although they still match.
    fn refresh(self) -> Refresh<'a> {
        match self {
            Mode::Write { refresh, .. } => refresh,
            Mode::Locked | Mode::Links | Mode::Check => Refresh::None,
        }
    }

    /// Whether `spm link` overrides are ignored: `ai.lock` never depends on
    /// them, and a lock-only sync writes nothing else (so a missing linked
    /// directory can't block it), nor does a check.
    fn ignores_links(self) -> bool {
        self.lock_only() || self.check()
    }

    /// Whether a pin reused from `ai.lock` may keep what it recorded from its
    /// checkout (a plugin's bundled skills) because that checkout may go
    /// unread: a lock-only sync does not fetch it, a check may not have it.
    fn carries_reused_pins(self) -> bool {
        self.lock_only() || self.check()
    }

    /// How a plugin's checkout is obtained; `fresh` are the pins this run
    /// (re)resolved.
    fn fetch(self, fresh: &BTreeSet<DepName>) -> Fetch<'_> {
        match self {
            Mode::Write {
                apply: Apply::LockOnly,
                ..
            } => Fetch::Fresh(fresh),
            Mode::Write {
                apply: Apply::Install,
                ..
            }
            | Mode::Locked
            | Mode::Links => Fetch::Online,
            Mode::Check => Fetch::Offline,
        }
    }

    /// How the transitive walk treats pins and the store.
    fn walk(self) -> transitive::Mode {
        match self {
            Mode::Write { refresh, .. } => transitive::Mode::Resolve {
                refresh: refresh.transitive(),
            },
            Mode::Locked | Mode::Links => transitive::Mode::Locked,
            Mode::Check => transitive::Mode::Check,
        }
    }

    /// Where progress goes, names padded to `width`: nowhere in a check.
    fn reporter(self, width: usize) -> Reporter {
        if self.check() {
            Reporter::silent()
        } else {
            Reporter::new(width)
        }
    }

    /// Who rejects a missing or out-of-sync `ai.lock` in this mode.
    fn enforcer(self) -> Enforcer {
        match self {
            Mode::Links => Enforcer::Links,
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
        Request {
            manifest: &validated,
            links: session.links()?,
            loaded: None,
            mode: Mode::Write {
                refresh: Refresh::None,
                apply,
            },
        },
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
struct Plan<'a> {
    dir: PathBuf,
    targets: Vec<String>,
    /// Whether any skill or plugin is declared (else nothing is announced).
    declares_any: bool,
    vendors: Vec<Box<dyn Vendor>>,
    lock: Lockfile,
    materialized: Vec<MaterializedSkill>,
    plugins: Vec<MaterializedPlugin>,
    previously_owned: Ownership,
    mode: Mode<'a>,
    report: Reporter,
    /// The `spm link` overrides to persist after applying (with `managed`
    /// refreshed), or `None` when the links file is already up to date.
    links: Option<Links>,
    _snapshots: Option<apply::Snapshots>,
    /// [`Mode::Check`] only: the verdict on `ai.lock`.
    verdict: LockFindings,
}

/// How one [`plan`] runs, as every phase needs it.
#[derive(Clone, Copy)]
struct Run<'a> {
    mode: Mode<'a>,
    /// Where per-entry progress goes, aligned over skills + plugins.
    report: Reporter,
}

/// What [`plan`] is asked to plan.
struct Request<'a> {
    manifest: &'a Manifest,
    /// The `spm link` overrides, read under the planning session — opened for
    /// [`Access::Links`] when they were edited ([`Mode::Links`]) — which the
    /// caller holds until the plan has been applied (it is only borrowed by
    /// [`plan`], so no failure can release it early), so no other command
    /// changes link state in between.
    links: Links,
    /// The scope's state already read under the session (or why it couldn't
    /// be), used instead of reading `ai.lock` and ownership again; a load
    /// error fails the plan just where reading them would have.
    loaded: Option<Result<&'a ScopeSnapshot, &'a anyhow::Error>>,
    mode: Mode<'a>,
}

/// Resolve `req.manifest` -> lock, populate the store, and run every check
/// (source policy, content scan, collisions, `--locked` drift). Writes nothing
/// outside the global store: no target output, no `ai.json`, no `ai.lock`.
fn plan<'a>(session: &ScopeSession, req: Request<'a>) -> Result<Plan<'a>> {
    let Request {
        manifest,
        mut links,
        loaded,
        mode,
    } = req;
    let scope = session.scope();
    let width = manifest
        .dependencies()
        .map(|(_, n, _)| n.len())
        .max()
        .unwrap_or(0);
    let run = Run {
        mode,
        report: mode.reporter(width),
    };
    let validate::Preflight {
        policy,
        mut linked,
        vendors,
    } = validate::preflight(session, manifest, &links, mode)?;
    let dir = session.dir().to_path_buf();
    let (prev, previously_owned) = previous(session, manifest, &links, loaded)?;
    // `--locked` only: every way `ai.lock` disagrees with `ai.json` — and,
    // for `Mode::Check` only, the entries whose store checkout is missing,
    // so what they contribute to `ai.lock` could not be verified offline.
    let mut findings = LockFindings::default();
    let mut res = resolve::direct(manifest, prev, &dir, run, &linked, &mut findings)?;
    // Scan and materialize linked content from one snapshot, so a later edit
    // to a working copy can't reach a target unscanned (or only some targets).
    let snapshots = apply::Snapshots::take(&mut linked)?;
    let fetched = resolve::plugins(manifest, &mut res, &linked, run, &mut findings)?;
    let mut expansion = resolve::skills(manifest, &mut res, run, &policy, &linked)?;
    if mode.locked() {
        let resolve_transitive = manifest.resolve_transitive;
        resolve::judge_transitive(&mut res, &mut expansion, resolve_transitive, &mut findings);
    }
    // Only skill subtrees are walked: a plugin missing from the store keeps
    // its locked entry, so it never makes the built lock partial.
    let unwalked_skills = expansion.unverified.clone();
    findings.unverified.extend(expansion.unverified);
    let mut selected = expansion.selected;
    let mut lock = res.lock;
    resolve::fold_transitive(&mut lock, expansion.transitive, run.report)?;
    validate::collisions(&mut selected, fetched.skills, &lock)?;
    // Only scanned content reaches the targets; a check selects names only.
    let materialized: Vec<MaterializedSkill> = selected
        .into_iter()
        .filter_map(Selected::materialized)
        .collect();
    // A check writes nothing, so it has nothing to guard.
    if !mode.check() {
        let out = validate::Output {
            vendors: &vendors,
            previously_owned: &previously_owned,
            materialized: &materialized,
            plugins: &fetched.plugins,
            linked: &linked,
            snapshot: snapshots.as_ref().map(|s| s.root.as_path()),
        };
        validate::guard(scope, &dir, manifest, out, mode.lock_only())?;
    }
    if mode.locked() && findings.in_sync() {
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
    let verdict = if mode.check() {
        findings
    } else {
        if mode.locked() {
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
    let links = (!mode.check() && session.links().ok().as_ref() != Some(&links)).then_some(links);
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
        mode,
        report: run.report,
        links,
        _snapshots: snapshots,
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
    let req = Request {
        manifest,
        links,
        loaded: Some(state),
        mode: Mode::Check,
    };
    match plan(session, req) {
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
            let req = Request {
                manifest: &manifest,
                links: Links::default(),
                loaded: None,
                mode,
            };
            let err = plan(&session, req).err().unwrap();
            format!("{err:#}")
        };
        for mode in [Mode::install(false), Mode::Locked, Mode::Check] {
            assert!(err(Access::Read, mode).contains("requires the state lock"));
        }
        for access in [Access::Read, Access::Write] {
            assert!(err(access, Mode::Links).contains("exclusive links lock"));
        }
        // Past the guards with the right lock (failing later: there is no ai.lock).
        assert!(err(Access::Links, Mode::Links).contains("ai.lock not found"));
        assert!(err(Access::Write, Mode::Check).contains("ai.lock not found"));
        std::fs::remove_dir_all(root).unwrap();
    }

    const MODES: [Mode<'static>; 6] = [
        Mode::Write {
            refresh: Refresh::None,
            apply: Apply::Install,
        },
        Mode::Write {
            refresh: Refresh::All,
            apply: Apply::LockOnly,
        },
        Mode::Write {
            refresh: Refresh::Only("a"),
            apply: Apply::Install,
        },
        Mode::Locked,
        Mode::Links,
        Mode::Check,
    ];

    /// A check reports nothing: its reporter is the silent one, whatever the
    /// width; every other mode reports.
    #[test]
    fn only_a_check_reports_nothing() {
        for mode in MODES {
            assert_eq!(
                mode.reporter(7) == Reporter::silent(),
                mode == Mode::Check,
                "{mode:?}"
            );
        }
    }

    /// `spm update <name>` refreshes the named root only: its walk reuses
    /// every transitive pin, which only a bare `spm update` refreshes.
    #[test]
    fn only_a_bare_update_refreshes_the_transitive_frontier() {
        let walk = |refresh| {
            Mode::Write {
                refresh,
                apply: Apply::LockOnly,
            }
            .walk()
        };
        assert_eq!(
            walk(Refresh::update(None)),
            transitive::Mode::Resolve { refresh: true }
        );
        for refresh in [Refresh::update(Some("a")), Refresh::None] {
            assert_eq!(walk(refresh), transitive::Mode::Resolve { refresh: false });
        }
    }

    /// Without a refresh nothing is re-resolved; `spm update <name>` refreshes
    /// only that direct root and never the transitive frontier.
    #[test]
    fn refresh_is_surgical_for_a_named_update() {
        let named = Refresh::update(Some("a"));
        assert!(named.direct("a") && !named.direct("b") && !named.transitive());
        let bare = Refresh::update(None);
        assert!(bare.direct("a") && bare.direct("b") && bare.transitive());
        assert!(!Refresh::None.direct("a") && !Refresh::None.transitive());
    }
}
