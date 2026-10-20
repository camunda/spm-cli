use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::{Manifest, SkillSpec};
use crate::scope::Scope;
use crate::vendor::{MaterializedPlugin, MaterializedSkill};
use crate::{resolver, store, transitive};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};

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
    if manifest.targets.is_empty() {
        bail!("ai.json declares no targets");
    }
    // Validate all targets up front before doing any network work.
    let vendors = manifest
        .targets
        .iter()
        .map(|t| crate::vendor::for_target(t))
        .collect::<Result<Vec<_>>>()?;
    if locked && !Lockfile::path_in(&dir).exists() {
        bail!(
            "--locked: {} not found\n{}",
            Lockfile::path_in(&dir).display(),
            locked_fix_hint(scope)
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
    let previously_managed: Vec<String> = prev
        .skills
        .keys()
        .cloned()
        .chain(
            prev.plugins
                .values()
                .flat_map(|l| l.bundled_skills.iter().cloned()),
        )
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
        let requested = spec.version()?; // validates selectors
        let reference = requested.label();
        let refresh = force_refresh && only.is_none_or(|o| o == name);

        // Reuse the pinned commit if the request is unchanged and we're not refreshing.
        let reuse = match reusable_pin(prev.skills.remove(name), spec, refresh)? {
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
        let short = &locked.commit[..locked.commit.len().min(8)];
        println!("  {name:<width$}  {reference} @ {short}  ({how})");
        lock.skills.insert(name.clone(), locked);
    }

    // Resolve full plugins the same way. A plugin's version reuse also depends
    // on the pinned commit being unchanged; its bundled-skill list is (re)filled
    // from the store checkout below, so we never carry a stale one forward.
    for (name, spec) in &manifest.plugins {
        let requested = spec.version()?;
        let reference = requested.label();
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
        locked.bundled_skills.clear();
        locked.also_satisfies.clear();
        let short = &locked.commit[..locked.commit.len().min(8)];
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
    for (name, locked) in &mut lock.plugins {
        let ensured = store::ensure(locked).with_context(|| format!("fetching plugin `{name}`"))?;
        println!(
            "  {name:<width$}  {}",
            if ensured.fetched { "fetched" } else { "cached" }
        );
        if !crate::plugin::looks_like_plugin(&ensured.path) {
            eprintln!(
                "warning: plugin `{name}` ({} @ {}) has no `.claude-plugin/plugin.json`{} — \
                 is it really a plugin? (only its bundled skills, if any, will be materialized)",
                locked.git,
                locked.reference,
                locked
                    .path
                    .as_deref()
                    .map(|p| format!(" under `{p}`"))
                    .unwrap_or_default()
            );
        }
        let bundled = crate::plugin::plugin_skills(&ensured.path, &ensured.root)
            .with_context(|| format!("enumerating skills bundled in plugin `{name}`"))?;
        locked.bundled_skills = bundled.iter().map(|s| s.name.clone()).collect();
        // Scan the whole plugin tree (agents, scripts, hooks, bundled skills)
        // before any of it is materialized into an agent-discovered directory.
        crate::scan::enforce(name, &ensured.path, &ensured.root)
            .with_context(|| format!("scanning plugin `{name}`"))?;
        plugin_skills.extend(bundled);
        plugins.push(MaterializedPlugin {
            name: name.clone(),
            path: ensured.path,
            root: ensured.root,
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
                "--locked: ai.lock is out of sync with ai.json ({} problem(s)):\n{}\n{}",
                mismatches.len(),
                mismatches
                    .iter()
                    .map(|m| format!("  - {m}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                locked_fix_hint(scope)
            );
        }
    }
    // Same resolved skills + plugins projected into every configured vendor.
    if !manifest.skills.is_empty() || !manifest.plugins.is_empty() {
        println!("materializing: {}", manifest.targets.join(", "));
    }
    for vendor in &vendors {
        vendor.materialize(scope, &lock.id, &materialized, &previously_managed)?;
        vendor.materialize_plugins(scope, &lock.id, &plugins, &previously_managed_plugins)?;
    }
    // Only persist `ai.lock` once every fallible step (resolution, fetching,
    // collision checks, and vendor materialization) has succeeded — otherwise
    // a failed `install` could leave behind a lockfile pointing at state that
    // was never actually materialized. `--locked` never writes it: it already
    // matches (checked above).
    if !locked {
        lock.save(&dir)?;
    }
    Ok(lock.skills.len() + lock.plugins.len())
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

/// How to fix a `--locked` failure, spelled for the scope in use.
fn locked_fix_hint(scope: &Scope) -> String {
    let g = if matches!(scope, Scope::Global) {
        " -g"
    } else {
        ""
    };
    format!("run `spm install{g}` (without --locked) and commit ai.lock")
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
