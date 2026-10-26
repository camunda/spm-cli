//! Lint policy guard: shipped code in `src/` never silences a lint. Warnings
//! are fixed, not suppressed (AGENTS.md "Always-green policy"), so any
//! `#[allow(..)]`, `#![allow(..)]`, `#[expect(..)]` or a `cfg_attr` wrapping
//! one fails here with its `file:line`.
//!
//! The source is parsed with `syn`, not scanned: a node gated by a `cfg` that
//! requires `test` is skipped whole (so field, arm, parameter and generic
//! boundaries are the parser's, not ours), and anything the guard doesn't
//! recognise as test-only counts as shipped, so a gap can only over-report.

use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};
use syn::{Attribute, Meta};

use crate::source::{args, name, parse, requires_test, rust_files, skip_test_only};

fn is_lint_level(meta: &Meta) -> bool {
    matches!(name(meta).as_deref(), Some("allow" | "expect"))
}

/// Whether `meta` is a lint-level attribute, or a `cfg_attr` (that can apply
/// in a shipped build) wrapping one.
fn suppresses(meta: &Meta) -> bool {
    if is_lint_level(meta) {
        return true;
    }
    if name(meta).as_deref() != Some("cfg_attr") {
        return false;
    }
    // Arguments that don't parse can't be proven test-only: they count.
    let Some(args) = args(meta) else {
        return true;
    };
    let shipped = args.first().is_some_and(|p| !requires_test(p));
    shipped && args.iter().skip(1).any(suppresses)
}

/// Lines of lint suppressions in `tokens`, which syn leaves unparsed (macro
/// bodies): any `#[allow(..)]`-shaped attribute counts.
fn token_suppressions(tokens: TokenStream, lines: &mut Vec<usize>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for (k, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Punct(p) if p.as_char() == '#' => {
                let bracket = trees[k + 1..].iter().find(|t| match t {
                    TokenTree::Punct(p) => p.as_char() != '!',
                    _ => true,
                });
                if let Some(TokenTree::Group(g)) = bracket {
                    let body = g.stream();
                    if let Ok(meta) = syn::parse2::<Meta>(body) {
                        if suppresses(&meta) {
                            lines.push(p.span().start().line);
                        }
                    }
                }
            }
            TokenTree::Group(g) => token_suppressions(g.stream(), lines),
            _ => {}
        }
    }
}

#[derive(Default)]
struct Guard {
    lines: Vec<usize>,
}

impl<'ast> Visit<'ast> for Guard {
    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        if suppresses(&attr.meta) {
            self.lines.push(attr.pound_token.spans[0].start().line);
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        token_suppressions(mac.tokens.clone(), &mut self.lines);
        visit::visit_macro(self, mac);
    }

    skip_test_only!();
}

/// 1-based lines of `src` holding a lint suppression outside test code.
fn suppressions(src: &str) -> Vec<usize> {
    let file = parse(src);
    let mut guard = Guard::default();
    guard.visit_file(&file);
    guard.lines.sort_unstable();
    guard.lines.dedup();
    guard.lines
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
#[allow(dead_code)] fn a() {}
#[expect(dead_code)] fn b() {}
#[r#allow(dead_code)] fn c() {}
#[cfg_attr(unix, allow(dead_code))] fn d() {}
#[cfg_attr(unix, cfg_attr(unix, expect(dead_code)))] fn e() {}
#[r#cfg_attr(unix, allow(dead_code))] fn h() {}
#[cfg_attr(feature = \"x\" + 1, allow(dead_code))] fn i() {}
mod live { #![allow(unused)] }
fn f(x: u8) { #[allow(unused)] let _y = x; }
macro_rules! m { () => { #[allow(unused)] fn g() {} }; }
";
    assert_eq!(suppressions(src), vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
}

/// Items, fields, arms, parameters, generics and modules gated on a predicate
/// that requires `test` are exempt, whatever their shape or attribute order;
/// anything else (`any(test, unix)`, `not(test)`) ships.
#[test]
fn suppressions_exempt_exactly_test_only_code() {
    let src = "\
#[allow(a)] #[cfg(test)] fn t1() {}
#[cfg(all(test, not(windows)))] mod t2 { #[allow(b)] fn f() {} #[allow(c)] fn g() {} }
mod t3 { #![cfg(test)] #[allow(d)] fn f() {} #[allow(e)] fn g() {} }
#[cfg(test)] fn t4<A, B>() where A: Copy, B: Copy { #[allow(f)] let _ = 0; }
#[cfg(test)] struct T5<const N: usize = { 1 + 1 }> { #[allow(g)] b: [u8; N] }
#[cfg_attr(test, allow(h))] fn t6() {}
#[cfg(test)] macro_rules! t7 { () => { #[allow(i)] fn x() {} }; }
#[cfg(any(test, unix))] #[allow(live1)] fn l1() {}
#[cfg(not(test))] #[allow(live2)] fn l2() {}
struct S { #[cfg(test)] flag: bool }
#[allow(live3)] fn l3() {}
fn g<#[cfg(test)] T>() { #[allow(live4)] let _ = 0; }
pub fn k<#[cfg(test)] const N: usize, #[allow(live5)] t>() {}
fn p(#[cfg(test)] _x: u8) { #[allow(live6)] let _ = 0; #[cfg(test)] { #[allow(k)] let _ = 1; } }
fn e(x: u8) {
    match x {
        #[cfg(test)] 0 if x < 1 => { #[allow(j)] let _ = x; }
        #[cfg(test)] r#type if r#type < 1 => true,
        #[allow(live7)] _ => {}
    }
}
";
    assert_eq!(suppressions(src), vec![8, 9, 11, 12, 13, 14, 19]);
}

#[test]
fn suppressions_ignores_comments_and_literals() {
    let src = "\
// #[allow(a)]
/* #[allow(b)] /* nested */ */
const S: &str = \"#[allow(c)]\";
const R: &str = r#\"#[allow(d)]\"#;
const C: &core::ffi::CStr = cr#\"a\"b\"#;
#[allow(live)] fn f() {}
";
    assert_eq!(suppressions(src), vec![6]);
}
