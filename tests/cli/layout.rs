//! Repository layout guards.

use std::path::{Path, PathBuf};

/// Most lines any Rust source or test file may hold: past this, split it into
/// focused modules (see `tests/cli/` and `src/git/`) rather than let one file
/// become a monolith again.
const MAX_FILE_LINES: usize = 2_500;

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_rust_file_exceeds_the_line_budget() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "tests"] {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(
        files.iter().any(|f| f.ends_with("tests/cli/main.rs")),
        "the walk must reach the test modules"
    );
    let over: Vec<String> = files
        .iter()
        .filter_map(|f| {
            let lines = std::fs::read_to_string(f).unwrap().lines().count();
            (lines > MAX_FILE_LINES).then(|| format!("{} ({lines} lines)", f.display()))
        })
        .collect();
    assert!(
        over.is_empty(),
        "split into focused modules, each at most {MAX_FILE_LINES} lines: {over:?}"
    );
}
