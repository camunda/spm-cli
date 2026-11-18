//! The resolve phase of a sync: pin every dependency `ai.json` declares
//! (reusing `ai.lock` where it still matches), populate the store, and walk
//! nested `ai.json` dependencies — building the lock a sync would write and,
//! under `--locked`, recording every way the previous one disagrees with it.
//! Nothing here writes outside the global store.

use super::{unlink_command, Run};
use crate::cli::verdict::LockFindings;
use crate::fetch::{Checkout, Reporter};
use crate::ids::DepName;
use crate::links::{Linked, Links};
use crate::lockfile::{LockEntry, LockedPlugin, LockedSkill, Lockfile, Pin};
use crate::manifest::{DependencyKind, Manifest, SkillSpec};
use crate::materialized::{MaterializedPlugin, Selected};
use crate::scope::Scope;
use crate::{resolver, transitive};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The direct dependencies pinned by [`direct`].
pub(super) struct Resolution {
    /// The lock being built: the project id and every direct pin.
    pub(super) lock: Lockfile,
    /// The previous lock without the direct pins taken from it: its
    /// transitive pins, and any stale entry.
    pub(super) prev: Lockfile,
    /// Direct entries (re)resolved by this run rather than reused from
    /// `ai.lock`: the only ones a lock-only sync fetches.
    pub(super) fresh: BTreeSet<DepName>,
    /// Linked plugins whose pin was reused, so their locked bundled skills
    /// are carried forward rather than re-enumerated from a checkout.
    pub(super) reused_linked_plugins: BTreeSet<DepName>,
    /// `--locked` only: direct skills that failed the pin check and so are
    /// not walked (their transitive subtree is never visited).
    pub(super) skipped_roots: BTreeSet<DepName>,
}

/// Pin every direct dependency of `manifest` — one pass, skills before
/// plugins — reusing its entry in `prev` (the previous `ai.lock`, read from
/// `dir`) when it still matches. Under `--locked` nothing is resolved: every
/// pin that can't be reused, a missing project id and every direct entry
/// `ai.json` no longer declares is recorded in `findings` instead. `linked`
/// are the `spm link` overrides in effect.
pub(super) fn direct(
    manifest: &Manifest,
    mut prev: Lockfile,
    dir: &Path,
    run: Run,
    linked: &BTreeMap<DepName, Linked>,
    findings: &mut LockFindings,
) -> Result<Resolution> {
    let (mode, report) = (run.mode, run.report);
    // Stable, path-independent project id: reuse the locked one, else mint & persist.
    let id = if prev.id.is_empty() {
        if mode.locked() {
            findings.missing_id = true;
        }
        crate::lockfile::generate_id(dir)
    } else {
        prev.id.clone()
    };
    let mut lock = Lockfile {
        id,
        ..Default::default()
    };

    if !manifest.declares_any() {
        report.say(format_args!("no skills or plugins declared"));
    } else {
        report.say(format_args!(
            "resolving {} skill(s) + {} plugin(s) for: {}",
            manifest.skills.len(),
            manifest.plugins.len(),
            manifest.targets.join(", ")
        ));
    }

    let mut fresh: BTreeSet<DepName> = BTreeSet::new();
    let mut reused_linked_plugins: BTreeSet<DepName> = BTreeSet::new();
    let mut skipped_roots: BTreeSet<DepName> = BTreeSet::new();
    // Pin reuse, `--locked` mismatches and the status line are shared
    // (`resolve_entry`); only the kind-specific bookkeeping differs.
    for (kind, name, spec) in manifest.dependencies() {
        let prev_entry = match kind {
            DependencyKind::Skill => prev.skills.remove(name).map(Previous::Skill),
            DependencyKind::Plugin => prev.plugins.remove(name).map(Previous::Plugin),
        };
        let entry = match resolve_entry(
            kind,
            name,
            spec,
            prev_entry,
            mode.refresh().direct(name),
            mode.locked(),
        )? {
            Ok(entry) => entry,
            Err(mismatch) => {
                findings.drift.push(mismatch);
                // A skill failing the pin check is not walked, so its
                // transitive subtree is never visited.
                if kind == DependencyKind::Skill {
                    skipped_roots.insert(name.clone());
                }
                continue;
            }
        };
        let how = if entry.fresh {
            fresh.insert(name.clone());
            "resolved"
        } else {
            "locked"
        };
        let short = entry.pin.commit.short();
        let reference = entry.pin.display_reference();
        let status = match kind {
            DependencyKind::Skill => how.to_string(),
            DependencyKind::Plugin => format!("{kind}, {how}"),
        };
        report.entry(name, format_args!("{reference} @ {short}  ({status})"));
        match (kind, entry.prev) {
            // A direct skill never carries transitive provenance, whatever a
            // reused entry held (e.g. a skill promoted from transitive to
            // direct). Re-resolved (e.g. `spm update <name>`) to the very same
            // git + path + commit: the selectors previously verified to resolve
            // to that commit still do, so keep them as evidence. Otherwise a
            // nested edge onto this root by another, since-moved selector would
            // be re-resolved — cascading a named update into the transitive
            // frontier — and report a spurious conflict. A bare `spm update`
            // refreshes the walk and ignores evidence.
            (DependencyKind::Skill, prev) => {
                let evidence = match prev {
                    Some(Previous::Skill(p)) if p.pin.same_content(&entry.pin) => p.also_satisfies,
                    _ => Vec::new(),
                };
                lock.skills
                    .insert(name.clone(), LockedSkill::direct(entry.pin, evidence));
            }
            // A plugin's bundled-skill list is (re)filled from the store
            // checkout below, so a stale one is never carried forward. A reused
            // pin is the same commit, so its bundled skills are too. They are
            // carried forward rather than re-enumerated from a checkout when
            // that checkout may go unread (see `Mode::carries_reused_pins`) or
            // for a linked plugin (keeping `ai.lock` unaffected by the link).
            (DependencyKind::Plugin, prev) => {
                let carried =
                    !entry.fresh && (mode.carries_reused_pins() || linked.contains_key(name));
                let bundled_skills = match prev {
                    Some(Previous::Plugin(p)) if carried => p.bundled_skills,
                    _ => Vec::new(),
                };
                if !entry.fresh && linked.contains_key(name) {
                    reused_linked_plugins.insert(name.clone());
                }
                lock.plugins.insert(
                    name.clone(),
                    LockedPlugin {
                        pin: entry.pin,
                        bundled_skills,
                    },
                );
            }
        }
    }

    // Direct entries left in the previous lock are no longer declared.
    // Transitive skills are judged after the walk (see [`judge_transitive`]).
    if mode.locked() {
        for (name, l) in prev.dependencies() {
            if !matches!(l, LockEntry::Skill(s) if s.is_transitive()) {
                findings.stale.push(format!(
                    "{} `{name}`: in ai.lock but not declared in ai.json",
                    l.kind()
                ));
            }
        }
    }
    Ok(Resolution {
        lock,
        prev,
        fresh,
        reused_linked_plugins,
        skipped_roots,
    })
}

/// The plugins [`plugins`] fetched (or linked) for materialization.
pub(super) struct Plugins {
    pub(super) plugins: Vec<MaterializedPlugin>,
    /// Their bundled skills, flattened later into the list every vendor
    /// consumes.
    pub(super) skills: Vec<Selected>,
    /// The skills bundled by linked plugins (recorded in the links file).
    pub(super) linked_skills: Vec<DepName>,
}

/// Populate the store for the plugins pinned in `res`: we need each plugin's
/// checkout to enumerate its bundled skills before writing the lockfile (so
/// `ai.lock` records the full component set) and before flattening those
/// skills into the shared list every vendor consumes. Every plugin tree is
/// scanned before any of it can be materialized. A check fetches nothing: a
/// plugin missing from the store is recorded in `findings` as unverified.
pub(super) fn plugins(
    manifest: &Manifest,
    res: &mut Resolution,
    linked: &BTreeMap<DepName, Linked>,
    run: Run,
    findings: &mut LockFindings,
) -> Result<Plugins> {
    let report = run.report;
    let fetch = run.mode.fetch(&res.fresh);
    let mut out = Plugins {
        plugins: Vec::new(),
        skills: Vec::new(),
        linked_skills: Vec::new(),
    };
    if !manifest.plugins.is_empty() {
        report.say(format_args!("fetching plugins into store"));
    }
    for (name, locked) in &mut res.lock.plugins {
        if fetch.skips(name) {
            report.unchanged(name);
            continue;
        }
        let link = linked.get(name);
        // A linked plugin only needs its locked checkout to record the commit's
        // bundled skills in `ai.lock` when its pin was freshly resolved.
        let checkout = if link.is_some() && res.reused_linked_plugins.contains(name) {
            None
        } else {
            match fetch.checkout(DependencyKind::Plugin, name, &locked.pin, report)? {
                // Not in the store: keep the locked bundled skills (carried
                // over above), unverified.
                Checkout::Unverified => {
                    findings.unverified.push(name.to_string());
                    continue;
                }
                checkout => Some(checkout),
            }
        };
        let ensured = checkout.as_ref().and_then(Checkout::ensured);
        // A check only inspects the store: it gates and materializes nothing.
        let inspect = matches!(checkout, Some(Checkout::Inspect(_)));
        let (path, root) = if let Some(l) = link {
            report.entry(name, format_args!("linked → {}", l.display));
            (l.content.clone(), l.content.clone())
        } else if let Some(e) = ensured {
            (e.path.clone(), e.root.clone())
        } else {
            bail!("plugin `{name}` was neither linked nor fetched");
        };
        if !inspect {
            warn_unless_plugin(name, &path, link, &locked.pin);
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
            out.linked_skills
                .extend(bundled.iter().map(|s| s.name.clone()));
        }
        if inspect {
            out.skills
                .extend(bundled.into_iter().map(|s| Selected::Named(s.name)));
            continue;
        }
        // Scan the whole plugin tree (agents, scripts, hooks, bundled skills)
        // before any of it is materialized into an agent-discovered directory:
        // the plugin and its bundled skills are only built from that scan.
        let scanned = crate::scan::enforce(name, &path, &root)
            .with_context(|| format!("scanning plugin `{name}`"))?;
        let plugin = MaterializedPlugin::new(name.clone(), scanned);
        for s in bundled {
            out.skills.push(Selected::Materialized(
                plugin.bundled_skill(s.name, s.path)?,
            ));
        }
        out.plugins.push(plugin);
    }
    Ok(out)
}

/// Warn when the plugin `name` at `path` (from `link`, else its `locked`
/// source) has no `.claude-plugin/plugin.json`.
fn warn_unless_plugin(name: &DepName, path: &Path, link: Option<&Linked>, locked: &Pin) {
    if crate::plugin::looks_like_plugin(path) {
        return;
    }
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

/// Populate the store for the skills pinned in `res`, run the security gate,
/// and collect absolute paths for the vendors — recursing into nested
/// `ai.json` dependencies when the root manifest opts in
/// (`resolveTransitive`). Each direct pin in `res.lock` gets the
/// `also_satisfies` evidence this walk recorded for it; the transitive skills
/// are returned to be folded into the lock (see [`fold_transitive`]).
pub(super) fn skills(
    manifest: &Manifest,
    res: &mut Resolution,
    run: Run,
    policy: &crate::policy::Policy,
    linked: &BTreeMap<DepName, Linked>,
) -> Result<transitive::Output> {
    let report = run.report;
    if !manifest.skills.is_empty() {
        report.say(format_args!("fetching skills into store"));
    }
    // Lock-only without transitive resolution: a reused skill is a leaf whose
    // pin is carried over as is, so only the (re)resolved ones are walked. With
    // it on, every skill is walked: its nested `ai.json` shapes the lock.
    let fetch = run.mode.fetch(&res.fresh);
    let skips = |name: &DepName| !manifest.resolve_transitive && fetch.skips(name);
    for name in res.lock.skills.keys().filter(|n| skips(n)) {
        report.unchanged(name);
    }
    let walked: BTreeMap<DepName, LockedSkill> = res
        .lock
        .skills
        .iter()
        .filter(|(n, _)| !skips(n))
        .map(|(n, l)| (n.clone(), l.clone()))
        .collect();
    let mut expansion = transitive::expand(
        &walked,
        &transitive::Ctx {
            prev: &res.prev.skills,
            resolve_transitive: manifest.resolve_transitive,
            mode: run.mode.walk(),
            report,
            policy,
            links: linked,
        },
    )?;
    // `also_satisfies` evidence is rebuilt by every walk: a direct root keeps
    // only what this walk recorded for it (nothing when transitive is off).
    let mut direct_also_satisfies = std::mem::take(&mut expansion.direct_also_satisfies);
    for (name, l) in &mut res.lock.skills {
        l.also_satisfies = direct_also_satisfies.remove(name).unwrap_or_default();
    }
    Ok(expansion)
}

/// `--locked`: record in `findings` the transitive pins of `res.prev` the
/// walk in `expansion` no longer reached (stale) and the ones it needed but
/// found missing (unresolved).
pub(super) fn judge_transitive(
    res: &mut Resolution,
    expansion: &mut transitive::Output,
    resolve_transitive: bool,
    findings: &mut LockFindings,
) {
    let lock_misses = std::mem::take(&mut expansion.lock_misses);
    let reached: BTreeSet<&str> = expansion
        .transitive
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    findings.stale.extend(stale_transitive_pins(
        &res.prev.skills,
        &reached,
        resolve_transitive,
        std::mem::take(&mut res.skipped_roots)
            .into_iter()
            .chain(lock_misses.iter().map(|(n, _)| n.clone()))
            .chain(std::mem::take(&mut expansion.uncertain_misses))
            .chain(expansion.unverified.iter().cloned()),
    ));
    findings
        .unresolved
        .extend(lock_misses.into_iter().map(|(_, m)| m));
}

/// Fold the transitively-resolved skills into `lock`. A synthesized name
/// colliding with a directly-declared dependency is a hard error (the
/// synthesized names are hash-suffixed to make this astronomically unlikely).
/// `skills` and `plugins` share one flat dependency-name namespace (enforced
/// at manifest load, see `manifest/load.rs`), so the name must be free in *both*
/// lock maps — a transitive skill colliding with a root plugin would write
/// the same name into both maps and make `list`/`update <name>` ambiguous.
pub(super) fn fold_transitive(
    lock: &mut Lockfile,
    transitive: Vec<(DepName, LockedSkill)>,
    report: Reporter,
) -> Result<()> {
    for (tname, tlocked) in transitive {
        if lock.contains_dependency(&tname) {
            bail!(
                "transitive skill name collision: `{tname}` clashes with a directly-declared \
                 skill or plugin — rename the direct dependency or pin the dependency that \
                 brings it in"
            );
        }
        report.entry(
            &tname,
            format_args!("transitive (via {})", tlocked.requested_by().join(", ")),
        );
        lock.skills.insert(tname, tlocked);
    }
    Ok(())
}

/// Resolve every `spm link` override that names a declared skill or plugin to
/// its directory. A link whose name is no longer declared is ignored with a
/// warning (it stays in the links file until `spm unlink`); a declared one whose
/// directory is gone is an error, so a sync never silently falls back to the
/// locked version while the user believes a local override is active.
pub(super) fn links(
    scope: &Scope,
    manifest: &Manifest,
    links: &Links,
) -> Result<BTreeMap<DepName, Linked>> {
    let mut out = BTreeMap::new();
    for name in links.links.keys() {
        if !manifest.contains_dependency(name) {
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

/// A direct dependency's entry in the previous `ai.lock`.
enum Previous {
    Skill(LockedSkill),
    Plugin(LockedPlugin),
}

impl Previous {
    fn pin(&self) -> &Pin {
        match self {
            Self::Skill(l) => &l.pin,
            Self::Plugin(l) => &l.pin,
        }
    }
}

/// A direct dependency pinned by [`resolve_entry`].
struct Entry {
    pin: Pin,
    /// Resolved by this run rather than reused from `ai.lock`.
    fresh: bool,
    /// Its entry in the previous `ai.lock`, for what a pin carries over.
    prev: Option<Previous>,
}

/// Pin the direct dependency `name` of `kind`: reuse `prev` (its entry in the
/// previous `ai.lock`) when it pins exactly `spec` and `refresh` is off, else
/// resolve `spec`. Under `locked` nothing is resolved: a pin that can't be
/// reused yields `Ok(Err(mismatch))`, the line `--locked` reports. The single
/// pin rule for skills and plugins alike.
fn resolve_entry(
    kind: DependencyKind,
    name: &DepName,
    spec: &SkillSpec,
    prev: Option<Previous>,
    refresh: bool,
    locked: bool,
) -> Result<std::result::Result<Entry, String>> {
    spec.version()?; // validates selectors
    let (pin, fresh) = match reusable_pin(prev.as_ref().map(Previous::pin), spec, refresh)? {
        Ok(p) => (p.clone(), false),
        Err(why) if locked => return Ok(Err(format!("{kind} `{name}`: {why}"))),
        Err(_) => (
            resolver::resolve(spec).with_context(|| format!("resolving {kind} `{name}`"))?,
            true,
        ),
    };
    Ok(Ok(Entry { pin, fresh, prev }))
}

/// The pin in `prev` if it can be reused for `spec` without re-resolving, else
/// why not (missing, refreshing, or the request changed).
fn reusable_pin<'a>(
    prev: Option<&'a Pin>,
    spec: &SkillSpec,
    refresh: bool,
) -> Result<std::result::Result<&'a Pin, String>> {
    let Some(p) = prev else {
        return Ok(Err("missing from ai.lock".to_string()));
    };
    if refresh {
        return Ok(Err("refreshing".to_string()));
    }
    Ok(match p.drift_from(spec)? {
        None => Ok(p),
        Some(changes) => Err(format!("changed since ai.lock was written ({changes})")),
    })
}

/// `--locked`: describe every transitive pin left in `prev` (direct entries
/// already removed) that the walk did not reach. A pin that hangs off a
/// subtree the walk skipped — a direct root or edge that itself failed the pin
/// check, seeded by `unwalked` — may still be needed, so it is not judged (that
/// root's own mismatch is already reported). With `resolveTransitive` off
/// nothing is walked, so every transitive pin is stale.
pub(super) fn stale_transitive_pins(
    prev: &BTreeMap<DepName, LockedSkill>,
    reached: &BTreeSet<&str>,
    resolve_transitive: bool,
    unwalked: impl IntoIterator<Item = DepName>,
) -> Vec<String> {
    let unreached: Vec<(&DepName, &LockedSkill)> = prev
        .iter()
        .filter(|(n, l)| l.is_transitive() && !reached.contains(n.as_str()))
        .collect();
    let mut unwalked: BTreeSet<DepName> = if resolve_transitive {
        unwalked.into_iter().collect()
    } else {
        BTreeSet::new()
    };
    // Fixed point: anything requested by an unwalked node is itself unwalked.
    loop {
        let before = unwalked.len();
        for (n, l) in &unreached {
            if l.requested_by().iter().any(|r| unwalked.contains(r)) {
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

#[cfg(test)]
mod tests {
    use super::super::Mode;
    use super::*;
    use crate::ids::dep;
    use crate::lockfile::{Requesters, Satisfied};

    fn spec(selector: (&str, &str)) -> SkillSpec {
        let mut s = SkillSpec {
            git: "https://example.test/repo.git".into(),
            tag: None,
            branch: None,
            commit: None,
            range: None,
            path: None,
        };
        let v = Some(selector.1.to_string());
        match selector.0 {
            "tag" => s.tag = v,
            "branch" => s.branch = v,
            _ => {}
        }
        s
    }

    fn pin(spec: &SkillSpec) -> Pin {
        let commit = crate::ids::CommitSha::try_from("a".repeat(40)).unwrap();
        Pin {
            git: spec.git.clone(),
            reference: crate::ids::LockReference::from(spec.version().unwrap()),
            store: crate::ids::StoreKey::derive(&spec.git, &commit),
            commit,
            resolved_tag: None,
            path: spec.path.clone(),
        }
    }

    /// A previous transitive skill entry, with `also_satisfies` evidence.
    fn skill(spec: &SkillSpec) -> LockedSkill {
        let mut l = LockedSkill::transitive(pin(spec), Requesters::new(dep("parent")));
        l.also_satisfies = vec![Satisfied {
            reference: crate::ids::LockReference::try_from("tag:v0").unwrap(),
            resolved_tag: None,
        }];
        l
    }

    fn plugin(spec: &SkillSpec) -> LockedPlugin {
        LockedPlugin {
            pin: pin(spec),
            bundled_skills: vec![dep("bundled")],
        }
    }

    fn previous(kind: DependencyKind, spec: &SkillSpec) -> Previous {
        match kind {
            DependencyKind::Skill => Previous::Skill(skill(spec)),
            DependencyKind::Plugin => Previous::Plugin(plugin(spec)),
        }
    }

    /// Under `--locked`, a pin that can't be reused becomes the mismatch line,
    /// worded the same for both kinds; nothing is resolved.
    #[test]
    fn resolve_entry_reports_locked_mismatches_per_kind() {
        let main = spec(("branch", "main"));
        let mismatch =
            |kind, prev, refresh| match resolve_entry(kind, &dep("x"), &main, prev, refresh, true)
                .unwrap()
            {
                Ok(_) => panic!("expected a mismatch"),
                Err(m) => m,
            };
        for kind in DependencyKind::ALL {
            assert_eq!(
                mismatch(kind, None, false),
                format!("{kind} `x`: missing from ai.lock")
            );
            assert_eq!(
                mismatch(kind, Some(previous(kind, &spec(("tag", "v1")))), false),
                format!(
                    "{kind} `x`: changed since ai.lock was written (`tag:v1` -> `branch:main`)"
                )
            );
            assert_eq!(
                mismatch(kind, Some(previous(kind, &main)), true),
                format!("{kind} `x`: refreshing")
            );
        }
    }

    /// An unchanged pin is reused untouched and hands back the previous entry
    /// (what the caller may carry over).
    #[test]
    fn resolve_entry_reuses_an_unchanged_pin_with_its_previous_entry() {
        let main = spec(("branch", "main"));
        for kind in DependencyKind::ALL {
            for locked in [false, true] {
                let prev = previous(kind, &main);
                let entry = resolve_entry(kind, &dep("x"), &main, Some(prev), false, locked)
                    .unwrap()
                    .unwrap();
                assert!(!entry.fresh);
                assert_eq!(entry.pin, pin(&main));
                assert_eq!(entry.prev.as_ref().map(Previous::pin), Some(&pin(&main)));
            }
        }
    }

    /// Selectors are validated before any pin is considered.
    #[test]
    fn resolve_entry_rejects_an_invalid_selector() {
        let none = spec(("none", ""));
        let err = match resolve_entry(DependencyKind::Plugin, &dep("x"), &none, None, false, true) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(format!("{err}").contains("set one of tag/branch/commit/version"));
    }

    const GIT: &str = "https://example.test/repo.git";

    fn manifest(json: &str) -> Manifest {
        Manifest::parse(json, Path::new("ai.json")).unwrap()
    }

    fn run(mode: Mode<'static>) -> Run<'static> {
        Run {
            mode,
            report: Reporter::silent(),
        }
    }

    /// `--locked` resolves nothing: matching pins are reused (with direct-only
    /// cleanup), and a missing id, every mismatch and every stale direct entry
    /// are recorded. A skill failing its pin check is not walked; transitive
    /// pins are left for the walk to judge.
    #[test]
    fn locked_direct_reuses_pins_and_records_every_mismatch() {
        let m = manifest(&format!(
            r#"{{"targets": ["claude"],
                "skills": {{"a": {{"git": "{GIT}", "branch": "main"}},
                            "b": {{"git": "{GIT}", "tag": "v2"}}}},
                "plugins": {{"p": {{"git": "{GIT}", "branch": "main"}}}}}}"#
        ));
        let main = skill(&spec(("branch", "main")));
        let direct_skill = LockedSkill::direct(main.pin.clone(), Vec::new());
        let prev = Lockfile {
            id: String::new(),
            skills: BTreeMap::from([
                (dep("a"), main.clone()),
                (dep("b"), skill(&spec(("tag", "v1")))),
                (dep("gone"), direct_skill),
                (dep("t"), main.clone()),
            ]),
            plugins: BTreeMap::from([
                (dep("p"), plugin(&spec(("branch", "main")))),
                (dep("old"), plugin(&spec(("branch", "main")))),
            ]),
        };
        let mut findings = LockFindings::default();
        let res = direct(
            &m,
            prev,
            Path::new("."),
            run(Mode::Locked),
            &BTreeMap::new(),
            &mut findings,
        )
        .unwrap();

        assert!(findings.missing_id && res.lock.id.starts_with("spm-"));
        assert_eq!(
            findings.drift,
            ["skill `b`: changed since ai.lock was written (`tag:v1` -> `tag:v2`)"]
        );
        assert_eq!(
            findings.stale,
            [
                "skill `gone`: in ai.lock but not declared in ai.json",
                "plugin `old`: in ai.lock but not declared in ai.json",
            ]
        );
        assert_eq!(res.skipped_roots, BTreeSet::from([dep("b")]));
        assert!(res.fresh.is_empty());
        let a = &res.lock.skills["a"];
        assert!(!a.is_transitive(), "a direct skill is never transitive");
        assert_eq!(a.also_satisfies, main.also_satisfies, "evidence is kept");
        assert!(!res.lock.skills.contains_key("b"));
        let p = &res.lock.plugins["p"];
        assert!(p.bundled_skills.is_empty(), "refilled from the checkout");
        assert!(res.prev.skills.contains_key("t"), "left for the walk");
    }

    /// A check never refills a reused plugin's bundled skills from a checkout
    /// it may not have, so they are carried forward; a locked id is kept.
    #[test]
    fn a_check_carries_a_reused_plugins_bundled_skills_forward() {
        let m = manifest(&format!(
            r#"{{"targets": ["claude"], "plugins": {{"p": {{"git": "{GIT}", "branch": "main"}}}}}}"#
        ));
        let prev = Lockfile {
            id: "spm-1234abcd".into(),
            plugins: BTreeMap::from([(dep("p"), plugin(&spec(("branch", "main"))))]),
            ..Default::default()
        };
        let mut findings = LockFindings::default();
        let res = direct(
            &m,
            prev,
            Path::new("."),
            run(Mode::Check),
            &BTreeMap::new(),
            &mut findings,
        )
        .unwrap();
        assert!(findings.in_sync() && !findings.missing_id);
        assert_eq!(res.lock.id, "spm-1234abcd");
        assert_eq!(res.lock.plugins["p"].bundled_skills, [dep("bundled")]);
    }

    /// A transitive skill may not take a name a direct skill or plugin holds;
    /// any other is folded into the lock's skills.
    #[test]
    fn fold_transitive_rejects_a_name_taken_by_a_direct_dependency() {
        let main = skill(&spec(("branch", "main")));
        let mut lock = Lockfile {
            plugins: BTreeMap::from([(dep("x"), plugin(&spec(("branch", "main"))))]),
            ..Default::default()
        };
        let err = fold_transitive(
            &mut lock,
            vec![(dep("x"), main.clone())],
            Reporter::silent(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.starts_with("transitive skill name collision: `x`"),
            "{err}"
        );
        assert!(lock.skills.is_empty());
        fold_transitive(
            &mut lock,
            vec![(dep("y"), main.clone())],
            Reporter::silent(),
        )
        .unwrap();
        assert_eq!(lock.skills["y"], main);
    }

    /// The walk's lock misses become unresolved findings, and their names
    /// (with the skipped roots) exempt their subtrees from the stale check.
    #[test]
    fn judge_transitive_records_misses_and_drains_them() {
        let mut res = Resolution {
            lock: Lockfile::default(),
            prev: Lockfile::default(),
            fresh: BTreeSet::new(),
            reused_linked_plugins: BTreeSet::new(),
            skipped_roots: BTreeSet::from([dep("b")]),
        };
        let mut expansion = transitive::Output {
            selected: Vec::new(),
            transitive: Vec::new(),
            direct_also_satisfies: BTreeMap::new(),
            lock_misses: vec![(dep("m"), "skill `m`: missing from ai.lock".into())],
            unverified: Vec::new(),
            uncertain_misses: Vec::new(),
        };
        let mut findings = LockFindings::default();
        judge_transitive(&mut res, &mut expansion, true, &mut findings);
        assert_eq!(findings.unresolved, ["skill `m`: missing from ai.lock"]);
        assert!(findings.stale.is_empty());
        assert!(res.skipped_roots.is_empty() && expansion.lock_misses.is_empty());
    }
}
