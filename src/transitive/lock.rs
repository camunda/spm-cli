//! Lock synthesis for transitive children: reusing `ai.lock` pins, recording
//! the edges `--locked` refused to resolve, and normalizing the provenance the
//! walk accumulated into a stable lockfile.

use super::identity::{key_of, Key};
use super::{Ctx, Output};
use crate::ids::{LockReference, StoreKey};
use crate::lockfile::LockedSkill;
use crate::manifest::SkillSpec;
use crate::resolver;
use anyhow::Result;
use std::collections::BTreeSet;

/// A requirement the lock has no pin for under its synthesized name.
pub(super) struct Miss {
    pub(super) key: Key,
    pub(super) name: String,
    /// The selector the edge requested.
    pub(super) requested: LockReference,
    pub(super) problem: String,
}

/// Resolve a nested skill spec to a locked entry, reusing the previous
/// lockfile's pinned commit only when *this exact edge* — keyed by its
/// deterministic synthesized name `synth` — resolved the same `git+ref+path`
/// last time and we are not refreshing, so an unchanged transitive branch
/// dependency is not re-`ls-remote`d every sync.
///
/// Reuse is scoped to the synthesized name rather than matched against any
/// previous entry with the same `git+ref+path`: a brand-new edge (a child the
/// updated root did not previously reach) has no entry under its synth name, so
/// it resolves fresh at the current tip instead of silently inheriting a stale
/// pin left behind by a removed root — or one that belonged to another root.
///
/// Freshness cascade contract — deliberately scoped to the *whole walk*, not
/// per-parent:
///
/// * A bare `spm update` (no name) sets `ctx.refresh = true`, so every
///   transitive child is re-resolved to its latest commit.
/// * Any other sync (`install`, `add`, `sync`, or `spm update <name>`) leaves
///   `ctx.refresh = false`, so a child whose spec is unchanged reuses its
///   pinned commit — even when a moving `branch`/`tag` selector has since
///   advanced upstream.
///
/// In particular, `spm update <name>` re-resolves the *named direct root* to
/// latest and re-reads its nested manifest (so a newly-declared child is
/// resolved fresh, and a child whose pin the new manifest changed is
/// re-resolved because the ref no longer matches the reused entry), but a child
/// the root still requests by the *same* moving ref stays pinned. This
/// non-cascade is intentional: transitive children are deduplicated across
/// roots (a diamond shares one entry), so a single-name update cannot advance
/// one root's subtree without risking an unrelated root's dependency. The whole
/// transitive frontier advances only under a bare `spm update`. See
/// `docs/guide/transitive-dependencies.md`.
///
/// Under `--locked` (`ctx.locked`) a child without a reusable pin is never
/// resolved: `Ok(None)` is returned and the caller records the miss.
pub(super) fn resolve_child(
    spec: &SkillSpec,
    synth: &str,
    ctx: &Ctx,
) -> Result<Option<LockedSkill>> {
    if !ctx.refresh {
        if let Some(prev) = ctx.prev.get(synth) {
            if prev.drift_from(spec)?.is_none() {
                let commit = prev.commit.clone();
                return Ok(Some(LockedSkill {
                    git: spec.git.clone(),
                    reference: LockReference::from(spec.version()?),
                    store: StoreKey::derive(&spec.git, &commit),
                    path: spec.path.clone(),
                    commit,
                    resolved_tag: prev.resolved_tag.clone(),
                    bundled_skills: Vec::new(),
                    requested_by: Vec::new(),
                    also_satisfies: Vec::new(),
                }));
            }
        }
    }
    if ctx.locked {
        return Ok(None);
    }
    resolver::resolve(spec).map(Some)
}

/// Describe a transitive edge that `--locked` refused to resolve.
pub(super) fn lock_miss(
    synth: &str,
    declared: &str,
    parent: &str,
    spec: &SkillSpec,
    ctx: &Ctx,
) -> Result<String> {
    let what = match ctx.prev.get(synth) {
        Some(prev) => format!(
            "changed in ai.lock ({})",
            prev.drift_from(spec)?.unwrap_or_default()
        ),
        None => "missing from ai.lock".to_string(),
    };
    Ok(format!(
        "transitive skill `{synth}` (`{declared}` required by `{parent}`): {what}"
    ))
}

/// Sort the walk's lock misses into [`Output::lock_misses`] and
/// [`Output::uncertain_misses`].
pub(super) fn sort_misses(out: &mut Output, misses: Vec<Miss>, ctx: &Ctx) {
    let reached: BTreeSet<&str> = out.transitive.iter().map(|(n, _)| n.as_str()).collect();
    // A miss is only uncertain when an unverified subtree may own the pin:
    // the lock pins the same identity under a name the walk didn't reach, and
    // that pin covers the requested selector (as its own reference or as
    // `also_satisfies` evidence) exactly as the dedup would require.
    let unreached: Vec<&LockedSkill> = if out.unverified.is_empty() {
        Vec::new()
    } else {
        ctx.prev
            .iter()
            .filter(|(n, l)| !l.requested_by.is_empty() && !reached.contains(n.as_str()))
            .map(|(_, l)| l)
            .collect()
    };
    for m in misses {
        let covered = unreached.iter().any(|l| {
            key_of(&l.git, &l.path) == m.key
                && (l.reference == m.requested
                    || l.also_satisfies.iter().any(|s| s.reference == m.requested))
        });
        if covered && !ctx.prev.contains_key(m.name.as_str()) {
            out.uncertain_misses.push(m.name);
        } else {
            out.lock_misses.push((m.name, m.problem));
        }
    }
}

/// Stable committed lockfile: provenance lists must not churn with traversal
/// order.
pub(super) fn sort_provenance(out: &mut Output) {
    for (_, l) in &mut out.transitive {
        l.requested_by.sort();
        l.requested_by.dedup();
        l.also_satisfies.sort();
        l.also_satisfies.dedup();
    }
    for v in out.direct_also_satisfies.values_mut() {
        v.sort();
        v.dedup();
    }
}
