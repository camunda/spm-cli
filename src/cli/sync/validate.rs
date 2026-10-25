//! The checks of a sync that judge what it resolved before anything is
//! written: the source policy and targets up front ([`preflight`]), skill and
//! plugin name collisions, the overlap guard, and the `--locked` backstop.

use super::guard::{guard_working_copies, Writes};
use super::{resolve, LockMode, Run};
use crate::cli::verdict::{LockFindings, LockVerdict};
use crate::ids::DepName;
use crate::links::{Linked, Links};
use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::Manifest;
use crate::owned::{Owned, Ownership};
use crate::policy::Policy;
use crate::scope::Scope;
use crate::vendor::{MaterializedPlugin, MaterializedSkill, Vendor};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::Path;

use crate::cli::session::{Access, ScopeSession};

/// What a sync needs settled before it resolves anything.
pub(super) struct Preflight {
    pub(super) policy: std::sync::Arc<Policy>,
    /// The `spm link` overrides in effect (none for a lock-only sync or a
    /// check, which ignore links).
    pub(super) linked: BTreeMap<DepName, Linked>,
    /// One per target in `ai.json`.
    pub(super) vendors: Vec<Box<dyn Vendor>>,
}

/// Everything a sync of `manifest` in `mode` checks before any network
/// access: the lock `session` holds, the targets, the source policy (every
/// denied source reported at once), the `links` overrides, and — when `mode`
/// requires one — that `ai.lock` exists.
pub(super) fn preflight(
    session: &ScopeSession,
    manifest: &Manifest,
    links: &Links,
    mode: LockMode,
    run: Run,
) -> Result<Preflight> {
    if mode == LockMode::Links && session.access() != Access::Links {
        bail!("internal error: editing links requires the exclusive links lock");
    }
    if session.access() == Access::Read {
        bail!("internal error: a sync requires the state lock");
    }
    let scope = session.scope();
    let dir = session.dir();
    if manifest.targets.is_empty() {
        bail!("ai.json declares no targets");
    }
    // Source policy: judge every direct source before any network access, and
    // report all denied ones at once.
    let policy = crate::policy::activate(manifest, dir)?;
    // `ai.lock` never depends on links, and a lock-only sync writes nothing
    // else, so it ignores them (a missing linked directory can't block it).
    let linked = if run.lock_only || run.check {
        BTreeMap::new()
    } else {
        resolve::links(scope, manifest, links)?
    };
    let denied = denied_sources(
        manifest,
        &policy,
        linked.iter().map(|(n, l)| (n.as_str(), l.display.as_str())),
    );
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
    if run.locked && !Lockfile::path_in(dir).exists() {
        LockVerdict::Missing {
            path: Lockfile::path_in(dir),
            declared: manifest.declares_any(),
        }
        .enforce(mode.enforcer(), scope)?;
    }
    Ok(Preflight {
        policy,
        linked,
        vendors,
    })
}

/// Describe each dependency of `manifest` — and each linked directory in
/// `links` (`(name, recorded path)`) — that `policy` does not allow. The one
/// rule [`preflight`] enforces before any network access and `spm doctor` reports.
pub(in crate::cli) fn denied_sources<'a>(
    manifest: &Manifest,
    policy: &crate::policy::Policy,
    links: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<String> {
    manifest
        .dependencies()
        .filter_map(|(kind, name, spec)| {
            policy
                .check(&spec.git)
                .err()
                .map(|e| format!("{kind} `{name}`: {e:#}"))
        })
        .chain(links.into_iter().filter_map(|(name, display)| {
            policy
                .check_local(display)
                .err()
                .map(|e| format!("link `{name}`: {e:#}"))
        }))
        .collect()
}

/// Flatten plugin-bundled skills into the same list every vendor consumes, so
/// even skills-only targets get them, and reject every name collision in the
/// result and in `lock`.
///
/// A bundled skill name that collides with a standalone skill (or another
/// plugin's skill) is a hard error: silently overwriting a skill's content by
/// manifest-key ordering is exactly the kind of drift AGENTS.md's "no drift
/// surfaces" rule forbids. A lock-only sync materializes nothing and fetches
/// just some plugins, so the names it carries over unfetched are checked from
/// the lock.
pub(super) fn collisions(
    materialized: &mut Vec<MaterializedSkill>,
    plugin_skills: Vec<MaterializedSkill>,
    lock: &Lockfile,
) -> Result<()> {
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
    // From the lock, not the fetched plugins: a lock-only sync fetches only
    // the plugins it (re)resolves, yet must not write a lock an install rejects.
    reject_case_folding_collisions("plugin", lock.plugins.keys().map(DepName::as_str))?;
    reject_locked_skill_collisions(lock)
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

/// What a sync is about to write, for [`guard`].
pub(super) struct Output<'a> {
    pub(super) vendors: &'a [Box<dyn Vendor>],
    pub(super) previously_owned: &'a Ownership,
    pub(super) materialized: &'a [MaterializedSkill],
    pub(super) plugins: &'a [MaterializedPlugin],
    pub(super) linked: &'a BTreeMap<DepName, Linked>,
    pub(super) snapshot: Option<&'a Path>,
}

/// Run the overlap guard ([`guard_working_copies`]) for a sync of `manifest`
/// about to write `out`.
pub(super) fn guard(
    scope: &Scope,
    dir: &Path,
    manifest: &Manifest,
    out: Output,
    run: Run,
) -> Result<()> {
    let lock_only = run.lock_only;
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
                let mut owned = out.previously_owned.for_target(t);
                owned
                    .skills
                    .extend(out.materialized.iter().map(|m| m.name.clone()));
                owned
                    .plugins
                    .extend(out.plugins.iter().map(|p| p.name.clone()));
                owned
            })
            .collect();
        (out.vendors, outputs)
    };
    guard_working_copies(
        scope,
        dir,
        guarded_vendors,
        out.linked,
        &outputs,
        out.snapshot,
        Writes {
            lock_only,
            // A lock-only sync freezes only a legacy assumption (see
            // `Plan::apply`); any other sync records what it installs.
            ownership: !lock_only || matches!(out.previously_owned, Ownership::Legacy(_)),
        },
    )
}

/// Backstop for the whole defect class of `--locked`: whatever the specific
/// checks missed (a hand-edited `bundled_skills`/`requested_by`/store key,
/// ...), it must never install a set that differs from the committed lock in
/// `dir`. Compared as JSON values, so line endings/formatting don't matter;
/// every difference from the `lock` spm built is recorded in `findings`.
/// `unwalked` are the skills whose transitive subtree was not walked.
pub(super) fn backstop(
    dir: &Path,
    resolve_transitive: bool,
    lock: &Lockfile,
    unwalked: &[String],
    findings: &mut LockFindings,
) -> Result<()> {
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(Lockfile::path_in(dir))?)?;
    // An unverified transitive subtree was not walked, so the lock built here
    // lacks it: only the entries it did build are compared.
    if resolve_transitive && !unwalked.is_empty() {
        findings
            .rewritten
            .extend(verified_lock_drift(&on_disk, lock, unwalked)?);
    } else if on_disk != serde_json::to_value(lock)? {
        findings.rewritten.push(
            "ai.lock differs from what spm would write (e.g. a plugin's bundled skills \
             or a transitive skill's requesters changed)"
                .to_string(),
        );
    }
    Ok(())
}

/// The whole-lock backstop of `--locked` for a walk that left `unverified`
/// subtrees unwalked: every entry the walk built must equal its `on_disk`
/// counterpart, except that an unwalked subtree may add to a *skill* entry
/// requesters (an unverified skill, or a pin the walk didn't reach) and
/// `also_satisfies` evidence. Plugins are never walked, so they must match
/// exactly. Entries the walk didn't build are judged by the miss and stale
/// checks instead.
fn verified_lock_drift(
    on_disk: &serde_json::Value,
    lock: &Lockfile,
    unverified: &[String],
) -> Result<Vec<String>> {
    use serde_json::Value;
    let mut drift = Vec::new();
    // An unwalked subtree only adds skill entries: every other top-level field
    // (the project `id`, the never-walked `plugins` map, any stray key) must
    // match exactly.
    let outer = |v: Value| -> serde_json::Map<String, Value> {
        let mut fields = match v {
            Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        fields.remove("skills");
        fields
    };
    let (ours, theirs) = (outer(serde_json::to_value(lock)?), outer(on_disk.clone()));
    let mut keys: Vec<&String> = ours.keys().chain(theirs.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let (ours, theirs) = (ours.get(key), theirs.get(key));
        if ours == theirs {
            continue;
        }
        if key != "plugins" {
            drift.push(format!(
                "ai.lock top-level `{key}` differs from what spm would write"
            ));
            continue;
        }
        // Name the plugins that differ; only a shape difference (e.g. an
        // empty map spm omits) is left to report as the whole map.
        let entries = |v: Option<&Value>| v.and_then(Value::as_object).cloned().unwrap_or_default();
        let (ours, theirs) = (entries(ours), entries(theirs));
        let mut names: Vec<&String> = ours.keys().chain(theirs.keys()).collect();
        names.sort();
        names.dedup();
        let before = drift.len();
        for name in names.into_iter().filter(|n| ours.get(*n) != theirs.get(*n)) {
            drift.push(format!(
                "ai.lock entry for plugin `{name}` differs from what spm would write (e.g. \
                 its bundled skills changed)"
            ));
        }
        if drift.len() == before {
            drift.push("ai.lock `plugins` differs from what spm would write".to_string());
        }
    }
    let unwalked = |r: &str| {
        unverified.iter().any(|u| u == r)
            || (on_disk["skills"].get(r).is_some() && !lock.skills.contains_key(r))
    };
    let list = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    // The walk writes both lists sorted and deduplicated (see
    // `transitive::expand`) and omits them when empty, so additions are only
    // tolerated in that form.
    fn strict<T: Ord>(v: &[T]) -> bool {
        v.windows(2).all(|w| w[0] < w[1])
    }
    let canonical = |disk: &Value| {
        let present_empty = ["requested_by", "also_satisfies"].iter().any(|k| {
            disk.get(k)
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        });
        !present_empty
            && serde_json::from_value::<LockedSkill>(disk.clone())
                .is_ok_and(|l| strict(&l.requested_by) && strict(&l.also_satisfies))
    };
    for (name, entry) in &lock.skills {
        let Some(disk) = on_disk["skills"].get(name.as_str()) else {
            continue;
        };
        let mut disk = disk.clone();
        let entry = serde_json::to_value(entry)?;
        // Only a transitive pin (one the walk gave requesters) can gain
        // requesters from a skipped subtree; a direct pin never has any.
        let tolerated: &[&str] = if !canonical(&disk) {
            &[]
        } else if !list(&entry, "requested_by").is_empty() {
            &["requested_by", "also_satisfies"]
        } else {
            &["also_satisfies"]
        };
        for &key in tolerated {
            let (ours, theirs) = (list(&entry, key), list(&disk, key));
            let added_by_unwalked = ours.iter().all(|v| theirs.contains(v))
                && theirs
                    .iter()
                    .filter(|v| !ours.contains(v))
                    .all(|v| key == "also_satisfies" || v.as_str().is_some_and(unwalked));
            if added_by_unwalked {
                match entry.get(key) {
                    Some(v) => disk[key] = v.clone(),
                    None => {
                        if let Some(fields) = disk.as_object_mut() {
                            fields.remove(key);
                        }
                    }
                }
            }
        }
        if disk != entry {
            drift.push(format!(
                "ai.lock entry for skill `{name}` differs from what spm would write (e.g. \
                 a transitive skill's requesters changed)"
            ));
        }
    }
    Ok(drift)
}
