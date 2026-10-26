#![cfg(test)]
//! Fixture helpers shared by the scanner's unit tests.

use std::path::{Path, PathBuf};

use super::engine::scan_path;
use super::finding::Finding;

/// A fresh, unique scratch directory.
pub(super) fn tmp_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "spm-scan-dir-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Scan a scratch tree holding the single file `name` (a relative path, whose
/// parent dirs are created) with `body`.
pub(super) fn scan_str(name: &str, body: impl AsRef<[u8]>) -> Vec<Finding> {
    let dir = tmp_dir();
    let file = dir.join(Path::new(name));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, body).unwrap();
    let out = scan_path(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    out
}

pub(super) fn has(findings: &[Finding], rule: &str) -> bool {
    findings.iter().any(|f| f.rule == rule)
}

/// Minimal encoder used only to build base64 test fixtures.
pub(super) fn base64_encode(input: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
