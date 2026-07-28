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

/// A target tool (Claude, Copilot, ...) that can be taught to auto-load skills
/// from the store without copying them into the project tree.
pub trait Vendor {
    #[allow(dead_code)] // part of the adapter contract; not all call sites use it yet
    fn name(&self) -> &'static str;

    /// Generate/refresh whatever config makes this vendor load `skills`.
    fn materialize(&self, project_root: &Path, skills: &[MaterializedSkill]) -> Result<()>;

    /// Remove everything this vendor generated for `project_root`.
    fn clean(&self, project_root: &Path) -> Result<()>;
}

pub fn for_target(target: &str) -> Result<Box<dyn Vendor>> {
    match target {
        "claude" => Ok(Box::new(claude::Claude)),
        "copilot" => Ok(Box::new(copilot::Copilot)),
        other => bail!("unknown target `{other}` (supported: claude, copilot)"),
    }
}
