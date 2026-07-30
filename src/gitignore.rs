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

/// Remove the `comment` + `entry` block spm added to `.gitignore`. Only the
/// spm-owned block is removed: the `comment` line is always dropped, but an
/// `entry` line is dropped **only when it immediately follows** that comment
/// (i.e. the block spm actually writes). An identical ignore the user authored
/// independently — without spm's comment above it — is preserved.
pub fn remove(project_root: &Path, comment: &str, entry: &str) -> Result<()> {
    let path = project_root.join(".gitignore");
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };

    let mut kept: Vec<&str> = Vec::new();
    let mut prev_was_spm_comment = false;
    for line in existing.lines() {
        let t = line.trim();
        if t == comment {
            // Always drop spm's own comment line.
            prev_was_spm_comment = true;
            continue;
        }
        if t == entry && prev_was_spm_comment {
            // Drop the entry only when it is the one spm wrote under its comment.
            prev_was_spm_comment = false;
            continue;
        }
        prev_was_spm_comment = false;
        kept.push(line);
    }
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

    /// Regression: an ignore the user authored themselves (no spm comment above
    /// it) must survive `remove`, even if it is textually identical to `entry`.
    #[test]
    fn remove_preserves_user_authored_entry_without_spm_comment() {
        let tmp = std::env::temp_dir().join(format!("spm-gi-user-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let gi = tmp.join(".gitignore");
        // User already ignores `.spm-test/` for their own reasons — no spm block.
        std::fs::write(&gi, format!("target/\n{ENTRY}\n")).unwrap();

        remove(&tmp, COMMENT, ENTRY).unwrap();
        let after = std::fs::read_to_string(&gi).unwrap();
        assert!(
            after.contains(ENTRY),
            "user-authored entry must be preserved: {after}"
        );
        assert_eq!(after, format!("target/\n{ENTRY}\n"));

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// The spm-owned block is removed while an identical user-authored ignore
    /// elsewhere in the file is left intact.
    #[test]
    fn remove_deletes_only_spm_block_keeping_duplicate_user_entry() {
        let tmp = std::env::temp_dir().join(format!("spm-gi-dup-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let gi = tmp.join(".gitignore");
        // A user-authored copy up top, plus spm's own block below.
        let content = format!("{ENTRY}\n\n{COMMENT}\n{ENTRY}\n");
        std::fs::write(&gi, &content).unwrap();

        remove(&tmp, COMMENT, ENTRY).unwrap();
        let after = std::fs::read_to_string(&gi).unwrap();
        assert!(!after.contains(COMMENT), "spm comment gone: {after}");
        assert_eq!(
            after.matches(ENTRY).count(),
            1,
            "only the user-authored entry remains: {after}"
        );
        assert_eq!(after, format!("{ENTRY}\n"));

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
