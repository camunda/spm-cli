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
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, Meta, Token};

use crate::source::rust_files;

/// The attribute's name, without a raw-identifier prefix (`r#allow`).
fn name(meta: &Meta) -> Option<String> {
    meta.path().get_ident().map(|i| i.unraw().to_string())
}

fn is_lint_level(meta: &Meta) -> bool {
    matches!(name(meta).as_deref(), Some("allow" | "expect"))
}

/// The comma-separated arguments of a list attribute, or `None` if they don't
/// parse as meta items.
fn args(meta: &Meta) -> Option<Vec<Meta>> {
    match meta {
        Meta::List(list) => list
            .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .ok()
            .map(|p| p.into_iter().collect()),
        _ => Some(Vec::new()),
    }
}

fn nested(meta: &Meta) -> Vec<Meta> {
    args(meta).unwrap_or_default()
}

/// Whether `pred` (a `cfg` predicate) can only hold when `test` does.
fn requires_test(pred: &Meta) -> bool {
    match name(pred).as_deref() {
        Some("test") => matches!(pred, Meta::Path(_)),
        Some("all") => nested(pred).iter().any(requires_test),
        Some("any") => {
            let preds = nested(pred);
            !preds.is_empty() && preds.iter().all(requires_test)
        }
        _ => false,
    }
}

fn is_cfg_test(attr: &Attribute) -> bool {
    let meta = &attr.meta;
    name(meta).as_deref() == Some("cfg") && nested(meta).first().is_some_and(requires_test)
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

macro_rules! skip_test_only {
    ($($method:ident($node:ty) via $walk:ident;)*) => {
        $(fn $method(&mut self, node: &'ast $node) {
            if !node.attrs.iter().any(is_cfg_test) {
                visit::$walk(self, node);
            }
        })*
    };
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

    skip_test_only! {
        visit_file(syn::File) via visit_file;
        visit_item_const(syn::ItemConst) via visit_item_const;
        visit_item_enum(syn::ItemEnum) via visit_item_enum;
        visit_item_extern_crate(syn::ItemExternCrate) via visit_item_extern_crate;
        visit_item_fn(syn::ItemFn) via visit_item_fn;
        visit_item_foreign_mod(syn::ItemForeignMod) via visit_item_foreign_mod;
        visit_item_impl(syn::ItemImpl) via visit_item_impl;
        visit_item_macro(syn::ItemMacro) via visit_item_macro;
        visit_item_mod(syn::ItemMod) via visit_item_mod;
        visit_item_static(syn::ItemStatic) via visit_item_static;
        visit_item_struct(syn::ItemStruct) via visit_item_struct;
        visit_item_trait(syn::ItemTrait) via visit_item_trait;
        visit_item_trait_alias(syn::ItemTraitAlias) via visit_item_trait_alias;
        visit_item_type(syn::ItemType) via visit_item_type;
        visit_item_union(syn::ItemUnion) via visit_item_union;
        visit_item_use(syn::ItemUse) via visit_item_use;
        visit_foreign_item_fn(syn::ForeignItemFn) via visit_foreign_item_fn;
        visit_foreign_item_static(syn::ForeignItemStatic) via visit_foreign_item_static;
        visit_foreign_item_type(syn::ForeignItemType) via visit_foreign_item_type;
        visit_foreign_item_macro(syn::ForeignItemMacro) via visit_foreign_item_macro;
        visit_trait_item_const(syn::TraitItemConst) via visit_trait_item_const;
        visit_trait_item_fn(syn::TraitItemFn) via visit_trait_item_fn;
        visit_trait_item_type(syn::TraitItemType) via visit_trait_item_type;
        visit_trait_item_macro(syn::TraitItemMacro) via visit_trait_item_macro;
        visit_impl_item_const(syn::ImplItemConst) via visit_impl_item_const;
        visit_impl_item_fn(syn::ImplItemFn) via visit_impl_item_fn;
        visit_impl_item_type(syn::ImplItemType) via visit_impl_item_type;
        visit_impl_item_macro(syn::ImplItemMacro) via visit_impl_item_macro;
        visit_field(syn::Field) via visit_field;
        visit_variant(syn::Variant) via visit_variant;
        visit_arm(syn::Arm) via visit_arm;
        visit_field_value(syn::FieldValue) via visit_field_value;
        visit_local(syn::Local) via visit_local;
        visit_type_param(syn::TypeParam) via visit_type_param;
        visit_const_param(syn::ConstParam) via visit_const_param;
        visit_lifetime_param(syn::LifetimeParam) via visit_lifetime_param;
        visit_receiver(syn::Receiver) via visit_receiver;
        visit_pat_type(syn::PatType) via visit_pat_type;
        visit_expr_array(syn::ExprArray) via visit_expr_array;
        visit_expr_assign(syn::ExprAssign) via visit_expr_assign;
        visit_expr_async(syn::ExprAsync) via visit_expr_async;
        visit_expr_await(syn::ExprAwait) via visit_expr_await;
        visit_expr_binary(syn::ExprBinary) via visit_expr_binary;
        visit_expr_block(syn::ExprBlock) via visit_expr_block;
        visit_expr_break(syn::ExprBreak) via visit_expr_break;
        visit_expr_call(syn::ExprCall) via visit_expr_call;
        visit_expr_cast(syn::ExprCast) via visit_expr_cast;
        visit_expr_closure(syn::ExprClosure) via visit_expr_closure;
        visit_expr_const(syn::ExprConst) via visit_expr_const;
        visit_expr_continue(syn::ExprContinue) via visit_expr_continue;
        visit_expr_field(syn::ExprField) via visit_expr_field;
        visit_expr_for_loop(syn::ExprForLoop) via visit_expr_for_loop;
        visit_expr_group(syn::ExprGroup) via visit_expr_group;
        visit_expr_if(syn::ExprIf) via visit_expr_if;
        visit_expr_index(syn::ExprIndex) via visit_expr_index;
        visit_expr_infer(syn::ExprInfer) via visit_expr_infer;
        visit_expr_let(syn::ExprLet) via visit_expr_let;
        visit_expr_lit(syn::ExprLit) via visit_expr_lit;
        visit_expr_loop(syn::ExprLoop) via visit_expr_loop;
        visit_expr_macro(syn::ExprMacro) via visit_expr_macro;
        visit_expr_match(syn::ExprMatch) via visit_expr_match;
        visit_expr_method_call(syn::ExprMethodCall) via visit_expr_method_call;
        visit_expr_paren(syn::ExprParen) via visit_expr_paren;
        visit_expr_path(syn::ExprPath) via visit_expr_path;
        visit_expr_range(syn::ExprRange) via visit_expr_range;
        visit_expr_raw_addr(syn::ExprRawAddr) via visit_expr_raw_addr;
        visit_expr_reference(syn::ExprReference) via visit_expr_reference;
        visit_expr_repeat(syn::ExprRepeat) via visit_expr_repeat;
        visit_expr_return(syn::ExprReturn) via visit_expr_return;
        visit_expr_struct(syn::ExprStruct) via visit_expr_struct;
        visit_expr_try(syn::ExprTry) via visit_expr_try;
        visit_expr_try_block(syn::ExprTryBlock) via visit_expr_try_block;
        visit_expr_tuple(syn::ExprTuple) via visit_expr_tuple;
        visit_expr_unary(syn::ExprUnary) via visit_expr_unary;
        visit_expr_unsafe(syn::ExprUnsafe) via visit_expr_unsafe;
        visit_expr_while(syn::ExprWhile) via visit_expr_while;
        visit_expr_yield(syn::ExprYield) via visit_expr_yield;
        visit_stmt_macro(syn::StmtMacro) via visit_stmt_macro;
    }
}

/// 1-based lines of `src` holding a lint suppression outside test code.
fn suppressions(src: &str) -> Vec<usize> {
    let file = syn::parse_file(src).unwrap_or_else(|e| panic!("cannot parse source: {e}"));
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
