//! Single-gateway guards: each side effect that has one canonical
//! implementation in `src/` (spawning `git`, writing spm's JSON files, taking
//! the OS state lock, listing directories) may only be reached from the files
//! allow-listed for it. A new call site elsewhere fails with its `file:line`;
//! route it through the gateway instead, or add the file deliberately with a
//! reason. An allow-listed file that no longer uses the primitive fails too
//! (stale entry), so the lists can only shrink.
//!
//! Only shipped code counts: files are tokenized, so comments are gone and a
//! literal never matches, and identifiers only `#[cfg(test)]` code reaches
//! (tests legitimately spawn `git`/`mkfifo` or write fixtures) are ignored,
//! through the shared [`skip_test_only`] visitor. Anything the visitor does not
//! prove test-only (a macro body, an attribute argument) counts as shipped, so
//! a gap can only over-report.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::str::FromStr;

use proc_macro2::{Delimiter, Ident, Span, TokenStream, TokenTree};
use syn::ext::IdentExt;
use syn::visit::{self, Visit};

use crate::source::{parse, shipped_files, skip_test_only};

/// One gateway: what reaches the primitive, and the only files (relative to
/// the crate root, `/`-separated) that may use it, each with the reason it is
/// allowed.
struct Guard {
    name: &'static str,
    /// Where new call sites should go instead.
    gateway: &'static str,
    /// Token sequences (`Command::new`, `.lock(`) that reach the primitive.
    patterns: &'static [&'static str],
    /// Items an import (`use std::fs::{read, write};`, grouped or not) brings
    /// into scope as a bare name no pattern can see.
    imports: &'static [&'static str],
    allowed: &'static [(&'static str, &'static str)],
}

const GUARDS: &[Guard] = &[
    Guard {
        name: "process spawning",
        gateway: "`git::command` (src/git/command.rs)",
        // Any process spawn, not only `git`: the program name is a literal or
        // a constant this scan cannot follow, and spm runs nothing but `git`.
        patterns: &["Command::new", "process::Command", "CommandExt"],
        imports: &["process::Command"],
        allowed: &[(
            "src/git/command.rs",
            "the git gateway: hardened env, failure capture, option-injection guard",
        )],
    },
    Guard {
        name: "file writes",
        gateway: "`jsonfile::save`/`save_atomic`/`Staged` (src/jsonfile.rs)",
        patterns: &[
            "fs::write",
            "fs::copy",
            "fs::rename",
            "fs::*",
            "File::create",
            "File::create_new",
            "File::options",
            "OpenOptions",
        ],
        imports: &["fs::write", "fs::copy", "fs::rename"],
        allowed: &[
            (
                "src/jsonfile.rs",
                "the JSON gateway: in-place `save` and the atomic `Staged` replace \
                 (`fs::rename` publishes the staged file)",
            ),
            (
                "src/fsutil.rs",
                "`copy_tree` copies skill content (`fs::copy`) into a target",
            ),
            (
                "src/store.rs",
                "the empty restricted-checkout marker in the store, not JSON state",
            ),
            (
                "src/gitignore.rs",
                "appends spm's entries to the user's `.gitignore`, a text file",
            ),
            (
                "src/cli/session.rs",
                "opens the state lock files to lock them; nothing is written",
            ),
            (
                "src/pathid.rs",
                "opens a read-only Windows handle to query a file's identity",
            ),
        ],
    },
    Guard {
        name: "OS file locks",
        gateway: "`ScopeSession` (src/cli/session.rs)",
        patterns: &[
            "flock",
            "fcntl",
            "LockFileEx",
            "UnlockFileEx",
            "try_lock",
            "try_lock_shared",
            "lock_shared",
            "unlock",
            "TryLockError",
            "File::lock(",
        ],
        imports: &[],
        allowed: &[(
            "src/cli/session.rs",
            "the private `StateLock` behind every command's state lock",
        )],
    },
    Guard {
        // `File::lock` shares its name with `Mutex::lock` and `Stdin::lock`, so
        // every blocking `.lock()` call is pinned, whatever the receiver.
        name: "blocking `.lock()` calls",
        gateway: "`ScopeSession` (src/cli/session.rs) for file locks",
        patterns: &[".lock("],
        imports: &[],
        allowed: &[
            (
                "src/cli/session.rs",
                "the private `StateLock` waiting for a busy state lock",
            ),
            (
                "src/policy.rs",
                "`Mutex::lock` on the process-wide active policy",
            ),
            ("src/cli/common.rs", "`Stdin::lock` to read a confirmation"),
            ("src/cli/target.rs", "`Stdin::lock` to read a confirmation"),
        ],
    },
    Guard {
        name: "directory listing",
        gateway: "`fsutil::walk_tree` (src/fsutil.rs) for recursive traversal",
        patterns: &["read_dir"],
        imports: &[],
        allowed: &[
            (
                "src/fsutil.rs",
                "`walk_tree` itself, the boundary-aware traversal",
            ),
            ("src/links.rs", "one level: the per-project links files"),
            (
                "src/owned.rs",
                "one level: the per-project ownership records",
            ),
            (
                "src/skillcheck.rs",
                "one level: a container's immediate sub-skills",
            ),
            ("src/plugin.rs", "one level: a plugin's `skills/` directory"),
            (
                "src/vendor/mod.rs",
                "one level: stale skill dirs in a target's skills dir",
            ),
            (
                "src/store.rs",
                "the store root (one level), plus `dir_size`, a recursive size \
                 sum that must count symlinks without following them",
            ),
            (
                "src/cli/sync/guard.rs",
                "recursive scans of a linked working copy for symlinks and hard \
                 links: they must see links, not follow them as `walk_tree` does",
            ),
            (
                "src/cli/target.rs",
                "recursive walk collecting every nested symlink a remaining \
                 target depends on, to keep `target remove` from deleting it",
            ),
        ],
    },
];

/// Where an identifier starts: `(line, column)`, the same in every parse of
/// one source.
type Pos = (usize, usize);

fn pos(span: Span) -> Pos {
    let start = span.start();
    (start.line, start.column)
}

/// One token of a file: an identifier (unraw), a punctuation character, a
/// group delimiter, or `""` for a literal (so it never matches).
struct Token {
    text: String,
    line: usize,
    ident: Option<Pos>,
}

fn flatten(tokens: TokenStream, out: &mut Vec<Token>) {
    for tree in tokens {
        let line = tree.span().start().line;
        match tree {
            TokenTree::Ident(i) => out.push(Token {
                text: i.unraw().to_string(),
                line,
                ident: Some(pos(i.span())),
            }),
            TokenTree::Punct(p) => out.push(Token {
                text: p.as_char().to_string(),
                line,
                ident: None,
            }),
            TokenTree::Literal(_) => out.push(Token {
                text: String::new(),
                line,
                ident: None,
            }),
            TokenTree::Group(g) => {
                let (open, close) = match g.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                let delim = |text: &str, span: Span| Token {
                    text: text.into(),
                    line: span.start().line,
                    ident: None,
                };
                if !open.is_empty() {
                    out.push(delim(open, g.span_open()));
                }
                flatten(g.stream(), out);
                if !close.is_empty() {
                    out.push(delim(close, g.span_close()));
                }
            }
        }
    }
}

/// `pattern` as [`Token`] texts: identifiers whole, anything else one
/// character at a time, whitespace dropped.
fn pattern_tokens(pattern: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prev_ident = false;
    for c in pattern.chars() {
        let ident = c.is_alphanumeric() || c == '_';
        match out.last_mut() {
            Some(last) if ident && prev_ident => last.push(c),
            _ if c.is_whitespace() => {}
            _ => out.push(c.to_string()),
        }
        prev_ident = ident;
    }
    out
}

/// Every identifier position a walk reaches, macro bodies included.
#[derive(Default)]
struct Idents(HashSet<Pos>);

impl Idents {
    fn add_tokens(&mut self, tokens: TokenStream) {
        for tree in tokens {
            match tree {
                TokenTree::Ident(i) => {
                    self.0.insert(pos(i.span()));
                }
                TokenTree::Group(g) => self.add_tokens(g.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Idents {
    fn visit_ident(&mut self, i: &'ast Ident) {
        self.0.insert(pos(i.span()));
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.add_tokens(mac.tokens.clone());
        visit::visit_macro(self, mac);
    }
}

/// A shipped `use` leaf importing a name: the path it names, and its line.
struct Binding {
    path: Vec<String>,
    line: usize,
}

/// The identifiers and plain imports of shipped code: `#[cfg(test)]` nodes are
/// skipped whole.
#[derive(Default)]
struct Shipped {
    idents: Idents,
    prefix: Vec<String>,
    bindings: Vec<Binding>,
}

impl Shipped {
    fn bind(&mut self, last: &Ident) {
        let mut path = self.prefix.clone();
        path.push(last.unraw().to_string());
        let line = last.span().start().line;
        self.bindings.push(Binding { path, line });
    }
}

impl<'ast> Visit<'ast> for Shipped {
    fn visit_ident(&mut self, i: &'ast Ident) {
        self.idents.visit_ident(i);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.idents.add_tokens(mac.tokens.clone());
        visit::visit_macro(self, mac);
    }

    // Reached only through a shipped `use` (`visit_item_use` skips test ones),
    // and walked by hand to keep the path leading to each leaf.
    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        match tree {
            syn::UseTree::Path(p) => {
                self.visit_ident(&p.ident);
                self.prefix.push(p.ident.unraw().to_string());
                self.visit_use_tree(&p.tree);
                self.prefix.pop();
            }
            syn::UseTree::Name(n) => {
                self.visit_ident(&n.ident);
                self.bind(&n.ident);
            }
            syn::UseTree::Rename(r) => {
                self.visit_ident(&r.ident);
                self.visit_ident(&r.rename);
            }
            syn::UseTree::Glob(_) => {}
            syn::UseTree::Group(g) => {
                for tree in &g.items {
                    self.visit_use_tree(tree);
                }
            }
        }
    }

    skip_test_only!();
}

/// A file as the guards read it: its tokens, which of its identifiers only
/// test code reaches, and its shipped bindings.
struct Source {
    tokens: Vec<Token>,
    test_only: HashSet<Pos>,
    bindings: Vec<Binding>,
}

impl Source {
    fn new(src: &str) -> Self {
        let file = parse(src);
        let mut all = Idents::default();
        all.visit_file(&file);
        let mut shipped = Shipped::default();
        shipped.visit_file(&file);
        let mut tokens = Vec::new();
        let stream = TokenStream::from_str(src).unwrap_or_else(|e| panic!("cannot lex: {e}"));
        flatten(stream, &mut tokens);
        Source {
            tokens,
            test_only: all.0.difference(&shipped.idents.0).copied().collect(),
            bindings: shipped.bindings,
        }
    }

    /// 1-based lines of shipped code holding `pattern`'s tokens in sequence.
    fn occurrences(&self, pattern: &str) -> Vec<usize> {
        let want = pattern_tokens(pattern);
        self.tokens
            .windows(want.len())
            .filter(|run| {
                run.iter().zip(&want).all(|(t, w)| &t.text == w)
                    && run
                        .iter()
                        .all(|t| t.ident.is_none_or(|p| !self.test_only.contains(&p)))
            })
            .map(|run| run[0].line)
            .collect()
    }

    /// `(line, item)` for each shipped import of one of `guard`'s `imports`.
    fn bindings(&self, guard: &Guard) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for b in &self.bindings {
            let item = guard.imports.iter().find(|item| {
                let item: Vec<&str> = item.split("::").collect();
                b.path.len() >= item.len() && b.path[b.path.len() - item.len()..] == item
            });
            out.extend(item.map(|i| (b.line, (*i).to_string())));
        }
        out
    }
}

/// Every violation of `guard` over `files` (relative path, source): a use
/// outside the allow-list, or an allow-listed file that is gone or no longer
/// uses the primitive.
fn violations(guard: &Guard, files: &[(String, Source)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, src) in files {
        let hits: BTreeSet<(usize, String)> = guard
            .patterns
            .iter()
            .flat_map(|&pat| {
                src.occurrences(pat)
                    .into_iter()
                    .map(move |l| (l, pat.to_string()))
            })
            .chain(src.bindings(guard))
            .collect();
        let allowed = guard.allowed.iter().any(|(f, _)| f == path);
        if allowed && hits.is_empty() {
            out.push(format!(
                "{}: {path} is allow-listed but no longer uses it; remove the stale entry",
                guard.name
            ));
        } else if !allowed {
            for (line, pat) in hits {
                out.push(format!(
                    "{}: {path}:{line} uses `{pat}`; go through {} (or allow-list the file with a reason)",
                    guard.name, guard.gateway
                ));
            }
        }
    }
    for (f, _) in guard.allowed {
        if !files.iter().any(|(path, _)| path == f) {
            out.push(format!(
                "{}: {f} is allow-listed but does not exist; remove the stale entry",
                guard.name
            ));
        }
    }
    out
}

#[test]
fn side_effects_go_through_their_gateways() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files: Vec<(String, Source)> = shipped_files(&root.join("src/main.rs"))
        .iter()
        .map(|p| {
            let rel = p.strip_prefix(root).unwrap();
            let rel: Vec<_> = rel.iter().map(|c| c.to_string_lossy()).collect();
            (
                rel.join("/"),
                Source::new(&std::fs::read_to_string(p).unwrap()),
            )
        })
        .collect();
    assert!(
        files.iter().any(|(p, _)| p == "src/git/command.rs"),
        "scan found no sources: {:?}",
        files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    let problems: Vec<String> = GUARDS.iter().flat_map(|g| violations(g, &files)).collect();
    assert!(
        problems.is_empty(),
        "single-gateway violations:\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_allow_list_entry_has_a_reason_and_is_unique() {
    for g in GUARDS {
        for (i, (file, why)) in g.allowed.iter().enumerate() {
            assert!(!why.trim().is_empty(), "{}: {file} needs a reason", g.name);
            assert!(
                !g.allowed[..i].iter().any(|(f, _)| f == file),
                "{}: {file} listed twice",
                g.name
            );
        }
    }
}

fn occurrences(src: &str, pattern: &str) -> Vec<usize> {
    Source::new(src).occurrences(pattern)
}

#[test]
fn occurrences_sees_only_shipped_code() {
    let src = r#"
// Command::new("git") in a comment
/* std::fs::write(p, "x") */
const S: &str = "Command::new(\"git\")";
const GIT: &str = "git";
fn spawn() { let _ = std::process::Command :: new(GIT); }
fn write() { std::fs::write("ai.json", "{}").unwrap(); }
use std::os::windows::fs::OpenOptionsExt;
fn ok() { OpenOptions::new(); }
#[cfg(test)]
mod tests {
    fn fixture() { std::process::Command::new("git"); std::fs::write("x", ""); }
}
struct S2 {
    #[cfg(test)]
    f: std::process::Command,
}
fn m() { println!("{}", std::fs::read_dir(p)); }
"#;
    assert_eq!(occurrences(src, "Command::new"), vec![6]);
    assert_eq!(occurrences(src, "process::Command"), vec![6]);
    assert_eq!(occurrences(src, "fs::write"), vec![7]);
    assert_eq!(occurrences(src, "OpenOptions"), vec![9]);
    assert_eq!(occurrences(src, "try_lock"), Vec::<usize>::new());
    // Macro bodies are tokens syn does not parse: they count as shipped.
    assert_eq!(occurrences(src, "read_dir"), vec![18]);
}

#[test]
fn occurrences_matches_whole_identifiers() {
    let src = "fn f(m: M) { m.try_lock_shared(); m . lock (); x.read_dir_all(); fs::read_dir(p); r#unlock(); }";
    assert_eq!(occurrences(src, "try_lock"), Vec::<usize>::new());
    assert_eq!(occurrences(src, "try_lock_shared"), vec![1]);
    assert_eq!(occurrences(src, ".lock("), vec![1]);
    assert_eq!(occurrences(src, "read_dir"), vec![1]);
    assert_eq!(occurrences(src, "unlock"), vec![1]);
}

fn sources(files: &[(&str, &str)]) -> Vec<(String, Source)> {
    files
        .iter()
        .map(|(p, s)| (p.to_string(), Source::new(s)))
        .collect()
}

#[test]
fn violations_flags_new_sites_and_stale_entries() {
    let guard = Guard {
        name: "spawn",
        gateway: "the gateway",
        patterns: &["Command::new"],
        imports: &[],
        allowed: &[
            ("src/gate.rs", "the gateway"),
            ("src/stale.rs", "used to spawn"),
            ("src/gone.rs", "deleted since"),
        ],
    };
    let files = sources(&[
        ("src/gate.rs", "fn run() { Command::new(\"git\"); }"),
        ("src/stale.rs", "// Command::new(\"git\")\nfn f() {}"),
        (
            "src/new.rs",
            "fn a() {}\nfn b() { Command::new(\"sh\"); }\n#[cfg(test)]\nmod t { fn c() { Command::new(\"git\"); } }",
        ),
        ("src/clean.rs", "fn d() {}"),
    ]);
    assert_eq!(
        violations(&guard, &files),
        vec![
            "spawn: src/stale.rs is allow-listed but no longer uses it; remove the stale entry",
            "spawn: src/new.rs:2 uses `Command::new`; go through the gateway (or allow-list the file with a reason)",
            "spawn: src/gone.rs is allow-listed but does not exist; remove the stale entry",
        ]
    );
}

/// The real guard named `name`, so self-tests exercise the shipped tables.
fn guard(name: &str) -> &'static Guard {
    GUARDS.iter().find(|g| g.name == name).unwrap()
}

/// What the real guard `name` flags in `files`, as `path:line uses label`.
fn flagged(name: &str, files: &[(&str, &str)]) -> Vec<String> {
    let suffix = format!(
        "; go through {} (or allow-list the file with a reason)",
        guard(name).gateway
    );
    violations(guard(name), &sources(files))
        .into_iter()
        .filter_map(|v| {
            let v = v
                .strip_prefix(&format!("{name}: "))?
                .strip_suffix(&suffix)?;
            Some(v.to_string())
        })
        .collect()
}

#[test]
fn write_guard_sees_grouped_imports_and_create_new() {
    assert_eq!(
        flagged(
            "file writes",
            &[
                (
                    "src/grouped.rs",
                    "use std::fs::{read_to_string, write};\nfn f() { write(p, t); }",
                ),
                ("src/nested.rs", "use std::{fs::{File, write}};"),
                ("src/plain.rs", "use std::fs::copy;"),
                ("src/new.rs", "fn f() { File::create_new(p); }"),
                (
                    "src/moves.rs",
                    "fn f() { fs::copy(a, b); fs::rename(a, b); }"
                ),
                ("src/glob.rs", "use std::fs::*;"),
                (
                    "src/fine.rs",
                    "use std::fs::{self, File};\nuse std::io::Write;\nfn f() { fs::read(p); }",
                ),
                ("src/other.rs", "use std::fmt::{write, Write as _};"),
            ],
        ),
        vec![
            "src/grouped.rs:1 uses `fs::write`",
            "src/nested.rs:1 uses `fs::write`",
            "src/plain.rs:1 uses `fs::copy`",
            "src/new.rs:1 uses `File::create_new`",
            "src/moves.rs:1 uses `fs::copy`",
            "src/moves.rs:1 uses `fs::rename`",
            "src/glob.rs:1 uses `fs::*`",
        ]
    );
}

#[test]
fn lock_guard_sees_the_function_call_form() {
    assert_eq!(
        flagged(
            "OS file locks",
            &[("src/f.rs", "fn f(h: &File) { File::lock(h); }")]
        ),
        vec!["src/f.rs:1 uses `File::lock(`"]
    );
}
