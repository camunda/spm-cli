//! Where a command operates: the current project, or the user-global scope.
//!
//! spm manages skills in two scopes that share the same manifest → lock → store
//! → materialize pipeline but differ in *where* the manifest/lock live and
//! *where* vendors materialize skills:
//!
//! - [`Scope::Project`] — `ai.json`/`ai.lock` sit in the project root (cwd) and
//!   vendors materialize into project-local, gitignored dirs (the default).
//! - [`Scope::Global`] — `ai.json`/`ai.lock` live under `$SPM_HOME` and vendors
//!   materialize into user-global locations shared across every project.
//!
//! The global scope deliberately reuses the same shared store (`$SPM_HOME/store`)
//! as project installs, so it adds no new fetch/cache machinery.

use crate::paths;
use anyhow::Result;
use std::path::PathBuf;

/// Selects the project or the user-global scope for a command.
#[derive(Debug, Clone)]
pub enum Scope {
    /// Per-project scope: manifest/lock in `root` (cwd); project-local vendors.
    Project { root: PathBuf },
    /// User-global scope: manifest/lock under `$SPM_HOME`; user-global vendors.
    Global,
}

impl Scope {
    /// Build a scope from the parsed `--global` flag and the current directory.
    pub fn new(global: bool, cwd: PathBuf) -> Self {
        if global {
            Scope::Global
        } else {
            Scope::Project { root: cwd }
        }
    }

    /// Directory that holds this scope's `ai.json` + `ai.lock`.
    pub fn manifest_dir(&self) -> Result<PathBuf> {
        match self {
            Scope::Project { root } => Ok(root.clone()),
            Scope::Global => paths::spm_home(),
        }
    }

    pub fn is_global(&self) -> bool {
        matches!(self, Scope::Global)
    }

    /// Short human label for messages (`project` / `global`).
    pub fn label(&self) -> &'static str {
        match self {
            Scope::Project { .. } => "project",
            Scope::Global => "global",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::SpmHomeGuard;
    use std::path::Path;

    #[test]
    fn new_selects_variant_from_flag() {
        assert!(Scope::new(true, PathBuf::from("/tmp")).is_global());
        let p = Scope::new(false, PathBuf::from("/tmp/proj"));
        assert!(!p.is_global());
        assert_eq!(p.label(), "project");
        assert_eq!(Scope::Global.label(), "global");
    }

    #[test]
    fn project_manifest_dir_is_the_root() {
        let scope = Scope::Project {
            root: PathBuf::from("/tmp/proj"),
        };
        assert_eq!(scope.manifest_dir().unwrap(), PathBuf::from("/tmp/proj"));
    }

    #[test]
    fn global_manifest_dir_is_spm_home() {
        let home = std::env::temp_dir().join("scope-global-manifest-dir-test");
        let _guard = SpmHomeGuard::set(&home);
        assert_eq!(Scope::Global.manifest_dir().unwrap(), home);
        assert_eq!(Path::new(&home), Scope::Global.manifest_dir().unwrap());
    }
}
