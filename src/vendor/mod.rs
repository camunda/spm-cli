mod claude;
mod copilot;

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// A resolved skill ready to be wired into a vendor: a name plus the absolute
/// path to its content in the global store.
pub struct MaterializedSkill {
    pub name: String,
    pub path: PathBuf,
}

/// A per-target snapshot of what is materialized in the *current* checkout vs.
/// what `ai.lock` declares. Powers `spm status` and, in particular, the
/// "fresh worktree/clone not installed here" diagnostic: because spm materializes
/// skills into gitignored dirs, a new git worktree sees nothing until
/// `spm install` runs inside it.
pub struct VendorStatus {
    /// Where this vendor materializes skills in the project (project-local).
    pub location: PathBuf,
    /// Declared skills that are present on disk in this checkout.
    pub present: Vec<String>,
    /// Declared skills that are missing here (e.g. an uninstalled worktree).
    pub missing: Vec<String>,
    /// Materialized skills that are no longer declared in `ai.lock` (stale).
    pub stale: Vec<String>,
    /// Extra human-readable notes (e.g. a registration pointer problem).
    pub notes: Vec<String>,
}

/// Compare the declared (`expected`) skill names against the subdirectories
/// actually materialized under `skills_dir`. Shared by the vendor adapters,
/// whose skills all live one directory deep under a vendor-specific root.
pub fn classify(skills_dir: &Path, expected: &[String]) -> VendorStatus {
    let (mut present, mut missing) = (Vec::new(), Vec::new());
    for name in expected {
        if skills_dir.join(name).is_dir() {
            present.push(name.clone());
        } else {
            missing.push(name.clone());
        }
    }
    let mut stale = Vec::new();
    if let Ok(entries) = std::fs::read_dir(skills_dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                if let Ok(n) = e.file_name().into_string() {
                    if !expected.iter().any(|x| x == &n) {
                        stale.push(n);
                    }
                }
            }
        }
    }
    stale.sort();
    VendorStatus {
        location: skills_dir.to_path_buf(),
        present,
        missing,
        stale,
        notes: Vec::new(),
    }
}

/// A target tool (Claude, Copilot, ...) that materializes skills from the
/// global fetch cache into a project-local, gitignored directory where the tool
/// discovers them — never into a user-global vendor location.
pub trait Vendor {
    #[allow(dead_code)] // part of the adapter contract; not all call sites use it yet
    fn name(&self) -> &'static str;

    /// Generate/refresh whatever config makes this vendor load `skills`.
    /// `project_id` is the stable, path-independent id from the lockfile.
    fn materialize(
        &self,
        project_root: &Path,
        project_id: &str,
        skills: &[MaterializedSkill],
    ) -> Result<()>;

    /// Remove everything this vendor generated for this project.
    fn clean(&self, project_root: &Path, project_id: &str) -> Result<()>;

    /// Report what this vendor has materialized in the current checkout,
    /// compared against the declared `expected` skill names (from `ai.lock`).
    fn status(&self, project_root: &Path, expected: &[String]) -> Result<VendorStatus>;
}

/// Every target vendor spm knows how to materialize. Single source of truth for
/// the `for_target` dispatch, the CLI's interactive target picker, and the
/// "supported targets" error text — so those never drift apart. Keep in sync with
/// the `targets` enum in `schema/ai.schema.json` (guarded by a schema round-trip
/// test).
pub const ALL_TARGETS: &[&str] = &["claude", "copilot"];

pub fn for_target(target: &str) -> Result<Box<dyn Vendor>> {
    match target {
        "claude" => Ok(Box::new(claude::Claude)),
        "copilot" => Ok(Box::new(copilot::Copilot)),
        other => bail!(
            "unknown target `{other}` (supported: {})",
            ALL_TARGETS.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard the drift surface: every advertised target must actually resolve to
    /// a vendor, and every resolvable name must be advertised. If someone adds a
    /// match arm to `for_target` without listing it in `ALL_TARGETS` (or vice
    /// versa) this fails.
    #[test]
    fn all_targets_resolve_and_report_their_own_name() {
        for t in ALL_TARGETS {
            let v = for_target(t).expect("advertised target must resolve");
            assert_eq!(&v.name(), t, "vendor name must match its target key");
        }
    }
}
