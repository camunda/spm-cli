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
/// is skipped whole and anything else is walked as usual. With
/// `except visit_item_mod`, the implementor writes that one itself.
macro_rules! skip_test_only {
    () => {
        $crate::source::skip_test_only!(except visit_item_mod);
        $crate::source::skip_test_only! {
            visit_item_mod(syn::ItemMod) via visit_item_mod;
        }
    };
    (except visit_item_mod) => {
        $crate::source::skip_test_only! {
            visit_file(syn::File) via visit_file;
            visit_item_const(syn::ItemConst) via visit_item_const;
            visit_item_enum(syn::ItemEnum) via visit_item_enum;
            visit_item_extern_crate(syn::ItemExternCrate) via visit_item_extern_crate;
            visit_item_fn(syn::ItemFn) via visit_item_fn;
            visit_item_foreign_mod(syn::ItemForeignMod) via visit_item_foreign_mod;
            visit_item_impl(syn::ItemImpl) via visit_item_impl;
            visit_item_macro(syn::ItemMacro) via visit_item_macro;
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

/// Whether `meta` sets a module's file (`path = ".."`), directly or inside
/// a `cfg_attr`.
fn sets_path(meta: &Meta) -> bool {
    match name(meta).as_deref() {
        Some("path") => true,
        Some("cfg_attr") => args(meta).is_none_or(|a| a.iter().skip(1).any(sets_path)),
        _ => false,
    }
}

/// The out-of-line modules (`mod x;`) a file declares, as the inline-module
/// path leading to each, its name and whether only test code declares it.
#[derive(Default)]
struct ModDecls {
    inline: Vec<String>,
    test: bool,
    found: Vec<(Vec<String>, String, bool)>,
}

impl<'ast> syn::visit::Visit<'ast> for ModDecls {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        assert!(
            !m.attrs.iter().any(|a| sets_path(&a.meta)),
            "`#[path]` (or a `cfg_attr` setting one) on `mod {}` is not supported by \
             the source guards: teach `source::shipped_files` to follow it",
            m.ident
        );
        let test = self.test || m.attrs.iter().any(is_cfg_test);
        let name = m.ident.unraw().to_string();
        if m.content.is_none() {
            self.found.push((self.inline.clone(), name, test));
            return;
        }
        let outer = std::mem::replace(&mut self.test, test);
        self.inline.push(name);
        syn::visit::visit_item_mod(self, m);
        self.inline.pop();
        self.test = outer;
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // Test-only code ships nothing, whatever it generates.
        if self.test {
            return syn::visit::visit_macro(self, mac);
        }
        let included = mac
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident == "include")
            || emits_include(mac.tokens.clone());
        assert!(
            !included,
            "`include!` is not supported by the source guards, which cannot \
             tell which module owns the included code: declare it as a module"
        );
        let found = macro_mod_decls(mac.tokens.clone());
        assert!(
            found.is_empty(),
            "`{}` emitted by a macro is not supported by the source guards, \
             which cannot tell whether (or where) it ships: declare it directly",
            found.join("`, `")
        );
        syn::visit::visit_macro(self, mac);
    }

    skip_test_only!(except visit_item_mod);
}

/// Whether macro tokens invoke `include!` (at any nesting).
fn emits_include(tokens: proc_macro2::TokenStream) -> bool {
    use proc_macro2::TokenTree;
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    trees.iter().enumerate().any(|(k, tree)| match tree {
        TokenTree::Ident(i) => {
            i == "include"
                && matches!(trees.get(k + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!')
        }
        TokenTree::Group(g) => emits_include(g.stream()),
        _ => false,
    })
}

/// The module declarations in macro tokens other than an inline module with a
/// literal name (`mod x { .. }`): `mod x;`, and any whose name is captured
/// (`mod $name;`, `mod $name { .. }`, also inside repetitions), which may
/// declare a file the guards would then never classify.
fn macro_mod_decls(tokens: proc_macro2::TokenStream) -> Vec<String> {
    use proc_macro2::{Delimiter, TokenTree};
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let mut found = Vec::new();
    for (k, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Ident(m) if m == "mod" => {
                let inline = matches!(
                    (trees.get(k + 1), trees.get(k + 2)),
                    (Some(TokenTree::Ident(_)), Some(TokenTree::Group(g)))
                        if g.delimiter() == Delimiter::Brace
                );
                if !inline {
                    let next: Vec<String> =
                        trees[k..].iter().take(3).map(|t| t.to_string()).collect();
                    found.push(next.join(" "));
                }
            }
            TokenTree::Group(g) => found.extend(macro_mod_decls(g.stream())),
            _ => {}
        }
    }
    found
}

/// The module files of the crate rooted at `root` (`src/main.rs`) that a
/// non-test build compiles: the root plus every file reached through a
/// `mod x;` declaration outside test-only code. A file reached only through
/// test code (`#[cfg(test)] mod tests;`) ships nothing, so the guards never
/// read it. Every other `.rs` file under the root's directory must be one of
/// the two: a file the walk cannot classify (declared by a macro, behind
/// test-only items other than modules, or not at all) fails the guard rather
/// than escape it.
pub fn shipped_files(root: &Path) -> Vec<PathBuf> {
    let (mut shipped, mut test_only) = (Vec::new(), Vec::new());
    let mut queue = vec![(root.to_path_buf(), false)];
    while let Some((file, test)) = queue.pop() {
        let src = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        let parsed = parse(&src);
        // A file under `#![cfg(test)]` is test-only, and so is all it declares.
        let test = test || parsed.attrs.iter().any(is_cfg_test);
        let mut decls = ModDecls {
            test,
            ..ModDecls::default()
        };
        for item in &parsed.items {
            syn::visit::Visit::visit_item(&mut decls, item);
        }
        // `main.rs`/`mod.rs` own their directory; `x.rs` owns `x/`.
        let stem = file.file_stem().unwrap().to_string_lossy();
        let dir = if file == root || stem == "mod" {
            file.parent().unwrap().to_path_buf()
        } else {
            file.with_extension("")
        };
        for (inline, name, test) in decls.found {
            let base = inline.iter().fold(dir.clone(), |d, m| d.join(m));
            let flat = base.join(format!("{name}.rs"));
            let nested = base.join(&name).join("mod.rs");
            let found = [flat, nested].into_iter().find(|p| p.is_file());
            let found =
                found.unwrap_or_else(|| panic!("`mod {name};` in {} has no file", file.display()));
            queue.push((found, test));
        }
        if test { &mut test_only } else { &mut shipped }.push(file);
    }
    let mut all = Vec::new();
    rust_files(root.parent().unwrap(), &mut all);
    let unknown: Vec<String> = all
        .iter()
        .filter(|f| !shipped.contains(f) && !test_only.contains(f))
        .map(|f| format!("  {}", f.display()))
        .collect();
    assert!(
        unknown.is_empty(),
        "no `mod x;` the source guards can follow reaches these files, so they \
         cannot tell whether they ship:\n{}\nDeclare each with a plain \
         `mod x;` (not from a macro), or delete it if unused.",
        unknown.join("\n")
    );
    shipped.sort();
    shipped
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
