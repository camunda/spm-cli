//! Test-hygiene guard: test code never sleeps, never times itself against the
//! wall clock and never retries (AGENTS.md "No Such Thing as Flaky Tests",
//! "No Test Retries"). A test that waits for "long enough", asserts a
//! duration, or passes on a later attempt is non-deterministic; synchronize
//! on an event instead (see the barriers in `concurrency.rs`).
//!
//! Test code is everything under `tests/` plus every `src/` node or file
//! gated on a `cfg` requiring `test`. It is parsed with `syn`, so comments,
//! string literals and the names of `#[test]` functions (which may describe
//! the product's own retry behavior) are never flagged; identifiers inside
//! macro bodies (`assert!`, `proptest!`) are. CI workflows and a nextest
//! config, if any, may not retry or re-run either.
//!
//! This is a ratchet at zero: nothing is allow-listed.

use std::path::{Path, PathBuf};

use proc_macro2::{Ident, TokenStream, TokenTree};
use syn::ext::IdentExt;
use syn::visit::{self, Visit};
use syn::{Attribute, Meta};

use crate::source::{crate_files, parse, rust_files, track_test_only};

/// Why `ident` may not appear in test code, if it may not.
fn forbidden(ident: &Ident) -> Option<&'static str> {
    let name = ident.unraw().to_string();
    let lower = name.to_ascii_lowercase();
    if lower.contains("sleep") {
        Some("sleeps: wait on an event (channel, barrier, child exit) instead")
    } else if name == "Instant" || name == "elapsed" {
        Some("measures wall-clock time: assert on behavior, not on how long it took")
    } else if ["retry", "retries", "rerun", "flaky"]
        .iter()
        .any(|w| lower.contains(w))
    {
        Some("retries: a test must pass or fail deterministically on its first run")
    } else {
        None
    }
}

struct Guard {
    in_test: bool,
    /// Set by a `#[test]` attribute until its function's signature is seen,
    /// so that the function's own name is not judged.
    test_fn: bool,
    found: Vec<(usize, String)>,
}

impl Guard {
    fn report(&mut self, ident: &Ident) {
        if let Some(why) = forbidden(ident) {
            self.found
                .push((ident.span().start().line, format!("`{ident}` {why}")));
        }
    }

    fn tokens(&mut self, tokens: TokenStream) {
        for tree in tokens {
            match tree {
                TokenTree::Ident(i) => self.report(&i),
                TokenTree::Group(g) => self.tokens(g.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Guard {
    fn visit_ident(&mut self, ident: &'ast Ident) {
        if self.in_test {
            self.report(ident);
        }
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        if matches!(&attr.meta, Meta::Path(p) if p.is_ident("test")) {
            self.test_fn = true;
        }
        if let (true, Meta::List(list)) = (self.in_test, &attr.meta) {
            self.tokens(list.tokens.clone());
        }
        visit::visit_attribute(self, attr);
    }

    fn visit_signature(&mut self, sig: &'ast syn::Signature) {
        if !std::mem::take(&mut self.test_fn) {
            return visit::visit_signature(self, sig);
        }
        self.visit_generics(&sig.generics);
        for arg in &sig.inputs {
            self.visit_fn_arg(arg);
        }
        self.visit_return_type(&sig.output);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if self.in_test {
            self.tokens(mac.tokens.clone());
        }
        visit::visit_macro(self, mac);
    }

    track_test_only!();
}

/// `(line, why)` of each forbidden identifier in the test code of `src`;
/// `test` says whether the whole file is test code.
fn violations(src: &str, test: bool) -> Vec<(usize, String)> {
    let mut guard = Guard {
        in_test: test,
        test_fn: false,
        found: Vec::new(),
    };
    guard.visit_file(&parse(src));
    guard.found.sort();
    guard.found.dedup();
    guard.found
}

#[test]
fn test_code_never_sleeps_times_or_retries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (shipped, src_tests) = crate_files(&root.join("src/main.rs"));
    let mut suites = Vec::new();
    rust_files(&root.join("tests"), &mut suites);
    assert!(
        suites.iter().any(|f| f.ends_with("tests/cli/main.rs"))
            && src_tests.iter().any(|f| f.ends_with("contract_tests.rs"))
            && shipped.len() > 10,
        "the walk must reach every kind of test code"
    );
    let files: Vec<(PathBuf, bool)> = shipped
        .into_iter()
        .map(|f| (f, false))
        .chain(src_tests.into_iter().chain(suites).map(|f| (f, true)))
        .collect();
    let mut offenders = Vec::new();
    for (file, test) in &files {
        let src = std::fs::read_to_string(file).unwrap();
        let rel = file.strip_prefix(root).unwrap_or(file).display();
        for (line, why) in violations(&src, *test) {
            offenders.push(format!("{rel}:{line}: {why}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "non-deterministic test code:\n{}",
        offenders.join("\n")
    );
}

/// CI and the test runner config may not paper over a failure either:
/// no retry step or action, no re-run on failure, no nextest `retries`.
#[test]
fn ci_never_retries_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut configs: Vec<PathBuf> = std::fs::read_dir(root.join(".github/workflows"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml"))
        .collect();
    assert!(configs.iter().any(|p| p.ends_with("ci.yml")), "{configs:?}");
    configs.push(root.join(".config/nextest.toml"));
    let mut offenders = Vec::new();
    for file in configs.iter().filter(|p| p.is_file()) {
        let text = std::fs::read_to_string(file).unwrap();
        for (k, line) in text.lines().enumerate() {
            if repeats(line) {
                let rel = file.strip_prefix(root).unwrap_or(file).display();
                offenders.push(format!("{rel}:{}: {}", k + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "fix the failure instead of retrying it:\n{}",
        offenders.join("\n")
    );
}

/// Whether a config line asks for a retry or a re-run.
fn repeats(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    ["retry", "retries", "rerun", "re-run"]
        .iter()
        .any(|w| lower.contains(w))
}

#[test]
fn violations_flag_sleeps_timing_and_retries() {
    let src = "\
fn a() { std::thread::sleep(d); }
fn b() { let t = std::time::Instant::now(); }
fn c() { assert!(t.elapsed() < d); }
fn with_retries() {}
#[flaky] fn d() {}
#[test_case(retries = 3)] fn e() {}
fn f() { proptest! { fn g() { tokio::time::sleep(d); } } }
use std::thread::sleep as nap;
";
    let lines: Vec<usize> = violations(src, true).iter().map(|v| v.0).collect();
    assert_eq!(lines, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

/// In a shipped file only `cfg(test)`-gated code is judged; comments,
/// literals and a `#[test]` function's own name never are.
#[test]
fn violations_judge_only_test_code_identifiers() {
    let src = "\
fn live() { std::thread::sleep(d); }
// thread::sleep(d)
const S: &str = \"retry Instant elapsed\";
#[cfg(test)]
mod tests {
    #[test]
    fn fallback_retries_the_other_protocol() { assert!(run(\"retry\")); }
    fn helper() { std::thread::sleep(d); }
}
#[cfg(all(test, unix))] fn retry_twice() {}
";
    let lines: Vec<usize> = violations(src, false).iter().map(|v| v.0).collect();
    assert_eq!(lines, vec![8, 10]);
}

#[test]
fn repeats_flags_ci_retry_forms() {
    assert!(repeats("      - uses: nick-fields/retry@v3"));
    assert!(repeats("retries = 2"));
    assert!(repeats("  run: gh run rerun --failed"));
    assert!(!repeats("  run: cargo test --all"));
}
