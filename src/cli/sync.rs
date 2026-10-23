use crate::links::{Linked, Links};
use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::{Manifest, SkillSpec};
use crate::scope::Scope;
use crate::vendor::{MaterializedPlugin, MaterializedSkill, Vendor};
use crate::{resolver, store, transitive};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

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
) -> Result<usize> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir, scope)?;
    let links = Links::load(scope)?;
    let mode = if locked {
        LockMode::Locked
    } else {
        LockMode::Write
    };
    plan(scope, &manifest, links, force_refresh, only, mode)?.apply(scope)
}

/// Sync with an edited set of `spm link` overrides, persisting them only once
/// the sync has been planned successfully (an unreadable or blocked linked
/// directory leaves the links file untouched). `ai.json` and `ai.lock` are not
/// affected by links: the lock still pins every dependency's declared source,
/// so this runs in [`LockMode::Links`] — it never re-resolves a pin nor writes
/// `ai.lock`, and fails instead when `ai.lock` is out of sync with `ai.json`.
pub(super) fn sync_with_links(scope: &Scope, links: Links) -> Result<usize> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir, scope)?;
    plan(scope, &manifest, links, false, None, LockMode::Links)?.apply(scope)
}

/// How a sync may treat `ai.lock`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LockMode {
    /// Re-resolve what changed or is unpinned, then write `ai.lock`.
    Write,
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
pub(super) fn save_and_sync(scope: &Scope, manifest: &Manifest) -> Result<usize> {
    let dir = scope.manifest_dir()?;
    let path = Manifest::path_in(&dir);
    let text = manifest.to_text()?;
    // Validate exactly as the next `Manifest::load` will (schema, name
    // collisions across skills/plugins, ...), so an invalid edit is never saved.
    let manifest = Manifest::parse(&text, &path)?;
    let plan = plan(
        scope,
        &manifest,
        Links::load(scope)?,
        false,
        None,
        LockMode::Write,
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
    previously_managed: Vec<String>,
    previously_managed_plugins: Vec<String>,
    locked: bool,
    /// The `spm link` overrides to persist after applying (with `managed`
    /// refreshed), or `None` when the links file is already up to date.
    links: Option<Links>,
}

/// Resolve `manifest` -> lock, populate the store, and run every check (source
/// policy, content scan, collisions, `--locked` drift). Writes nothing outside
/// the global store: no target output, no `ai.json`, no `ai.lock`.
fn plan(
    scope: &Scope,
    manifest: &Manifest,
    mut links: Links,
    force_refresh: bool,
    only: Option<&str>,
    mode: LockMode,
) -> Result<Plan> {
    let locked = mode != LockMode::Write;
    let dir = scope.manifest_dir()?;
    if manifest.targets.is_empty() {
        bail!("ai.json declares no targets");
    }
    // Source policy: judge every direct source before any network access, and
    // report all denied ones at once.
    let policy = crate::policy::activate(manifest, &dir)?;
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
    let linked = resolve_links(scope, manifest, &links)?;
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

    // Skill names spm materialized on the previous sync. Captured before the
    // resolve loop mutates `prev`, so global/shared-dir vendors (which share a
    // directory with the user's own skills) can purge only the entries spm
    // previously owned but that were since dropped from the manifest.
    //
    // Plugin-bundled skills are flattened into the same shared skills dirs via
    // `materialize`, so the previous set must include them too — otherwise
    // removing a plugin (or shrinking its bundled skill set) would orphan those
    // skill dirs in shared locations.
    //
    // Skills last materialized from a *linked* plugin's working tree are not in
    // `ai.lock` (it records the locked commit's bundled skills), so the links
    // file remembers them for the same purpose.
    let previously_managed: Vec<String> = prev
        .skills
        .keys()
        .cloned()
        .chain(
            prev.plugins
                .values()
                .flat_map(|l| l.bundled_skills.iter().cloned()),
        )
        .chain(links.managed.iter().cloned())
        .collect();
    let previously_managed_plugins: Vec<String> = prev.plugins.keys().cloned().collect();

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
        // A linked plugin is not fetched when its pin is reused, so its locked
        // bundled-skill list is carried forward rather than re-enumerated from
        // the (unfetched) checkout — keeping `ai.lock` unaffected by the link.
        if how == "resolved" || !linked.contains_key(name) {
            locked.bundled_skills.clear();
        } else {
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
            (l.dir.clone(), l.dir.clone())
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
    let expansion = transitive::expand(
        &lock.skills,
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
    // of drift AGENTS.md's "no drift surfaces" rule forbids.
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
    reject_case_folding_collisions("plugin", plugins.iter().map(|p| p.name.as_str()))?;
    reject_locked_skill_collisions(&lock)?;
    let output_skills: Vec<String> = materialized
        .iter()
        .map(|m| m.name.clone())
        .chain(previously_managed.iter().cloned())
        .collect();
    let output_plugins: Vec<String> = plugins
        .iter()
        .map(|p| p.name.clone())
        .chain(previously_managed_plugins.iter().cloned())
        .collect();
    guard_working_copies(
        scope,
        &dir,
        &vendors,
        &linked,
        &output_skills,
        &output_plugins,
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
    Ok(Plan {
        dir,
        targets: manifest.targets.clone(),
        declares_any: !manifest.skills.is_empty() || !manifest.plugins.is_empty(),
        vendors,
        lock,
        materialized,
        plugins,
        previously_managed,
        previously_managed_plugins,
        locked,
        links,
    })
}

impl Plan {
    /// Project the planned skills + plugins into every configured target, then
    /// persist `ai.lock`. Returns the number of locked skills + plugins.
    fn apply(self, scope: &Scope) -> Result<usize> {
        let lock = &self.lock;
        if self.declares_any {
            println!("materializing: {}", self.targets.join(", "));
        }
        // Skills copied from a linked plugin's working tree are owned only via
        // the links file. Record the planned ones alongside the previous ones
        // before any vendor writes, so a failure part-way through never leaves
        // an agent-discovered skill that no later sync or `clean` knows to
        // remove; the final links file (below) narrows it once all succeeded.
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
        for vendor in &self.vendors {
            vendor.materialize(
                scope,
                &lock.id,
                &self.materialized,
                &self.previously_managed,
            )?;
            vendor.materialize_plugins(
                scope,
                &lock.id,
                &self.plugins,
                &self.previously_managed_plugins,
            )?;
        }
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
        } else if Links::path(scope)?.exists() {
            // Unchanged links still need their ignore rule, e.g. after
            // `.gitignore` was restored or deleted.
            Links::ensure_ignored(scope)?;
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

/// Before `vendors` write or delete anything for `skills`/`plugins`, reject
/// any overlap with a linked working copy: each `active` link, plus every one
/// the links file on disk still records (one being dropped by `spm unlink`,
/// re-pointed by `spm link`, or ignored as undeclared) — it is still the
/// user's data even when spm no longer copies from it. A recorded dir that is
/// gone has nothing left to protect. Every command that writes target output
/// or deletes it (`sync`, `clean`, `target remove`) must call this first.
pub(super) fn guard_working_copies(
    scope: &Scope,
    manifest_dir: &std::path::Path,
    vendors: &[Box<dyn Vendor>],
    active: &BTreeMap<String, Linked>,
    skills: &[String],
    plugins: &[String],
) -> Result<()> {
    let mut guarded: Vec<(&str, PathBuf, &str, bool)> = active
        .iter()
        .map(|(n, l)| (n.as_str(), l.dir.clone(), l.display.as_str(), true))
        .collect();
    let on_disk = Links::load(scope).unwrap_or_default();
    for name in on_disk.links.keys() {
        if let Ok(Some(l)) = on_disk.resolve(scope, name) {
            let display = on_disk.links[name].as_str();
            if !guarded
                .iter()
                .any(|(n, _, d, _)| n == name && *d == display)
            {
                guarded.push((name, l.dir, display, false));
            }
        }
    }
    reject_link_overlaps(scope, manifest_dir, vendors, &guarded, skills, plugins)
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
/// `guarded` lists `(name, canonical dir, dir as recorded, still linked?)`.
fn reject_link_overlaps(
    scope: &Scope,
    manifest_dir: &std::path::Path,
    vendors: &[Box<dyn Vendor>],
    guarded: &[(&str, PathBuf, &str, bool)],
    skills: &[String],
    plugins: &[String],
) -> Result<()> {
    use crate::pathid::{deletion_target, identity, resolve};
    if guarded.is_empty() {
        return Ok(());
    }
    // (owner, path as spelled, physical identity, written through?)
    let mut outputs = Vec::new();
    let mut patched: Vec<(&str, PathBuf)> = vec![
        ("spm", Manifest::path_in(manifest_dir)),
        ("spm", Lockfile::path_in(manifest_dir)),
        ("spm", Links::path(scope)?),
    ];
    if let Scope::Project { root } = scope {
        patched.push(("spm", root.join(".gitignore")));
    }
    for v in vendors {
        let mut paths = v.clean_paths(scope, skills)?;
        paths.extend(v.skill_paths(scope, skills)?);
        paths.extend(v.plugin_clean_paths(scope, plugins)?);
        paths.extend(v.plugin_paths(scope, plugins)?);
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
    for (name, dir, display, active) in guarded {
        let src = identity(dir)?;
        let mut route = Vec::new();
        resolve(&manifest_dir.join(display), &mut route)?;
        let route = route
            .iter()
            .map(|r| identity(r))
            .collect::<Result<Vec<_>>>()?;
        let hit = outputs
            .iter()
            .find(|(_, _, out, through)| {
                out.starts_with(&src)
                    || (!through
                        && (src.starts_with(out) || route.iter().any(|r| r.starts_with(out))))
            })
            .map(|(owner, p, ..)| (*owner, p.as_path(), String::new()));
        let hit = match hit {
            Some(h) => Some(h),
            None => hard_linked_into(dir, &patched)?.map(|(owner, p, alias)| {
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
            "linked directory `{display}` for `{name}` overlaps `{}`{how}, which {owner} writes \
             or deletes",
            p.display()
        );
        let g = if scope.is_global() { " -g" } else { "" };
        let relink = if *active {
            format!(
                "link `{name}` to a directory outside spm's output \
                 (`spm link {name} <dir>{g}`), or "
            )
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
