//! Source parsing shared by the guards that read `src/` (lint policy,
//! layout, architecture): files are parsed with `syn`, so comments and
//! literals are never code, and nodes gated by a `cfg` that requires `test`
//! are skipped whole at the parser's boundaries (fields, arms, parameters,
//! generics), so every guard judges exactly the code that ships.

use std::path::{Path, PathBuf};

use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::{Attribute, Meta, Token};

/// The attribute's name, without a raw-identifier prefix (`r#allow`).
pub fn name(meta: &Meta) -> Option<String> {
    meta.path().get_ident().map(|i| i.unraw().to_string())
}

/// The comma-separated arguments of a list attribute, or `None` if they don't
/// parse as meta items.
pub fn args(meta: &Meta) -> Option<Vec<Meta>> {
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
pub fn requires_test(pred: &Meta) -> bool {
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

/// Whether `attr` is a `cfg` that enables its node only under test.
pub fn is_cfg_test(attr: &Attribute) -> bool {
    let meta = &attr.meta;
    name(meta).as_deref() == Some("cfg") && nested(meta).first().is_some_and(requires_test)
}

/// Implements, inside an `impl Visit<'ast>`, every visit method of a node
/// that carries attributes so that a node gated on a `cfg` requiring `test`
/// is skipped whole and anything else is walked as usual.
macro_rules! skip_test_only {
    () => {
        $crate::source::skip_test_only! {
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
    };
    ($($method:ident($node:ty) via $walk:ident;)*) => {
        $(fn $method(&mut self, node: &'ast $node) {
            if !node.attrs.iter().any($crate::source::is_cfg_test) {
                syn::visit::$walk(self, node);
            }
        })*
    };
}
pub(crate) use skip_test_only;

/// Parses a `src/` file, failing the guard on a syntax `syn` cannot read.
pub fn parse(src: &str) -> syn::File {
    syn::parse_file(src).unwrap_or_else(|e| panic!("cannot parse source: {e}"))
}

/// Every `.rs` file under `dir`, recursively.
pub fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
