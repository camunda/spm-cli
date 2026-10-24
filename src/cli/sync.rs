use crate::links::{Linked, Links, StateLock};
use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::{Manifest, SkillSpec};
use crate::owned::{Owned, Ownership};
use crate::scope::Scope;
use crate::vendor::{MaterializedPlugin, MaterializedSkill, Vendor};
use crate::{resolver, store, transitive};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

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
    let state_lock = StateLock::acquire(scope, false)?;
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir, scope)?;
    let links = Links::load(scope)?;
    let mode = if locked {
        LockMode::Locked
    } else {
        LockMode::Write(apply)
    };
    plan(
        scope,
        &manifest,
        state_lock,
        links,
        force_refresh,
        only,
        mode,
    )?
    .apply(scope)
}

/// Sync with an edited set of `spm link` overrides, persisting them only once
/// the sync has been planned successfully (an unreadable or blocked linked
/// directory leaves the links file untouched). `ai.json` and `ai.lock` are not
/// affected by links: the lock still pins every dependency's declared source,
/// so this runs in [`LockMode::Links`] — it never re-resolves a pin nor writes
/// `ai.lock`, and fails instead when `ai.lock` is out of sync with `ai.json`.
/// `links` must have been read and edited under `state_lock`, held exclusively.
pub(super) fn sync_with_links(scope: &Scope, state_lock: StateLock, links: Links) -> Result<usize> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir, scope)?;
    plan(
        scope,
        &manifest,
        state_lock,
        links,
        false,
        None,
        LockMode::Links,
    )?
    .apply(scope)
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
}

impl LockMode {
    /// Prefix for an error raised because `ai.lock` is missing or out of sync.
    fn label(self) -> &'static str {
        match self {
            LockMode::Links => "`spm link`/`spm unlink` never write ai.lock",
            _ => "--locked",
        }
    }

    /// How to fix such an error, spelled for the scope in use.
    fn fix_hint(self, scope: &Scope) -> String {
        match self {
            LockMode::Links => format!(
                "run `{}` first to bring ai.lock in sync, then retry",
                install_command(scope)
            ),
            _ => format!(
                "run `{}` (without --locked) and commit ai.lock",
                install_command(scope)
            ),
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
/// `state_lock` must have been taken before `manifest` was read (see
/// [`StateLock`]), so a command queued behind another edits what that one left.
pub(super) fn save_and_sync(
    scope: &Scope,
    state_lock: StateLock,
    manifest: &Manifest,
    apply: Apply,
) -> Result<usize> {
    let dir = scope.manifest_dir()?;
    let path = Manifest::path_in(&dir);
    let text = manifest.to_text()?;
    // Validate exactly as the next `Manifest::load` will (schema, name
    // collisions across skills/plugins, ...), so an invalid edit is never saved.
    let manifest = Manifest::parse(&text, &path)?;
    let plan = plan(
        scope,
        &manifest,
        state_lock,
        Links::load(scope)?,
        false,
        None,
        LockMode::Write(apply),
    )?;
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
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
    _snapshots: Option<Snapshots>,
    _state_lock: StateLock,
    apply: Apply,
}

/// Private copies of the linked directories, made with the same safe-copy
/// rules as materialization (no `.git`, no symlink leaving the directory), and
/// removed once the sync is done.
struct Snapshots {
    root: PathBuf,
}

impl Snapshots {
    /// Snapshot every link in `linked`, pointing its `content` at the copy.
    fn take(linked: &mut BTreeMap<String, Linked>) -> Result<Option<Self>> {
        if linked.is_empty() {
            return Ok(None);
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!("spm-links-{}-{nanos}", std::process::id()));
        // A fresh directory only its owner can enter: the copies may come from
        // a working copy other users can't read.
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            let mut b = builder;
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            b
        };
        builder
            .create(&root)
            .with_context(|| format!("creating {}", root.display()))?;
        let snapshots = Self {
            root: std::fs::canonicalize(&root)
                .with_context(|| format!("resolving {}", root.display()))?,
        };
        // Numbered slots, not names: names that differ only in case (e.g. a
        // skill `Foo` and a plugin `foo`) must not share one on a
        // case-insensitive filesystem.
        for (slot, (name, l)) in linked.iter_mut().enumerate() {
            if crate::fsutil::contains(&l.dir, &snapshots.root) {
                bail!(
                    "linked directory `{}` for `{name}` contains spm's temporary directory \
                     `{}` — link the skill or plugin directory itself",
                    l.display,
                    snapshots.root.display()
                );
            }
            let content = snapshots.root.join(slot.to_string());
            crate::fsutil::copy_tree(&l.dir, &content, &l.dir)
                .with_context(|| format!("snapshotting linked `{name}` ({})", l.display))?;
            l.content = content;
        }
        Ok(Some(snapshots))
    }
}

impl Drop for Snapshots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Resolve `manifest` -> lock, populate the store, and run every check (source
/// policy, content scan, collisions, `--locked` drift). Writes nothing outside
/// the global store: no target output, no `ai.json`, no `ai.lock`.
///
/// `links` must have been read under `state_lock` — exclusively when they were
/// edited (`LockMode::Links`) — and the plan holds it until it has been applied,
/// so no other command changes link state in between.
fn plan(
    scope: &Scope,
    manifest: &Manifest,
    state_lock: StateLock,
    mut links: Links,
    force_refresh: bool,
    only: Option<&str>,
    mode: LockMode,
) -> Result<Plan> {
    if mode == LockMode::Links && !state_lock.is_exclusive() {
        bail!("internal error: editing links requires the exclusive links lock");
    }
    let (locked, apply) = match mode {
        LockMode::Write(apply) => (false, apply),
        LockMode::Locked | LockMode::Links => (true, Apply::Install),
    };
    let lock_only = apply == Apply::LockOnly;
    let dir = scope.manifest_dir()?;
    if manifest.targets.is_empty() {
        bail!("ai.json declares no targets");
    }
    // Source policy: judge every direct source before any network access, and
    // report all denied ones at once.
    let policy = crate::policy::activate(manifest, &dir)?;
    // `ai.lock` never depends on links, and a lock-only sync writes nothing
    // else, so it ignores them (a missing linked directory can't block it).
    let mut linked = if lock_only {
        BTreeMap::new()
    } else {
        resolve_links(scope, manifest, &links)?
    };
    let denied: Vec<String> = manifest
        .skills
        .iter()
        .map(|(n, s)| ("skill", n, s))
        .chain(manifest.plugins.iter().map(|(n, s)| ("plugin", n, s)))
        .filter_map(|(kind, name, spec)| {
            policy
                .check(&spec.git)
                .err()
                .map(|e| format!("{kind} `{name}`: {e:#}"))
        })
        .chain(linked.iter().filter_map(|(name, l)| {
            policy
                .check_local(&l.display)
                .err()
                .map(|e| format!("link `{name}`: {e:#}"))
        }))
        .collect();
    if !denied.is_empty() {
        bail!(
            "{} dependenc{} not allowed by the source policy:\n{}",
            denied.len(),
            if denied.len() == 1 { "y is" } else { "ies are" },
            denied
                .iter()
                .map(|d| format!("  - {d}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    // Validate all targets up front before doing any network work.
    let vendors = manifest
        .targets
        .iter()
        .map(|t| crate::vendor::for_target(t))
        .collect::<Result<Vec<_>>>()?;
    if locked && !Lockfile::path_in(&dir).exists() {
        bail!(
            "{}: {} not found\n{}",
            mode.label(),
            Lockfile::path_in(&dir).display(),
            mode.fix_hint(scope)
        );
    }
    let mut prev = Lockfile::load_or_default(&dir)?;
    // `--locked` only: every way `ai.lock` disagrees with `ai.json`.
    let mut mismatches: Vec<String> = Vec::new();
    // `--locked` only: direct skills that failed the pin check and so are not
    // walked (their transitive subtree is never visited).
    let mut skipped_roots: BTreeSet<String> = BTreeSet::new();

    // What spm owns in each target before this sync (see `crate::owned`):
    // global/shared-dir vendors share a directory with the user's own skills,
    // so they purge only entries spm put there that are no longer wanted.
    let previously_owned = Ownership::load(scope, &prev, &links.managed, &manifest.targets)?;

    // Stable, path-independent project id: reuse the locked one, else mint & persist.
    let id = if prev.id.is_empty() {
        if locked {
            mismatches.push("ai.lock has no project `id`".to_string());
        }
        crate::lockfile::generate_id(&dir)
    } else {
        prev.id.clone()
    };
    let mut lock = Lockfile {
        id,
        ..Default::default()
    };

    // Column width for aligned per-entry output (skills + plugins).
    let width = manifest
        .skills
        .keys()
        .chain(manifest.plugins.keys())
        .map(String::len)
        .max()
        .unwrap_or(0);

    if manifest.skills.is_empty() && manifest.plugins.is_empty() {
        println!("no skills or plugins declared");
    } else {
        println!(
            "resolving {} skill(s) + {} plugin(s) for: {}",
            manifest.skills.len(),
            manifest.plugins.len(),
            manifest.targets.join(", ")
        );
    }

    // Direct entries (re)resolved by this run rather than reused from ai.lock:
    // the only ones a lock-only sync fetches.
    let mut fresh: BTreeSet<String> = BTreeSet::new();
    for (name, spec) in &manifest.skills {
        spec.version()?; // validates selectors
        let refresh = force_refresh && only.is_none_or(|o| o == name);

        // Reuse the pinned commit if the request is unchanged and we're not refreshing.
        let prev_entry = prev.skills.remove(name);
        let prev_evidence = prev_entry.as_ref().map(|p| {
            (
                p.git.clone(),
                p.path.clone(),
                p.commit.clone(),
                p.also_satisfies.clone(),
            )
        });
        let reuse = match reusable_pin(prev_entry, spec, refresh)? {
            Ok(l) => Some(l),
            Err(why) if locked => {
                mismatches.push(format!("skill `{name}`: {why}"));
                skipped_roots.insert(name.clone());
                continue;
            }
            Err(_) => None,
        };

        let (mut locked, how) = match reuse {
            Some(l) => (l, "locked"),
            None => (
                resolver::resolve(spec).with_context(|| format!("resolving skill `{name}`"))?,
                "resolved",
            ),
        };
        // A direct dependency never carries transitive provenance. Clear any
        // `requested_by` a reused entry may have inherited — e.g. a lock whose
        // entry was previously a synthesized transitive skill before being
        // promoted to a direct dependency — so `spm status`/`list` (which key
        // "transitive" purely on a non-empty `requested_by`) never misreport a
        // direct skill as transitive. Freshly resolved entries are already empty.
        locked.requested_by.clear();
        // Re-resolved (e.g. `spm update <name>`) to the very same git + path +
        // commit: the selectors previously verified to resolve to that commit
        // still do, so keep them as evidence. Otherwise a nested edge onto this
        // root by another, since-moved selector would be re-resolved — cascading
        // a named update into the transitive frontier — and report a spurious
        // conflict. A bare `spm update` refreshes the walk and ignores evidence.
        if let Some((git, path, commit, evidence)) = prev_evidence {
            if git == locked.git && path == locked.path && commit == locked.commit {
                locked.also_satisfies = evidence;
            }
        }
        if how == "resolved" {
            fresh.insert(name.clone());
        }
        let short = &locked.commit[..locked.commit.len().min(8)];
        let reference = locked.display_reference();
        println!("  {name:<width$}  {reference} @ {short}  ({how})");
        lock.skills.insert(name.clone(), locked);
    }

    let mut reused_linked_plugins: BTreeSet<String> = BTreeSet::new();
    // Resolve full plugins the same way. A plugin's version reuse also depends
    // on the pinned commit being unchanged; its bundled-skill list is (re)filled
    // from the store checkout below, so we never carry a stale one forward.
    for (name, spec) in &manifest.plugins {
        spec.version()?;
        let refresh = force_refresh && only.is_none_or(|o| o == name);
        let reuse = match reusable_pin(prev.plugins.remove(name), spec, refresh)? {
            Ok(l) => Some(l),
            Err(why) if locked => {
                mismatches.push(format!("plugin `{name}`: {why}"));
                continue;
            }
            Err(_) => None,
        };
        let (mut locked, how) = match reuse {
            Some(l) => (l, "locked"),
            None => (
                resolver::resolve(spec).with_context(|| format!("resolving plugin `{name}`"))?,
                "resolved",
            ),
        };
        if how == "resolved" {
            fresh.insert(name.clone());
        }
        // A reused pin is the same commit, so its bundled skills are too. They
        // are carried forward rather than re-enumerated from a checkout when
        // that checkout is not used: by a lock-only sync (which does not fetch
        // the plugin again) or for a linked plugin (keeping `ai.lock`
        // unaffected by the link).
        if how == "resolved" || !(lock_only || linked.contains_key(name)) {
            locked.bundled_skills.clear();
        } else if linked.contains_key(name) {
            reused_linked_plugins.insert(name.clone());
        }
        locked.also_satisfies.clear();
        let short = &locked.commit[..locked.commit.len().min(8)];
        let reference = locked.display_reference();
        println!("  {name:<width$}  {reference} @ {short}  (plugin, {how})");
        lock.plugins.insert(name.clone(), locked);
    }

    // Direct entries left in the previous lock are no longer declared. Skills
    // with a non-empty `requested_by` are transitive; they are judged after the
    // walk below.
    if locked {
        for name in prev
            .skills
            .iter()
            .filter(|(_, l)| l.requested_by.is_empty())
            .map(|(n, _)| n)
        {
            mismatches.push(format!(
                "skill `{name}`: in ai.lock but not declared in ai.json"
            ));
        }
        for name in prev.plugins.keys() {
            mismatches.push(format!(
                "plugin `{name}`: in ai.lock but not declared in ai.json"
            ));
        }
    }

    // Scan and materialize linked content from one snapshot, so a later edit
    // to a working copy can't reach a target unscanned (or only some targets).
    let snapshots = Snapshots::take(&mut linked)?;

    // Populate the store for plugins first: we need each plugin's checkout to
    // enumerate its bundled skills before writing the lockfile (so `ai.lock`
    // records the full component set) and before flattening those skills into
    // the shared list every vendor consumes.
    let mut plugins: Vec<MaterializedPlugin> = Vec::new();
    let mut plugin_skills: Vec<MaterializedSkill> = Vec::new();
    if !manifest.plugins.is_empty() {
        println!("fetching plugins into store");
    }
    let mut linked_plugin_skills: Vec<String> = Vec::new();
    for (name, locked) in &mut lock.plugins {
        if lock_only && !fresh.contains(name) {
            println!("  {name:<width$}  unchanged (lock only, not fetched)");
            continue;
        }
        let link = linked.get(name);
        // A linked plugin only needs its locked checkout to record the commit's
        // bundled skills in `ai.lock` when its pin was freshly resolved.
        let ensured = if link.is_some() && reused_linked_plugins.contains(name) {
            None
        } else {
            let ensured =
                store::ensure(locked).with_context(|| format!("fetching plugin `{name}`"))?;
            println!(
                "  {name:<width$}  {}",
                if ensured.fetched { "fetched" } else { "cached" }
            );
            Some(ensured)
        };
        let (path, root) = if let Some(l) = link {
            println!("  {name:<width$}  linked → {}", l.display);
            (l.content.clone(), l.content.clone())
        } else if let Some(e) = &ensured {
            (e.path.clone(), e.root.clone())
        } else {
            bail!("plugin `{name}` was neither linked nor fetched");
        };
        if !crate::plugin::looks_like_plugin(&path) {
            let source = match link {
                Some(l) => format!("linked → {}", l.display),
                None => format!(
                    "{} @ {}{}",
                    locked.git,
                    locked.display_reference(),
                    locked
                        .path
                        .as_deref()
                        .map(|p| format!(" under `{p}`"))
                        .unwrap_or_default()
                ),
            };
            eprintln!(
                "warning: plugin `{name}` ({source}) has no `.claude-plugin/plugin.json` — \
                 is it really a plugin? (only its bundled skills, if any, will be materialized)"
            );
        }
        let bundled = crate::plugin::plugin_skills(&path, &root)
            .with_context(|| format!("enumerating skills bundled in plugin `{name}`"))?;
        match (link, ensured) {
            (Some(_), Some(e)) => {
                locked.bundled_skills = crate::plugin::plugin_skills(&e.path, &e.root)
                    .with_context(|| format!("enumerating skills bundled in plugin `{name}`"))?
                    .into_iter()
                    .map(|s| s.name)
                    .collect();
            }
            (Some(_), None) => {}
            (None, _) => locked.bundled_skills = bundled.iter().map(|s| s.name.clone()).collect(),
        }
        if link.is_some() {
            linked_plugin_skills.extend(bundled.iter().map(|s| s.name.clone()));
        }
        // Scan the whole plugin tree (agents, scripts, hooks, bundled skills)
        // before any of it is materialized into an agent-discovered directory.
        crate::scan::enforce(name, &path, &root)
            .with_context(|| format!("scanning plugin `{name}`"))?;
        plugin_skills.extend(bundled);
        plugins.push(MaterializedPlugin {
            name: name.clone(),
            path,
            root,
        });
    }

    // Populate the store, run the security gate, and collect absolute paths for
    // the vendors — recursing into nested `ai.json` dependencies when the root
    // manifest opts in (`resolveTransitive`). Transitive skills are folded into
    // the same flat list every vendor consumes and pinned into `ai.lock`
    // alongside the directly-declared ones.
    if !manifest.skills.is_empty() {
        println!("fetching skills into store");
    }
    // Lock-only without transitive resolution: a reused skill is a leaf whose
    // pin is carried over as is, so only the (re)resolved ones are walked. With
    // it on, every skill is walked: its nested `ai.json` shapes the lock.
    let walked: BTreeMap<String, LockedSkill> = if lock_only && !manifest.resolve_transitive {
        for name in lock.skills.keys().filter(|n| !fresh.contains(*n)) {
            println!("  {name:<width$}  unchanged (lock only, not fetched)");
        }
        lock.skills
            .iter()
            .filter(|(n, _)| fresh.contains(*n))
            .map(|(n, l)| (n.clone(), l.clone()))
            .collect()
    } else {
        lock.skills.clone()
    };
    let expansion = transitive::expand(
        &walked,
        &transitive::Ctx {
            prev: &prev.skills,
            resolve_transitive: manifest.resolve_transitive,
            // Only a bare `spm update` (no name) refreshes the transitive
            // frontier. `spm update <name>` (`only = Some(..)`) is deliberately
            // surgical: it re-resolves the named direct root above and re-reads
            // its nested manifest, but does NOT cascade a moving-ref refresh
            // into that root's unchanged transitive children — because those
            // children are deduplicated across roots, so refreshing one root's
            // subtree could advance an unrelated root's dependency. See
            // `resolve_child` and docs/guide/transitive-dependencies.md.
            refresh: force_refresh && only.is_none(),
            locked,
            width,
            policy: &policy,
            links: &linked,
        },
    )?;
    let mut materialized: Vec<MaterializedSkill> = expansion.materialized;
    // `also_satisfies` evidence is rebuilt by every walk: a direct root keeps
    // only what this walk recorded for it (nothing when transitive is off).
    let mut direct_also_satisfies = expansion.direct_also_satisfies;
    for (name, l) in &mut lock.skills {
        l.also_satisfies = direct_also_satisfies.remove(name).unwrap_or_default();
    }
    if locked {
        let reached: BTreeSet<&str> = expansion
            .transitive
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        mismatches.extend(stale_transitive_pins(
            &prev.skills,
            &reached,
            manifest.resolve_transitive,
            skipped_roots
                .into_iter()
                .chain(expansion.lock_misses.iter().map(|(n, _)| n.clone())),
        ));
        mismatches.extend(expansion.lock_misses.into_iter().map(|(_, m)| m));
    }
    // Fold the transitively-resolved skills into `ai.lock`. A synthesized name
    // colliding with a directly-declared dependency is a hard error (the
    // synthesized names are hash-suffixed to make this astronomically unlikely).
    // `skills` and `plugins` share one flat dependency-name namespace (enforced
    // at manifest load, see `manifest.rs`), so the name must be free in *both*
    // lock maps — a transitive skill colliding with a root plugin would write
    // the same name into both maps and make `list`/`update <name>` ambiguous.
    for (tname, tlocked) in expansion.transitive {
        if lock.skills.contains_key(&tname) || lock.plugins.contains_key(&tname) {
            bail!(
                "transitive skill name collision: `{tname}` clashes with a directly-declared \
                 skill or plugin — rename the direct dependency or pin the dependency that \
                 brings it in"
            );
        }
        println!(
            "  {tname:<width$}  transitive (via {})",
            tlocked.requested_by.join(", ")
        );
        lock.skills.insert(tname, tlocked);
    }

    // Flatten plugin-bundled skills into the same list every vendor consumes, so
    // even skills-only targets get them. A bundled skill name that collides with
    // a standalone skill (or another plugin's skill) is a hard error: silently
    // overwriting a skill's content by manifest-key ordering is exactly the kind
    // of drift AGENTS.md's "no drift surfaces" rule forbids. A lock-only sync
    // materializes nothing and fetches just some plugins, so the names it
    // carries over unfetched are checked from the lock below.
    for s in plugin_skills {
        if materialized.iter().any(|m| m.name == s.name) {
            bail!(
                "skill name collision: `{}` is provided by more than one skill/plugin — \
                 rename the conflicting skill or plugin-bundled skill",
                s.name
            );
        }
        materialized.push(s);
    }

    // Vendor skill AND plugin directories are case-insensitive on Windows and
    // the default macOS filesystem, so two entries whose names differ only by
    // case resolve to the same directory — the materializer would remove the
    // first and silently overwrite it with the second. The byte-for-byte guards
    // above catch exact clashes early with tailored guidance; this is the single
    // canonical backstop that rejects *case-folding* collisions across the whole
    // materialized set (direct, transitive, and plugin-bundled skills) and,
    // separately, across plugins — before any vendor writes a byte.
    reject_case_folding_collisions("skill", materialized.iter().map(|m| m.name.as_str()))?;
    // From the lock, not the fetched `plugins`: a lock-only sync fetches only
    // the plugins it (re)resolves, yet must not write a lock an install rejects.
    reject_case_folding_collisions("plugin", lock.plugins.keys().map(String::as_str))?;
    reject_locked_skill_collisions(&lock)?;
    // Each target writes what it held before plus the new set — its own names
    // only, so another target's leftovers don't count as its output. A
    // lock-only sync writes no target output — only `ai.json`, `ai.lock` and
    // machine-local state — so only those writes are guarded.
    let (guarded_vendors, outputs): (&[Box<dyn Vendor>], Vec<Owned>) = if lock_only {
        (&[], Vec::new())
    } else {
        let outputs = manifest
            .targets
            .iter()
            .map(|t| {
                let mut owned = previously_owned.for_target(t);
                owned
                    .skills
                    .extend(materialized.iter().map(|m| m.name.clone()));
                owned.plugins.extend(plugins.iter().map(|p| p.name.clone()));
                owned
            })
            .collect();
        (&vendors, outputs)
    };
    guard_working_copies(
        scope,
        &dir,
        guarded_vendors,
        &linked,
        &outputs,
        snapshots.as_ref().map(|s| s.root.as_path()),
        lock_only,
    )?;
    if locked {
        // Backstop for the whole defect class: whatever the specific checks above
        // missed (a hand-edited `bundled_skills`/`requested_by`/store key, ...),
        // `--locked` must never install a set that differs from the committed
        // lock. Compared as JSON values, so line endings/formatting don't matter.
        if mismatches.is_empty() {
            let on_disk: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(Lockfile::path_in(&dir))?)?;
            if on_disk != serde_json::to_value(&lock)? {
                mismatches.push(
                    "ai.lock differs from what spm would write (e.g. a plugin's bundled skills \
                     or a transitive skill's requesters changed)"
                        .to_string(),
                );
            }
        }
        if !mismatches.is_empty() {
            bail!(
                "{}: ai.lock is out of sync with ai.json ({} problem(s)):\n{}\n{}",
                mode.label(),
                mismatches.len(),
                mismatches
                    .iter()
                    .map(|m| format!("  - {m}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                mode.fix_hint(scope)
            );
        }
    }
    linked_plugin_skills.sort();
    linked_plugin_skills.dedup();
    links.managed = linked_plugin_skills;
    // Only rewrite the links file when it changes (a `link`/`unlink`, or a
    // linked plugin's bundled skills moving), never on a plain sync.
    let links = (Links::load(scope).ok().as_ref() != Some(&links)).then_some(links);
    if let Some(links) = &links {
        links.document(scope)?;
    }
    Ok(Plan {
        dir,
        targets: manifest.targets.clone(),
        declares_any: !manifest.skills.is_empty() || !manifest.plugins.is_empty(),
        vendors,
        lock,
        materialized,
        plugins,
        previously_owned,
        locked,
        links,
        _snapshots: snapshots,
        _state_lock: state_lock,
        apply,
    })
}

impl Plan {
    /// Project the planned skills + plugins into every configured target, then
    /// persist `ai.lock`. Returns the number of locked skills + plugins.
    fn apply(self, scope: &Scope) -> Result<usize> {
        let lock = &self.lock;
        if self.apply == Apply::LockOnly {
            // Nothing is installed or removed now: the targets keep what they
            // hold, and the next install or clean must still know it is spm's.
            self.previously_owned.freeze(scope, &self.targets)?;
            lock.save(&self.dir)?;
            println!(
                "wrote ai.lock only; run `{}` to install into the targets",
                install_command(scope)
            );
            return Ok(lock.skills.len() + lock.plugins.len());
        }
        if self.declares_any {
            println!("materializing: {}", self.targets.join(", "));
        }
        // Skills copied from a linked plugin's working tree are listed in the
        // links file (an spm without an ownership record falls back to it).
        // Record the planned ones alongside the previous ones before any
        // vendor writes; the final links file (below) narrows it once all
        // succeeded. Each target's record claims them as they are written.
        if let Some(links) = &self.links {
            let on_disk = Links::load(scope)?;
            let mut pending = on_disk.clone();
            pending.managed.extend(links.managed.iter().cloned());
            pending.managed.sort();
            pending.managed.dedup();
            if pending != on_disk {
                pending.save(scope)?;
            }
        }
        let desired = Owned {
            skills: self.materialized.iter().map(|m| m.name.clone()).collect(),
            plugins: self.plugins.iter().map(|p| p.name.clone()).collect(),
        };
        self.previously_owned
            .install(scope, &self.targets, &desired, |i, owned, claim| {
                let vendor = &self.vendors[i];
                vendor.materialize(
                    scope,
                    &lock.id,
                    &self.materialized,
                    &owned.skill_names(),
                    claim,
                )?;
                vendor.materialize_plugins(scope, &lock.id, &self.plugins, &owned.plugin_names())
            })?;
        // Only persist `ai.lock` once every fallible step (resolution, fetching,
        // collision checks, and vendor materialization) has succeeded —
        // otherwise a failed `install` could leave behind a lockfile pointing at
        // state that was never actually materialized. `--locked` never writes
        // it: it already matches (checked in `plan`).
        if !self.locked {
            lock.save(&self.dir)?;
        }
        if let Some(links) = &self.links {
            links.save(scope)?;
        }
        Ok(lock.skills.len() + lock.plugins.len())
    }
}

/// Resolve every `spm link` override that names a declared skill or plugin to
/// its directory. A link whose name is no longer declared is ignored with a
/// warning (it stays in the links file until `spm unlink`); a declared one whose
/// directory is gone is an error, so a sync never silently falls back to the
/// locked version while the user believes a local override is active.
fn resolve_links(
    scope: &Scope,
    manifest: &Manifest,
    links: &Links,
) -> Result<BTreeMap<String, Linked>> {
    let mut out = BTreeMap::new();
    for name in links.links.keys() {
        if !manifest.skills.contains_key(name) && !manifest.plugins.contains_key(name) {
            eprintln!(
                "warning: ignoring link `{name}`: it is not declared in ai.json — run `{}` to \
                 drop it",
                unlink_command(scope, name)
            );
            continue;
        }
        if let Some(l) = links.resolve(scope, name)? {
            out.insert(name.clone(), l);
        }
    }
    Ok(out)
}

/// A working copy the overlap guard protects.
struct Guarded {
    name: String,
    /// Canonical dir.
    dir: PathBuf,
    /// The path as recorded (its route counts too), anchored at its scope's base.
    recorded: PathBuf,
    /// The path as recorded, for messages.
    display: String,
    /// Where it was linked, for messages: empty for this command's scope.
    origin: String,
    /// Still linked in this command's scope (so relinking is a way out)?
    active: bool,
}

/// Before `vendors` write or delete anything — each only for its own
/// `outputs` entry (the names it holds or is about to write) — reject
/// any overlap with a linked working copy: each `active` link, plus every one
/// recorded on this machine — in this scope's links file (one being dropped by
/// `spm unlink`, re-pointed by `spm link`, or ignored as undeclared) and in
/// every other scope's (a global link may point into a project's output, or one
/// project's into another's). Each is still the user's data even when this
/// command doesn't copy from it. A recorded dir that is gone has nothing left
/// to protect. Every command that writes target output or deletes it (`sync`,
/// `clean`, `target remove`) must call this first. A `lock_only` sync writes
/// just `ai.json` and `ai.lock` (pass no `vendors`), so only those count.
pub(super) fn guard_working_copies(
    scope: &Scope,
    manifest_dir: &std::path::Path,
    vendors: &[Box<dyn Vendor>],
    active: &BTreeMap<String, Linked>,
    outputs: &[Owned],
    snapshot: Option<&std::path::Path>,
    lock_only: bool,
) -> Result<()> {
    assert_eq!(vendors.len(), outputs.len(), "one output set per vendor");
    let mut guarded: Vec<Guarded> = active
        .iter()
        .map(|(n, l)| Guarded {
            name: n.clone(),
            dir: l.dir.clone(),
            recorded: manifest_dir.join(&l.display),
            display: l.display.clone(),
            origin: String::new(),
            active: true,
        })
        .collect();
    // Fail closed: a links file or recorded dir that can't be read is not
    // "nothing to protect". Only a dir that is gone (or no longer a dir) is
    // skipped, so `spm unlink` can always drop a dead link.
    let own_file = Links::path(scope)?;
    let mut state_files = vec![own_file.clone()];
    for rec in Links::all_recorded()? {
        let own = rec.file == own_file;
        let (base, origin) = if own {
            (manifest_dir.to_path_buf(), String::new())
        } else {
            (rec.base, format!(" (linked {})", rec.origin))
        };
        if !own {
            state_files.push(rec.file);
        }
        for (name, display) in rec.links.links {
            let recorded = base.join(&display);
            let dir = match recorded.canonicalize() {
                Ok(d) if d.is_dir() => d,
                Ok(_) => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "inspecting linked directory `{display}` for `{name}`{origin} — spm \
                             can't rule out that this command would modify or delete it"
                        )
                    })
                }
            };
            if !guarded
                .iter()
                .any(|g| g.name == name && g.recorded == recorded)
            {
                guarded.push(Guarded {
                    name,
                    dir,
                    recorded,
                    display,
                    origin: origin.clone(),
                    active: false,
                });
            }
        }
    }
    reject_link_overlaps(
        scope,
        manifest_dir,
        vendors,
        &guarded,
        Kept {
            state_files: &state_files,
            snapshot,
            lock_only,
        },
        outputs,
    )
}

/// What the targets' writes and deletions must leave alone besides working
/// copies (see [`reject_link_overlaps`]).
struct Kept<'a> {
    /// The links files this command must not wipe (this scope's first, which
    /// it also writes).
    state_files: &'a [PathBuf],
    /// Where this command copied linked content to materialize it from.
    snapshot: Option<&'a std::path::Path>,
    /// Writes only `ai.json` and `ai.lock`: neither the links file (nor its
    /// removal) nor `.gitignore`.
    lock_only: bool,
}

/// Reject a linked working copy that overlaps any location a target writes or
/// deletes. A source inside the output (e.g. linking Codex's own
/// `.agents/skills/<name>`) would be wiped before it is copied — losing
/// uncommitted edits — and output inside the source (e.g. linking the project
/// root) would be copied into itself. Files spm edits in place (`ai.json`,
/// `ai.lock`, the links file, the project `.gitignore`, a vendor's settings)
/// are followed through symlinks, since writing them would modify whatever they
/// point at. Compared by filesystem identity, so symlinks and spelling aliases
/// (case on a case-insensitive filesystem) match. The recorded path's access
/// route counts too: deleting a symlink (or a dir holding one) it passes
/// through would break — or, once regenerated, silently re-point — the link.
///
/// Nor may the targets wipe what `kept` lists: a links file (dropping every
/// link), or the snapshot before they copy from it.
fn reject_link_overlaps(
    scope: &Scope,
    manifest_dir: &std::path::Path,
    vendors: &[Box<dyn Vendor>],
    guarded: &[Guarded],
    kept: Kept,
    names: &[Owned],
) -> Result<()> {
    use crate::pathid::{deletion_target, identity, resolve, NodeId};
    let Kept {
        state_files,
        snapshot,
        lock_only,
    } = kept;
    // Nothing to protect: no working copy, and no links file to lose.
    if guarded.is_empty() && !state_files.iter().any(|f| f.symlink_metadata().is_ok()) {
        return Ok(());
    }
    // (owner, path as spelled, physical identity, written through?)
    let mut outputs = Vec::new();
    let mut patched: Vec<(&str, PathBuf)> = vec![
        ("spm", Manifest::path_in(manifest_dir)),
        ("spm", Lockfile::path_in(manifest_dir)),
    ];
    if !lock_only {
        patched.push(("spm", Links::path(scope)?));
        if let Scope::Project { root } = scope {
            patched.push(("spm", root.join(".gitignore")));
        }
    }
    for (v, owned) in vendors.iter().zip(names) {
        let (skills, plugins) = (owned.skill_names(), owned.plugin_names());
        let mut paths = v.clean_paths(scope, &skills)?;
        paths.extend(v.skill_paths(scope, &skills)?);
        paths.extend(v.plugin_clean_paths(scope, &plugins)?);
        paths.extend(v.plugin_paths(scope, &plugins)?);
        for p in paths {
            let id = identity(&deletion_target(&p)?)?;
            outputs.push((v.name(), p, id, false));
        }
        patched.extend(v.patched_files(scope)?.into_iter().map(|p| (v.name(), p)));
    }
    for (owner, p) in &patched {
        let id = identity(&resolve(p, &mut Vec::new())?)?;
        outputs.push((owner, p.clone(), id, true));
    }
    // The links file itself must survive the targets' cleanup: inside their
    // output (by its entry or where it points) it would be wiped, silently
    // dropping every link. Nor may any other file spm or a target patches in
    // place (`.gitignore`, a settings file, ...) resolve to it: that write would
    // corrupt it.
    for links_file in state_files {
        // Its entry, where it points, and every symlink on the way: deleting
        // any of those loses the file (or access to it) just the same.
        let mut hops = Vec::new();
        let mut state = vec![
            identity(&deletion_target(links_file)?)?,
            identity(&resolve(links_file, &mut hops)?)?,
        ];
        for h in &hops {
            state.push(identity(h)?);
        }
        // A hard link shares no path component with the links file, so only
        // the files' own identities show it.
        let hit = outputs
            .iter()
            .find(|(_, p, out, through)| {
                (!through || p != links_file) && state.iter().any(|s| s.starts_with(out))
            })
            .map(|(owner, p, _, through)| (*owner, p.as_path(), *through))
            .or_else(|| {
                patched
                    .iter()
                    .find(|(_, p)| p != links_file && same_file(p, links_file))
                    .map(|(owner, p)| (*owner, p.as_path(), true))
            });
        if let Some((owner, p, through)) = hit {
            let (relation, effect) = if through {
                ("is the same file as", "writes in place")
            } else {
                ("lies inside", "writes or deletes")
            };
            let owner = if owner == "spm" {
                "spm".to_string()
            } else {
                format!("target `{owner}`")
            };
            bail!(
                "the links file `{}` {relation} `{}`, which {owner} {effect}; refusing to \
                 continue, as that would drop your links — remove whatever puts it there (e.g. \
                 a symlink) and retry",
                links_file.display(),
                p.display()
            );
        }
    }
    // The snapshot root is canonical, so its identity is its whole route.
    if let Some(root) = snapshot {
        let id = identity(root)?;
        if let Some((owner, p, ..)) = outputs
            .iter()
            .find(|(_, _, out, through)| !through && id.starts_with(out))
        {
            bail!(
                "spm's temporary directory `{}` lies inside `{}`, which target `{owner}` \
                 writes or deletes, so linked content copied there would be lost — point the \
                 temporary directory elsewhere (`TMPDIR`; `TMP`/`TEMP` on Windows) and retry",
                root.display(),
                p.display()
            );
        }
    }
    // This scope's links file is also deleted once it holds nothing, which
    // removes the entry at its path (not what a final symlink there points at).
    if !lock_only {
        let links_file = state_files[0].clone();
        let id = identity(&deletion_target(&links_file)?)?;
        outputs.push(("spm", links_file, id, false));
    }
    for g in guarded {
        let (name, display, active) = (&g.name, &g.display, g.active);
        let src = identity(&g.dir)?;
        // Every location the content depends on: the recorded path's own hops,
        // and those of each symlink inside the tree the copy follows (one that
        // is broken, cyclic or escapes the tree is skipped by the copy and the
        // scan alike, so it is no route), tagged with the symlink for messages.
        let mut route: Vec<(Option<PathBuf>, Vec<NodeId>)> = Vec::new();
        let mut hops = Vec::new();
        resolve(&g.recorded, &mut hops)?;
        for h in &hops {
            route.push((None, identity(h)?));
        }
        for link in symlinks_in(&g.dir)? {
            if crate::fsutil::resolve_within_canonical(&g.dir, &link).is_none() {
                continue;
            }
            let mut hops = Vec::new();
            resolve(&link, &mut hops)?;
            for h in &hops {
                route.push((Some(link.clone()), identity(h)?));
            }
        }
        let hit = outputs.iter().find_map(|(owner, p, out, through)| {
            if out.starts_with(&src) || (!through && src.starts_with(out)) {
                return Some((*owner, p.as_path(), String::new()));
            }
            if *through {
                return None;
            }
            let (via, _) = route.iter().find(|(_, r)| r.starts_with(out))?;
            let how = via
                .as_ref()
                .map(|v| format!(" (through the symlink `{}`)", v.display()))
                .unwrap_or_default();
            Some((*owner, p.as_path(), how))
        });
        let hit = match hit {
            Some(h) => Some(h),
            None => hard_linked_into(&g.dir, &patched)?.map(|(owner, p, alias)| {
                (owner, p, format!(" (hard-linked as `{}`)", alias.display()))
            }),
        };
        let Some((owner, p, how)) = hit else {
            continue;
        };
        let owner = if owner == "spm" {
            "spm".to_string()
        } else {
            format!("target `{owner}`")
        };
        let clash = format!(
            "linked directory `{display}` for `{name}`{} overlaps `{}`{how}, which {owner} \
             writes or deletes",
            g.origin,
            p.display()
        );
        let flag = if scope.is_global() { " -g" } else { "" };
        let relink = if active {
            format!(
                "link `{name}` to a directory outside spm's output \
                 (`spm link {name} <dir>{flag}`), or "
            )
        } else if !g.origin.is_empty() {
            "unlink it where it was linked, or ".to_string()
        } else {
            String::new()
        };
        bail!(
            "{clash}; refusing to continue, as that could modify or delete the working copy — \
             {relink}remove whatever makes them overlap (e.g. a symlink) and retry"
        );
    }
    Ok(())
}

/// Every symlink inside `dir`, not descending into `.git` (never copied) or
/// through symlinks.
fn symlinks_in(dir: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                found.push(entry.path());
            } else if kind.is_dir() && entry.file_name() != ".git" {
                dirs.push(entry.path());
            }
        }
    }
    Ok(found)
}

/// A file spm writes in place (`files`: owner, path) that is a hard link to a
/// file inside `dir`: writing it would modify the working copy too, though no
/// path relation shows it. Only files with more than one link are looked for,
/// so the tree is walked only when there is something to find. Windows has no
/// stable std API for link counts or file ids, so this is Unix-only.
#[cfg(unix)]
fn hard_linked_into<'a>(
    dir: &std::path::Path,
    files: &'a [(&'a str, PathBuf)],
) -> Result<Option<(&'a str, &'a std::path::Path, PathBuf)>> {
    use std::os::unix::fs::MetadataExt;
    let linked: Vec<(u64, u64, usize)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, (_, f))| {
            std::fs::metadata(f)
                .ok()
                .filter(|m| m.is_file() && m.nlink() > 1)
                .map(|m| (m.dev(), m.ino(), i))
        })
        .collect();
    if linked.is_empty() {
        return Ok(None);
    }
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let entry = entry?;
            // Not following symlinks: one pointing out of the tree is not in it.
            let m = entry.metadata()?;
            if m.is_dir() {
                dirs.push(entry.path());
            } else if let Some((.., i)) = linked
                .iter()
                .find(|(dev, ino, _)| *dev == m.dev() && *ino == m.ino())
            {
                let (owner, file) = &files[*i];
                return Ok(Some((owner, file.as_path(), entry.path())));
            }
        }
    }
    Ok(None)
}

/// Whether `a` and `b` are the same existing file, however reached (a symlink
/// or a hard link). Windows has no stable std API for file ids, so there only
/// the path checks apply.
#[cfg(unix)]
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_a: &std::path::Path, _b: &std::path::Path) -> bool {
    false
}

#[cfg(not(unix))]
fn hard_linked_into<'a>(
    _dir: &std::path::Path,
    _files: &'a [(&'a str, PathBuf)],
) -> Result<Option<(&'a str, &'a std::path::Path, PathBuf)>> {
    Ok(None)
}

/// The checks above judge the *effective* skill set, where a linked plugin's
/// working tree replaces its locked bundle. `ai.lock` must stay installable
/// without the link (by teammates, CI, or after `spm unlink`), so its own skill
/// namespace — standalone and transitive skills plus every plugin's locked
/// bundle — must be free of exact and case-folding clashes too.
fn reject_locked_skill_collisions(lock: &Lockfile) -> Result<()> {
    let mut seen: std::collections::HashMap<String, (&str, String)> =
        std::collections::HashMap::new();
    let entries = lock
        .skills
        .keys()
        .map(|n| (n.as_str(), format!("skill `{n}`")))
        .chain(lock.plugins.iter().flat_map(|(p, l)| {
            l.bundled_skills
                .iter()
                .map(move |s| (s.as_str(), format!("plugin `{p}`")))
        }));
    for (name, by) in entries {
        if let Some((other, other_by)) = seen.insert(name.to_lowercase(), (name, by.clone())) {
            bail!(
                "skill name collision in ai.lock: `{other}` ({other_by}) and `{name}` ({by}) \
                 would install to the same directory — a linked working copy may not bundle \
                 one of them, but installing ai.lock without the link would; rename the \
                 conflicting skill or plugin-bundled skill"
            );
        }
    }
    Ok(())
}

/// `spm unlink <name>`, spelled for the scope in use.
pub(super) fn unlink_command(scope: &Scope, name: &str) -> String {
    let g = if scope.is_global() { " -g" } else { "" };
    format!("spm unlink {name}{g}")
}

/// The pin in `prev` if it can be reused for `spec` without re-resolving, else
/// why not (missing, refreshing, or the request changed).
fn reusable_pin(
    prev: Option<LockedSkill>,
    spec: &SkillSpec,
    refresh: bool,
) -> Result<std::result::Result<LockedSkill, String>> {
    let Some(l) = prev else {
        return Ok(Err("missing from ai.lock".to_string()));
    };
    if refresh {
        return Ok(Err("refreshing".to_string()));
    }
    Ok(match l.drift_from(spec)? {
        None => Ok(l),
        Some(changes) => Err(format!("changed since ai.lock was written ({changes})")),
    })
}

/// `--locked`: describe every transitive pin left in `prev` (direct entries
/// already removed) that the walk did not reach. A pin that hangs off a
/// subtree the walk skipped — a direct root or edge that itself failed the pin
/// check, seeded by `unwalked` — may still be needed, so it is not judged (that
/// root's own mismatch is already reported). With `resolveTransitive` off
/// nothing is walked, so every transitive pin is stale.
fn stale_transitive_pins(
    prev: &BTreeMap<String, LockedSkill>,
    reached: &BTreeSet<&str>,
    resolve_transitive: bool,
    unwalked: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let unreached: Vec<(&String, &LockedSkill)> = prev
        .iter()
        .filter(|(n, l)| !l.requested_by.is_empty() && !reached.contains(n.as_str()))
        .collect();
    let mut unwalked: BTreeSet<String> = if resolve_transitive {
        unwalked.into_iter().collect()
    } else {
        BTreeSet::new()
    };
    // Fixed point: anything requested by an unwalked node is itself unwalked.
    loop {
        let before = unwalked.len();
        for (n, l) in &unreached {
            if l.requested_by.iter().any(|r| unwalked.contains(r)) {
                unwalked.insert((*n).clone());
            }
        }
        if unwalked.len() == before {
            break;
        }
    }
    unreached
        .into_iter()
        .filter(|(n, _)| !unwalked.contains(*n))
        .map(|(n, _)| format!("transitive skill `{n}`: in ai.lock but no longer required"))
        .collect()
}

/// `spm install`, spelled for the scope in use.
pub(super) fn install_command(scope: &Scope) -> &'static str {
    if matches!(scope, Scope::Global) {
        "spm install -g"
    } else {
        "spm install"
    }
}

/// Reject materialized names that differ only by case. Vendor directories are
/// case-insensitive on Windows and the default macOS filesystem, so two such
/// names resolve to the same on-disk directory and the second silently
/// overwrites the first. `kind` labels the set (`"skill"`/`"plugin"`) in the
/// error. This is the canonical backstop; byte-for-byte guards elsewhere catch
/// exact clashes earlier with more specific guidance.
fn reject_case_folding_collisions<'a>(
    kind: &str,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    for name in names {
        if let Some(other) = seen.insert(name.to_lowercase(), name) {
            if other != name {
                bail!(
                    "{kind} name collision: `{other}` and `{name}` differ only by case — vendor \
                     directories are case-insensitive on Windows and macOS, so one would \
                     silently overwrite the other; rename or re-pin one of them"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh source dir under the system temp dir holding `files`.
    fn source(label: &str, files: &[(&str, &str)]) -> PathBuf {
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

    fn linked(dir: &std::path::Path) -> Linked {
        Linked {
            dir: dir.to_path_buf(),
            content: dir.to_path_buf(),
            display: dir.display().to_string(),
        }
    }

    /// Each link gets its own slot, even when names only differ in case (a
    /// skill `Foo` and a plugin `foo`), which would share a directory on a
    /// case-insensitive filesystem; the root is private and removed on drop.
    #[test]
    fn snapshots_are_private_distinct_and_temporary() {
        let skill = source("skill", &[("SKILL.md", "skill")]);
        let plugin = source("plugin", &[(".claude-plugin/plugin.json", "{}")]);
        let mut map = BTreeMap::from([
            ("Foo".to_string(), linked(&skill)),
            ("foo".to_string(), linked(&plugin)),
        ]);
        let snapshots = Snapshots::take(&mut map).unwrap().unwrap();
        let (s, p) = (&map["Foo"].content, &map["foo"].content);
        assert_eq!(
            std::fs::read_to_string(s.join("SKILL.md")).unwrap(),
            "skill"
        );
        assert!(
            !s.join(".claude-plugin").exists(),
            "plugin merged into skill"
        );
        assert!(!p.join("SKILL.md").exists(), "skill merged into plugin");
        assert!(p.join(".claude-plugin/plugin.json").is_file());
        assert_eq!(map["Foo"].dir, skill, "the working copy stays protected");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&snapshots.root)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let root = snapshots.root.clone();
        drop(snapshots);
        assert!(!root.exists());
        std::fs::remove_dir_all(skill).unwrap();
        std::fs::remove_dir_all(plugin).unwrap();
    }
}
