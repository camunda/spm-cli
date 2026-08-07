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
/// RAII guard that holds `ENV_LOCK` for its entire lifetime and restores
/// `SPM_HOME` to its pre-test value on drop, even if the test body panics —
/// otherwise a failing assertion would leave the env var mutated, and the
/// lock unreleased, for whichever test runs next in the same process.
pub(crate) struct SpmHomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Option<String>,
}

#[cfg(test)]
impl SpmHomeGuard {
    /// Locks `ENV_LOCK` (tolerating poisoning from an earlier panicked test,
    /// so one bad test doesn't cascade into every later test failing to
    /// acquire the lock) and snapshots the current `SPM_HOME`.
    pub(crate) fn set(value: &std::path::Path) -> Self {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("SPM_HOME").ok();
        std::env::set_var("SPM_HOME", value);
        Self { _lock, saved }
    }

    pub(crate) fn unset() -> Self {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("SPM_HOME").ok();
        std::env::remove_var("SPM_HOME");
        Self { _lock, saved }
    }
}

#[cfg(test)]
impl Drop for SpmHomeGuard {
    fn drop(&mut self) {
        match &self.saved {
            Some(v) => std::env::set_var("SPM_HOME", v),
            None => std::env::remove_var("SPM_HOME"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `SPM_HOME` unset, `spm_home`/`store_dir` must fall back to
    /// `~/.spm` (resp. `~/.spm/store`) rather than erroring — every other test
    /// in this codebase sandboxes `SPM_HOME`, so this default path is
    /// otherwise never exercised.
    #[test]
    fn spm_home_and_store_dir_default_when_env_unset() {
        let _guard = SpmHomeGuard::unset();

        let home = spm_home().expect("must fall back to the real home dir");
        assert!(
            home.ends_with(".spm"),
            "expected a `.spm` suffix, got {home:?}"
        );
        let store = store_dir().expect("store_dir must derive from spm_home");
        assert!(store.ends_with(".spm/store") || store.ends_with(".spm\\store"));
    }

    /// A custom `SPM_HOME` is honored verbatim (no `.spm` suffix appended).
    #[test]
    fn spm_home_honors_custom_env_var() {
        let custom = std::env::temp_dir().join("custom-spm-home-for-test");
        let _guard = SpmHomeGuard::set(&custom);

        let home = spm_home().unwrap();
        assert_eq!(home, custom);
    }
}
