//! Transitive skill-dependency resolution.
//!
//! When the root project opts in (`resolveTransitive: true` in its `ai.json`),
//! every skill spm resolves may itself ship an `ai.json` co-located with its
//! content, declaring *further* skills. This module walks that graph: it fetches
//! each resolved skill into the store, runs the content scan **before** reading
//! its nested manifest, and recursively resolves the skills that manifest
//! declares — folding everything into the same flat, deduplicated list every
//! vendor consumes.
//!
//! Identity for cycle detection, dedup, and version-conflict detection is the
//! normalized `(git, path)` pair (see [`identity::normalize_git`]) — never the raw URL
//! string or the dependency-author-controlled skill name. Two structures guard
//! the walk:
//!
//! - a **DFS stack** of the identities on the current recursion branch, which
//!   catches a true cycle (A → B → A) and yields a readable chain; and
//! - a **global resolved map** keyed by the same identity, which lets a diamond
//!   (A → D, A → C → D) resolve `D` exactly once without mistaking it for a
//!   cycle, and detects a version conflict when the same identity would need two
//!   different commits.
//!
//! Direct roots are additionally **pre-seeded** into the resolved map before the
//! walk (see [`expand`]): every directly-declared skill is a fixed install
//! point, so a transitive back-edge whose identity lands on a root is satisfied
//! by that root and resolves as a diamond (dedup), not a cycle. The DFS stack is
//! therefore **not** the sole cycle authority — a cycle routed through a direct
//! root terminates at the pre-seeded root by design, and only a cycle wholly
//! among non-root (transitive) identities is reported as a hard `dependency
//! cycle` error.
//!
//! A transitively-resolved skill's materialized name is synthesized
//! deterministically as `{requester}__{declared}-{short_hash}` so it is stable
//! across runs (letting the sync reuse fast-path key on it), collision-resistant
//! across unrelated dependencies, and free of path separators.
//!
//! Layout:
//! - `identity`: the normalized `(git, path)` identity and the synthesized
//!   names derived from it.
//! - `walk`: [`expand`] and the DFS over nested `ai.json` files (cycle guard,
//!   dedup, caps, scan-before-recurse).
//! - `lock`: reusing `ai.lock` pins for transitive children, recording
//!   `--locked` misses and normalizing the lock provenance after the walk.
//! - `diagnostics`: rendering of requester chains and version conflicts.

mod diagnostics;
mod identity;
mod lock;
mod walk;

use crate::ids::DepName;
use crate::lockfile::{LockedSkill, Satisfied};
use crate::materialized::MaterializedSkill;
use std::collections::BTreeMap;

pub use walk::expand;

/// Hard cap on transitive recursion depth, independent of the opt-in flag. A
/// backstop against a pathological (or hostile) dependency graph: even with the
/// cycle guard, a very deep legitimate chain would fetch a repo per level, so we
/// refuse past this and tell the user which chain hit the limit.
pub const MAX_DEPTH: usize = 8;

/// Upper bound (bytes) on a synthesized transitive skill name. Names become
/// directory components, and common filesystems cap a component at 255 bytes;
/// this leaves headroom for suffixes added around the name.
pub const MAX_SYNTH_NAME_BYTES: usize = 200;

/// Hard cap on the total number of distinct transitive skills (nodes) resolved
/// across the whole walk, independent of depth. `MAX_DEPTH` only bounds the
/// length of a single chain; without a global budget a dependency whose
/// `ai.json` fans out to thousands of distinct children — each triggering a
/// remote resolution, checkout, scan, and materialization — could exhaust an
/// opted-in project's time/disk (a fan-out DoS). We refuse once the budget is
/// exceeded and name the edge that tipped it over.
pub const MAX_NODES: usize = 256;

/// Hard cap on the total number of transitive *edges* (nested dependency
/// declarations) examined across the whole walk. Bounds work that does not add a
/// new node but is still per-edge expensive — e.g. many edges onto one identity
/// pinned at *different* refs each force a fresh remote resolution to check for a
/// conflict. Kept comfortably above `MAX_NODES` so a legitimate diamond-heavy
/// graph is unaffected while a pathological fan-out still trips.
pub const MAX_EDGES: usize = 1024;

/// Inputs the walk needs from the surrounding sync.
pub struct Ctx<'a> {
    /// The previous lockfile's skill entries, consulted to reuse a pinned commit
    /// for an unchanged transitive child (avoiding an `ls-remote` every sync).
    pub prev: &'a BTreeMap<DepName, LockedSkill>,
    /// Whether transitive resolution is enabled (root manifest's flag).
    pub resolve_transitive: bool,
    /// Re-resolve transitive children to their latest commit. This is a single
    /// global flag, set only by a bare `spm update` (no name) — see the
    /// `resolve_child` doc for why a single-name `spm update <name>`
    /// deliberately leaves it `false` and does not cascade into the named
    /// root's transitive frontier.
    pub refresh: bool,
    /// `spm install --locked`: never resolve a transitive child. A child whose
    /// edge has no matching `ai.lock` pin is recorded in [`Output::lock_misses`]
    /// (and its subtree skipped) instead of being resolved.
    pub locked: bool,
    /// Read-only check (`spm doctor`, with `locked`): never fetch, scan or
    /// print. A skill whose checkout is not already in the store is recorded in
    /// [`Output::unverified`] and its subtree is not walked.
    pub check: bool,
    /// Column width for the aligned per-skill fetch output.
    pub width: usize,
    /// The effective source policy every nested edge must pass before it is
    /// resolved or fetched.
    pub policy: &'a crate::policy::Policy,
    /// `spm link` overrides for *direct* skills: materialized (and scanned) from
    /// the local directory instead of the store checkout. With transitive
    /// resolution on, the root's nested `ai.json` is still read from its locked
    /// checkout, so `ai.lock` is unaffected by the override.
    pub links: &'a BTreeMap<DepName, crate::links::Linked>,
}

/// Result of the walk: the flattened, deduplicated skill list to hand to the
/// vendors, plus the transitive lock entries to fold into `ai.lock`.
pub struct Output {
    pub materialized: Vec<MaterializedSkill>,
    /// Synthesized-name → locked entry for every transitively-resolved skill.
    pub transitive: Vec<(DepName, LockedSkill)>,
    /// `also_satisfies` evidence recorded for *direct* roots (by name): the
    /// other selectors nested edges requested for a root's identity that
    /// resolved to the root's commit. Transitive entries carry theirs in place.
    pub direct_also_satisfies: BTreeMap<DepName, Vec<Satisfied>>,
    /// `--locked` only: `(synthesized name, description)` for each transitive
    /// edge that would have needed a fresh resolution because `ai.lock` has no
    /// matching pin for it. That edge's subtree is not walked.
    pub lock_misses: Vec<(String, String)>,
    /// [`Ctx::check`] only: skills whose store checkout is missing, so they
    /// (and their nested dependencies) could not be verified offline.
    pub unverified: Vec<String>,
    /// [`Ctx::check`] only: synthesized names of misses that may be artifacts
    /// of an [`Output::unverified`] subtree. The lock has no entry under the
    /// miss's own name but pins the same identity under a name this walk never
    /// reached, so the unwalked subtree could claim it first, under that name.
    /// These are not in `lock_misses`.
    pub uncertain_misses: Vec<String>,
}
