//! The transitive walk: [`expand`] fetches, scans and materializes every direct
//! skill, then recursively follows the nested `ai.json` each one ships.

use super::diagnostics::{chain_str, conflict_error, ConflictSide};
use super::identity::{key_of, synth_name, Key};
use super::lock::{lock_miss, resolve_child, sort_misses, sort_provenance, Miss};
use super::{Ctx, Output, MAX_DEPTH, MAX_EDGES, MAX_NODES};
use crate::ids::{CommitSha, DepName, LockReference};
use crate::lockfile::{LockedSkill, Satisfied};
use crate::manifest::{DependencyManifest, Manifest};
use crate::materialized::MaterializedSkill;
use crate::store;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One resolved identity in the global dedup/conflict map.
struct Resolved {
    commit: CommitSha,
    reference: LockReference,
    /// Names, from a direct root down to this node, at first resolution — used to
    /// render a readable requester chain in a conflict error.
    chain: Vec<String>,
    /// Index into [`Output::transitive`] for a transitive node, so a later
    /// diamond edge can append its requester to `requested_by`. `None` for a
    /// directly-declared skill (those keep an empty `requested_by`).
    out_idx: Option<usize>,
}

/// Mutable state threaded through the recursive [`visit`] walk: the DFS stack of
/// identities on the current branch, the global dedup/conflict map, and the
/// growing output. Bundled into one value so the traversal function stays within
/// a sane argument count (and reads as "walk state + this node") instead of
/// passing each accumulator by hand.
struct Walk {
    stack: Vec<(Key, String)>,
    resolved: std::collections::HashMap<Key, Resolved>,
    out: Output,
    /// Every lock miss, sorted into [`Output::lock_misses`] and
    /// [`Output::uncertain_misses`] after the walk.
    misses: Vec<Miss>,
    /// Total transitive edges examined across the whole walk, capped by
    /// [`MAX_EDGES`] to bound per-edge work that does not create a new node.
    edges: usize,
}

/// Locate this skill's co-located nested `ai.json`, requiring it to resolve
/// **inside** the checkout boundary (`root`).
///
/// The content scanner (`scan::enforce`) deliberately skips a symlink whose
/// target escapes `root` (or reaches into `.git`) — so a hostile repository
/// could hide its nested manifest behind such a link, have it skipped by the
/// scan, and yet still have it read here, bypassing the scan-before-recurse
/// guarantee and triggering arbitrary dependency fetches. Reuse the single
/// boundary-aware resolver (`fsutil::resolve_within_canonical`, shared with the
/// copy/scan traversals) so the manifest we read is exactly the content the scan
/// covered. Returns `None` when there is simply no nested manifest; errors when
/// one exists but escapes the boundary.
fn nested_manifest_path(content: &Path, root: &Path) -> Result<Option<PathBuf>> {
    let nested = Manifest::path_in(content);
    // `exists()` follows symlinks: a broken/absent link reads as "no manifest".
    if !nested.exists() {
        return Ok(None);
    }
    match crate::fsutil::resolve_within_canonical(root, &nested) {
        Some(p) => Ok(Some(p)),
        None => bail!(
            "nested {} at {} resolves outside the skill checkout (a symlink escaping the \
             checkout boundary, or into `.git`); refusing to read it — it would bypass the \
             content scan that guards transitive resolution",
            crate::manifest::MANIFEST_FILE,
            nested.display()
        ),
    }
}

/// Fetch, security-scan, and materialize every direct skill, then (when opted
/// in) recursively resolve and materialize the skills their nested `ai.json`
/// files declare.
pub fn expand(direct: &BTreeMap<DepName, LockedSkill>, ctx: &Ctx) -> Result<Output> {
    let mut resolved: std::collections::HashMap<Key, Resolved> = std::collections::HashMap::new();

    // Seed the dedup/conflict map with the direct skills. Two direct skills that
    // share an identity but pin different commits are a genuine conflict once
    // transitive resolution is in play (the same skill would resolve two ways).
    // Only relevant when opted in — with the flag off the map is never consulted
    // (a leaf `visit` returns before recursing), so behavior is exactly as before.
    //
    // Pre-seeding also fixes each direct root as an install point: a later
    // transitive edge whose identity matches a root dedups against this
    // pre-seeded entry (a diamond) instead of tripping the DFS-stack cycle guard
    // — a direct root legitimately satisfies and terminates a back-edge to
    // itself. Consequently only a cycle wholly among non-root transitive
    // identities reaches the stack guard and hard-errors.
    if ctx.resolve_transitive {
        for (name, locked) in direct {
            let key = key_of(&locked.git, &locked.path);
            if let Some(existing) = resolved.get(&key) {
                if existing.commit != locked.commit {
                    return Err(conflict_error(
                        &key,
                        ConflictSide {
                            reference: &existing.reference,
                            commit: &existing.commit,
                            chain: &existing.chain,
                        },
                        ConflictSide {
                            reference: &locked.reference,
                            commit: &locked.commit,
                            chain: &[name.to_string()],
                        },
                    ));
                }
            } else {
                resolved.insert(
                    key,
                    Resolved {
                        commit: locked.commit.clone(),
                        reference: locked.reference.clone(),
                        chain: vec![name.to_string()],
                        out_idx: None,
                    },
                );
            }
        }
    }

    // Walk each direct skill as a DFS root. BTreeMap iteration is sorted, so the
    // traversal order — and therefore every synthesized name and provenance
    // chain — is deterministic across runs. The DFS stack is balanced (every
    // push is popped) by the time a root returns, so one `Walk` is reused across
    // roots, carrying the shared dedup map and accumulated output.
    let mut w = Walk {
        stack: Vec::new(),
        resolved,
        out: Output {
            materialized: Vec::new(),
            transitive: Vec::new(),
            direct_also_satisfies: BTreeMap::new(),
            lock_misses: Vec::new(),
            unverified: Vec::new(),
            uncertain_misses: Vec::new(),
        },
        misses: Vec::new(),
        edges: 0,
    };
    for (name, locked) in direct {
        visit(&mut w, name, locked, 0, direct, ctx)?;
    }

    let mut out = w.out;
    sort_misses(&mut out, w.misses, ctx);
    sort_provenance(&mut out);
    Ok(out)
}

fn visit(
    w: &mut Walk,
    name: &DepName,
    locked: &LockedSkill,
    depth: usize,
    direct: &BTreeMap<DepName, LockedSkill>,
    ctx: &Ctx,
) -> Result<()> {
    let width = ctx.width;
    let link = if depth == 0 {
        ctx.links.get(name)
    } else {
        None
    };
    if let Some(l) = link {
        println!("  {name:<width$}  linked → {}", l.display);
        if !l.content.join("SKILL.md").is_file() {
            eprintln!(
                "warning: linked skill `{name}` has no SKILL.md at the root of `{}` — agents \
                 may ignore it (link the skill directory itself)",
                l.display
            );
        }
        crate::scan::enforce(name, &l.content, &l.content)
            .with_context(|| format!("scanning linked skill `{name}`"))?;
        w.out.materialized.push(MaterializedSkill {
            name: name.clone(),
            path: l.content.clone(),
            root: l.content.clone(),
        });
        // Without transitive resolution a linked root needs nothing from the
        // store at all.
        if !ctx.resolve_transitive {
            return Ok(());
        }
    }

    // Fetch into the store and run the pre-materialize security gate BEFORE
    // reading this node's nested manifest — a blocked node aborts before any of
    // its subtree is ever fetched.
    let ensured = if ctx.check {
        match store::cached(locked).with_context(|| format!("inspecting skill `{name}`"))? {
            Some(ensured) => ensured,
            None => {
                w.out.unverified.push(name.to_string());
                return Ok(());
            }
        }
    } else {
        let ensured = store::ensure(locked).with_context(|| format!("fetching skill `{name}`"))?;
        println!(
            "  {name:<width$}  {}",
            if ensured.fetched { "fetched" } else { "cached" }
        );
        ensured
    };
    if link.is_none() && !ctx.check {
        crate::skillcheck::warn_if_not_loadable(
            name,
            &locked.git,
            &locked.reference.label(),
            locked.path.as_deref(),
            &ensured.path,
            &ensured.root,
        );
    }
    // A linked root's locked checkout is still the source of its nested
    // manifest, so it passes the gate too before its subtree is walked. A check
    // materializes and fetches nothing, so it has nothing to gate.
    if !ctx.check {
        crate::scan::enforce(name, &ensured.path, &ensured.root)
            .with_context(|| format!("scanning skill `{name}`"))?;
    }
    if link.is_none() {
        w.out.materialized.push(MaterializedSkill {
            name: name.clone(),
            path: ensured.path.clone(),
            root: ensured.root.clone(),
        });
    }

    if !ctx.resolve_transitive {
        return Ok(());
    }

    // Only the `ai.json` co-located with this skill's own content is read — never
    // a fallback to a monorepo's repo-root manifest for a subdir-pinned skill.
    // Require it to resolve inside the checkout boundary so a symlinked manifest
    // the scanner skipped cannot smuggle in unscanned transitive dependencies.
    let Some(manifest_path) = nested_manifest_path(&ensured.path, &ensured.root)? else {
        return Ok(());
    };
    // Read the boundary-checked, symlink-resolved path itself — not
    // `ensured.path/ai.json` re-derived — so a symlink swapped in after the
    // check cannot redirect the load to an outside, unscanned manifest.
    let dep = DependencyManifest::load_file(&manifest_path)
        .with_context(|| format!("reading nested dependencies of `{name}`"))?;
    if dep.skills.is_empty() {
        return Ok(());
    }
    if depth >= MAX_DEPTH {
        let mut chain: Vec<String> = w.stack.iter().map(|(_, n)| n.clone()).collect();
        chain.push(name.to_string());
        bail!(
            "transitive dependency depth cap ({MAX_DEPTH}) exceeded at: {}\n\
             a dependency chain this deep is almost certainly a mistake; \
             flatten it or reduce nesting",
            chain_str(&chain)
        );
    }

    let self_key = key_of(&locked.git, &locked.path);
    w.stack.push((self_key, name.to_string()));

    for (declared, spec) in &dep.skills {
        // Edge budget: count every nested declaration examined, and refuse once
        // the whole-walk cap is hit — before any further remote resolution — so
        // a huge fan-out (even one that only re-touches existing identities)
        // cannot run unbounded per-edge work.
        w.edges += 1;
        if w.edges > MAX_EDGES {
            let mut chain: Vec<String> = w.stack.iter().map(|(_, n)| n.clone()).collect();
            chain.push(declared.to_string());
            bail!(
                "transitive dependency edge cap ({MAX_EDGES}) exceeded at: {}\n\
                 a dependency graph this wide is almost certainly a mistake (or hostile); \
                 reduce the fan-out of nested `ai.json` dependencies",
                chain_str(&chain)
            );
        }
        // Source policy: refuse a nested source outside the policy before any
        // resolution or fetch, naming the chain of requesters that led here.
        if let Err(e) = ctx.policy.check(&spec.git) {
            let mut chain: Vec<String> = w.stack.iter().map(|(_, n)| n.clone()).collect();
            chain.push(declared.to_string());
            return Err(e.context(format!(
                "nested skill `{declared}` required by `{name}` (via {})",
                chain_str(&chain)
            )));
        }
        spec.version()
            .with_context(|| format!("nested skill `{declared}` required by `{name}`"))?;
        let ckey = key_of(&spec.git, &spec.path);
        // Deterministic materialized name for *this* edge. Computed up front so
        // both the version-conflict re-resolve and the fresh-node branch key
        // lockfile reuse on the same stable identity (see `resolve_child`).
        let synth = synth_name(name, declared, &ckey);

        // Cycle: the child identity is an ancestor still on the current branch.
        // A back-edge whose identity is on the active DFS branch is a cycle —
        // *unless* it lands on a pre-seeded direct root. A direct root is a
        // fixed install point that legitimately satisfies and terminates a
        // back-edge to itself (a diamond), even while it is still on the current
        // branch; the dedup branch below records the provenance. Only a
        // back-edge onto an active *transitive* node (one with a real
        // `out_idx`) is a genuine self-dependency. Checking `out_idx.is_none()`
        // on the resolved entry distinguishes the two, since only pre-seeded
        // roots carry `out_idx: None`.
        let on_branch = w.stack.iter().position(|(k, _)| k == &ckey);
        let lands_on_direct_root = w.resolved.get(&ckey).is_some_and(|r| r.out_idx.is_none());
        if let Some(pos) = on_branch.filter(|_| !lands_on_direct_root) {
            let mut chain: Vec<String> = w.stack[pos..].iter().map(|(_, n)| n.clone()).collect();
            chain.push(format!("{declared} (= {})", w.stack[pos].1));
            bail!(
                "dependency cycle detected: {}\n\
                 `{}` (git `{}`{}) transitively depends on itself",
                chain_str(&chain),
                declared,
                spec.git,
                spec.path
                    .as_deref()
                    .map(|p| format!(", path `{p}`"))
                    .unwrap_or_default()
            );
        }

        // Already resolved elsewhere in the graph: a diamond (dedup) or a
        // version conflict.
        if let Some(existing) = w.resolved.get(&ckey) {
            // Reuse the already-resolved commit when this edge requests the same
            // reference the node was resolved at: the identity + reference match,
            // so re-resolving would only repeat a remote lookup — and worse, a
            // moving ref (branch, or a retagged tag) could resolve to a different
            // commit the second time and report a *phantom* version conflict for
            // what is really one shared node. Only when a genuinely different
            // reference is requested do we resolve again, to detect a real conflict.
            //
            // The deduplicated graph keeps a single lock entry per identity, so a
            // differing selector has no pin of its own. Instead, the entry records
            // in `also_satisfies` every other selector verified to resolve to its
            // commit; that evidence is reused (unless refreshing) so this edge is
            // re-checked without a remote lookup — and `install --locked` accepts
            // a lock that plain `install` produced.
            let requested = LockReference::from(spec.version()?);
            if requested != existing.reference {
                let pinned = match existing.out_idx {
                    Some(idx) => ctx.prev.get(&w.out.transitive[idx].0),
                    None => direct.get(existing.chain[0].as_str()),
                };
                let evidence = pinned
                    .filter(|p| !ctx.refresh && p.commit == existing.commit)
                    .and_then(|p| p.also_satisfies.iter().find(|s| s.reference == requested));
                let (child_commit, resolved_tag) = if let Some(e) = evidence {
                    (existing.commit.clone(), e.resolved_tag.clone())
                } else {
                    let Some(child) = resolve_child(spec, &synth, ctx).with_context(|| {
                        format!("resolving nested skill `{declared}` of `{name}`")
                    })?
                    else {
                        w.misses.push(Miss {
                            key: ckey.clone(),
                            name: synth.clone(),
                            requested: requested.clone(),
                            problem: lock_miss(&synth, declared, name, spec, ctx)?,
                        });
                        continue;
                    };
                    (child.commit, child.resolved_tag)
                };
                if existing.commit != child_commit {
                    let mut cur_chain: Vec<String> =
                        w.stack.iter().map(|(_, n)| n.clone()).collect();
                    cur_chain.push(declared.to_string());
                    return Err(conflict_error(
                        &ckey,
                        ConflictSide {
                            reference: &existing.reference,
                            commit: &existing.commit,
                            chain: &existing.chain,
                        },
                        ConflictSide {
                            reference: &requested,
                            commit: &child_commit,
                            chain: &cur_chain,
                        },
                    ));
                }
                let requested = Satisfied {
                    reference: requested,
                    resolved_tag,
                };
                match existing.out_idx {
                    Some(idx) => w.out.transitive[idx].1.also_satisfies.push(requested),
                    None => w
                        .out
                        .direct_also_satisfies
                        .entry(DepName::try_from(&existing.chain[0])?)
                        .or_default()
                        .push(requested),
                }
            }
            // Same identity, no conflict: a diamond. This also covers an edge
            // that lands on a *pre-seeded direct root*: the root is a fixed
            // install point that satisfies the back-edge, so it dedups here
            // rather than tripping the DFS-stack cycle guard above. Record this
            // requester on the shared transitive entry (directs keep an empty
            // requested_by — a root is pre-seeded with `out_idx: None`, so a
            // root diamond appends nothing).
            if let Some(idx) = existing.out_idx {
                w.out.transitive[idx].1.requested_by.push(name.clone());
            }
            continue;
        }

        // Fresh node: validate the (already-computed) synthesized name *before*
        // resolving, so a collision or bad name fails fast without a needless
        // remote lookup. The synthesized name also scopes lockfile reuse to this
        // exact edge (see `resolve_child`).
        let synth = DepName::try_from(synth)
            .with_context(|| format!("synthesized name for nested skill `{declared}`"))?;
        // Node budget: a fresh node is the expensive unit (remote resolve,
        // checkout, scan, materialize). Refuse before resolving it once the
        // whole-walk cap is hit, so a wide fan-out cannot exhaust time/disk.
        if w.out.transitive.len() >= MAX_NODES {
            let mut chain: Vec<String> = w.stack.iter().map(|(_, n)| n.clone()).collect();
            chain.push(declared.to_string());
            bail!(
                "transitive dependency node cap ({MAX_NODES}) exceeded at: {}\n\
                 a dependency graph this large is almost certainly a mistake (or hostile); \
                 reduce the number of nested `ai.json` dependencies",
                chain_str(&chain)
            );
        }
        if direct.contains_key(&synth) || w.out.transitive.iter().any(|(n, _)| n == &synth) {
            bail!(
                "transitive skill name collision: `{synth}` (from `{declared}` required by \
                 `{name}`) already names another skill — pin one of the conflicting \
                 dependencies to a version that does not ship it"
            );
        }
        let Some(mut child) = resolve_child(spec, &synth, ctx)
            .with_context(|| format!("resolving nested skill `{declared}` of `{name}`"))?
        else {
            w.misses.push(Miss {
                key: ckey,
                name: synth.to_string(),
                requested: LockReference::from(spec.version()?),
                problem: lock_miss(&synth, declared, name, spec, ctx)?,
            });
            continue;
        };
        child.requested_by = vec![name.clone()];

        let mut child_chain: Vec<String> = w.stack.iter().map(|(_, n)| n.clone()).collect();
        child_chain.push(synth.to_string());
        let idx = w.out.transitive.len();
        w.resolved.insert(
            ckey,
            Resolved {
                commit: child.commit.clone(),
                reference: child.reference.clone(),
                chain: child_chain,
                out_idx: Some(idx),
            },
        );
        w.out.transitive.push((synth.clone(), child.clone()));
        visit(w, &synth, &child, depth + 1, direct, ctx)?;
    }

    w.stack.pop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A nested `ai.json` that is a symlink escaping the checkout boundary must
    /// be refused — the scanner skips such a link, so reading it here would
    /// bypass the scan-before-recurse guarantee.
    #[cfg(unix)]
    #[test]
    fn nested_manifest_path_rejects_symlink_escaping_boundary() {
        use std::os::unix::fs::symlink;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "spm-transitive-nested-escape-{}-{nanos}",
            std::process::id()
        ));
        let checkout = base.join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        // A real manifest living outside the checkout — the exfiltration target.
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("ai.json"), r#"{"skills":{}}"#).unwrap();
        // Inside the checkout, `ai.json` is a symlink to the outside manifest.
        symlink(outside.join("ai.json"), checkout.join("ai.json")).unwrap();

        let root = std::fs::canonicalize(&checkout).unwrap();
        let err = nested_manifest_path(&root, &root).unwrap_err();
        assert!(
            format!("{err:#}").contains("outside the skill checkout"),
            "{err:#}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// A regular, in-boundary nested `ai.json` is accepted, and a checkout with
    /// no manifest reads as "no transitive deps".
    #[test]
    fn nested_manifest_path_accepts_in_boundary_and_absent() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let checkout = std::env::temp_dir().join(format!(
            "spm-transitive-nested-ok-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&checkout).unwrap();
        let root = std::fs::canonicalize(&checkout).unwrap();
        assert_eq!(nested_manifest_path(&root, &root).unwrap(), None);

        std::fs::write(root.join("ai.json"), r#"{"skills":{}}"#).unwrap();
        assert!(nested_manifest_path(&root, &root).unwrap().is_some());
        std::fs::remove_dir_all(&checkout).ok();
    }
}
