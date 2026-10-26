//! Architecture guard: the top-level module graph of `src/` is pinned.
//!
//! Only module files a non-test build compiles are read (a file behind
//! `#[cfg(test)] mod tests;` is not), and they are parsed with `syn`, so
//! comments, doc links, string literals and `#[cfg(test)]` nodes (at the
//! parser's boundaries) are never read as code; raw identifiers are read as
//! their names. Every `crate::<module>` reference in what is left (grouped
//! `use crate::{a, b::c}` imports, inline paths, paths in macro bodies,
//! `super::` chains climbing to the crate root, also across `use` groups,
//! `self::x` and qualified `x::..` paths in `main.rs`, and whole-path strings
//! of non-doc attributes like `#[serde(with = "crate::x")]`, unless behind a
//! test-only `cfg_attr`, included) becomes an edge
//! between top-level modules, where a module is the first path component
//! under `src/` (`src/cli/**` is `cli`, `src/main.rs` is `main`). The result
//! must equal [`ALLOWED_EDGES`] exactly, so a layering change is a deliberate,
//! reviewed edit of that table, never a side effect. Crate-root globs and
//! aliases (`use crate::*`, `use super::*` out of a top-level module,
//! `use crate as x`, `use super as x` reaching the root,
//! `extern crate self as x`) would hide edges, so they fail, as do paths the
//! guard cannot resolve: macro fragments that may reach the root
//! (`crate::$m`, a bare `crate` passed to a macro) and relative paths in a
//! `macro_rules!` body (`super::`, `self::`, `$root::`), which resolve
//! wherever the macro is invoked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::ext::IdentExt;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::Token;

use crate::source::{
    args, cfg_test, is_cfg_test, parse, requires_test, shipped_files, skip_test_only,
};

/// The allowed top-level dependency edges: the single source of truth for the
/// layering of `src/`. A module absent here depends on no other module.
const ALLOWED_EDGES: &[(&str, &[&str])] = &[
    ("main", &["cli"]),
    (
        "cli",
        &[
            "fsutil",
            "git",
            "gitignore",
            "ids",
            "jsonutil",
            "links",
            "lockfile",
            "manifest",
            "owned",
            "pathid",
            "paths",
            "plugin",
            "policy",
            "resolver",
            "scan",
            "scope",
            "shell",
            "skillcheck",
            "store",
            "transitive",
            "vendor",
        ],
    ),
    (
        "transitive",
        &[
            "fsutil",
            "git",
            "ids",
            "links",
            "lockfile",
            "manifest",
            "policy",
            "resolver",
            "scan",
            "skillcheck",
            "store",
            "vendor",
        ],
    ),
    (
        "owned",
        &[
            "ids", "jsonfile", "links", "lockfile", "pathid", "paths", "scope", "vendor",
        ],
    ),
    (
        "vendor",
        &[
            "fsutil",
            "gitignore",
            "ids",
            "jsonutil",
            "paths",
            "plugin",
            "scope",
        ],
    ),
    (
        "plugin",
        &["fsutil", "ids", "manifest", "skillcheck", "vendor"],
    ),
    ("resolver", &["git", "ids", "lockfile", "manifest"]),
    (
        "links",
        &["ids", "jsonfile", "lockfile", "paths", "scope", "shell"],
    ),
    ("lockfile", &["ids", "jsonfile", "manifest"]),
    (
        "manifest",
        &["ids", "jsonfile", "policy", "schema", "scope"],
    ),
    ("policy", &["git", "manifest", "paths"]),
    ("git", &["ids", "policy"]),
    ("ids", &["manifest"]),
    ("store", &["git", "lockfile", "paths", "policy"]),
    ("scan", &["fsutil", "jsonutil"]),
    ("skillcheck", &["fsutil"]),
    ("jsonutil", &["jsonfile"]),
    ("scope", &["paths"]),
];

/// Modules that must depend on no other module of the crate.
const LEAVES: &[&str] = &["fsutil", "jsonfile", "pathid", "paths", "schema", "shell"];

/// The only modules allowed to depend on `cli`: it is the top of the stack.
const CLI_DEPENDENTS: &[&str] = &["main"];

/// Dependency cycles tolerated today, as the module groups they bind: each
/// group is a strongly connected component of the graph (every module in it
/// reaches every other). This list may only shrink: a group that grows or
/// merges with another, or a new cyclic group, fails as a layering regression
/// to fix, and a group that splits or shrinks fails as stale until the list is
/// shrunk to match.
const KNOWN_CYCLIC_GROUPS: &[&[&str]] =
    &[&["git", "ids", "manifest", "policy"], &["plugin", "vendor"]];

const TABLE: &str = "tests/cli/architecture.rs";

type Graph = BTreeMap<String, BTreeSet<String>>;

/// The name standing for a crate-root reference that hides which modules it
/// reaches: a glob (`crate::*`) or an alias of the root (`crate as x`).
const OPAQUE: &str = "*";

/// Marks a name that is an edge only if it names a top-level module: the
/// head of a qualified path in a `macro_rules!` body (`beta::run()`), which
/// resolves at the invocation, where it may be a root module.
const MAYBE: char = '?';

/// How a path ends: a name, a renamed name (`x as y`) or a glob.
enum Leaf {
    Name(String),
    Rename(String),
    Glob,
}

/// The crate-root name `prefix` + `leaf` reaches from a module `depth` levels
/// below the root, if it reaches one: the segment after `crate::`, after a
/// `super::` chain that climbs out of the top-level module, or (at the root)
/// after `self::` or at the head of a qualified path, or [`OPAQUE`] for a glob
/// or an alias of the root itself.
fn resolve(prefix: &[String], leaf: Leaf, depth: usize) -> Option<String> {
    let mut full = prefix.to_vec();
    let alias = match leaf {
        Leaf::Name(n) if n == "self" => false,
        Leaf::Rename(n) if n == "self" => true,
        Leaf::Name(n) => {
            full.push(n);
            false
        }
        Leaf::Rename(n) => {
            full.push(n);
            true
        }
        Leaf::Glob => true,
    };
    let rest = if full.first().is_some_and(|s| s == "crate") {
        &full[1..]
    } else {
        let climbs = full.iter().take_while(|s| *s == "super").count();
        if climbs == 0 && depth == 0 {
            // At the crate root, `self::x` and a qualified `x::..` start at a
            // root item (a bare `x` is a local name).
            let explicit = full.first().is_some_and(|s| s == "self");
            if !explicit && prefix.is_empty() {
                return None;
            }
            &full[usize::from(explicit)..]
        } else if climbs == 0 || climbs < depth {
            return None;
        } else {
            &full[climbs..]
        }
    };
    match rest.first() {
        Some(name) => Some(name.clone()),
        None if alias => Some(OPAQUE.into()),
        None => None,
    }
}

/// Collects the crate-root names a file's shipped code reaches, with the line
/// of each. `syn` has already dropped comments, so only paths, `use` trees,
/// tokens of macro bodies and path strings of attributes are left to read.
struct Refs {
    /// Module depth of the code being visited: the file's, plus one per
    /// enclosing inline `mod`.
    depth: usize,
    out: Vec<(String, usize)>,
    /// Shipped inline modules at the crate root (`mod x { .. }` in
    /// `main.rs`): top-level modules the file-based module map cannot see.
    root_mods: Vec<String>,
    /// Inside a `macro_rules!` body, whose relative paths resolve wherever
    /// the macro is invoked, not where it is defined.
    rules: bool,
    /// Inside a macro's input, which the macro may transform at will (drop a
    /// `#[cfg(test)]` and emit the item it gated): no attribute gates there.
    input: bool,
}

impl Refs {
    fn push(&mut self, name: Option<String>, line: usize) {
        if let Some(name) = name {
            self.out.push((name, line));
        }
    }

    /// A path out of `prefix` the guard cannot read (a macro fragment like
    /// `crate::$module` or `crate::{a, $($more),*}`): [`OPAQUE`] if `prefix`
    /// reaches the crate root, since it may name any module.
    fn unresolved(&mut self, prefix: &[String], line: usize) {
        let opaque = resolve(prefix, Leaf::Glob, self.depth).map(|_| OPAQUE.to_string());
        self.push(opaque, line);
    }

    fn path(&mut self, path: &syn::Path, line: Option<usize>) {
        if path.leading_colon.is_some() {
            return; // `::x` names an external crate
        }
        let mut segs: Vec<String> = path
            .segments
            .iter()
            .map(|s| s.ident.unraw().to_string())
            .collect();
        let line = line.unwrap_or_else(|| path.segments[0].ident.span().start().line);
        // A path string a `macro_rules!` body emits resolves where it is
        // invoked, like the body's other relative paths.
        if self.rules && segs.first().is_some_and(|s| s == "super" || s == "self") {
            self.out.push((OPAQUE.into(), line));
            return;
        }
        if self.rules && segs.len() > 1 && segs[0] != "crate" {
            self.out.push((format!("{MAYBE}{}", segs[0]), line));
        }
        let Some(last) = segs.pop() else {
            return;
        };
        self.push(resolve(&segs, Leaf::Name(last), self.depth), line);
    }

    fn use_tree(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>, line: usize) {
        let resolved = match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.unraw().to_string());
                self.use_tree(&p.tree, prefix, line);
                prefix.pop();
                return;
            }
            syn::UseTree::Group(g) => {
                for item in &g.items {
                    self.use_tree(item, prefix, line);
                }
                return;
            }
            // At the crate root, `use x;` and `use x as y;` name a root item.
            syn::UseTree::Name(n) if self.depth == 0 && prefix.is_empty() => {
                Some(n.ident.unraw().to_string())
            }
            // `use crate as root;` aliases the root itself.
            syn::UseTree::Rename(r) if self.depth == 0 && prefix.is_empty() => {
                Some(match r.ident.unraw().to_string() {
                    root if root == "crate" || root == "self" => OPAQUE.into(),
                    name => name,
                })
            }
            syn::UseTree::Name(n) => {
                resolve(prefix, Leaf::Name(n.ident.unraw().to_string()), self.depth)
            }
            syn::UseTree::Rename(r) => resolve(
                prefix,
                Leaf::Rename(r.ident.unraw().to_string()),
                self.depth,
            ),
            syn::UseTree::Glob(_) => resolve(prefix, Leaf::Glob, self.depth),
        };
        self.push(resolved, line);
    }

    /// References in tokens `syn` leaves unparsed (macro bodies, attribute
    /// arguments): `crate::`/`super::` paths (`$crate::` too: a macro defined
    /// in `src/` names this crate) and, inside attributes (`strings` set, or a
    /// `#[..]` within a macro body), string literals holding a whole path
    /// (`#[serde(with = "crate::x")]`) unless they are a `doc` value.
    fn tokens(&mut self, tokens: TokenStream, strings: bool) {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        let punct = |k: usize, c: char| matches!(trees.get(k), Some(TokenTree::Punct(p)) if p.as_char() == c);
        let ident = |k: usize| match trees.get(k) {
            Some(TokenTree::Ident(i)) => Some(i.unraw().to_string()),
            _ => None,
        };
        let sep = |k: usize| punct(k, ':') && punct(k + 1, ':');
        let mut k = 0;
        while k < trees.len() {
            match &trees[k] {
                // An attribute a macro emits: read like a parsed one.
                TokenTree::Punct(p) if p.as_char() == '#' => {
                    let inner = punct(k + 1, '!');
                    // An item any of whose attributes is `#[cfg(test)]`,
                    // in whatever order, is skipped whole, those attributes
                    // included (anything else, e.g. a generic parameter, is
                    // read).
                    if let Some(rest) =
                        test_item_rest(&trees[k..]).filter(|_| !inner && !self.input)
                    {
                        k = trees.len() - rest;
                        continue;
                    }
                    let j = k + 1 + usize::from(inner);
                    if let Some(TokenTree::Group(g)) = trees.get(j) {
                        if g.delimiter() == Delimiter::Bracket {
                            k = j + 1;
                            match syn::parse2::<syn::Meta>(g.stream()) {
                                // `#![cfg(test)]` gates the whole enclosing
                                // group.
                                Ok(meta) if inner && cfg_test(&meta) && !self.input => {
                                    k = trees.len();
                                }
                                Ok(meta) => self.meta(&meta),
                                Err(_) => {
                                    let first = g.stream().into_iter().next();
                                    let doc =
                                        matches!(&first, Some(TokenTree::Ident(i)) if i == "doc");
                                    if !doc {
                                        self.tokens(g.stream(), true);
                                    }
                                }
                            }
                            continue;
                        }
                    }
                }
                // A macro invocation's input: see `input`.
                TokenTree::Group(g) if k >= 2 && punct(k - 1, '!') && ident(k - 2).is_some() => {
                    let outer = std::mem::replace(&mut self.input, true);
                    self.tokens(g.stream(), strings);
                    self.input = outer;
                }
                TokenTree::Group(g) => self.tokens(g.stream(), strings),
                TokenTree::Literal(lit) if strings => {
                    let is_doc =
                        k >= 2 && punct(k - 1, '=') && ident(k - 2).as_deref() == Some("doc");
                    let path = syn::parse2::<syn::LitStr>(TokenTree::Literal(lit.clone()).into())
                        .ok()
                        .and_then(|s| s.parse::<syn::Path>().ok());
                    if let Some(path) = path.filter(|_| !is_doc) {
                        let (line, from) = (lit.span().start().line, self.out.len());
                        self.path(&path, Some(line));
                        // `crate::x::f::<crate::y::T>`: the arguments too.
                        for seg in &path.segments {
                            self.visit_path_arguments(&seg.arguments);
                        }
                        for found in &mut self.out[from..] {
                            found.1 = line;
                        }
                    }
                }
                // `mod x { .. }` a macro emits: one level deeper (and, in
                // `main.rs`, a crate-root inline module).
                TokenTree::Ident(i) if i == "mod" && ident(k + 1).is_some() => {
                    if let Some(TokenTree::Group(g)) = trees.get(k + 2) {
                        if g.delimiter() == Delimiter::Brace {
                            if self.depth == 0 {
                                self.root_mods.push(ident(k + 1).unwrap_or_default());
                            }
                            self.depth += 1;
                            self.tokens(g.stream(), strings);
                            self.depth -= 1;
                            k += 3;
                            continue;
                        }
                    }
                }
                // `use ..;` a macro emits: read like a parsed one if it
                // parses (`use ::x;` names an external crate).
                TokenTree::Ident(i) if i == "use" => {
                    let end = (k + 1..trees.len()).find(|&e| punct(e, ';'));
                    let item = end.and_then(|e| {
                        let tokens: TokenStream = trees[k..=e].iter().cloned().collect();
                        syn::parse2::<syn::ItemUse>(tokens).ok()
                    });
                    if let (Some(e), Some(item)) = (end, item) {
                        if item.leading_colon.is_none() {
                            self.macro_use(&item.tree, i.span().start().line);
                        }
                        k = e + 1;
                        continue;
                    }
                    // In a `macro_rules!` body, an import of a fragment
                    // (`use $m as dep;`) may name any module; one rooted at
                    // `crate`/`$crate` is read token by token.
                    let rooted = ident(k + 1).as_deref() == Some("crate")
                        || (punct(k + 1, '$') && ident(k + 2).as_deref() == Some("crate"));
                    if self.rules && !rooted {
                        self.out.push((OPAQUE.into(), i.span().start().line));
                    }
                }
                // `pub(crate)`/`pub(in crate::x)` restrict visibility; they
                // use nothing.
                TokenTree::Ident(i)
                    if i == "pub"
                        && matches!(trees.get(k + 1), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis) =>
                {
                    k += 2;
                    continue;
                }
                // `x::..` in a `macro_rules!` body names a root module if the
                // macro is invoked at the root.
                TokenTree::Ident(i)
                    if self.rules
                        && !matches!(i.to_string().as_str(), "crate" | "super" | "self")
                        && sep(k + 1)
                        && !(k > 1 && sep(k - 2))
                        && !(k > 0 && punct(k - 1, '$')) =>
                {
                    self.out
                        .push((format!("{MAYBE}{}", i.unraw()), i.span().start().line));
                }
                // `super::`/`self::` in a `macro_rules!` body: its depth is
                // the invocation's, which may be any module. So is a path
                // rooted at a fragment (`$root::cli`), which an invocation
                // may fill with `crate`.
                TokenTree::Ident(i)
                    if self.rules
                        && (i == "super" || (i == "self" && sep(k + 1)))
                        && !(k > 1 && sep(k - 2)) =>
                {
                    self.out.push((OPAQUE.into(), i.span().start().line));
                }
                TokenTree::Punct(p)
                    if self.rules
                        && p.as_char() == '$'
                        && (ident(k + 1).is_some_and(|n| n != "crate")
                            || matches!(trees.get(k + 1), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis))
                        && sep(k + 2)
                        && !(k > 1 && sep(k - 2)) =>
                {
                    self.out.push((OPAQUE.into(), p.span().start().line));
                }
                // At the crate root, `self::x` and `x::..` start at a root item.
                TokenTree::Ident(i)
                    if self.depth == 0
                        && !matches!(i.to_string().as_str(), "crate" | "super")
                        && sep(k + 1)
                        && !(k > 1 && sep(k - 2))
                        && !(k > 0 && punct(k - 1, '$')) =>
                {
                    let name = if i == "self" {
                        ident(k + 3).unwrap_or_else(|| OPAQUE.into())
                    } else {
                        i.unraw().to_string()
                    };
                    self.out.push((name, i.span().start().line));
                }
                TokenTree::Ident(i)
                    if matches!(i.to_string().as_str(), "crate" | "super")
                        && !(k > 1 && sep(k - 2)) =>
                {
                    let line = i.span().start().line;
                    let mut prefix = vec![i.to_string()];
                    let mut j = k + 1;
                    let mut cut = false;
                    while sep(j) {
                        j += 2;
                        match (&trees.get(j), ident(j)) {
                            (_, Some(n)) if matches!(n.as_str(), "crate" | "super" | "self") => {
                                prefix.push(n);
                                j += 1;
                            }
                            (_, Some(n)) => {
                                let leaf = if ident(j + 1).as_deref() == Some("as") {
                                    Leaf::Rename(n)
                                } else {
                                    Leaf::Name(n)
                                };
                                self.push(resolve(&prefix, leaf, self.depth), line);
                                cut = true;
                                break;
                            }
                            (Some(TokenTree::Punct(p)), _) if p.as_char() == '*' => {
                                self.push(resolve(&prefix, Leaf::Glob, self.depth), line);
                                cut = true;
                                break;
                            }
                            (Some(TokenTree::Group(g)), _) => {
                                let parser =
                                    Punctuated::<syn::UseTree, Token![,]>::parse_terminated;
                                match parser.parse2(g.stream()) {
                                    Ok(items) => {
                                        for item in items {
                                            self.use_tree(&item, &mut prefix.clone(), line);
                                        }
                                    }
                                    Err(_) => {
                                        // Any `super` inside may climb further.
                                        let mut deepest = prefix.clone();
                                        deepest.extend(std::iter::repeat_n(
                                            "super".to_string(),
                                            supers_in(g.stream()),
                                        ));
                                        self.unresolved(&deepest, line);
                                    }
                                }
                                cut = true;
                                break;
                            }
                            _ => {
                                self.unresolved(&prefix, line);
                                cut = true;
                                break;
                            }
                        }
                    }
                    // `crate as x`, `super as x`, `crate self as x`, or a bare
                    // `crate`/`super::super` chain a macro may splice into a
                    // path root (`call!(crate)` for `$root::cli::run()`).
                    if !cut {
                        let last = prefix.pop().unwrap_or_default();
                        let aliased = ident(j).as_deref() == Some("as")
                            || (last == "crate"
                                && ident(j).as_deref() == Some("self")
                                && ident(j + 1).as_deref() == Some("as"));
                        let bare = !(k > 0 && punct(k - 1, '$'))
                            && !(k > 0 && ident(k - 1).as_deref() == Some("extern"));
                        if aliased || bare {
                            self.push(resolve(&prefix, Leaf::Rename(last), self.depth), line);
                        }
                    }
                    k = j.max(k + 1);
                    continue;
                }
                _ => {}
            }
            k += 1;
        }
    }
}

impl Refs {
    /// A `use` tree a macro emits. In a `macro_rules!` body it resolves at
    /// the invocation: a `crate::` branch is still exact, a `super`/`self`
    /// one may reach anything, and any other head is a module if the macro is
    /// invoked at the root.
    fn macro_use(&mut self, tree: &syn::UseTree, line: usize) {
        if !self.rules {
            return self.use_tree(tree, &mut Vec::new(), line);
        }
        let head = match tree {
            syn::UseTree::Group(g) => {
                g.items.iter().for_each(|t| self.macro_use(t, line));
                return;
            }
            syn::UseTree::Path(p) => p.ident.unraw().to_string(),
            syn::UseTree::Name(n) => n.ident.unraw().to_string(),
            syn::UseTree::Rename(r) => r.ident.unraw().to_string(),
            syn::UseTree::Glob(_) => OPAQUE.into(),
        };
        match head.as_str() {
            "crate" => self.use_tree(tree, &mut Vec::new(), line),
            "super" | "self" | OPAQUE => self.out.push((OPAQUE.into(), line)),
            _ => self.out.push((format!("{MAYBE}{head}"), line)),
        }
    }
}

/// If `trees` starts with a whole item an outer `#[cfg(test)]` gates, how
/// many token trees follow it.
fn test_item_rest(trees: &[TokenTree]) -> Option<usize> {
    let tokens: TokenStream = trees.iter().cloned().collect();
    let parser = |input: syn::parse::ParseStream| {
        let attrs = input.call(syn::Attribute::parse_outer)?;
        if !attrs.iter().any(|a| cfg_test(&a.meta)) {
            return Err(input.error("not test-only"));
        }
        input.parse::<syn::Item>()?;
        input.parse::<TokenStream>()
    };
    parser
        .parse2(tokens)
        .ok()
        .map(|rest| rest.into_iter().count())
}

/// How many `super` segments `tokens` holds, at any nesting.
fn supers_in(tokens: TokenStream) -> usize {
    tokens
        .into_iter()
        .map(|t| match t {
            TokenTree::Ident(i) if i == "super" => 1,
            TokenTree::Group(g) => supers_in(g.stream()),
            _ => 0,
        })
        .sum()
}

impl<'ast> Visit<'ast> for Refs {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if let syn::Item::ExternCrate(e) = item {
            // `extern crate self as x;` aliases the root.
            if e.ident == "self" && e.rename.is_some() && !e.attrs.iter().any(is_cfg_test) {
                self.out.push((OPAQUE.into(), e.ident.span().start().line));
            }
        }
        let inline_mod = matches!(item, syn::Item::Mod(m) if m.content.is_some());
        if let syn::Item::Mod(m) = item {
            if inline_mod && self.depth == 0 && !m.attrs.iter().any(is_cfg_test) {
                self.root_mods.push(m.ident.unraw().to_string());
            }
        }
        self.depth += usize::from(inline_mod);
        visit::visit_item(self, item);
        self.depth -= usize::from(inline_mod);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.path(path, None);
        visit::visit_path(self, path);
    }

    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        let line = tree.span().start().line;
        self.use_tree(tree, &mut Vec::new(), line);
    }

    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        self.meta(&attr.meta);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let (rules, input) = (self.rules, self.input);
        let definition = mac.path.is_ident("macro_rules");
        self.rules |= definition;
        self.input = !definition;
        self.tokens(mac.tokens.clone(), false);
        (self.rules, self.input) = (rules, input);
        visit::visit_macro(self, mac);
    }

    /// `pub(crate)`/`pub(in crate::x)` restrict visibility; they use nothing.
    fn visit_visibility(&mut self, _: &'ast syn::Visibility) {}

    skip_test_only!();
}

impl Refs {
    /// An attribute's references: its path and arguments, but neither a
    /// `doc` value nor (outside a macro's input) the payload of a `cfg_attr`
    /// only `test` enables.
    fn meta(&mut self, meta: &syn::Meta) {
        if meta.path().is_ident("doc") {
            return;
        }
        if meta.path().is_ident("cfg_attr") {
            if let Some(args) = args(meta) {
                if self.input || !args.first().is_some_and(requires_test) {
                    args.iter().skip(1).for_each(|m| self.meta(m));
                }
                return;
            }
        }
        self.visit_path(meta.path());
        match meta {
            syn::Meta::Path(_) => {}
            syn::Meta::List(list) => self.tokens(list.tokens.clone(), true),
            syn::Meta::NameValue(nv) => {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(lit),
                    ..
                }) = &nv.value
                {
                    self.tokens(TokenTree::Literal(lit.token()).into(), true);
                }
                self.visit_expr(&nv.value);
            }
        }
    }
}

/// The crate-root names a file's shipped code reaches: every `crate::<name>`
/// and every `super::` chain that climbs out of the top-level module.
/// `file_depth` is the file's module depth (`src/x.rs` and `src/x/mod.rs` are
/// 1, `src/x/y.rs` is 2, `src/main.rs` is 0).
fn crate_refs(src: &str, file_depth: usize) -> Vec<(String, usize)> {
    let mut refs = Refs {
        depth: file_depth,
        out: Vec::new(),
        root_mods: Vec::new(),
        rules: false,
        input: false,
    };
    refs.visit_file(&parse(src));
    assert!(
        refs.root_mods.is_empty(),
        "inline modules at the crate root ({:?}) are top-level modules the layering \
         guard ({TABLE}) cannot see, since it maps modules from files.\n\
         Move each into its own file (`mod x;` + `src/x.rs`).",
        refs.root_mods
    );
    let mut out = refs.out;
    out.sort_by_key(|r| r.1);
    out.dedup();
    out
}

/// The top-level module `rel` (relative to `src/`) belongs to, and its depth.
fn module_of(rel: &Path) -> (String, usize) {
    let parts: Vec<String> = rel
        .iter()
        .map(|p| p.to_string_lossy().trim_end_matches(".rs").to_string())
        .collect();
    // `mod.rs` is its directory's module; only the crate root's `main.rs` is.
    let last = parts.last().map(String::as_str);
    let dir_module = last == Some("mod") || parts == ["main"];
    let depth = parts.len() - usize::from(dir_module);
    (parts[0].clone(), depth)
}

/// The actual graph of `src/`, with the first `file:line` of every edge.
fn source_graph() -> (Graph, BTreeMap<(String, String), String>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let modules: BTreeSet<String> = shipped_files(&src.join("main.rs"))
        .iter()
        .map(|f| module_of(f.strip_prefix(&src).unwrap()).0)
        .collect();
    assert!(
        modules.contains("main") && modules.contains("cli") && modules.len() > 10,
        "scan found too few modules: {modules:?}"
    );
    graph_of_crate(root)
}

/// The graph of the crate at `root` (its `src/main.rs` and the module files a
/// non-test build compiles), with the first `file:line` of every edge.
fn graph_of_crate(root: &Path) -> (Graph, BTreeMap<(String, String), String>) {
    let src = root.join("src");
    let files = shipped_files(&src.join("main.rs"));
    let modules: BTreeSet<String> = files
        .iter()
        .map(|f| module_of(f.strip_prefix(&src).unwrap()).0)
        .collect();
    let (mut graph, mut sites, mut opaque) = (Graph::new(), BTreeMap::new(), Vec::new());
    for file in &files {
        let (from, depth) = module_of(file.strip_prefix(&src).unwrap());
        let text = std::fs::read_to_string(file).unwrap();
        let rel = file.strip_prefix(root).unwrap();
        for (name, line) in crate_refs(&text, depth) {
            if name == OPAQUE {
                opaque.push(format!("  {}:{line}", rel.display()));
                continue;
            }
            let name = match name.strip_prefix(MAYBE) {
                Some(head) if modules.contains(head) => head.to_string(),
                Some(_) => continue,
                None => name,
            };
            // Anything not a module is an item defined in `main.rs`.
            let to = if modules.contains(&name) {
                name
            } else {
                "main".into()
            };
            if to == from {
                continue;
            }
            graph.entry(from.clone()).or_default().insert(to.clone());
            sites
                .entry((from.clone(), to))
                .or_insert_with(|| format!("{}:{line}", rel.display()));
        }
    }
    assert!(
        opaque.is_empty(),
        "crate-root globs, aliases and paths the guard cannot resolve (macro \
         fragments, `super::`/`self::` in `macro_rules!` bodies) hide which modules \
         they use, so the layering guard ({TABLE}) cannot see those edges:\n{}\n\
         Name the items by absolute path (`use crate::module::Item;`, \
         `$crate::module::Item` in a macro).",
        opaque.join("\n")
    );
    (graph, sites)
}

fn allowed_graph() -> Graph {
    ALLOWED_EDGES
        .iter()
        .map(|(from, tos)| {
            (
                from.to_string(),
                tos.iter().map(|t| t.to_string()).collect(),
            )
        })
        .collect()
}

fn edges(graph: &Graph) -> BTreeSet<(String, String)> {
    graph
        .iter()
        .flat_map(|(f, ts)| ts.iter().map(move |t| (f.clone(), t.clone())))
        .collect()
}

/// The strongly connected components of `graph` (Tarjan), each sorted.
fn sccs(graph: &Graph) -> Vec<BTreeSet<String>> {
    struct Tarjan<'g> {
        graph: &'g Graph,
        index: BTreeMap<&'g str, usize>,
        low: BTreeMap<&'g str, usize>,
        stack: Vec<&'g str>,
        on_stack: BTreeSet<&'g str>,
        out: Vec<BTreeSet<String>>,
    }
    impl<'g> Tarjan<'g> {
        fn visit(&mut self, node: &'g str) {
            let at = self.index.len();
            self.index.insert(node, at);
            self.low.insert(node, at);
            self.stack.push(node);
            self.on_stack.insert(node);
            for next in self.graph.get(node).into_iter().flatten() {
                let next = next.as_str();
                if !self.index.contains_key(next) {
                    self.visit(next);
                    let low = self.low[node].min(self.low[next]);
                    self.low.insert(node, low);
                } else if self.on_stack.contains(next) {
                    let low = self.low[node].min(self.index[next]);
                    self.low.insert(node, low);
                }
            }
            if self.low[node] == self.index[node] {
                let mut group = BTreeSet::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(member);
                    group.insert(member.to_string());
                    if member == node {
                        break;
                    }
                }
                self.out.push(group);
            }
        }
    }
    let mut tarjan = Tarjan {
        graph,
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        out: Vec::new(),
    };
    let nodes: BTreeSet<&str> = graph
        .iter()
        .flat_map(|(f, ts)| std::iter::once(f).chain(ts))
        .map(String::as_str)
        .collect();
    for node in nodes {
        if !tarjan.index.contains_key(node) {
            tarjan.visit(node);
        }
    }
    tarjan.out
}

/// Every way the cyclic groups of `graph` (its strongly connected components
/// of two or more modules) differ from `known`.
fn cycle_violations(graph: &Graph, known: &[&[&str]]) -> Vec<String> {
    let known: Vec<BTreeSet<String>> = known
        .iter()
        .map(|g| g.iter().map(|m| m.to_string()).collect())
        .collect();
    let actual: Vec<BTreeSet<String>> = sccs(graph).into_iter().filter(|g| g.len() > 1).collect();
    let mut errors = Vec::new();
    for group in actual.iter().filter(|g| !known.contains(g)) {
        if known.iter().any(|k| group.is_subset(k)) {
            continue; // what is left of a split group: reported below as stale
        }
        let inside: Vec<_> = known.iter().filter(|k| k.is_subset(group)).collect();
        errors.push(if inside.is_empty() {
            format!(
                "new dependency cycle among {group:?}, not in KNOWN_CYCLIC_GROUPS ({TABLE}).\n\
                 Break it (move the shared code down a layer); KNOWN_CYCLIC_GROUPS may only shrink."
            )
        } else {
            format!(
                "known cyclic group(s) {inside:?} grew into the cycle {group:?} \
                 (KNOWN_CYCLIC_GROUPS, {TABLE}).\nBreak the new dependency; \
                 KNOWN_CYCLIC_GROUPS may only shrink."
            )
        });
    }
    for group in known.iter().filter(|k| !actual.contains(k)) {
        if actual.iter().any(|g| group.is_subset(g)) {
            continue; // reported above as grown
        }
        let now: Vec<_> = actual.iter().filter(|g| !g.is_disjoint(group)).collect();
        errors.push(format!(
            "KNOWN_CYCLIC_GROUPS ({TABLE}) lists {group:?}, which is no longer a cycle \
             (cyclic groups among these modules now: {now:?}).\nShrink KNOWN_CYCLIC_GROUPS to match."
        ));
    }
    errors
}

#[test]
fn module_graph_matches_the_allowed_edges() {
    let (actual, sites) = source_graph();
    let (actual, allowed) = (edges(&actual), edges(&allowed_graph()));
    let new: Vec<String> = actual
        .difference(&allowed)
        .map(|e| format!("  {} -> {}  ({})", e.0, e.1, sites[e]))
        .collect();
    let stale: Vec<String> = allowed
        .difference(&actual)
        .map(|(f, t)| format!("  {f} -> {t}"))
        .collect();
    assert!(
        new.is_empty(),
        "new module dependencies not in ALLOWED_EDGES ({TABLE}):\n{}\n\
         Prefer keeping the layering; if the edge is deliberate, add it to \
         ALLOWED_EDGES so the change is reviewed.",
        new.join("\n")
    );
    assert!(
        stale.is_empty(),
        "ALLOWED_EDGES ({TABLE}) lists dependencies the source no longer has:\n{}\n\
         Remove them from ALLOWED_EDGES.",
        stale.join("\n")
    );
}

#[test]
fn module_graph_keeps_its_invariants() {
    let (graph, sites) = source_graph();
    let site = |f: &str, t: &str| sites[&(f.to_string(), t.to_string())].clone();
    for leaf in LEAVES {
        let deps = graph.get(*leaf).cloned().unwrap_or_default();
        assert!(
            deps.is_empty(),
            "`{leaf}` must stay a leaf module but depends on {deps:?} (e.g. {})",
            site(leaf, deps.first().unwrap())
        );
    }
    for (from, tos) in &graph {
        assert!(
            !tos.contains("cli") || CLI_DEPENDENTS.contains(&from.as_str()),
            "only {CLI_DEPENDENTS:?} may depend on `cli`, but `{from}` does ({})",
            site(from, "cli")
        );
    }
    let errors = cycle_violations(&graph, KNOWN_CYCLIC_GROUPS);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn crate_refs_reads_imports_and_inline_paths() {
    let src = "\
use crate::{alpha, beta::Thing, gamma::{x, y}, self};
use crate :: delta;
fn f() { let _ = crate::epsilon::call(1); crate::zeta!(); }
pub(crate) fn g() {}
#[serde(serialize_with = \"crate::eta::serialize\")]
struct S;
";
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(
        names,
        ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"]
    );
    assert_eq!(crate_refs(src, 1)[4].1, 3, "references carry their line");
}

#[test]
fn crate_refs_reads_attributes_without_their_comments_or_docs() {
    let src = r#"
#[derive(/* crate::ghost */ Debug)]
struct A;
#[cfg_attr(unix, doc = "crate::ghost")]
fn b() {}
#[thiserror(error = "crate::ghost failed")]
struct C;
#[cfg_attr(unix, serde(with = "crate::alpha"))]
struct D;
#[crate::beta]
fn e() {}
"#;
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["alpha", "beta"]);
    // Generic arguments of a path string are read too.
    let src = "#[serde(serialize_with = \"crate::alpha::f::<crate::beta::B, _>\")]\nstruct S;";
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["alpha", "beta"]);
    // A `cfg_attr` payload only `test` enables is test code, nested or not.
    let src = r#"
#[cfg_attr(test, serde(serialize_with = "crate::ghost::f"))]
#[cfg_attr(unix, cfg_attr(all(test, unix), serde(with = "crate::ghost")))]
#[cfg_attr(unix, cfg_attr(not(test), serde(with = "crate::alpha")))]
struct S;
"#;
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["alpha"]);
}

/// Literals `syn` lexes (raw C strings with an embedded quote) never hide or
/// invent code, and `cfg(test)` nodes end at the parser's boundaries.
#[test]
fn crate_refs_bounds_literals_and_test_code_by_syntax() {
    for src in [
        "const C: &core::ffi::CStr = cr#\"a\"b\"#; fn f() { crate::alpha::run(); }",
        "fn f<#[cfg(test)] T>() { crate::alpha::run(); }",
        "fn f(#[cfg(test)] x: u8) { crate::alpha::run(); }",
    ] {
        let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
        assert_eq!(names, ["alpha"], "{src}");
    }
    let src = "#[cfg(test)] fn f<A, B>() { crate::alpha::run(); }";
    assert!(crate_refs(src, 1).is_empty(), "{src}");
}

#[test]
fn crate_refs_reads_raw_identifiers_as_their_names() {
    let src = "\
use crate::r#alpha::Thing;
use crate::{r#beta, gamma::r#type};
fn f() { crate::r#delta::run(); }
macro_rules! m { () => { crate::r#epsilon::run() }; }
#[serde(with = \"crate::r#zeta\")]
struct S;
";
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(
        names,
        ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"]
    );
}

/// Only files a non-test build compiles count: a module declared behind
/// `#[cfg(test)]` (and everything below it) is skipped.
#[test]
fn graph_skips_files_only_test_code_declares() {
    let sb = crate::common::Sandbox::new();
    let root = sb.root.join("crate");
    let write = |rel: &str, text: &str| {
        let path = root.join("src").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("main.rs", "mod alpha;\nmod beta;\nmod gamma;\n");
    // Test-only code may generate modules however it likes.
    write(
        "alpha.rs",
        "mod inner;\n#[cfg(test)]\nmod tests;\n#[cfg(test)]\nmod cases {\n\
         macro_rules! cases { ($name:ident) => { mod $name { use super::super::*; } }; }\n\
         cases!(one);\n}\n",
    );
    write("alpha/inner.rs", "use crate::beta::B;\n");
    write("alpha/tests.rs", "use crate::gamma::G;\nmod deeper;\n");
    write("alpha/tests/deeper.rs", "use crate::gamma::G;\n");
    // A file gated as a whole (`#![cfg(test)]`) is test-only with all below it.
    write("gamma.rs", "pub struct G;\nmod checks;\n");
    write(
        "gamma/checks.rs",
        "#![cfg(test)]\nmod more;\nuse crate::alpha::A;\n",
    );
    write("gamma/checks/more.rs", "use crate::alpha::A;\n");
    write("beta/mod.rs", "pub struct B;\nmod nested { mod leaf; }\n");
    write("beta/nested/leaf.rs", "use crate::gamma::G;\n");
    let (graph, _) = graph_of_crate(&root);
    assert_eq!(graph, graph_of(&[("alpha", "beta"), ("beta", "gamma")]));
}

/// At the crate root every spelling of a root path is an edge: `crate::x`,
/// `self::x`, a qualified `x::..`, in code or macro tokens, and `use x;` or
/// `use x as y;`; a bare local name that happens to match a module is not.
#[test]
fn graph_reads_every_spelling_of_root_paths_in_main() {
    let sb = crate::common::Sandbox::new();
    let root = sb.root.join("crate");
    let write = |rel: &str, text: &str| {
        let path = root.join("src").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let mods = [
        "alpha", "beta", "gamma", "delta", "epsilon", "eta", "theta", "zeta",
    ];
    let mut main: String = mods.iter().map(|m| format!("mod {m};\n")).collect();
    main.push_str(
        "use self::beta::B;\n\
         use eta as renamed;\n\
         use theta;\n\
         fn main() {\n\
         \x20   crate::alpha::run();\n\
         \x20   gamma::run();\n\
         \x20   println!(\"{}\", delta::D);\n\
         \x20   println!(\"{}\", self::epsilon::E);\n\
         \x20   renamed::run();\n\
         \x20   let zeta = 1;\n\
         \x20   drop(zeta);\n\
         }\n",
    );
    write("main.rs", &main);
    for m in mods {
        write(&format!("{m}.rs"), "");
    }
    let (graph, _) = graph_of_crate(&root);
    let edges: Vec<(&str, &str)> = mods[..7].iter().map(|m| ("main", *m)).collect();
    assert_eq!(graph, graph_of(&edges));
}

/// A qualified `beta::..` in a `macro_rules!` body resolves where the macro
/// is invoked: at the root it is the top-level `beta`. It counts as an edge of
/// the defining module (spell it `$crate::beta::..`), while heads that name no
/// module (`fmt::Display`) do not.
#[test]
fn graph_reads_unqualified_macro_paths_as_their_modules() {
    let sb = crate::common::Sandbox::new();
    let root = sb.root.join("crate");
    let write = |rel: &str, text: &str| {
        let path = root.join("src").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let mods = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"];
    let mut main: String = mods.iter().map(|m| format!("mod {m};\n")).collect();
    // Imports a macro emits, defined at the root or elsewhere, count too.
    main.push_str(
        "macro_rules! here { () => { use gamma as renamed; renamed::run(); } }\n\
         fn main() { alpha::run_beta!(); here!(); }\n",
    );
    write("main.rs", &main);
    write(
        "alpha.rs",
        "macro_rules! run_beta { () => {\n\
         \x20   beta::run();\n\
         \x20   std::fmt::Display::fmt;\n\
         \x20   use {delta as d, epsilon};\n\
         } }\n\
         pub(crate) use run_beta;\n",
    );
    for m in &mods[1..] {
        write(&format!("{m}.rs"), "");
    }
    write("zeta.rs", "m! { use super::delta as d; }\n");
    let (graph, _) = graph_of_crate(&root);
    assert_eq!(
        graph,
        graph_of(&[
            ("main", "alpha"),
            ("main", "gamma"),
            ("alpha", "beta"),
            ("alpha", "delta"),
            ("alpha", "epsilon"),
            ("zeta", "delta"),
        ])
    );
}

/// `super::` in a `macro_rules!` body resolves where the macro is invoked:
/// defined in `git/repository.rs` (where `super::cli` stays inside `git`) but
/// invoked from `policy`, it reaches the top-level `cli`. Such a path fails the
/// guard instead of hiding the `policy -> cli` edge.
#[test]
fn graph_rejects_relative_paths_in_macro_definitions() {
    let sb = crate::common::Sandbox::new();
    let root = sb.root.join("crate");
    let write = |rel: &str, text: &str| {
        let path = root.join("src").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("main.rs", "mod cli;\nmod git;\nmod policy;\n");
    write("cli.rs", "pub fn run() {}\n");
    write(
        "git.rs",
        "mod repository;\npub(crate) use repository::run_cli;\n",
    );
    write(
        "git/repository.rs",
        "macro_rules! run_cli { () => {\n    super::cli::run()\n}; }\n\
         pub(crate) use run_cli;\n",
    );
    write("policy.rs", "fn go() { crate::git::run_cli!(); }\n");
    let err = std::panic::catch_unwind(|| graph_of_crate(&root)).expect_err("hidden edge");
    let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
    let site = Path::new("src").join("git").join("repository.rs");
    assert!(msg.contains(&format!("{}:2", site.display())), "{msg}");
}

/// A file the module walk cannot classify as shipped or test-only fails the
/// guard instead of escaping it: a macro-emitted declaration (even of a file
/// test code also declares), an `include!` (however spelled or renamed), one
/// an alternate
/// `#[cfg_attr(.., path = ..)]` points at, or one nothing declares.
#[test]
fn graph_rejects_files_it_cannot_classify() {
    // (`main.rs`, other files, expected in the failure)
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let cases: [Case; 12] = [
        (
            "use std::include as splice;\n#[cfg(test)]\nmod hidden;\nsplice!(\"hidden.rs\");\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "`include` (as",
        ),
        (
            "use std::{include as splice};\n#[cfg(test)]\nmod hidden;\nsplice!(\"hidden.rs\");\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "`include` (as",
        ),
        (
            "#[cfg(test)]\nmod hidden;\nr#include!(\"hidden.rs\");\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "`include` (as",
        ),
        (
            "#[cfg(test)]\nmod hidden;\n\
             macro_rules! src { () => { std::r#include!(\"hidden.rs\"); }; }\nsrc!();\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "`include` (as",
        ),
        (
            "#[cfg(test)]\nmod hidden;\ninclude!(\"hidden.rs\");\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "`include` (as",
        ),
        (
            "#[cfg(test)]\nmod hidden;\n\
             macro_rules! src { () => { std::include!(\"hidden.rs\"); }; }\nsrc!();\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "`include` (as",
        ),
        (
            "#[cfg(test)]\nmod hidden;\n\
             macro_rules! modules { ($name:ident) => { mod $name; }; }\n\
             #[cfg(not(test))]\nmodules!(hidden);\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "emitted by a macro",
        ),
        (
            "#[cfg(test)]\nmod hidden;\n\
             macro_rules! modules { ($($name:ident),*) => { $(mod $name;)* }; }\n\
             #[cfg(not(test))]\nmodules!(hidden);\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "emitted by a macro",
        ),
        (
            "macro_rules! modules { () => { mod hidden; }; }\nmodules!();\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "emitted by a macro",
        ),
        (
            "#[cfg(test)]\nmod hidden;\n\
             macro_rules! modules { () => { mod hidden; }; }\n\
             #[cfg(not(test))]\nmodules!();\n",
            &[("hidden.rs", "use crate::x::Y;\n")],
            "emitted by a macro",
        ),
        (
            "#[cfg_attr(unix, path = \"alternate.rs\")]\nmod foo;\n",
            &[("foo.rs", ""), ("alternate.rs", "")],
            "#[path]",
        ),
        ("", &[("orphan.rs", "")], "orphan.rs"),
    ];
    for (main, files, expected) in cases {
        let sb = crate::common::Sandbox::new();
        let src = sb.root.join("crate/src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), main).unwrap();
        for (rel, text) in files {
            std::fs::write(src.join(rel), text).unwrap();
        }
        let err = std::panic::catch_unwind(|| shipped_files(&src.join("main.rs"))).expect_err(main);
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(msg.contains(expected), "{main}: {msg}");
    }
}

#[test]
fn module_of_counts_only_the_crate_root_main_as_its_directory() {
    assert_eq!(module_of(Path::new("main.rs")), ("main".into(), 0));
    assert_eq!(module_of(Path::new("foo/main.rs")), ("foo".into(), 2));
    assert_eq!(module_of(Path::new("foo/mod.rs")), ("foo".into(), 1));
    assert_eq!(module_of(Path::new("foo/bar.rs")), ("foo".into(), 2));
}

/// Macro fragments that may name any module through the crate root
/// (`crate::$module`, `crate::{a, $($more),*}`) hide edges, so they fail;
/// fragments that stay inside the module are fine.
#[test]
fn crate_refs_flags_macro_paths_it_cannot_read() {
    for src in [
        "macro_rules! m { ($m:ident) => { crate::$m::run() }; }",
        "macro_rules! m { ($($x:ident),*) => { use crate::{alpha, $($x),*}; }; }",
        "macro_rules! m { ($m:ident) => { super::$m::run() }; }",
    ] {
        let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
        assert!(names.contains(&OPAQUE.to_string()), "{src}: {names:?}");
    }
    // In an invocation the tokens resolve where they are: an unreadable group
    // fails only if a `super` inside it may climb out of the top-level module.
    let src = "m! { use super::{super::{a b}}; }";
    let names: Vec<String> = crate_refs(src, 2).into_iter().map(|r| r.0).collect();
    assert_eq!(names, [OPAQUE], "{src}");
    assert!(crate_refs("m! { use super::{a b}; }", 2).is_empty());
    // A `macro_rules!` body resolves where the macro is invoked, which may be
    // any module: its relative paths fail at any depth.
    for src in [
        "macro_rules! m { () => { super::x::run() }; }",
        "macro_rules! m { () => { self::x::run() }; }",
        "macro_rules! m { () => { use super::{x}; }; }",
    ] {
        let names: Vec<String> = crate_refs(src, 3).into_iter().map(|r| r.0).collect();
        assert_eq!(names, [OPAQUE], "{src}");
    }
    assert!(crate_refs("macro_rules! m { ($s:expr) => { self.0 + $s }; }", 1).is_empty());
    // So does a path rooted at a fragment an invocation may fill with
    // `crate`, and a bare `crate` such an invocation passes.
    for (src, depth) in [
        (
            "macro_rules! m { ($root:ident) => { $root::cli::run() }; }",
            3,
        ),
        ("call!(crate);", 3),
        ("call!(super);", 1),
    ] {
        let names: Vec<String> = crate_refs(src, depth).into_iter().map(|r| r.0).collect();
        assert_eq!(names, [OPAQUE], "{src}");
    }
    for src in [
        "macro_rules! m { ($m:ident) => { use $m as dep; dep::run(); }; }",
        "macro_rules! m { ($m:ident) => { use {$m, alpha}; }; }",
    ] {
        let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
        assert!(names.contains(&OPAQUE.to_string()), "{src}: {names:?}");
    }
    for src in [
        "macro_rules! m { () => { $crate::alpha::run() }; }",
        "m! { pub(crate) fn f() {} pub(in crate::alpha) fn g() {} }",
        "macro_rules! m { ($($x:ident),*) => { use $crate::alpha::{$($x),*}; }; }",
    ] {
        let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
        assert!(!names.contains(&OPAQUE.to_string()), "{src}: {names:?}");
    }
    assert!(crate_refs("call!(super);", 2).is_empty());
    // A `super` chain reaching the root, and a repeated fragment as a path
    // root, are the same splice.
    for (src, depth) in [
        ("call!(super::super);", 2),
        (
            "macro_rules! m { ($($root:ident)::+) => { $($root)::+::cli::run() }; }",
            3,
        ),
    ] {
        let names: Vec<String> = crate_refs(src, depth).into_iter().map(|r| r.0).collect();
        assert_eq!(names, [OPAQUE], "{src}");
    }
    assert!(crate_refs("call!(super::super);", 3).is_empty());
    assert!(crate_refs("m! { extern crate alloc; }", 1).is_empty());
}

/// Top-level inline modules in `main.rs` would hide their edges behind
/// `main`, so they fail; test-only ones, and inline modules deeper down, are
/// fine.
#[test]
fn crate_refs_rejects_inline_modules_at_the_crate_root() {
    let src = "mod alpha { pub fn f() { crate::beta::f(); } }\n\
               mod beta { pub fn f() { crate::alpha::f(); } }\n";
    let err = std::panic::catch_unwind(|| crate_refs(src, 0)).expect_err(src);
    let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("\"alpha\", \"beta\""), "{msg}");
    let src = "macro_rules! modules { () => {\n\
               mod alpha { pub fn f() { crate::beta::f(); } }\n\
               mod beta { pub fn f() { crate::alpha::f(); } }\n\
               }; }\nmodules!();\n";
    let err = std::panic::catch_unwind(|| crate_refs(src, 0)).expect_err(src);
    let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("\"alpha\", \"beta\""), "{msg}");
    crate_refs("#[cfg(test)]\nmod tests { use crate::alpha::X; }\n", 0);
    crate_refs("mod inner { use crate::alpha::X; }\n", 1);
}

/// Macros defined in `src/` name this crate through `$crate`, and the
/// attributes they emit carry path strings like parsed ones; their other
/// string arguments and docs stay prose.
#[test]
fn crate_refs_reads_macro_crate_paths_and_attributes() {
    let src = r##"
macro_rules! m { () => { $crate::alpha::run() }; }
macro_rules! s { () => {
    struct S {
        #[serde(serialize_with = "crate::beta::serialize")]
        a: u8,
        #[doc = "crate::ghost"]
        b: u8,
    }
    #![cfg_attr(unix, doc = "crate::ghost")]
    fn f() { println!("crate::ghost"); g("crate::ghost"); }
}; }
"##;
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["alpha", "beta"]);
    // Test-only attributes and items a macro emits are test code.
    let src = r#"
macro_rules! t { () => {
    #[cfg_attr(test, serde(with = "crate::ghost"))]
    struct S;
    #[cfg(test)]
    fn t() { crate::ghost::x() }
    #[cfg(test)]
    use crate::ghost::Y;
    #[derive(crate::ghost::Derive)]
    #[cfg(test)]
    struct U;
    #[cfg(test)]
    #[derive(crate::ghost::Derive)]
    struct V;
    #[cfg_attr(unix, serde(with = "crate::alpha"))]
    struct T;
    fn u() { crate::beta::x() }
}; }
"#;
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["alpha", "beta"]);
    // A macro's input gates nothing: the macro may drop the attribute and
    // emit what it gated, at the top level or nested in a definition.
    let src = r#"
macro_rules! emit { (#[cfg(test)] $item:item) => { $item }; }
emit! { #[cfg(test)] fn live() { crate::alpha::run(); } }
m! { #![cfg(test)] use crate::beta::Y; }
n! { #[cfg_attr(test, serde(with = "crate::gamma"))] struct S; }
macro_rules! wrap { () => { emit! { #[cfg(test)] fn live() { crate::delta::run(); } } }; }
"#;
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["alpha", "beta", "gamma", "delta"]);
    // A test-only node that is not a whole item (a generic parameter) gates
    // only itself: what follows is read.
    let src = "macro_rules! m { () => { fn f<#[cfg(test)] T>() { crate::cli::run(); } }; }";
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["cli"]);
    // A relative path string a `macro_rules!` body emits resolves where the
    // macro is invoked, so it fails at any depth.
    for attr in [
        "#[serde(with = \"super::cli\")]",
        "#[serde(with = \"self::cli\")]",
    ] {
        let src = format!("macro_rules! m {{ () => {{ {attr} struct S; }}; }}");
        let names: Vec<String> = crate_refs(&src, 2).into_iter().map(|r| r.0).collect();
        assert_eq!(names, [OPAQUE], "{src}");
    }
}

#[test]
fn crate_refs_carries_relative_depth_through_groups() {
    let names =
        |src: &str| -> Vec<String> { crate_refs(src, 2).into_iter().map(|r| r.0).collect() };
    assert_eq!(names("use super::{super::alpha};"), ["alpha"]);
    assert_eq!(
        names("use super::{super::{alpha, beta::X}};"),
        ["alpha", "beta"]
    );
    assert_eq!(names("use super::{super::*};"), [OPAQUE]);
    assert!(names("use super::{sibling, inner::X};").is_empty());
}

#[test]
fn crate_refs_ignores_docs_literals_macros_and_test_code() {
    let src = r#"
//! See [`crate::inner_doc`].
/// Links [`crate::outer_doc`] and crate::prose.
// crate::comment
/* crate::block */
const S: &str = "crate::string";
#[cfg(test)]
mod tests {
    use crate::test_only;
}
#[cfg(test)]
use crate::test_import;
#[doc = "crate::doc_attr"]
fn live() { crate::shipped(); }
"#;
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["shipped"]);
}

#[test]
fn crate_refs_follows_super_only_out_of_the_top_level_module() {
    // `src/a.rs`: `super::` is the crate root, but not inside an inline mod.
    let top = "use super::{one, two};\nmod inner { use super::stays; }\n";
    let names: Vec<String> = crate_refs(top, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["one", "two"]);
    // `src/a/b.rs`: one `super::` is `a`, two climb out to the root.
    let nested = "use super::sibling;\nuse super::super::three;\n";
    let names: Vec<String> = crate_refs(nested, 2).into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["three"]);
}

fn graph_of(edges: &[(&str, &str)]) -> Graph {
    let mut g = Graph::new();
    for (f, t) in edges {
        g.entry(f.to_string()).or_default().insert(t.to_string());
    }
    g
}

#[test]
fn cycle_check_catches_a_known_group_that_grew() {
    // Known `a <-> b` plus new `b -> c -> a`: the longer cycle reuses a known
    // edge, so only the component view sees it.
    let graph = graph_of(&[("a", "b"), ("b", "a"), ("b", "c"), ("c", "a")]);
    let errors = cycle_violations(&graph, &[&["a", "b"]]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("grew"), "{errors:?}");
    // Two known groups merged by a new edge each way.
    let graph = graph_of(&[
        ("a", "b"),
        ("b", "a"),
        ("c", "d"),
        ("d", "c"),
        ("b", "c"),
        ("d", "a"),
    ]);
    let errors = cycle_violations(&graph, &[&["a", "b"], &["c", "d"]]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("grew"), "{errors:?}");
}

#[test]
fn cycle_check_catches_a_new_cycle() {
    let graph = graph_of(&[("a", "b"), ("b", "a"), ("c", "d"), ("d", "e"), ("e", "c")]);
    let errors = cycle_violations(&graph, &[&["a", "b"]]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("new dependency cycle"), "{errors:?}");
}

#[test]
fn cycle_check_reports_a_split_or_broken_group_as_stale() {
    let graph = graph_of(&[("a", "b"), ("b", "a"), ("c", "d"), ("d", "c")]);
    assert!(cycle_violations(&graph, &[&["a", "b"], &["c", "d"]]).is_empty());
    // `{a, b, c}` split: only `a <-> b` is left.
    let graph = graph_of(&[("a", "b"), ("b", "a"), ("b", "c")]);
    let errors = cycle_violations(&graph, &[&["a", "b", "c"]]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("Shrink KNOWN_CYCLIC_GROUPS"),
        "{errors:?}"
    );
    // `{a, b}` broken entirely.
    let errors = cycle_violations(&graph_of(&[("a", "b")]), &[&["a", "b"]]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("Shrink KNOWN_CYCLIC_GROUPS"),
        "{errors:?}"
    );
}

#[test]
fn cycle_check_passes_an_acyclic_graph() {
    let graph = graph_of(&[("a", "b"), ("b", "c"), ("a", "c")]);
    assert!(cycle_violations(&graph, &[]).is_empty());
}

#[test]
fn crate_refs_flags_imports_that_hide_edges() {
    let opaque = |src: &str, depth: usize| -> Vec<String> {
        crate_refs(src, depth)
            .into_iter()
            .map(|r| r.0)
            .filter(|n| n == OPAQUE)
            .collect()
    };
    for src in [
        "use crate::*;",
        "use crate::{alpha, *};",
        "use crate as root;",
        "use crate::{self as root};",
        "use crate::self as root;",
        "extern crate self as root;",
        "macro_rules! m { () => { use crate as root; }; }",
    ] {
        assert_eq!(opaque(src, 1), [OPAQUE], "{src}");
    }
    assert_eq!(opaque("use super::*;", 1), [OPAQUE]);
    assert_eq!(opaque("use super::super::*;", 2), [OPAQUE]);
    // Aliasing the root through `super` hides edges just as `crate as` does.
    let aliased = "use super as root;\nfn f() { root::cli::run(); }";
    assert_eq!(opaque(aliased, 1), [OPAQUE]);
    assert_eq!(opaque("use super::super as root;", 2), [OPAQUE]);
    assert_eq!(opaque("use super::{super as root};", 2), [OPAQUE]);
    // At the crate root too, while ordinary module aliases are edges.
    assert_eq!(
        opaque("use crate as root;\nfn f() { root::beta::run(); }", 0),
        [OPAQUE]
    );
    assert!(opaque("use alpha as renamed;", 0).is_empty());
    assert!(opaque("use super as parent;", 2).is_empty());
    // Globs that stay inside the top-level module, or in test code, are fine.
    for (src, depth) in [
        ("use super::*;", 2),
        ("mod inner { use super::*; }", 1),
        ("#[cfg(test)]\nmod tests { use super::*; }", 1),
        ("use crate::alpha::*;", 1),
        ("pub(crate) use self::x::*;", 1),
        ("m! { mod inner { use super::*; } }", 1),
        // A leading `::` names an external crate.
        ("macro_rules! m { () => { use ::std::path::Path; }; }", 1),
        (
            "macro_rules! m { () => { use ::std::{fs, io as stdio}; }; }",
            1,
        ),
    ] {
        assert!(opaque(src, depth).is_empty(), "{src}");
    }
    let refs = crate_refs("macro_rules! m { () => { use ::std::path::Path; }; }", 1);
    assert!(refs.is_empty(), "{refs:?}");
}
