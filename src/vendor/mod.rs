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
}

pub fn for_target(target: &str) -> Result<Box<dyn Vendor>> {
    match target {
        "claude" => Ok(Box::new(claude::Claude)),
        "copilot" => Ok(Box::new(copilot::Copilot)),
        other => bail!("unknown target `{other}` (supported: claude, copilot)"),
    }
}
