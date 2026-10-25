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
//! reviewed edit of that table, never a side effect.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::source::{attribute, rust_files, shipped_code};

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

/// Dependency cycles tolerated today, each as the modules along it
/// (`[a, b]` is `a -> b -> a`). This list may only shrink: a cycle that is
/// broken must be removed (the guard fails on it as stale), and a new cycle is
/// a layering regression to fix, not to list here.
const KNOWN_CYCLES: &[&[&str]] = &[
    &["manifest", "policy"],
    &["ids", "manifest"],
    &["plugin", "vendor"],
    &["git", "policy"],
];

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

/// The first segments named at `i`, just after a `crate::`-rooted prefix: one
/// identifier, or each item of a `{a, b::c, ..}` group.
fn first_segments(code: &[char], i: usize) -> Vec<String> {
    if code.get(i) != Some(&'{') {
        return vec![ident_at(code, i)];
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
                names.push(ident_at(code, j));
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
    let line = |i: usize| code[..i].iter().filter(|&&c| c == '\n').count() + 1;
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
                if let Some(rest) = path_sep(&code, i + "crate".len()) {
                    refs.extend(
                        first_segments(&code, rest)
                            .into_iter()
                            .map(|n| (n, line(i))),
                    );
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
    let (mut graph, mut sites) = (Graph::new(), BTreeMap::new());
    for file in &files {
        let (from, depth) = module_of(file.strip_prefix(&src).unwrap());
        let text = std::fs::read_to_string(file).unwrap();
        for (name, line) in crate_refs(&text, depth) {
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
            let rel = file.strip_prefix(root).unwrap();
            sites
                .entry((from.clone(), to))
                .or_insert_with(|| format!("{}:{line}", rel.display()));
        }
    }
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

/// The edges along a listed cycle: `[a, b]` is `a -> b` and `b -> a`.
fn cycle_edges(cycle: &[&str]) -> Vec<(String, String)> {
    (0..cycle.len())
        .map(|k| (cycle[k].into(), cycle[(k + 1) % cycle.len()].into()))
        .collect()
}

/// A cycle in `graph` as the modules along it, if any.
fn find_cycle(graph: &Graph) -> Option<Vec<String>> {
    fn visit(
        node: &str,
        graph: &Graph,
        done: &mut BTreeSet<String>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if let Some(at) = path.iter().position(|n| n == node) {
            return Some(path[at..].to_vec());
        }
        if !done.insert(node.to_string()) {
            return None;
        }
        path.push(node.to_string());
        for next in graph.get(node).into_iter().flatten() {
            if let Some(cycle) = visit(next, graph, done, path) {
                return Some(cycle);
            }
        }
        path.pop();
        None
    }
    let mut done = BTreeSet::new();
    graph
        .keys()
        .find_map(|n| visit(n, graph, &mut done, &mut Vec::new()))
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
    let all = edges(&graph);
    let mut rest = graph.clone();
    for cycle in KNOWN_CYCLES {
        for (f, t) in cycle_edges(cycle) {
            assert!(
                all.contains(&(f.clone(), t.clone())),
                "KNOWN_CYCLES ({TABLE}) lists {cycle:?}, but `{f} -> {t}` is gone: \
                 the cycle is broken, remove it from KNOWN_CYCLES (it may only shrink)"
            );
            if let Some(tos) = rest.get_mut(&f) {
                tos.remove(&t);
            }
        }
    }
    if let Some(cycle) = find_cycle(&rest) {
        panic!(
            "dependency cycle outside KNOWN_CYCLES ({TABLE}): {} -> {}\n\
             Break it (move the shared code down a layer); KNOWN_CYCLES may only shrink.",
            cycle.join(" -> "),
            cycle[0]
        );
    }
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

#[test]
fn find_cycle_spots_indirect_cycles() {
    let graph = |edges: &[(&str, &str)]| {
        let mut g = Graph::new();
        for (f, t) in edges {
            g.entry(f.to_string()).or_default().insert(t.to_string());
        }
        g
    };
    assert_eq!(
        find_cycle(&graph(&[("a", "b"), ("b", "c"), ("a", "c")])),
        None
    );
    let cycle = find_cycle(&graph(&[("a", "b"), ("b", "c"), ("c", "a")])).unwrap();
    assert_eq!(cycle, ["a", "b", "c"]);
}
