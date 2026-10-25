//! Single-gateway guards: each side effect that has one canonical
//! implementation in `src/` (spawning `git`, writing spm's JSON files, taking
//! the OS state lock, listing directories) may only be reached from the files
//! allow-listed for it. A new call site elsewhere fails with its `file:line`;
//! route it through the gateway instead, or add the file deliberately with a
//! reason. An allow-listed file that no longer uses the primitive fails too
//! (stale entry), so the lists can only shrink.
//!
//! Only shipped code counts: comments, string literals and `#[cfg(test)]`
//! items (which legitimately spawn `git`/`mkfifo` or write fixtures) are
//! ignored, through the shared [`shipped_code`] view.

use std::path::Path;

use crate::source::{line_at, rust_files, shipped_code};

/// One gateway: the `patterns` that reach the primitive, and the only files
/// (relative to the crate root, `/`-separated) that may use them, each with
/// the reason it is allowed.
struct Guard {
    name: &'static str,
    /// Where new call sites should go instead.
    gateway: &'static str,
    patterns: &'static [&'static str],
    allowed: &'static [(&'static str, &'static str)],
}

const GUARDS: &[Guard] = &[
    Guard {
        name: "process spawning",
        gateway: "`git::command` (src/git/command.rs)",
        // Any process spawn, not only `git`: the program name is a literal or
        // a constant this scan cannot follow, and spm runs nothing but `git`.
        // `process::{` and `process::Command` catch a renamed import.
        patterns: &[
            "Command::new",
            "process::Command",
            "process::{",
            "CommandExt",
        ],
        allowed: &[(
            "src/git/command.rs",
            "the git gateway: hardened env, failure capture, option-injection guard",
        )],
    },
    Guard {
        name: "file writes",
        gateway: "`jsonfile::save`/`save_atomic`/`Staged` (src/jsonfile.rs)",
        patterns: &["fs::write", "File::create", "File::options", "OpenOptions"],
        allowed: &[
            (
                "src/jsonfile.rs",
                "the JSON gateway: in-place `save` and the atomic `Staged` replace",
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
        ],
        allowed: &[(
            "src/cli/session.rs",
            "the private `StateLock` behind every command's state lock",
        )],
    },
    Guard {
        // `File::lock` shares its name with `Mutex::lock` and `Stdin::lock`, so
        // every blocking `.lock(` call is pinned, whatever the receiver.
        name: "blocking `.lock()` calls",
        gateway: "`ScopeSession` (src/cli/session.rs) for file locks",
        patterns: &[".lock("],
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

/// 1-based lines of `src`'s shipped code holding `pattern`. Whitespace is
/// allowed between tokens (`Command :: new`), and a pattern starting or ending
/// with an identifier character only matches a whole identifier there
/// (`OpenOptions` does not match `OpenOptionsExt`).
fn occurrences(src: &str, pattern: &str) -> Vec<usize> {
    let code = shipped_code(src);
    let p: Vec<char> = pattern.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut lines = Vec::new();
    for i in 0..code.len() {
        if ident(p[0]) && i > 0 && ident(code[i - 1]) {
            continue;
        }
        let mut j = i;
        let matched = p.iter().enumerate().all(|(k, &want)| {
            if k > 0 && !(ident(p[k - 1]) && ident(want)) {
                while code.get(j).is_some_and(|c| c.is_whitespace()) {
                    j += 1;
                }
            }
            let hit = code.get(j) == Some(&want);
            j += 1;
            hit
        });
        if matched && !(ident(p[p.len() - 1]) && code.get(j).is_some_and(|&c| ident(c))) {
            lines.push(line_at(&code, i));
        }
    }
    lines
}

/// Every violation of `guard` over `files` (relative path, source): a use
/// outside the allow-list, or an allow-listed file that is gone or no longer
/// uses any of the patterns.
fn violations(guard: &Guard, files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, src) in files {
        let mut hits: Vec<(usize, &str)> = guard
            .patterns
            .iter()
            .flat_map(|&pat| occurrences(src, pat).into_iter().map(move |l| (l, pat)))
            .collect();
        hits.sort();
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
    let mut paths = Vec::new();
    rust_files(&root.join("src"), &mut paths);
    let files: Vec<(String, String)> = paths
        .iter()
        .map(|p| {
            let rel = p.strip_prefix(root).unwrap();
            let rel: Vec<_> = rel.iter().map(|c| c.to_string_lossy()).collect();
            (rel.join("/"), std::fs::read_to_string(p).unwrap())
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
"#;
    assert_eq!(occurrences(src, "Command::new"), vec![6]);
    assert_eq!(occurrences(src, "process::Command"), vec![6]);
    assert_eq!(occurrences(src, "fs::write"), vec![7]);
    assert_eq!(occurrences(src, "OpenOptions"), vec![9]);
    assert_eq!(occurrences(src, "try_lock"), Vec::<usize>::new());
}

#[test]
fn occurrences_matches_whole_identifiers() {
    let src = "fn f(m: M) { m.try_lock_shared(); m . lock (); x.read_dir_all(); fs::read_dir(p); }";
    assert_eq!(occurrences(src, "try_lock"), Vec::<usize>::new());
    assert_eq!(occurrences(src, "try_lock_shared"), vec![1]);
    assert_eq!(occurrences(src, ".lock("), vec![1]);
    assert_eq!(occurrences(src, "read_dir"), vec![1]);
}

#[test]
fn violations_flags_new_sites_and_stale_entries() {
    let guard = Guard {
        name: "spawn",
        gateway: "the gateway",
        patterns: &["Command::new"],
        allowed: &[
            ("src/gate.rs", "the gateway"),
            ("src/stale.rs", "used to spawn"),
            ("src/gone.rs", "deleted since"),
        ],
    };
    let files = [
        ("src/gate.rs", "fn run() { Command::new(\"git\"); }"),
        ("src/stale.rs", "// Command::new(\"git\")\nfn f() {}"),
        (
            "src/new.rs",
            "fn a() {}\nfn b() { Command::new(\"sh\"); }\n#[cfg(test)]\nmod t { fn c() { Command::new(\"git\"); } }",
        ),
        ("src/clean.rs", "fn d() {}"),
    ]
    .map(|(p, s)| (p.to_string(), s.to_string()));
    assert_eq!(
        violations(&guard, &files),
        vec![
            "spawn: src/stale.rs is allow-listed but no longer uses it; remove the stale entry",
            "spawn: src/new.rs:2 uses `Command::new`; go through the gateway (or allow-list the file with a reason)",
            "spawn: src/gone.rs is allow-listed but does not exist; remove the stale entry",
        ]
    );
}
