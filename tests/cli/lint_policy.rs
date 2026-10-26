//! Lint policy guard: shipped code in `src/` never silences a lint. Warnings
//! are fixed, not suppressed (AGENTS.md "Always-green policy"), so any
//! `#[allow(..)]`, `#![allow(..)]`, `#[expect(..)]` or a `cfg_attr` wrapping
//! one fails here with its `file:line`. Test code (`#[cfg(test)]` items) and
//! comments/string literals are not shipped and are ignored.

use std::path::Path;

use crate::source::{attribute, line_at, rust_files, shipped_code};

/// Whether a compacted attribute body silences a lint.
fn suppresses(body: &str) -> bool {
    let lint_level = |b: &str| b.starts_with("allow(") || b.starts_with("expect(");
    lint_level(body)
        || (body.starts_with("cfg_attr(")
            && (body.contains(",allow(") || body.contains(",expect(")))
}

/// 1-based lines of `src` holding a lint suppression outside test code.
fn suppressions(src: &str) -> Vec<usize> {
    let code = shipped_code(src);
    let mut lines = Vec::new();
    let mut i = 0;
    while i < code.len() {
        match attribute(&code, i) {
            Some((body, end)) => {
                if suppresses(&body) {
                    lines.push(line_at(&code, i));
                }
                i = end;
            }
            None => i += 1,
        }
    }
    lines
}

#[test]
fn src_never_suppresses_lints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    assert!(
        files.iter().any(|f| f.ends_with("src/main.rs")),
        "scan found no sources: {files:?}"
    );
    let mut offenders = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file).unwrap();
        for line in suppressions(&src) {
            let rel = file.strip_prefix(root).unwrap_or(file);
            offenders.push(format!("{}:{line}", rel.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "lint suppressions in shipped code (fix the warning instead):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn suppressions_flags_every_attribute_form() {
    let src = "\
fn a() {}
#[allow(dead_code)]
fn b() {}
#![allow(clippy::all)]
# [ allow ( unused ) ]
#[expect(dead_code)]
#[cfg_attr(feature = \"x\", allow(unused))]
#[cfg(not(test))]
mod live { #[allow(unused)] fn c() {} }
#[cfg(any(test, unix))]
mod m { #[allow(x)] fn f() {} }
struct T { #[cfg(test)] a: u8, #[allow(dead_code)] b: u8 }
struct U { a: u8, #[cfg(test)] b: u8 }
#[allow(after_field)]
fn d(x: u8) { match x { #[cfg(test)] 0 => {} #[allow(unused)] _ => {} } }
#[derive(Debug)]
struct S;
";
    assert_eq!(suppressions(src), vec![2, 4, 5, 6, 7, 9, 11, 12, 14, 15]);
}

#[test]
fn suppressions_ignores_test_code_comments_and_literals() {
    let src = r####"
// #[allow(dead_code)]
/* #[allow(dead_code)] /* nested */ #[allow(x)] */
/// docs: #[allow(dead_code)]
const S: &str = "#[allow(dead_code)] \" #[allow(x)]";
const R: &str = r#"#[allow(dead_code)]"#;
const C: char = '"';
fn lifetime<'a>(x: &'a str) -> &'a str { x }
#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn helper() { let _ = [0u8; 2]; }
}
#[cfg(test)]
#[allow(dead_code)]
fn test_only() {}
#[cfg(all(test, unix))]
mod unix_tests { #[allow(x)] fn h() {} }
#[cfg(test)] mod inline_tests { #[allow(x)] fn g() { let _ = (1, 2); } }
#[allow(after_tests)]
fn live() {}
"####;
    assert_eq!(suppressions(src), vec![20]);
}

/// Raw identifiers name the same attribute: `r#allow` silences a lint too.
#[test]
fn suppressions_flags_raw_identifier_attributes() {
    let src = "\
#[r#allow(dead_code)]
fn a() {}
#[cfg_attr(not(test), r#allow(dead_code))]
fn b() {}
#[r#expect(unused)]
fn c() {}
";
    assert_eq!(suppressions(src), vec![1, 3, 5]);
}

/// A test-only parameter ends at the `)` closing the parameter list; the
/// function body after it ships.
#[test]
fn suppressions_stops_a_test_only_item_at_its_enclosing_delimiter() {
    let src = "\
fn f(#[cfg(test)] _x: u8) {
    #[allow(unused_variables)]
    let x = 1;
}
fn g(a: [u8; 1], #[cfg(test)] _y: u8) -> u8 { #[allow(x)] let _ = 0; a[0] }
";
    assert_eq!(suppressions(src), vec![2, 5]);
}

/// The test-code exemption covers an item's whole attribute list, whatever
/// the order of its attributes.
#[test]
fn suppressions_ignores_attributes_before_a_test_only_cfg() {
    let src = "\
#[allow(dead_code)]
#[cfg(test)]
fn helper() {}
#[allow(dead_code)] /// docs
#[derive(Debug)] #[cfg(test)] struct T;
struct S { #[allow(dead_code)] #[cfg(test)] a: u8, b: u8 }
#[allow(after)]
fn live() {}
#[allow(before_live)]
fn live2() {}
#[cfg(test)]
fn t() {}
#![allow(inner)]
#[cfg(test)] mod m {}
";
    assert_eq!(suppressions(src), vec![7, 9, 13]);
}
