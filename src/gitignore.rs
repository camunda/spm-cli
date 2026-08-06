//! Idempotent helpers for managing spm-owned entries in a project's
//! `.gitignore`.
//!
//! Both vendor adapters materialize skills into the project tree and must keep
//! those materialized files out of version control. This is the single
//! canonical implementation they share, parameterized by the `comment` + `entry`
//! each vendor owns.

use anyhow::{Context, Result};
use std::io::ErrorKind;
use std::path::Path;

/// Read an existing `.gitignore`, returning `None` when the file simply does
/// not exist. Any other error (invalid UTF-8, permissions, ...) is surfaced
/// with context instead of being silently treated as an empty file, which could
/// otherwise clobber real content.
fn read_existing(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Ensure `entry` (preceded by an explanatory `comment`) is present in the
/// project's `.gitignore`, appending the block once. Idempotent: an existing
/// `entry` line (however authored) is left untouched.
pub fn ensure(project_root: &Path, comment: &str, entry: &str) -> Result<()> {
    let path = project_root.join(".gitignore");
    let existing = read_existing(&path)?.unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;

    const COMMENT: &str = "# spm-managed test entry";
    const ENTRY: &str = ".spm-test/";

    #[test]
    fn ensure_appends_block_and_is_idempotent() {
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

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A `.gitignore` that can't be decoded as UTF-8 must surface an error
    /// rather than being silently treated as empty (which would clobber it).
    #[test]
    fn ensure_surfaces_read_error_instead_of_clobbering() {
        let tmp = std::env::temp_dir().join(format!("spm-gi-badutf8-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let gi = tmp.join(".gitignore");
        // Invalid UTF-8 bytes: read_to_string fails with InvalidData, not NotFound.
        std::fs::write(&gi, [0xff, 0xfe, 0x00, 0x9f]).unwrap();

        let err = ensure(&tmp, COMMENT, ENTRY);
        assert!(err.is_err(), "expected a surfaced read error");
        // The original bytes must be left intact, not overwritten.
        assert_eq!(std::fs::read(&gi).unwrap(), vec![0xff, 0xfe, 0x00, 0x9f]);

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
