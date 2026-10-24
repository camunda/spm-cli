//! Physical, filesystem-identity view of paths: resolve symlinks component by
//! component and compare locations by identity (inode, or canonical
//! case-folded names on Windows) so spelling aliases still match. Used wherever
//! spm must prove one location is not inside another before deleting or
//! copying (`spm target remove`, `spm link`).

use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

/// One component of a path's filesystem identity (see [`identity`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NodeId {
    /// An existing entry, by device and inode (not following a final symlink).
    #[cfg(unix)]
    Inode(u64, u64),
    /// A component compared by name: a missing one, or on Windows any one,
    /// case-folded after canonicalizing its existing prefix.
    Name(String),
}

/// The identity of the location `p` resolves to, if it exists.
pub fn existing_identity(p: &Path) -> Result<Option<Vec<NodeId>>> {
    let fin = resolve(p, &mut Vec::new())?;
    if std::fs::symlink_metadata(&fin).is_err() {
        return Ok(None);
    }
    identity(&fin).map(Some)
}

/// The identity of physical path `p` as one [`NodeId`] per component from the
/// root, so `a` is `b` or an ancestor of it iff `identity(b)` starts with
/// `identity(a)` — regardless of how either is spelled (e.g. `.AGENTS` vs
/// `.agents` on a case-insensitive filesystem).
#[cfg(unix)]
pub fn identity(p: &Path) -> Result<Vec<NodeId>> {
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
pub fn identity(p: &Path) -> Result<Vec<NodeId>> {
    use anyhow::Context;
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
pub fn deletion_target(p: &Path) -> Result<PathBuf> {
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
pub fn resolve(p: &Path, traversed: &mut Vec<PathBuf>) -> Result<PathBuf> {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("spm-pathid-{name}-{}-{nanos}", std::process::id()));
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
