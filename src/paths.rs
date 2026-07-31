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
