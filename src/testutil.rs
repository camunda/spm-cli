#![cfg(test)]
//! Helpers shared by unit tests across modules.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh scratch path under the temp dir, named `<prefix>-<pid>-<n>`: the
/// pid keeps test processes apart and a process-wide counter keeps parallel
/// tests apart. Not the wall clock, whose resolution lets two threads get the
/// same name (#195). Whatever an earlier run with the same pid left at the
/// path is removed; the path itself is not created.
pub(crate) fn scratch_path(prefix: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::remove_dir_all(&path).ok();
    path
}
