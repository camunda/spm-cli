use anyhow::{Context, Result};
use std::path::PathBuf;

/// Root of the global spm store, e.g. `~/.spm`.
/// Skills are fetched here once and shared across every project (pnpm-style).
pub fn spm_home() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("SPM_HOME") {
        return Ok(PathBuf::from(custom));
    }
    let base = directories::BaseDirs::new().context("could not determine home directory")?;
    Ok(base.home_dir().join(".spm"))
}

/// Content store: one immutable dir per (repo, commit).
pub fn store_dir() -> Result<PathBuf> {
    Ok(spm_home()?.join("store"))
}

/// Where generated vendor config lives (outside the project tree).
pub fn vendors_dir() -> Result<PathBuf> {
    Ok(spm_home()?.join("vendors"))
}

/// Per-project generated dir for a vendor: `~/.spm/vendors/<vendor>/<project-key>`.
/// The key is the sanitized absolute project path (machine-specific, which is why
/// the pointer into it is never committed).
pub fn vendor_project_dir(vendor: &str, project_root: &std::path::Path) -> Result<PathBuf> {
    let key: String = project_root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    Ok(vendors_dir()?.join(vendor).join(key.trim_matches('_')))
}
