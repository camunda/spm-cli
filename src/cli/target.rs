use super::clean::managed_names;
use super::sync::sync;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::scope::Scope;
use crate::vendor;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

/// `spm target add [vendors...]`: append target vendors to an existing ai.json
/// and materialize skills for them. Explicit `vendors` are validated and added
/// verbatim; with none given, the vendors *not yet configured* are offered as an
/// interactive numbered picker. Adding an already-configured target is a skip,
/// not an error, so the command is safe to re-run.
pub(super) fn target_add(scope: &Scope, vendors: Vec<String>) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir, scope)?;

    let requested = if vendors.is_empty() {
        let available: Vec<&str> = vendor::ALL_TARGETS
            .iter()
            .copied()
            .filter(|t| !manifest.targets.iter().any(|cur| cur == t))
            .collect();
        if available.is_empty() {
            println!(
                "all supported targets already configured: {}",
                manifest.targets.join(", ")
            );
            return Ok(());
        }
        prompt_targets(&available)?
    } else {
        // Validate every explicitly-requested vendor up front so a typo aborts
        // before the manifest is touched.
        for v in &vendors {
            vendor::for_target(v)?;
        }
        vendors
    };

    let mut added = Vec::new();
    for v in requested {
        if manifest.targets.iter().any(|cur| cur == &v) {
            println!("target `{v}` already configured — skipping");
            continue;
        }
        manifest.targets.push(v.clone());
        added.push(v);
    }

    if added.is_empty() {
        println!("no new targets added");
        return Ok(());
    }

    manifest.save(&dir)?;
    // Materialize existing skills into the newly-added vendor(s).
    sync(scope, false, None, false)?;
    println!("added target(s): {}", added.join(", "));
    Ok(())
}

/// `spm target remove <vendors...>`: drop target vendors from ai.json and clean
/// what spm materialized for them. Every vendor is validated up front, so a typo
/// aborts before anything is touched; a vendor that isn't configured is a skip,
/// so the command is safe to re-run. Removing every remaining target is refused
/// (`sync` needs at least one) — `spm clean` is the tool for that.
pub(super) fn target_remove(scope: &Scope, vendors: Vec<String>) -> Result<()> {
    for v in &vendors {
        vendor::for_target(v)?;
    }
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir, scope)?;

    let mut removed: Vec<String> = Vec::new();
    for v in vendors {
        if removed.contains(&v) {
            continue;
        }
        if manifest.targets.contains(&v) {
            removed.push(v);
        } else {
            println!("target `{v}` not configured — skipping");
        }
    }
    if removed.is_empty() {
        println!("no targets removed");
        return Ok(());
    }

    let remaining: Vec<String> = manifest
        .targets
        .iter()
        .filter(|t| !removed.contains(t))
        .cloned()
        .collect();
    if remaining.is_empty() {
        let clean = if scope.is_global() {
            "spm clean --global"
        } else {
            "spm clean"
        };
        bail!(
            "cannot remove the last target ({}): ai.json needs at least one; \
             use `{clean}` to remove all generated vendor config instead",
            removed.join(", ")
        );
    }

    let lock = Lockfile::load_or_default(&dir)?;
    let (managed, managed_plugins) = managed_names(&lock);
    // Physical locations a remaining target still loads skills, plugins or
    // their metadata from (`content`), plus every location reaching them
    // depends on (`traversed`: symlinks, and dirs passed through before a
    // `..`), including symlinks nested anywhere inside the content. A removed
    // vendor must not delete a traversed location or its ancestors, nor content,
    // its ancestors or anything inside it — e.g. `codex` and `amp` share
    // `.agents/skills/<name>/`, and a vendor dir may be symlinked onto (or into)
    // another target's output. Locations are compared by filesystem identity so
    // spelling aliases (e.g. case on a case-insensitive filesystem) still match.
    let mut needed: Vec<(&str, Vec<NodeId>, bool)> = Vec::new();
    for t in &remaining {
        let v = vendor::for_target(t)?;
        let mut paths = v.skill_paths(scope, &managed)?;
        paths.extend(v.plugin_paths(scope, &managed_plugins)?);
        let (mut traversed, mut content) = (Vec::new(), Vec::new());
        let mut visited = std::collections::HashSet::new();
        for p in paths {
            reach(&p, &mut traversed, &mut content, &mut visited)?;
        }
        for (paths, is_content) in [(traversed, false), (content, true)] {
            for p in paths {
                needed.push((t.as_str(), identity(&p)?, is_content));
            }
        }
    }
    for t in &removed {
        let v = vendor::for_target(t)?;
        let mut candidates = v.clean_paths(scope, &managed)?;
        candidates.extend(v.plugin_clean_paths(scope, &managed_plugins)?);
        let mut deletable = Vec::new();
        for p in candidates {
            let target = identity(&deletion_target(&p)?)?;
            let owner = needed.iter().find(|(_, n, content)| {
                n.starts_with(&target) || (*content && target.starts_with(n))
            });
            match owner {
                Some((owner, ..)) => {
                    println!("keeping {}: still used by target `{owner}`", p.display())
                }
                None => deletable.push(p),
            }
        }
        vendor::remove_paths(&deletable)?;
        v.deregister(scope)?;
        v.deregister_plugins(scope)?;
    }

    manifest.targets = remaining;
    manifest.save(&dir)?;
    println!("removed target(s): {}", removed.join(", "));
    Ok(())
}

/// Record what loading from `p` depends on: resolve it (pushing traversed
/// locations), push the physical location as content, then walk it for nested
/// symlinks — each of which is a further dependency to resolve and reach.
fn reach(
    p: &Path,
    traversed: &mut Vec<PathBuf>,
    content: &mut Vec<PathBuf>,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Result<()> {
    let fin = resolve(p, traversed)?;
    if !visited.insert(fin.clone()) {
        return Ok(());
    }
    content.push(fin.clone());
    let mut dirs = vec![fin];
    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue
            }
            Err(e) => return Err(anyhow!(e).context(format!("reading {}", dir.display()))),
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                traversed.push(entry.path());
                reach(&entry.path(), traversed, content, visited)?;
            } else if ft.is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    Ok(())
}

/// One component of a path's filesystem identity (see [`identity`]).
#[derive(Debug, PartialEq, Eq)]
enum NodeId {
    /// An existing entry, by device and inode (not following a final symlink).
    #[cfg(unix)]
    Inode(u64, u64),
    /// A component compared by name: a missing one, or on Windows any one,
    /// case-folded after canonicalizing its existing prefix.
    Name(String),
}

/// The identity of physical path `p` as one [`NodeId`] per component from the
/// root, so `a` is `b` or an ancestor of it iff `identity(b)` starts with
/// `identity(a)` — regardless of how either is spelled (e.g. `.AGENTS` vs
/// `.agents` on a case-insensitive filesystem).
#[cfg(unix)]
fn identity(p: &Path) -> Result<Vec<NodeId>> {
    use std::os::unix::fs::MetadataExt;
    let mut ids = Vec::new();
    let mut cur = PathBuf::new();
    let mut missing = false;
    for comp in p.components() {
        cur.push(comp);
        if !missing {
            match std::fs::symlink_metadata(&cur) {
                Ok(m) => {
                    ids.push(NodeId::Inode(m.dev(), m.ino()));
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing = true,
                Err(e) => return Err(anyhow!(e).context(format!("inspecting {}", cur.display()))),
            }
        }
        ids.push(NodeId::Name(
            comp.as_os_str().to_string_lossy().into_owned(),
        ));
    }
    Ok(ids)
}

/// Windows has no stable inode API, so the longest existing prefix is
/// canonicalized instead — normalizing `\\?\` vs plain prefixes (which
/// `read_link` mixes), 8.3 short names and case — without following a final
/// symlink; the rest is compared case-folded, as NTFS is case-insensitive.
#[cfg(not(unix))]
fn identity(p: &Path) -> Result<Vec<NodeId>> {
    let comps: Vec<_> = p.components().collect();
    let mut base = PathBuf::new();
    let mut existing = 0;
    for n in (1..=comps.len()).rev() {
        let prefix: PathBuf = comps[..n].iter().collect();
        let meta = match std::fs::symlink_metadata(&prefix) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(anyhow!(e).context(format!("inspecting {}", prefix.display()))),
        };
        let canonical = |q: &Path| {
            std::fs::canonicalize(q).with_context(|| format!("canonicalizing {}", q.display()))
        };
        base = match (
            meta.file_type().is_symlink(),
            prefix.parent(),
            prefix.file_name(),
        ) {
            (true, Some(parent), Some(name)) => canonical(parent)?.join(name),
            _ => canonical(&prefix)?,
        };
        existing = n;
        break;
    }
    let names = base.components().chain(comps[existing..].iter().copied());
    Ok(names
        .map(|c| NodeId::Name(c.as_os_str().to_string_lossy().to_lowercase()))
        .collect())
}

/// The physical location `remove_dir_all(p)` deletes: `p`'s parent resolved
/// through symlinks, joined with its last component — which is not followed, as
/// removing a symlink removes only the link.
fn deletion_target(p: &Path) -> Result<PathBuf> {
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => Ok(resolve(parent, &mut Vec::new())?.join(name)),
        _ => resolve(p, &mut Vec::new()),
    }
}

/// Resolve `p` (anchored to the working directory if relative, e.g. under a
/// relative `SPM_HOME`) through symlinks component by component, returning the
/// physical location and pushing onto `traversed` every location the path
/// depends on along the way: each symlink, and each dir passed through before a
/// `..`. Missing components are kept as spelled (they hold nothing to delete or
/// protect); other errors, including a symlink loop, are propagated rather than
/// treated as "not shared".
fn resolve(p: &Path, traversed: &mut Vec<PathBuf>) -> Result<PathBuf> {
    let abs = if p.is_relative() {
        std::env::current_dir()?.join(p)
    } else {
        p.to_path_buf()
    };
    resolve_hops(&abs, traversed, &mut 0)
}

fn resolve_hops(p: &Path, chain: &mut Vec<PathBuf>, hops: &mut u32) -> Result<PathBuf> {
    use std::path::Component;
    const MAX_HOPS: u32 = 40;
    let mut cur = PathBuf::new();
    let mut missing = false;
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => cur.push(comp),
            Component::CurDir => {}
            Component::ParentDir => {
                if !missing && cur.parent().is_some() {
                    chain.push(cur.clone());
                }
                cur.pop();
            }
            Component::Normal(name) => {
                let next = cur.join(name);
                if missing {
                    cur = next;
                    continue;
                }
                match std::fs::symlink_metadata(&next) {
                    Ok(m) if m.file_type().is_symlink() => {
                        *hops += 1;
                        if *hops > MAX_HOPS {
                            bail!("too many levels of symbolic links at {}", next.display());
                        }
                        let link = std::fs::read_link(&next).map_err(|e| {
                            anyhow!(e).context(format!("reading {}", next.display()))
                        })?;
                        chain.push(next);
                        cur = resolve_hops(&cur.join(link), chain, hops)?;
                    }
                    Ok(_) => cur = next,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        missing = true;
                        cur = next;
                    }
                    Err(e) => {
                        return Err(anyhow!(e).context(format!("resolving {}", next.display())))
                    }
                }
            }
        }
    }
    Ok(cur)
}

/// Interactive numbered picker over the vendors not yet configured. Reads a
/// comma-separated list of 1-based indices (or `all`) from stdin — a plain line
/// read, not a raw-terminal TUI, so it stays portable across Windows and Unix
/// shells and is drivable from tests with piped input.
fn prompt_targets(available: &[&str]) -> Result<Vec<String>> {
    use std::io::{self, BufRead, Write};

    println!("Select target(s) to add:");
    for (i, t) in available.iter().enumerate() {
        println!("  {}) {t}", i + 1);
    }
    print!("Enter numbers (comma-separated), or `all`: ");
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        bail!("no target selected");
    }
    if line.eq_ignore_ascii_case("all") {
        return Ok(available.iter().map(|s| s.to_string()).collect());
    }

    // Preserve the user's typed order while dropping duplicate picks.
    let mut chosen = Vec::new();
    for tok in line.split(',') {
        let tok = tok.trim();
        let n: usize = tok
            .parse()
            .map_err(|_| anyhow!("invalid selection `{tok}`: enter numbers from the list"))?;
        let vendor = available
            .get(
                n.checked_sub(1)
                    .ok_or_else(|| anyhow!("invalid selection `{tok}`: numbers start at 1"))?,
            )
            .ok_or_else(|| anyhow!("invalid selection `{tok}`: no such option"))?
            .to_string();
        if !chosen.contains(&vendor) {
            chosen.push(vendor);
        }
    }
    Ok(chosen)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("spm-target-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        resolve(&dir, &mut Vec::new()).unwrap()
    }

    #[test]
    fn resolve_records_every_link_and_keeps_missing_tail() {
        let tmp = scratch("chain");
        std::fs::create_dir_all(tmp.join("real")).unwrap();
        std::os::unix::fs::symlink("real", tmp.join("b")).unwrap();
        std::os::unix::fs::symlink(tmp.join("b"), tmp.join("a")).unwrap();
        let mut chain = Vec::new();
        let got = resolve(&tmp.join("a/missing/x"), &mut chain).unwrap();
        assert_eq!(got, tmp.join("real/missing/x"));
        assert_eq!(chain, [tmp.join("a"), tmp.join("b")]);
        assert_eq!(deletion_target(&tmp.join("a")).unwrap(), tmp.join("a"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn resolve_rejects_symlink_loops() {
        let tmp = scratch("loop");
        std::os::unix::fs::symlink("y", tmp.join("x")).unwrap();
        std::os::unix::fs::symlink("x", tmp.join("y")).unwrap();
        let err = resolve(&tmp.join("x/z"), &mut Vec::new()).unwrap_err();
        assert!(err.to_string().contains("too many levels"), "{err}");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
