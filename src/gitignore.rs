//! Idempotent helpers for managing spm-owned entries in a project's
//! `.gitignore`.
//!
//! Both vendor adapters materialize skills into the project tree and must keep
//! those materialized files out of version control. This is the single
//! canonical implementation they share, parameterized by the `comment` + `entry`
//! each vendor owns.

use anyhow::{Context, Result};
use std::path::Path;

/// Ensure `entry` (preceded by an explanatory `comment`) is present in the
/// project's `.gitignore`, appending the block once. Idempotent: an existing
/// `entry` line (however authored) is left untouched.
pub fn ensure(project_root: &Path, comment: &str, entry: &str) -> Result<()> {
    let path = project_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == entry) {
        return Ok(());
    }

    let mut out = existing;
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n'); // blank line separating our block from prior content
    }
    out.push_str(comment);
    out.push('\n');
    out.push_str(entry);
    out.push('\n');

    std::fs::write(&path, out).with_context(|| format!("updating {}", path.display()))
}

/// Remove the `comment` + `entry` block spm added to `.gitignore`. Lines the
/// user added independently are preserved.
pub fn remove(project_root: &Path, comment: &str, entry: &str) -> Result<()> {
    let path = project_root.join(".gitignore");
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };

    let mut kept: Vec<&str> = existing
        .lines()
        .filter(|l| {
            let t = l.trim();
            t != entry && t != comment
        })
        .collect();
    // Trim trailing blank lines left behind by the removal.
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }

    let out = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    std::fs::write(&path, out).with_context(|| format!("updating {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMENT: &str = "# spm-managed test entry";
    const ENTRY: &str = ".spm-test/";

    #[test]
    fn ensure_and_remove_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("spm-gi-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let gi = tmp.join(".gitignore");
        std::fs::write(&gi, "target/\n").unwrap();

        ensure(&tmp, COMMENT, ENTRY).unwrap();
        let after = std::fs::read_to_string(&gi).unwrap();
        assert!(after.contains(ENTRY), "{after}");
        assert!(after.contains(COMMENT), "{after}");
        assert!(
            after.starts_with("target/\n"),
            "preserves prior content: {after}"
        );

        // Idempotent: a second call must not duplicate the entry.
        ensure(&tmp, COMMENT, ENTRY).unwrap();
        let twice = std::fs::read_to_string(&gi).unwrap();
        assert_eq!(twice.matches(ENTRY).count(), 1, "{twice}");

        remove(&tmp, COMMENT, ENTRY).unwrap();
        let cleaned = std::fs::read_to_string(&gi).unwrap();
        assert!(!cleaned.contains(ENTRY), "{cleaned}");
        assert!(!cleaned.contains(COMMENT), "{cleaned}");
        assert_eq!(cleaned, "target/\n");

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
