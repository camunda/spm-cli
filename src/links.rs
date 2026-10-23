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
}

/// A link resolved against the filesystem, ready to materialize from.
#[derive(Debug, Clone)]
pub struct Linked {
    /// Canonical path of the linked directory: both the content and the
    /// containment boundary for symlink following and the content scan.
    pub dir: PathBuf,
    /// The path as recorded in the links file, for messages.
    pub display: String,
}

impl Links {
    /// Where this scope's links file lives.
    pub fn path(scope: &Scope) -> Result<PathBuf> {
        let home = paths::spm_home()?;
        Ok(match scope {
            Scope::Project { root } => {
                let canon = root
                    .canonicalize()
                    .with_context(|| format!("resolving project root {}", root.display()))?;
                home.join(PROJECT_DIR).join(project_file_name(&canon))
            }
            Scope::Global => home.join(FILE),
        })
    }

    /// Directory relative link paths are resolved against.
    fn base(scope: &Scope) -> Result<PathBuf> {
        scope.manifest_dir()
    }

    /// Load the links file; a missing file means no links.
    pub fn load(scope: &Scope) -> Result<Self> {
        Self::load_from(&Self::path(scope)?)
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
        self.save_to(&Self::path(scope)?)
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
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(p, text + "\n").with_context(|| format!("writing {}", p.display()))
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
            format!(
                "linked directory `{display}` for `{name}` not found — fix it with \
                 `spm link {name} <dir>{g}` or drop it with `spm unlink {name}{g}`"
            )
        })?;
        if !canon.is_dir() {
            bail!("linked path `{display}` for `{name}` is not a directory");
        }
        Ok(Some(Linked {
            dir: canon,
            display: display.clone(),
        }))
    }
}

/// The links file name for a project: its directory name (for humans browsing
/// `$SPM_HOME/links`) plus a stable hash of its canonical root (for
/// uniqueness). FNV-1a is used because, unlike std's `DefaultHasher`, its output
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
    for b in canon_root.to_string_lossy().bytes() {
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
    Ok(abs.to_string_lossy().into_owned())
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
