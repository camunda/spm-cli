//! Architecture guard: the top-level module graph of `src/` is pinned.
//!
//! Every `crate::<module>` reference in shipped code (comments, doc links,
//! string literals and `#[cfg(test)]` items excluded; grouped
//! `use crate::{a, b::c}` imports, inline paths, `super::` chains climbing to
//! the crate root and paths in attribute strings like
//! `#[serde(with = "crate::x")]` included) becomes an edge
//! between top-level modules, where a module is the first path component
//! under `src/` (`src/cli/**` is `cli`, `src/main.rs` is `main`). The result
//! must equal [`ALLOWED_EDGES`] exactly, so a layering change is a deliberate,
//! reviewed edit of that table, never a side effect. Crate-root globs and
//! aliases (`use crate::*`, `use super::*` out of a top-level module,
//! `use crate as x`, `extern crate self as x`) would hide edges, so they fail.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::source::{attribute, line_at, rust_files, shipped_code};

/// The allowed top-level dependency edges: the single source of truth for the
/// layering of `src/`. A module absent here depends on no other module.
const ALLOWED_EDGES: &[(&str, &[&str])] = &[
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

fn ident_at(code: &[char], i: usize) -> String {
    code[i.min(code.len())..]
        .iter()
        .take_while(|c| c.is_alphanumeric() || **c == '_')
        .collect()
}

fn skip_ws(code: &[char], mut i: usize) -> usize {
    while code.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// Whether `word` starts at `i` as a whole path segment (not part of a longer
/// identifier, a `$crate` macro path or a `foo::word` tail).
fn segment_at(code: &[char], i: usize, word: &str) -> bool {
    let before = i.checked_sub(1).map(|k| code[k]);
    if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == ':') {
        return false;
    }
    ident_at(code, i) == word
}

/// The offset just past `::` at `i` (whitespace allowed around it).
fn path_sep(code: &[char], i: usize) -> Option<usize> {
    let i = skip_ws(code, i);
    (code.get(i) == Some(&':') && code.get(i + 1) == Some(&':')).then(|| skip_ws(code, i + 2))
}

/// The name standing for a crate-root reference that hides which modules it
/// reaches: a glob (`crate::*`) or an alias of the root (`crate as x`).
const OPAQUE: &str = "*";

/// Whether `as` follows at `i` (whitespace allowed before it).
fn aliased_at(code: &[char], i: usize) -> bool {
    ident_at(code, skip_ws(code, i)) == "as"
}

/// The first segment named at `i`, just after a `crate::`-rooted prefix, or
/// [`OPAQUE`] for a glob or an aliased `self`.
fn first_segment(code: &[char], i: usize) -> String {
    let name = ident_at(code, i);
    if code.get(i) == Some(&'*') || (name == "self" && aliased_at(code, i + name.len())) {
        return OPAQUE.into();
    }
    name
}

/// The first segments named at `i`, just after a `crate::`-rooted prefix: one
/// identifier, or each item of a `{a, b::c, ..}` group.
fn first_segments(code: &[char], i: usize) -> Vec<String> {
    if code.get(i) != Some(&'{') {
        return vec![first_segment(code, i)];
    }
    let mut names = Vec::new();
    let (mut j, mut depth, mut item_start) = (i + 1, 0usize, true);
    while j < code.len() {
        match code[j] {
            '{' => depth += 1,
            '}' if depth == 0 => break,
            '}' => depth -= 1,
            ',' if depth == 0 => item_start = true,
            c if item_start && !c.is_whitespace() => {
                item_start = false;
                names.push(first_segment(code, j));
            }
            _ => {}
        }
        j += 1;
    }
    names
}

/// Whether the `{` at `i` opens an inline `mod name { .. }`.
fn opens_inline_mod(code: &[char], i: usize) -> bool {
    let mut j = i;
    while j > 0 && code[j - 1].is_whitespace() {
        j -= 1;
    }
    let name_end = j;
    while j > 0 && (code[j - 1].is_alphanumeric() || code[j - 1] == '_') {
        j -= 1;
    }
    if j == name_end {
        return false;
    }
    while j > 0 && code[j - 1].is_whitespace() {
        j -= 1;
    }
    j >= 3
        && code[j - 3..j] == ['m', 'o', 'd']
        && (j == 3 || !(code[j - 4].is_alphanumeric() || code[j - 4] == '_'))
}

/// The crate-root names a file's shipped code reaches: every `crate::<name>`
/// and every `super::` chain that climbs out of the top-level module.
/// `file_depth` is the file's module depth (`src/x.rs` and `src/x/mod.rs` are
/// 1, `src/x/y.rs` is 2, `src/main.rs` is 0).
fn crate_refs(src: &str, file_depth: usize) -> Vec<(String, usize)> {
    let mut code = shipped_code(src);
    // Paths in attribute strings (`#[serde(with = "crate::x::y")]`) are real
    // dependencies: restore the literals of every non-doc attribute.
    let original: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < code.len() {
        match attribute(&code, i) {
            Some((body, end)) => {
                if !body.starts_with("doc") {
                    code[i..end].copy_from_slice(&original[i..end]);
                }
                i = end;
            }
            None => i += 1,
        }
    }
    let line = |i: usize| line_at(&code, i);
    let mut braces: Vec<bool> = Vec::new();
    let mut refs = Vec::new();
    let mut i = 0;
    while i < code.len() {
        match code[i] {
            '{' => braces.push(opens_inline_mod(&code, i)),
            '}' => {
                braces.pop();
            }
            _ if segment_at(&code, i, "crate") => {
                let after = i + "crate".len();
                if let Some(rest) = path_sep(&code, after) {
                    refs.extend(
                        first_segments(&code, rest)
                            .into_iter()
                            .map(|n| (n, line(i))),
                    );
                } else if aliased_at(&code, after)
                    || first_segment(&code, skip_ws(&code, after)) == OPAQUE
                {
                    // `use crate as x;` / `extern crate self as x;`
                    refs.push((OPAQUE.into(), line(i)));
                }
            }
            _ if segment_at(&code, i, "super") => {
                let depth = file_depth + braces.iter().filter(|&&m| m).count();
                let (mut j, mut climbs) = (i, 0);
                while ident_at(&code, j) == "super" {
                    let Some(next) = path_sep(&code, j + "super".len()) else {
                        break;
                    };
                    climbs += 1;
                    j = next;
                }
                if climbs > 0 && climbs >= depth {
                    refs.extend(first_segments(&code, j).into_iter().map(|n| (n, line(i))));
                }
                i = j.max(i + 1);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    refs.retain(|(n, _)| !n.is_empty() && !matches!(n.as_str(), "self" | "super" | "crate"));
    refs
}

/// The top-level module `rel` (relative to `src/`) belongs to, and its depth.
fn module_of(rel: &Path) -> (String, usize) {
    let parts: Vec<String> = rel
        .iter()
        .map(|p| p.to_string_lossy().trim_end_matches(".rs").to_string())
        .collect();
    let last = parts.last().map(String::as_str);
    let depth = parts.len() - usize::from(matches!(last, Some("mod" | "main")));
    (parts[0].clone(), depth)
}

/// The actual graph of `src/`, with the first `file:line` of every edge.
fn source_graph() -> (Graph, BTreeMap<(String, String), String>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    let modules: BTreeSet<String> = files
        .iter()
        .map(|f| module_of(f.strip_prefix(&src).unwrap()).0)
        .collect();
    assert!(
        modules.contains("main") && modules.contains("cli") && modules.len() > 10,
        "scan found too few modules: {modules:?}"
    );
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
        "crate-root globs or aliases hide which modules they use, so the layering \
         guard ({TABLE}) cannot see those edges:\n{}\n\
         Import the items by name (`use crate::module::Item;`).",
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
";
    let names: Vec<String> = crate_refs(src, 1).into_iter().map(|r| r.0).collect();
    assert_eq!(
        names,
        ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"]
    );
    assert_eq!(crate_refs(src, 1)[4].1, 3, "references carry their line");
}

#[test]
fn crate_refs_ignores_docs_literals_macros_and_test_code() {
    let src = r#"
//! See [`crate::inner_doc`].
/// Links [`crate::outer_doc`] and crate::prose.
// crate::comment
/* crate::block */
const S: &str = "crate::string";
macro_rules! m { () => { $crate::macro_path() } }
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
    ] {
        assert_eq!(opaque(src, 1), [OPAQUE], "{src}");
    }
    assert_eq!(opaque("use super::*;", 1), [OPAQUE]);
    assert_eq!(opaque("use super::super::*;", 2), [OPAQUE]);
    // Globs that stay inside the top-level module, or in test code, are fine.
    for (src, depth) in [
        ("use super::*;", 2),
        ("mod inner { use super::*; }", 1),
        ("#[cfg(test)]\nmod tests { use super::*; }", 1),
        ("use crate::alpha::*;", 1),
        ("pub(crate) use self::x::*;", 1),
    ] {
        assert!(opaque(src, depth).is_empty(), "{src}");
    }
}
