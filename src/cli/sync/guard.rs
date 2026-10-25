//! The overlap guard: before anything writes or deletes target output, reject
//! every way that could reach a linked working copy, one of spm's own state
//! files, or the snapshot linked content is materialized from.

use crate::ids::DepName;
use crate::links::{Linked, Links};
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::owned::{Owned, Ownership};
use crate::pathid::{deletion_target, identity, resolve, NodeId};
use crate::scope::Scope;
use crate::vendor::Vendor;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A working copy the overlap guard protects.
struct Guarded {
    name: DepName,
    /// Canonical dir.
    dir: PathBuf,
    /// The path as recorded (its route counts too), anchored at its scope's base.
    recorded: PathBuf,
    /// The path as recorded, for messages.
    display: String,
    /// Where it was linked, for messages: empty for this command's scope.
    origin: String,
    /// Still linked in this command's scope (so relinking is a way out)?
    active: bool,
}

/// Before `vendors` write or delete anything — each only for its own
/// `outputs` entry (the names it holds or is about to write) — reject
/// any overlap with a linked working copy: each `active` link, plus every one
/// recorded on this machine — in this scope's links file (one being dropped by
/// `spm unlink`, re-pointed by `spm link`, or ignored as undeclared) and in
/// every other scope's (a global link may point into a project's output, or one
/// project's into another's). Each is still the user's data even when this
/// command doesn't copy from it. A recorded dir that is gone has nothing left
/// to protect. Every command that writes target output or deletes it (`sync`,
/// `clean`, `target remove`) must call this first. A lock-only sync writes
/// just `ai.json` and `ai.lock` (pass no `vendors`), plus the ownership record
/// when it freezes a legacy one (see [`Writes`]), so only those count.
pub(in crate::cli) fn guard_working_copies(
    scope: &Scope,
    manifest_dir: &Path,
    vendors: &[Box<dyn Vendor>],
    active: &BTreeMap<DepName, Linked>,
    outputs: &[Owned],
    snapshot: Option<&Path>,
    writes: Writes,
) -> Result<()> {
    assert_eq!(vendors.len(), outputs.len(), "one output set per vendor");
    let mut guarded: Vec<Guarded> = active
        .iter()
        .map(|(n, l)| Guarded {
            name: n.clone(),
            dir: l.dir.clone(),
            recorded: manifest_dir.join(&l.display),
            display: l.display.clone(),
            origin: String::new(),
            active: true,
        })
        .collect();
    // Fail closed: a links file or recorded dir that can't be read is not
    // "nothing to protect". Only a dir that is gone (or no longer a dir) is
    // skipped, so `spm unlink` can always drop a dead link.
    let own_file = Links::path(scope)?;
    let mut state_files = vec![StateFile::links(own_file.clone())];
    for rec in Links::all_recorded()? {
        let own = rec.file == own_file;
        let (base, origin) = if own {
            (manifest_dir.to_path_buf(), String::new())
        } else {
            (rec.base, format!(" (linked {})", rec.origin))
        };
        if !own {
            state_files.push(StateFile::links(rec.file));
        }
        for (name, display) in rec.links.links {
            let recorded = base.join(&display);
            let dir = match recorded.canonicalize() {
                Ok(d) if d.is_dir() => d,
                Ok(_) => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "inspecting linked directory `{display}` for `{name}`{origin} — spm \
                             can't rule out that this command would modify or delete it"
                        )
                    })
                }
            };
            if !guarded
                .iter()
                .any(|g| g.name == name && g.recorded == recorded)
            {
                guarded.push(Guarded {
                    name,
                    dir,
                    recorded,
                    display,
                    origin: origin.clone(),
                    active: false,
                });
            }
        }
    }
    state_files.extend(
        Ownership::all_paths(scope)?
            .into_iter()
            .map(|path| StateFile {
                path,
                what: "the ownership record",
                loss: "lose the record of which skills spm installed (so cleanup could \
                       delete skills it never installed)",
            }),
    );
    reject_link_overlaps(
        scope,
        manifest_dir,
        vendors,
        &guarded,
        Kept {
            state_files: &state_files,
            snapshot,
            writes,
        },
        outputs,
    )
}

/// A machine-local state file spm alone keeps, which the targets' writes and
/// deletions must never reach.
struct StateFile {
    path: PathBuf,
    /// How messages name it (`the links file`).
    what: &'static str,
    /// What wiping it costs (`drop your links`).
    loss: &'static str,
}

impl StateFile {
    fn links(path: PathBuf) -> Self {
        Self {
            path,
            what: "the links file",
            loss: "drop your links",
        }
    }
}

/// Which of spm's own state files a guarded command writes besides `ai.json`
/// and `ai.lock` (see [`guard_working_copies`]).
#[derive(Clone, Copy)]
pub(in crate::cli) struct Writes {
    /// Writes only `ai.json` and `ai.lock` (and maybe the ownership record):
    /// neither the links file (nor its removal) nor `.gitignore`.
    pub lock_only: bool,
    /// Saves the ownership record.
    pub ownership: bool,
}

impl Writes {
    /// An install, `clean` or `target remove`: every state file.
    pub const ALL: Self = Self {
        lock_only: false,
        ownership: true,
    };
}

/// What the targets' writes and deletions must leave alone besides working
/// copies (see [`reject_link_overlaps`]).
struct Kept<'a> {
    /// The state files this command must not wipe: every links file (this
    /// scope's first, which it also writes) and every ownership record.
    state_files: &'a [StateFile],
    /// Where this command copied linked content to materialize it from.
    snapshot: Option<&'a Path>,
    /// Which state files this command writes itself.
    writes: Writes,
}

/// Reject a linked working copy that overlaps any location a target writes or
/// deletes. A source inside the output (e.g. linking Codex's own
/// `.agents/skills/<name>`) would be wiped before it is copied — losing
/// uncommitted edits — and output inside the source (e.g. linking the project
/// root) would be copied into itself. Files spm edits in place (`ai.json`,
/// `ai.lock`, the links file, the project `.gitignore`, a vendor's settings)
/// are followed through symlinks, since writing them would modify whatever they
/// point at. Compared by filesystem identity, so symlinks and spelling aliases
/// (case on a case-insensitive filesystem) match. The recorded path's access
/// route counts too: deleting a symlink (or a dir holding one) it passes
/// through would break — or, once regenerated, silently re-point — the link.
///
/// Nor may the targets wipe what `kept` lists: a links file (dropping every
/// link), an ownership record, or the snapshot before they copy from it.
fn reject_link_overlaps(
    scope: &Scope,
    manifest_dir: &Path,
    vendors: &[Box<dyn Vendor>],
    guarded: &[Guarded],
    kept: Kept,
    names: &[Owned],
) -> Result<()> {
    let Kept {
        state_files,
        snapshot,
        writes,
    } = kept;
    // Nothing to protect: no working copy, no state file to lose, and none
    // about to be created (an ownership record written for the first time
    // must not land in a target's output either).
    if guarded.is_empty()
        && !writes.ownership
        && !state_files
            .iter()
            .any(|f| f.path.symlink_metadata().is_ok())
    {
        return Ok(());
    }
    let Touched { outputs, patched } =
        touched(scope, manifest_dir, vendors, names, state_files, writes)?;
    reject_state_file_overlaps(state_files, &outputs, &patched)?;
    if let Some(root) = snapshot {
        reject_snapshot_overlap(root, &outputs)?;
    }
    for g in guarded {
        reject_working_copy_overlap(scope, g, &outputs, &patched)?;
    }
    Ok(())
}

/// A location a guarded command writes or deletes: its owner (`spm` or a
/// target), the path as spelled, its physical identity, and whether it is
/// written through (a file patched in place, following symlinks).
type Output = (&'static str, PathBuf, Vec<NodeId>, bool);

/// Every location a guarded command writes or deletes.
struct Touched {
    outputs: Vec<Output>,
    /// The files written in place: (owner, path).
    patched: Vec<(&'static str, PathBuf)>,
}

/// Collect what `vendors` (each for its `names`) and spm itself write or
/// delete, given `writes`. `state_files[0]` is this scope's links file.
fn touched(
    scope: &Scope,
    manifest_dir: &Path,
    vendors: &[Box<dyn Vendor>],
    names: &[Owned],
    state_files: &[StateFile],
    writes: Writes,
) -> Result<Touched> {
    let lock_only = writes.lock_only;
    let mut outputs: Vec<Output> = Vec::new();
    let mut patched: Vec<(&'static str, PathBuf)> = vec![
        ("spm", Manifest::path_in(manifest_dir)),
        ("spm", Lockfile::path_in(manifest_dir)),
    ];
    if !lock_only {
        patched.push(("spm", Links::path(scope)?));
        if let Scope::Project { root } = scope {
            patched.push(("spm", root.join(".gitignore")));
        }
    }
    for (v, owned) in vendors.iter().zip(names) {
        let (skills, plugins) = (owned.skill_names(), owned.plugin_names());
        let mut paths = v.clean_paths(scope, &skills)?;
        paths.extend(v.skill_paths(scope, &skills)?);
        if let Some(p) = v.plugins() {
            paths.extend(p.plugin_clean_paths(scope, &plugins)?);
            paths.extend(p.plugin_paths(scope, &plugins)?);
        }
        for p in paths {
            let id = identity(&deletion_target(&p)?)?;
            outputs.push((v.name(), p, id, false));
        }
        if let Some(r) = v.registration() {
            patched.extend(r.patched_files(scope)?.into_iter().map(|p| (v.name(), p)));
        }
    }
    for (owner, p) in &patched {
        let id = identity(&resolve(p, &mut Vec::new())?)?;
        outputs.push((*owner, p.clone(), id, true));
    }
    // spm's own state writes count too, so none lands on another state file
    // (e.g. `$SPM_HOME/owned` aliasing `$SPM_HOME/links`). This scope's links
    // file is also deleted once it holds nothing, which removes the entry at
    // its path (not what a final symlink there points at).
    if !lock_only {
        let links_file = state_files[0].path.clone();
        let id = identity(&deletion_target(&links_file)?)?;
        outputs.push(("spm", links_file, id, false));
    }
    // Saving the ownership record replaces the entry at its path (beside a
    // temporary file in the same dir), following its parents' symlinks only.
    if writes.ownership {
        let record = Ownership::path(scope)?;
        let id = identity(&deletion_target(&record)?)?;
        outputs.push(("spm", record, id, false));
    }
    Ok(Touched { outputs, patched })
}

/// Each state file must survive the targets' cleanup: inside their output
/// (by its entry or where it points) it would be wiped, silently dropping
/// every link or the record of what spm installed. Nor may any other file
/// spm or a target patches in place (`.gitignore`, a settings file, ...)
/// resolve to it: that write would corrupt it.
fn reject_state_file_overlaps(
    state_files: &[StateFile],
    outputs: &[Output],
    patched: &[(&'static str, PathBuf)],
) -> Result<()> {
    for StateFile {
        path: links_file,
        what,
        loss,
    } in state_files
    {
        // Its entry, where it points, and every symlink on the way: deleting
        // any of those loses the file (or access to it) just the same.
        let mut hops = Vec::new();
        let mut state = vec![
            identity(&deletion_target(links_file)?)?,
            identity(&resolve(links_file, &mut hops)?)?,
        ];
        for h in &hops {
            state.push(identity(h)?);
        }
        // A hard link shares no path component with the links file, so only
        // the files' own identities show it.
        let hit = outputs
            .iter()
            .find(|(_, p, out, _)| {
                // Only a state file's own write (at its own path) is meant.
                p != links_file && state.iter().any(|s| s.starts_with(out))
            })
            .map(|(owner, p, _, through)| (*owner, p.as_path(), *through))
            .or_else(|| {
                patched
                    .iter()
                    .find(|(_, p)| p != links_file && same_file(p, links_file))
                    .map(|(owner, p)| (*owner, p.as_path(), true))
            });
        if let Some((owner, p, through)) = hit {
            let (relation, effect) = if through {
                ("is the same file as", "writes in place")
            } else {
                ("lies inside", "writes or deletes")
            };
            let owner = if owner == "spm" {
                "spm".to_string()
            } else {
                format!("target `{owner}`")
            };
            bail!(
                "{what} `{}` {relation} `{}`, which {owner} {effect}; refusing to continue, as \
                 that would {loss} — remove whatever puts it there (e.g. a symlink) and retry",
                links_file.display(),
                p.display()
            );
        }
    }
    Ok(())
}

/// Reject spm's temporary snapshot `root` lying inside any of `outputs`. The
/// snapshot root is canonical, so its identity is its whole route.
fn reject_snapshot_overlap(root: &Path, outputs: &[Output]) -> Result<()> {
    let id = identity(root)?;
    if let Some((owner, p, ..)) = outputs
        .iter()
        .find(|(_, _, out, through)| !through && id.starts_with(out))
    {
        bail!(
            "spm's temporary directory `{}` lies inside `{}`, which target `{owner}` \
             writes or deletes, so linked content copied there would be lost — point the \
             temporary directory elsewhere (`TMPDIR`; `TMP`/`TEMP` on Windows) and retry",
            root.display(),
            p.display()
        );
    }
    Ok(())
}

/// Reject the working copy `g` overlapping any of `outputs`, by path, by its
/// access route, or by a hard link to a `patched` file.
fn reject_working_copy_overlap(
    scope: &Scope,
    g: &Guarded,
    outputs: &[Output],
    patched: &[(&'static str, PathBuf)],
) -> Result<()> {
    let (name, display, active) = (&g.name, &g.display, g.active);
    let src = identity(&g.dir)?;
    // Every location the content depends on: the recorded path's own hops,
    // and those of each symlink inside the tree the copy follows (one that
    // is broken, cyclic or escapes the tree is skipped by the copy and the
    // scan alike, so it is no route), tagged with the symlink for messages.
    let mut route: Vec<(Option<PathBuf>, Vec<NodeId>)> = Vec::new();
    let mut hops = Vec::new();
    resolve(&g.recorded, &mut hops)?;
    for h in &hops {
        route.push((None, identity(h)?));
    }
    for link in symlinks_in(&g.dir)? {
        if crate::fsutil::resolve_within_canonical(&g.dir, &link).is_none() {
            continue;
        }
        let mut hops = Vec::new();
        resolve(&link, &mut hops)?;
        for h in &hops {
            route.push((Some(link.clone()), identity(h)?));
        }
    }
    let hit = outputs.iter().find_map(|(owner, p, out, through)| {
        if out.starts_with(&src) || (!through && src.starts_with(out)) {
            return Some((*owner, p.as_path(), String::new()));
        }
        if *through {
            return None;
        }
        let (via, _) = route.iter().find(|(_, r)| r.starts_with(out))?;
        let how = via
            .as_ref()
            .map(|v| format!(" (through the symlink `{}`)", v.display()))
            .unwrap_or_default();
        Some((*owner, p.as_path(), how))
    });
    let hit = match hit {
        Some(h) => Some(h),
        None => hard_linked_into(&g.dir, patched)?.map(|(owner, p, alias)| {
            (owner, p, format!(" (hard-linked as `{}`)", alias.display()))
        }),
    };
    let Some((owner, p, how)) = hit else {
        return Ok(());
    };
    let owner = if owner == "spm" {
        "spm".to_string()
    } else {
        format!("target `{owner}`")
    };
    let clash = format!(
        "linked directory `{display}` for `{name}`{} overlaps `{}`{how}, which {owner} \
         writes or deletes",
        g.origin,
        p.display()
    );
    let flag = if scope.is_global() { " -g" } else { "" };
    let relink = if active {
        format!(
            "link `{name}` to a directory outside spm's output \
             (`spm link{flag} {} <dir>`), or ",
            crate::shell::operands([name.as_str()])
        )
    } else if !g.origin.is_empty() {
        "unlink it where it was linked, or ".to_string()
    } else {
        String::new()
    };
    bail!(
        "{clash}; refusing to continue, as that could modify or delete the working copy — \
         {relink}remove whatever makes them overlap (e.g. a symlink) and retry"
    );
}

/// Every symlink inside `dir`, not descending into `.git` (never copied) or
/// through symlinks.
fn symlinks_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                found.push(entry.path());
            } else if kind.is_dir() && entry.file_name() != ".git" {
                dirs.push(entry.path());
            }
        }
    }
    Ok(found)
}

/// A file spm writes in place (`files`: owner, path) that is a hard link to a
/// file inside `dir`: writing it would modify the working copy too, though no
/// path relation shows it. Only files with more than one link are looked for,
/// so the tree is walked only when there is something to find. Windows has no
/// stable std API for link counts or file ids, so this is Unix-only.
#[cfg(unix)]
fn hard_linked_into<'a>(
    dir: &Path,
    files: &'a [(&'a str, PathBuf)],
) -> Result<Option<(&'a str, &'a Path, PathBuf)>> {
    use std::os::unix::fs::MetadataExt;
    let linked: Vec<(u64, u64, usize)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, (_, f))| {
            std::fs::metadata(f)
                .ok()
                .filter(|m| m.is_file() && m.nlink() > 1)
                .map(|m| (m.dev(), m.ino(), i))
        })
        .collect();
    if linked.is_empty() {
        return Ok(None);
    }
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let entry = entry?;
            // Not following symlinks: one pointing out of the tree is not in it.
            let m = entry.metadata()?;
            if m.is_dir() {
                dirs.push(entry.path());
            } else if let Some((.., i)) = linked
                .iter()
                .find(|(dev, ino, _)| *dev == m.dev() && *ino == m.ino())
            {
                let (owner, file) = &files[*i];
                return Ok(Some((owner, file.as_path(), entry.path())));
            }
        }
    }
    Ok(None)
}

/// Whether `a` and `b` are the same existing file, however reached (a symlink
/// or a hard link). Windows has no stable std API for file ids, so there only
/// the path checks apply.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_a: &Path, _b: &Path) -> bool {
    false
}

#[cfg(not(unix))]
fn hard_linked_into<'a>(
    _dir: &Path,
    _files: &'a [(&'a str, PathBuf)],
) -> Result<Option<(&'a str, &'a Path, PathBuf)>> {
    Ok(None)
}
