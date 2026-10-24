//! Local development overrides (`spm link` / `spm unlink`).
//!
//! A link points an *already-declared* skill or plugin at a local directory, so
//! its current working-tree content (uncommitted edits included) is
//! materialized instead of the commit pinned in `ai.lock`. Links are machine
//! state, never shared: they live in an spm-owned file under `$SPM_HOME`, never
//! in the checkout — `$SPM_HOME/links/<project>-<hash>.json` per project, or
//! `$SPM_HOME/links.json` for the global scope — and `ai.json`/`ai.lock` are
//! never touched by them, so teammates and CI are unaffected. Keeping them out
//! of the checkout means a repository can never supply (or commit) one: only
//! running `spm link` on this machine activates a local source.

use crate::manifest::validate_skill_name;
use crate::paths;
use crate::scope::Scope;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE: &str = "links.json";
/// `$SPM_HOME` subdirectory holding one links file per project.
const PROJECT_DIR: &str = "links";
/// `$SPM_HOME` file locked machine-wide by [`StateLock`].
const LOCK_FILE: &str = "links.lock";
/// Per-scope lock files, under `$SPM_HOME` (see [`StateLock`]).
const LOCK_DIR: &str = "locks";

/// The parsed links file.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Links {
    /// Dependency name → local directory, exactly as recorded by `spm link`: a
    /// path relative to the project root (project scope) or an absolute path.
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    /// Skill names the last sync materialized from *linked plugins*. A linked
    /// plugin's working tree may bundle skills its locked commit does not, and
    /// those are not recorded in `ai.lock`; remembering them lets the next sync
    /// remove them from shared skill dirs once they are gone (or unlinked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed: Vec<String>,
    /// The canonical project root a project-scope file belongs to: only
    /// written to disk (set by [`Links::save`]), so commands in *other* scopes
    /// can resolve its relative paths and protect its working copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<String>,
}

/// Every links file on this machine, loaded with what its paths are relative to.
pub struct Recorded {
    /// The links file.
    pub file: PathBuf,
    /// Base relative link paths resolve against.
    pub base: PathBuf,
    /// Which scope recorded them, for messages: `globally` or `in <root>`.
    pub origin: String,
    pub links: Links,
}

/// A link resolved against the filesystem, ready to materialize from.
#[derive(Debug, Clone)]
pub struct Linked {
    /// Canonical path of the linked directory (the working copy to protect).
    pub dir: PathBuf,
    /// Canonical path to read the content from — both the content and the
    /// containment boundary for symlink following and the content scan: `dir`
    /// itself, or a snapshot of it a sync scans and materializes from.
    pub content: PathBuf,
    /// The path as recorded in the links file, for messages.
    pub display: String,
}

impl Links {
    /// Where this scope's links file lives.
    pub fn path(scope: &Scope) -> Result<PathBuf> {
        let home = paths::spm_home()?;
        Ok(match scope {
            Scope::Project { root } => project_state_path(&home, PROJECT_DIR, root)?,
            Scope::Global => home.join(FILE),
        })
    }

    /// Every links file on this machine — the global one and each project's —
    /// so a command can protect working copies linked from any scope. Fails
    /// closed: one that can't be read is an error, not "no links".
    pub fn all_recorded() -> Result<Vec<Recorded>> {
        Self::all_recorded_in(&paths::spm_home()?)
    }

    fn all_recorded_in(home: &Path) -> Result<Vec<Recorded>> {
        let mut out = Vec::new();
        let global = home.join(FILE);
        out.push(Recorded {
            links: Self::load_from(&global)?,
            file: global,
            base: Scope::Global.manifest_dir()?,
            origin: "globally".into(),
        });
        let dir = home.join(PROJECT_DIR);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
        };
        for entry in entries {
            let file = entry
                .with_context(|| format!("reading {}", dir.display()))?
                .path();
            // Only links files: not staging files mid-publication (see
            // `save_to`) or unrelated entries like `.DS_Store`.
            if file.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let mut links = Self::load_from(&file)?;
            // A file with only ownership records (e.g. left by a plugin link
            // that failed part-way) still needs protecting: it's the only
            // record of what to clean up.
            if links.links.is_empty() && links.managed.is_empty() {
                continue;
            }
            let root = links.project.take().with_context(|| {
                format!(
                    "{} does not name its project — delete it and re-run `spm link`",
                    file.display()
                )
            })?;
            out.push(Recorded {
                file,
                origin: format!("in `{root}`"),
                base: PathBuf::from(root),
                links,
            });
        }
        Ok(out)
    }

    /// Directory relative link paths are resolved against.
    fn base(scope: &Scope) -> Result<PathBuf> {
        scope.manifest_dir()
    }

    /// Load the links file; a missing file means no links.
    pub fn load(scope: &Scope) -> Result<Self> {
        let mut links = Self::load_from(&Self::path(scope)?)?;
        links.project = None;
        Ok(links)
    }

    fn load_from(p: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
        };
        let links: Self =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        // Names are joined onto vendor skill dirs (and removed from shared ones),
        // so a hand-edited file must not smuggle a path in.
        for name in links.links.keys().chain(&links.managed) {
            validate_skill_name(name).with_context(|| format!("in {}", p.display()))?;
        }
        Ok(links)
    }

    /// Persist the links file, deleting it once it holds nothing.
    pub fn save(&self, scope: &Scope) -> Result<()> {
        self.document(scope)?.save_to(&Self::path(scope)?)
    }

    /// The document [`Links::save`] writes for `scope`. Call it while planning
    /// too, so whatever would make saving fail (a non-UTF-8 project root) is
    /// rejected before any target output changes.
    pub fn document(&self, scope: &Scope) -> Result<Self> {
        let mut doc = self.clone();
        doc.project = match scope {
            Scope::Project { root } if !(self.links.is_empty() && self.managed.is_empty()) => {
                let canon = canonical_root(root)?;
                let text = canon.to_str().with_context(|| {
                    format!(
                        "project path {} is not valid UTF-8, which links don't support",
                        canon.display()
                    )
                })?;
                Some(text.to_string())
            }
            _ => None,
        };
        Ok(doc)
    }

    fn save_to(&self, p: &Path) -> Result<()> {
        if self.links.is_empty() && self.managed.is_empty() {
            return match std::fs::remove_file(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(e).with_context(|| format!("removing {}", p.display()))
                }
                _ => Ok(()),
            };
        }
        let staged = self.stage(p)?;
        publish(&staged, p)
    }

    /// Write the complete file next to its destination (following a symlinked
    /// links file, so publishing replaces what it points at, not the link) as
    /// a staging file that readers skip, returning `(staging, destination)`.
    fn stage(&self, p: &Path) -> Result<(PathBuf, PathBuf)> {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let dest = match p.canonicalize() {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => p.to_path_buf(),
            Err(e) => return Err(e).with_context(|| format!("resolving {}", p.display())),
        };
        let name = dest.file_name().unwrap_or_default().to_string_lossy();
        let staging = dest.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&staging, text + "\n")
            .with_context(|| format!("writing {}", staging.display()))?;
        Ok((staging, dest))
    }

    /// Every skill name materialized for a scope with this lock and these
    /// links: locked skills, each plugin's locked bundled skills — except for
    /// an actively linked plugin, whose working tree's bundled skills
    /// (recorded in [`Links::managed`]) are materialized *instead*.
    pub fn effective_skill_names(&self, lock: &crate::lockfile::Lockfile) -> Vec<String> {
        let mut names: Vec<String> = lock.skills.keys().cloned().collect();
        for (name, l) in &lock.plugins {
            if !self.links.contains_key(name) {
                names.extend(l.bundled_skills.iter().cloned());
            }
        }
        names.extend(self.managed.iter().cloned());
        names.sort();
        names.dedup();
        names
    }

    /// Resolve the link recorded for `name` to an existing directory.
    pub fn resolve(&self, scope: &Scope, name: &str) -> Result<Option<Linked>> {
        let Some(display) = self.links.get(name) else {
            return Ok(None);
        };
        let dir = Self::base(scope)?.join(display);
        let g = if scope.is_global() { " -g" } else { "" };
        let canon = dir.canonicalize().with_context(|| {
            let arg = crate::shell::operands([name]);
            format!(
                "linked directory `{display}` for `{name}` not found — fix it with \
                 `spm link{g} {arg} <dir>` or drop it with `spm unlink{g} {arg}`"
            )
        })?;
        if !canon.is_dir() {
            bail!("linked path `{display}` for `{name}` is not a directory");
        }
        Ok(Some(Linked {
            content: canon.clone(),
            dir: canon,
            display: display.clone(),
        }))
    }
}

/// The locks over link state and target ownership, held by a mutating
/// command from before it reads link state until its last write or deletion:
///
/// - the scope's own lock, always exclusively: commands on the same project
///   (or on the global scope) run one at a time, so none plans from ownership
///   records (`managed`) another is about to replace;
/// - the machine-wide lock: exclusively by commands that may activate a link
///   (`spm link`, `spm unlink`), shared by every other one. So no link can be
///   published into another project's output between that project's command
///   checking for links and rewriting the output — while commands on
///   different projects still run side by side.
///
/// Always taken in that order, so two commands can't deadlock. Lock files live
/// in their own directory, never next to the links files (which may be
/// redirected into a target's output).
pub struct StateLock {
    _scope: std::fs::File,
    _machine: std::fs::File,
    exclusive: bool,
}

impl StateLock {
    /// Take both locks, waiting (and saying so on stderr) while another
    /// command holds one in a conflicting mode. `exclusive` is for the
    /// machine-wide lock. Take them before reading any link state.
    pub fn acquire(scope: &Scope, exclusive: bool) -> Result<Self> {
        let home = paths::spm_home()?;
        let stem = Links::path(scope)?
            .file_stem()
            .context("links file has no name")?
            .to_owned();
        let mut scope_lock = home.join(LOCK_DIR).join(stem);
        scope_lock.set_extension("lock");
        Ok(Self {
            _scope: lock(&scope_lock, true)?,
            _machine: lock(&home.join(LOCK_FILE), exclusive)?,
            exclusive,
        })
    }

    pub fn is_exclusive(&self) -> bool {
        self.exclusive
    }
}

/// Open (creating it and its directory) and lock the file `p`.
fn lock(p: &Path, exclusive: bool) -> Result<std::fs::File> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(p)
        .with_context(|| format!("opening {}", p.display()))?;
    let attempt = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    match attempt {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            eprintln!(
                "waiting for another spm command to finish (lock: {})...",
                p.display()
            );
            if exclusive {
                file.lock()
            } else {
                file.lock_shared()
            }
            .with_context(|| format!("locking {}", p.display()))?;
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(e).with_context(|| format!("locking {}", p.display()))
        }
    }
    Ok(file)
}

/// Atomically replace the destination with the staged file: links files are
/// read by every spm command on the machine (see [`Links::all_recorded`]), so
/// a concurrent reader must see either the old or the new file, never a
/// truncated one.
fn publish((staging, dest): &(PathBuf, PathBuf), p: &Path) -> Result<()> {
    std::fs::rename(staging, dest).with_context(|| {
        let _ = std::fs::remove_file(staging);
        format!("writing {}", p.display())
    })
}

/// Where machine-local state about the project at `root` lives: one file per
/// project under `home/<subdir>`, named by [`project_file_name`]. Keeping it
/// out of the checkout means a repository can never supply it.
pub(crate) fn project_state_path(home: &Path, subdir: &str, root: &Path) -> Result<PathBuf> {
    Ok(home
        .join(subdir)
        .join(project_file_name(&canonical_root(root)?)))
}

fn canonical_root(root: &Path) -> Result<PathBuf> {
    root.canonicalize()
        .with_context(|| format!("resolving project root {}", root.display()))
}

/// The links file name for a project: its directory name (for humans browsing
/// `$SPM_HOME/links`) plus a stable hash of its canonical root's native bytes
/// (for uniqueness). FNV-1a is used because, unlike std's `DefaultHasher`, its output
/// is fixed across Rust releases, so an upgrade never orphans a project's links.
fn project_file_name(canon_root: &Path) -> String {
    let label: String = canon_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    // Lossless (unlike `to_string_lossy`), and the UTF-8 bytes themselves for
    // any path that is valid UTF-8.
    for &b in canon_root.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{label}-{hash:016x}.json")
}

/// How `spm link` records `dir` (given relative to `cwd`): kept as typed when
/// relative in the project scope (where `cwd` is the project root), so a moved
/// or re-cloned project keeps working; otherwise made absolute, since the
/// global scope has no root a relative path could be anchored to.
pub fn record_path(scope: &Scope, cwd: &Path, dir: &str) -> Result<String> {
    let given = Path::new(dir);
    if given.is_relative() && !scope.is_global() {
        return Ok(dir.to_string());
    }
    let abs = std::path::absolute(cwd.join(given)).with_context(|| format!("resolving `{dir}`"))?;
    // Never lossy: a replaced byte would record (and later materialize) a
    // different directory.
    abs.to_str().map(str::to_string).with_context(|| {
        format!(
            "path {} is not valid UTF-8, which links don't support",
            abs.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d =
            std::env::temp_dir().join(format!("spm-links-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rejects_unsafe_names_in_hand_edited_file() {
        let root = tmp("unsafe");
        let file = root.join(FILE);
        for body in [
            r#"{"links":{"../x":"d"}}"#,
            r#"{"links":{},"managed":["a/b"]}"#,
        ] {
            std::fs::write(&file, body).unwrap();
            assert!(Links::load_from(&file).is_err(), "{body}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn empty_save_deletes_the_file() {
        let root = tmp("empty");
        let file = root.join(PROJECT_DIR).join(FILE);
        let mut links = Links::default();
        links.links.insert("a".into(), "dir".into());
        links.save_to(&file).unwrap();
        assert!(file.is_file());
        assert_eq!(Links::load_from(&file).unwrap(), links);
        Links::default().save_to(&file).unwrap();
        assert!(!file.exists());
        // Saving an empty set with no file is a no-op, not an error.
        Links::default().save_to(&file).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn project_file_name_is_stable_unique_and_safe() {
        let a = project_file_name(Path::new("/work/my app"));
        // Pinned (standard FNV-1a 64): a changed hash would orphan every existing project's links.
        assert_eq!(a, "my_app-c0a1125f1079eaa1.json");
        assert_ne!(a, project_file_name(Path::new("/other/my app")));
        assert!(!project_file_name(Path::new("/x/a\\b..c")).contains(['/', '\\']));
    }

    /// A reader running while a links file is being published sees the old
    /// complete file (never a truncated one) and skips the staging file and
    /// unrelated entries; once published, it sees the new one.
    #[test]
    fn publication_is_atomic_for_concurrent_readers() {
        let home = tmp("atomic");
        let dir = home.join(PROJECT_DIR);
        let file = dir.join("app-0000000000000000.json");
        let with = |link: &str| {
            let mut l = Links {
                project: Some(home.to_string_lossy().into_owned()),
                ..Links::default()
            };
            l.links.insert(link.into(), "dir".into());
            l
        };
        let read = || {
            let all = Links::all_recorded_in(&home).unwrap();
            let names: Vec<_> = all.iter().flat_map(|r| r.links.links.keys()).collect();
            names.into_iter().cloned().collect::<Vec<_>>()
        };
        with("old").save_to(&file).unwrap();
        std::fs::write(dir.join(".DS_Store"), [0xFFu8, 0, 1]).unwrap();
        std::fs::write(dir.join("notes.txt"), "not json").unwrap();

        let staged = with("new").stage(&file).unwrap();
        assert!(staged.0.exists());
        assert_eq!(read(), ["old"], "mid-publication");
        publish(&staged, &file).unwrap();
        assert!(!staged.0.exists());
        assert_eq!(read(), ["new"], "published");
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Saving through a symlinked links file updates what it points at and
    /// keeps the symlink.
    #[cfg(unix)]
    #[test]
    fn save_follows_a_symlinked_links_file() {
        let root = tmp("symlinked");
        let real = root.join("real.json");
        let file = root.join(FILE);
        std::fs::write(&real, "{}").unwrap();
        std::os::unix::fs::symlink(&real, &file).unwrap();
        let mut links = Links::default();
        links.links.insert("a".into(), "dir".into());
        links.save_to(&file).unwrap();
        assert!(std::fs::symlink_metadata(&file).unwrap().is_symlink());
        assert_eq!(Links::load_from(&real).unwrap(), links);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// An absolute path that isn't valid UTF-8 is rejected, not recorded lossily
    /// (which would point the link at a different directory).
    #[test]
    fn record_path_rejects_non_utf8_absolute_paths() {
        #[cfg(unix)]
        let cwd = {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(b"/work/\xFE"))
        };
        #[cfg(windows)]
        let cwd = {
            use std::os::windows::ffi::OsStringExt;
            let mut w: Vec<u16> = "C:\\work\\".encode_utf16().collect();
            w.push(0xD800);
            PathBuf::from(std::ffi::OsString::from_wide(&w))
        };
        let err = record_path(&Scope::Global, &cwd, "skill").unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"), "{err}");
        let project = Scope::Project { root: cwd.clone() };
        assert_eq!(record_path(&project, &cwd, "skill").unwrap(), "skill");
    }

    /// Roots that only differ in bytes that aren't valid UTF-8 (which a lossy
    /// conversion would fold together) still get distinct files.
    #[test]
    fn project_file_name_distinguishes_non_utf8_roots() {
        #[cfg(unix)]
        let (a, b) = {
            use std::os::unix::ffi::OsStrExt;
            let p = |b: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(b));
            (p(b"/work/\xFE/app"), p(b"/work/\xFF/app"))
        };
        #[cfg(windows)]
        let (a, b) = {
            use std::os::windows::ffi::OsStringExt;
            let p = |unit: u16| {
                let mut w: Vec<u16> = "C:\\work\\".encode_utf16().collect();
                w.push(unit);
                w.extend("\\app".encode_utf16());
                PathBuf::from(std::ffi::OsString::from_wide(&w))
            };
            (p(0xD800), p(0xD801))
        };
        assert_eq!(a.to_string_lossy(), b.to_string_lossy());
        assert_ne!(project_file_name(&a), project_file_name(&b));
    }

    #[test]
    fn record_path_keeps_project_relative_paths_and_absolutizes_otherwise() {
        let cwd = std::env::temp_dir();
        let project = Scope::Project { root: cwd.clone() };
        assert_eq!(
            record_path(&project, &cwd, "../skills/foo").unwrap(),
            "../skills/foo"
        );
        let global = record_path(&Scope::Global, &cwd, "foo").unwrap();
        assert!(Path::new(&global).is_absolute(), "{global}");
        assert!(Path::new(&global).ends_with("foo"), "{global}");
        let abs = cwd.join("bar");
        let kept = record_path(&project, &cwd, abs.to_str().unwrap()).unwrap();
        assert!(Path::new(&kept).is_absolute(), "{kept}");
    }
}
