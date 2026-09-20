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
//! normalized `(git, path)` pair (see [`normalize_git`]) — never the raw URL
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
//! A transitively-resolved skill's materialized name is synthesized
//! deterministically as `{requester}__{declared}-{short_hash}` so it is stable
//! across runs (letting the sync reuse fast-path key on it), collision-resistant
//! across unrelated dependencies, and free of path separators.

use crate::lockfile::{store_key, LockedSkill};
use crate::manifest::{validate_skill_name, DependencyManifest, Manifest, SkillSpec};
use crate::resolver;
use crate::store;
use crate::vendor::MaterializedSkill;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Hard cap on transitive recursion depth, independent of the opt-in flag. A
/// backstop against a pathological (or hostile) dependency graph: even with the
/// cycle guard, a very deep legitimate chain would fetch a repo per level, so we
/// refuse past this and tell the user which chain hit the limit.
pub const MAX_DEPTH: usize = 8;

/// Identity of a resolvable skill for transitive bookkeeping: the normalized git
/// URL plus the optional in-repo subpath. Two entries with the same git but a
/// different `path` are different skills (the monorepo-of-skills layout) and
/// never conflict.
type Key = (String, Option<String>);

/// Inputs the walk needs from the surrounding sync.
pub struct Ctx<'a> {
    /// The previous lockfile's skill entries, consulted to reuse a pinned commit
    /// for an unchanged transitive child (avoiding an `ls-remote` every sync).
    pub prev: &'a BTreeMap<String, LockedSkill>,
    /// Whether transitive resolution is enabled (root manifest's flag).
    pub resolve_transitive: bool,
    /// Re-resolve transitive children to their latest commit (a full `update`).
    pub refresh: bool,
    /// Column width for the aligned per-skill fetch output.
    pub width: usize,
}

/// Result of the walk: the flattened, deduplicated skill list to hand to the
/// vendors, plus the transitive lock entries to fold into `ai.lock`.
pub struct Output {
    pub materialized: Vec<MaterializedSkill>,
    /// Synthesized-name → locked entry for every transitively-resolved skill.
    pub transitive: Vec<(String, LockedSkill)>,
}

/// One resolved identity in the global dedup/conflict map.
struct Resolved {
    commit: String,
    reference: String,
    /// Names, from a direct root down to this node, at first resolution — used to
    /// render a readable requester chain in a conflict error.
    chain: Vec<String>,
    /// Index into [`Output::transitive`] for a transitive node, so a later
    /// diamond edge can append its requester to `requested_by`. `None` for a
    /// directly-declared skill (those keep an empty `requested_by`).
    out_idx: Option<usize>,
}

/// Normalize a git URL for transitive identity purposes only (cycle stack,
/// dedup map, conflict key, and the name-synthesis hash). This intentionally
/// does **not** feed [`store_key`]/store dedup — that is an orthogonal,
/// pre-existing behavior. The normalization: strip a trailing `/` and `.git`,
/// then lowercase the scheme + authority while preserving the path's case
/// (paths are case-sensitive on the server; hosts are not).
pub fn normalize_git(url: &str) -> String {
    let mut s = url.trim();
    s = s.trim_end_matches('/');
    s = s.strip_suffix(".git").unwrap_or(s);
    s = s.trim_end_matches('/');

    if let Some(idx) = s.find("://") {
        // scheme://authority/path
        let scheme = &s[..idx];
        let rest = &s[idx + 3..];
        let (authority, path) = match rest.find('/') {
            Some(p) => (&rest[..p], &rest[p..]),
            None => (rest, ""),
        };
        format!(
            "{}://{}{}",
            scheme.to_ascii_lowercase(),
            authority.to_ascii_lowercase(),
            path
        )
    } else if let Some(idx) = s.find(':') {
        // scp-style `user@host:path` (no scheme). Only treat as scp when the
        // colon precedes the first slash — otherwise it's a plain path.
        let before_colon = &s[..idx];
        if !before_colon.contains('/') {
            let authority = before_colon;
            let path = &s[idx..];
            format!("{}{}", authority.to_ascii_lowercase(), path)
        } else {
            s.to_string()
        }
    } else {
        s.to_string()
    }
}

/// Canonicalize a validated subpath into a stable lexical identity string.
///
/// `validate_subpath` (run before any spec reaches here) already rejects
/// absolute paths and `..`, but it *permits* `.` components and repeated or
/// trailing separators — so `skills/x`, `skills/./x`, `skills//x`, and
/// `skills/x/` all name the same content while producing different raw strings.
/// Using the raw string as the transitive identity would let those spellings
/// dodge dedup/version-conflict detection and materialize the same skill twice.
/// Fold every root-equivalent form (`.`, empty, `None`) to `None` and join the
/// remaining `Normal` components with `/` so identity is spelling-independent.
fn canonical_subpath(path: &Option<String>) -> Option<String> {
    let raw = path.as_deref()?;
    let parts: Vec<std::borrow::Cow<str>> = Path::new(raw)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy()),
            // CurDir dropped; validate_subpath already rejected the rest.
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn key_of(git: &str, path: &Option<String>) -> Key {
    (normalize_git(git), canonical_subpath(path))
}

/// Short, stable hex hash of a skill's normalized identity, for name synthesis.
fn short_hash(key: &Key) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let feed = |h: &mut u64, bytes: &[u8]| {
        for b in bytes {
            *h ^= *b as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(&mut h, key.0.as_bytes());
    feed(&mut h, b"\0");
    feed(&mut h, key.1.as_deref().unwrap_or("").as_bytes());
    format!("{:08x}", h & 0xffff_ffff)
}

/// Synthesize a transitive skill's materialized name:
/// `{requester}__{declared}-{short_hash}`.
fn synth_name(requester: &str, declared: &str, key: &Key) -> String {
    format!("{requester}__{declared}-{}", short_hash(key))
}

fn chain_str(chain: &[String]) -> String {
    chain.join(" -> ")
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
pub fn expand(direct: &BTreeMap<String, LockedSkill>, ctx: &Ctx) -> Result<Output> {
    let mut out = Output {
        materialized: Vec::new(),
        transitive: Vec::new(),
    };
    let mut resolved: std::collections::HashMap<Key, Resolved> = std::collections::HashMap::new();

    // Seed the dedup/conflict map with the direct skills. Two direct skills that
    // share an identity but pin different commits are a genuine conflict once
    // transitive resolution is in play (the same skill would resolve two ways).
    // Only relevant when opted in — with the flag off the map is never consulted
    // (a leaf `visit` returns before recursing), so behavior is exactly as before.
    if ctx.resolve_transitive {
        for (name, locked) in direct {
            let key = key_of(&locked.git, &locked.path);
            if let Some(existing) = resolved.get(&key) {
                if existing.commit != locked.commit {
                    return Err(conflict_error(
                        &key,
                        &existing.reference,
                        &existing.commit,
                        &existing.chain,
                        &locked.reference,
                        &locked.commit,
                        std::slice::from_ref(name),
                    ));
                }
            } else {
                resolved.insert(
                    key,
                    Resolved {
                        commit: locked.commit.clone(),
                        reference: locked.reference.clone(),
                        chain: vec![name.clone()],
                        out_idx: None,
                    },
                );
            }
        }
    }

    // Walk each direct skill as a DFS root. BTreeMap iteration is sorted, so the
    // traversal order — and therefore every synthesized name and provenance
    // chain — is deterministic across runs.
    for (name, locked) in direct {
        let mut stack: Vec<(Key, String)> = Vec::new();
        visit(
            name,
            locked,
            0,
            &mut stack,
            &mut resolved,
            &mut out,
            direct,
            ctx,
        )?;
    }

    // Stable committed lockfile: provenance lists must not churn with traversal
    // order.
    for (_, l) in &mut out.transitive {
        l.requested_by.sort();
        l.requested_by.dedup();
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn visit(
    name: &str,
    locked: &LockedSkill,
    depth: usize,
    stack: &mut Vec<(Key, String)>,
    resolved: &mut std::collections::HashMap<Key, Resolved>,
    out: &mut Output,
    direct: &BTreeMap<String, LockedSkill>,
    ctx: &Ctx,
) -> Result<()> {
    // Fetch into the store and run the pre-materialize security gate BEFORE
    // reading this node's nested manifest — a blocked node aborts before any of
    // its subtree is ever fetched.
    let ensured = store::ensure(locked).with_context(|| format!("fetching skill `{name}`"))?;
    let width = ctx.width;
    println!(
        "  {name:<width$}  {}",
        if ensured.fetched { "fetched" } else { "cached" }
    );
    crate::skillcheck::warn_if_not_loadable(
        name,
        &locked.git,
        &locked.reference,
        locked.path.as_deref(),
        &ensured.path,
        &ensured.root,
    );
    crate::scan::enforce(name, &ensured.path, &ensured.root)
        .with_context(|| format!("scanning skill `{name}`"))?;
    out.materialized.push(MaterializedSkill {
        name: name.to_string(),
        path: ensured.path.clone(),
        root: ensured.root.clone(),
    });

    if !ctx.resolve_transitive {
        return Ok(());
    }

    // Only the `ai.json` co-located with this skill's own content is read — never
    // a fallback to a monorepo's repo-root manifest for a subdir-pinned skill.
    // Require it to resolve inside the checkout boundary so a symlinked manifest
    // the scanner skipped cannot smuggle in unscanned transitive dependencies.
    if nested_manifest_path(&ensured.path, &ensured.root)?.is_none() {
        return Ok(());
    }
    let dep = DependencyManifest::load(&ensured.path)
        .with_context(|| format!("reading nested dependencies of `{name}`"))?;
    if dep.skills.is_empty() {
        return Ok(());
    }
    if depth >= MAX_DEPTH {
        let mut chain: Vec<String> = stack.iter().map(|(_, n)| n.clone()).collect();
        chain.push(name.to_string());
        bail!(
            "transitive dependency depth cap ({MAX_DEPTH}) exceeded at: {}\n\
             a dependency chain this deep is almost certainly a mistake; \
             flatten it or reduce nesting",
            chain_str(&chain)
        );
    }

    let self_key = key_of(&locked.git, &locked.path);
    stack.push((self_key, name.to_string()));

    for (declared, spec) in &dep.skills {
        spec.version()
            .with_context(|| format!("nested skill `{declared}` required by `{name}`"))?;
        let ckey = key_of(&spec.git, &spec.path);

        // Cycle: the child identity is an ancestor still on the current branch.
        if let Some(pos) = stack.iter().position(|(k, _)| k == &ckey) {
            let mut chain: Vec<String> = stack[pos..].iter().map(|(_, n)| n.clone()).collect();
            chain.push(format!("{declared} (= {})", stack[pos].1));
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
        if let Some(existing) = resolved.get(&ckey) {
            // Reuse the already-resolved commit when this edge requests the same
            // reference the node was resolved at: the identity + reference match,
            // so re-resolving would only repeat a remote lookup — and worse, a
            // moving ref (branch, or a retagged tag) could resolve to a different
            // commit the second time and report a *phantom* version conflict for
            // what is really one shared node. Only when a genuinely different
            // reference is requested do we resolve again, to detect a real conflict.
            let requested = spec.version()?.label();
            if requested != existing.reference {
                let child = resolve_child(spec, ctx)
                    .with_context(|| format!("resolving nested skill `{declared}` of `{name}`"))?;
                if existing.commit != child.commit {
                    let mut cur_chain: Vec<String> = stack.iter().map(|(_, n)| n.clone()).collect();
                    cur_chain.push(declared.clone());
                    return Err(conflict_error(
                        &ckey,
                        &existing.reference,
                        &existing.commit,
                        &existing.chain,
                        &child.reference,
                        &child.commit,
                        &cur_chain,
                    ));
                }
            }
            // Same identity, no conflict: a diamond. Record this requester on
            // the shared transitive entry (directs keep an empty requested_by).
            if let Some(idx) = existing.out_idx {
                out.transitive[idx].1.requested_by.push(name.to_string());
            }
            continue;
        }

        // Fresh node: resolve, synthesize a stable name, and recurse.
        let mut child = resolve_child(spec, ctx)
            .with_context(|| format!("resolving nested skill `{declared}` of `{name}`"))?;
        let synth = synth_name(name, declared, &ckey);
        validate_skill_name(&synth)
            .with_context(|| format!("synthesized name for nested skill `{declared}`"))?;
        if direct.contains_key(&synth) || out.transitive.iter().any(|(n, _)| n == &synth) {
            bail!(
                "transitive skill name collision: `{synth}` (from `{declared}` required by \
                 `{name}`) already names another skill — pin one of the conflicting \
                 dependencies to a version that does not ship it"
            );
        }
        child.requested_by = vec![name.to_string()];

        let mut child_chain: Vec<String> = stack.iter().map(|(_, n)| n.clone()).collect();
        child_chain.push(synth.clone());
        let idx = out.transitive.len();
        resolved.insert(
            ckey,
            Resolved {
                commit: child.commit.clone(),
                reference: child.reference.clone(),
                chain: child_chain,
                out_idx: Some(idx),
            },
        );
        out.transitive.push((synth.clone(), child.clone()));
        visit(&synth, &child, depth + 1, stack, resolved, out, direct, ctx)?;
    }

    stack.pop();
    Ok(())
}

/// Resolve a nested skill spec to a locked entry, reusing the previous
/// lockfile's pinned commit when an unchanged (same git+ref+path) entry exists
/// and we are not refreshing — so an unchanged transitive branch dependency is
/// not re-`ls-remote`d every sync. Freshness cascades from parent to child: a
/// child is re-resolved only when the parent was (its nested manifest reread) or
/// on an explicit `update`.
fn resolve_child(spec: &SkillSpec, ctx: &Ctx) -> Result<LockedSkill> {
    let reference = spec.version()?.label();
    if !ctx.refresh {
        if let Some(prev) = ctx
            .prev
            .values()
            .find(|l| l.git == spec.git && l.reference == reference && l.path == spec.path)
        {
            let commit = prev.commit.clone();
            return Ok(LockedSkill {
                git: spec.git.clone(),
                reference,
                store: store_key(&spec.git, &commit),
                path: spec.path.clone(),
                commit,
                bundled_skills: Vec::new(),
                requested_by: Vec::new(),
            });
        }
    }
    resolver::resolve(spec)
}

#[allow(clippy::too_many_arguments)]
fn conflict_error(
    key: &Key,
    ref_a: &str,
    commit_a: &str,
    chain_a: &[String],
    ref_b: &str,
    commit_b: &str,
    chain_b: &[String],
) -> anyhow::Error {
    let short = |c: &str| c[..c.len().min(8)].to_string();
    let path_note = key
        .1
        .as_deref()
        .map(|p| format!(" (path `{p}`)"))
        .unwrap_or_default();
    anyhow::anyhow!(
        "version conflict: `{}`{} is required at two different commits:\n  \
         {ref_a} @ {} (via {})\n  {ref_b} @ {} (via {})\n\
         resolve it by pinning both requesters to the same ref, or removing one dependency",
        key.0,
        path_note,
        short(commit_a),
        chain_str(chain_a),
        short(commit_b),
        chain_str(chain_b),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_git_suffix_and_trailing_slash() {
        assert_eq!(
            normalize_git("https://github.com/Org/Repo.git/"),
            "https://github.com/Org/Repo"
        );
        assert_eq!(
            normalize_git("https://github.com/Org/Repo"),
            "https://github.com/Org/Repo"
        );
    }

    #[test]
    fn normalize_lowercases_scheme_and_host_but_not_path() {
        assert_eq!(
            normalize_git("HTTPS://GitHub.COM/Org/Repo"),
            "https://github.com/Org/Repo"
        );
    }

    #[test]
    fn normalize_handles_scp_style() {
        assert_eq!(
            normalize_git("git@GitHub.com:Org/Repo.git"),
            "git@github.com:Org/Repo"
        );
    }

    #[test]
    fn normalize_treats_three_spellings_as_one_identity() {
        let a = normalize_git("https://github.com/o/r");
        let b = normalize_git("https://github.com/o/r.git");
        let c = normalize_git("https://GitHub.com/o/r/");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn normalize_leaves_local_path_case() {
        assert_eq!(normalize_git("/home/User/Repo"), "/home/User/Repo");
        assert_eq!(
            normalize_git("file:///home/User/Repo"),
            "file:///home/User/Repo"
        );
    }

    #[test]
    fn synth_name_is_stable_and_separator_free() {
        let key = (
            "https://github.com/o/r".to_string(),
            Some("skills/x".to_string()),
        );
        let a = synth_name("foo", "helper", &key);
        let b = synth_name("foo", "helper", &key);
        assert_eq!(a, b, "deterministic");
        assert!(
            validate_skill_name(&a).is_ok(),
            "must be a valid skill name: {a}"
        );
        assert!(a.starts_with("foo__helper-"));
    }

    #[test]
    fn different_identities_hash_differently() {
        let k1 = ("https://github.com/o/r".to_string(), None);
        let k2 = ("https://github.com/o/other".to_string(), None);
        assert_ne!(short_hash(&k1), short_hash(&k2));
    }

    #[test]
    fn canonical_subpath_folds_equivalent_spellings() {
        let want = Some("skills/x".to_string());
        assert_eq!(canonical_subpath(&Some("skills/x".into())), want);
        assert_eq!(canonical_subpath(&Some("skills/./x".into())), want);
        assert_eq!(canonical_subpath(&Some("skills//x".into())), want);
        assert_eq!(canonical_subpath(&Some("skills/x/".into())), want);
        assert_eq!(canonical_subpath(&Some("./skills/x".into())), want);
    }

    #[test]
    fn canonical_subpath_folds_root_equivalent_forms_to_none() {
        assert_eq!(canonical_subpath(&None), None);
        assert_eq!(canonical_subpath(&Some(".".into())), None);
        assert_eq!(canonical_subpath(&Some("".into())), None);
        assert_eq!(canonical_subpath(&Some("./".into())), None);
    }

    /// `skills/x` and `skills/./x` name the same content, so their transitive
    /// identity key must be equal — otherwise dedup/version-conflict detection
    /// can be bypassed and the same skill materialized twice.
    #[test]
    fn key_of_dedups_dot_component_paths() {
        let git = "https://github.com/o/r";
        assert_eq!(
            key_of(git, &Some("skills/x".into())),
            key_of(git, &Some("skills/./x".into()))
        );
    }

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
