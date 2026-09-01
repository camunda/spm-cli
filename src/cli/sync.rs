use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::scope::Scope;
use crate::vendor::{MaterializedPlugin, MaterializedSkill};
use crate::{resolver, store};
use anyhow::{bail, Context, Result};

/// Core pipeline shared by add/remove/update/install:
/// resolve manifest -> ai.lock -> populate store -> materialize vendor config.
///
/// `force_refresh` re-resolves refs to their latest commit; `only` limits the
/// refresh to a single skill (used by `update <name>`).
pub(super) fn sync(scope: &Scope, force_refresh: bool, only: Option<&str>) -> Result<usize> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir)?;
    if manifest.targets.is_empty() {
        bail!("ai.json declares no targets");
    }
    // Validate all targets up front before doing any network work.
    let vendors = manifest
        .targets
        .iter()
        .map(|t| crate::vendor::for_target(t))
        .collect::<Result<Vec<_>>>()?;
    let mut prev = Lockfile::load_or_default(&dir)?;

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
        let reuse = prev.skills.remove(name).filter(|l| {
            !refresh && l.git == spec.git && l.reference == reference && l.path == spec.path
        });

        let (locked, how) = match reuse {
            Some(l) => (l, "locked"),
            None => (
                resolver::resolve(spec).with_context(|| format!("resolving skill `{name}`"))?,
                "resolved",
            ),
        };
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
        let reuse = prev.plugins.remove(name).filter(|l| {
            !refresh && l.git == spec.git && l.reference == reference && l.path == spec.path
        });
        let (mut locked, how) = match reuse {
            Some(l) => (l, "locked"),
            None => (
                resolver::resolve(spec).with_context(|| format!("resolving plugin `{name}`"))?,
                "resolved",
            ),
        };
        locked.bundled_skills.clear();
        let short = &locked.commit[..locked.commit.len().min(8)];
        println!("  {name:<width$}  {reference} @ {short}  (plugin, {how})");
        lock.plugins.insert(name.clone(), locked);
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
        let bundled = crate::plugin::plugin_skills(&ensured.path)
            .with_context(|| format!("enumerating skills bundled in plugin `{name}`"))?;
        locked.bundled_skills = bundled.iter().map(|s| s.name.clone()).collect();
        // Scan the whole plugin tree (agents, scripts, hooks, bundled skills)
        // before any of it is materialized into an agent-discovered directory.
        crate::scan::enforce(name, &ensured.path)
            .with_context(|| format!("scanning plugin `{name}`"))?;
        plugin_skills.extend(bundled);
        plugins.push(MaterializedPlugin {
            name: name.clone(),
            path: ensured.path,
        });
    }

    // Populate the store and collect absolute paths for the vendor.
    if !manifest.skills.is_empty() {
        println!("fetching skills into store");
    }
    let mut materialized: Vec<MaterializedSkill> = Vec::new();
    for (name, locked) in &lock.skills {
        let ensured = store::ensure(locked).with_context(|| format!("fetching skill `{name}`"))?;
        println!(
            "  {name:<width$}  {}",
            if ensured.fetched { "fetched" } else { "cached" }
        );
        // Single source of truth for the "is this actually a loadable skill?"
        // check — run once here rather than per-vendor.
        crate::skillcheck::warn_if_not_loadable(
            name,
            &locked.git,
            &locked.reference,
            locked.path.as_deref(),
            &ensured.path,
        );
        // Pre-materialize security gate: scan the fetched content before it is
        // copied into any agent-discovered directory. Blocks on high/critical
        // findings unless SPM_ALLOW_SUSPICIOUS is set.
        crate::scan::enforce(name, &ensured.path)
            .with_context(|| format!("scanning skill `{name}`"))?;
        materialized.push(MaterializedSkill {
            name: name.clone(),
            path: ensured.path,
        });
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
    // was never actually materialized.
    lock.save(&dir)?;
    Ok(lock.skills.len() + lock.plugins.len())
}
