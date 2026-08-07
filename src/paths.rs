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

#[cfg(test)]
/// Serializes tests (here and in `store.rs`) that mutate the process-wide
/// `SPM_HOME` env var — `cargo test` runs unit tests concurrently within one
/// process, so unsynchronized mutation would be a data race between tests.
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// With `SPM_HOME` unset, `spm_home`/`store_dir` must fall back to
    /// `~/.spm` (resp. `~/.spm/store`) rather than erroring — every other test
    /// in this codebase sandboxes `SPM_HOME`, so this default path is
    /// otherwise never exercised.
    #[test]
    fn spm_home_and_store_dir_default_when_env_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved = std::env::var("SPM_HOME").ok();
        std::env::remove_var("SPM_HOME");

        let home = spm_home().expect("must fall back to the real home dir");
        assert!(
            home.ends_with(".spm"),
            "expected a `.spm` suffix, got {home:?}"
        );
        let store = store_dir().expect("store_dir must derive from spm_home");
        assert!(store.ends_with(".spm/store") || store.ends_with(".spm\\store"));

        match saved {
            Some(v) => std::env::set_var("SPM_HOME", v),
            None => std::env::remove_var("SPM_HOME"),
        }
    }

    /// A custom `SPM_HOME` is honored verbatim (no `.spm` suffix appended).
    #[test]
    fn spm_home_honors_custom_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved = std::env::var("SPM_HOME").ok();
        let custom = std::env::temp_dir().join("custom-spm-home-for-test");
        std::env::set_var("SPM_HOME", &custom);

        let home = spm_home().unwrap();
        assert_eq!(home, custom);

        match saved {
            Some(v) => std::env::set_var("SPM_HOME", v),
            None => std::env::remove_var("SPM_HOME"),
        }
    }
}
