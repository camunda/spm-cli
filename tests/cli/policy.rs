//! Source allowlist policy (`allowedSources` / machine policy file).

use crate::common::*;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Source allowlist policy (`allowedSources` / machine policy file)
// ---------------------------------------------------------------------------

/// A source that no test policy allows, and that is never served: if spm ever
/// contacted it the command would fail with a git error, not a policy one.
const EVIL_URL: &str = "https://evil.test/attacker/skill.git";

/// An empty directory to use as `PATH`: with it, spawning `git` fails, so a
/// command that still reports a *policy* error provably decided before any git
/// call. (Rust matches `PATH` case-insensitively on Windows, so this holds
/// there too.)
fn empty_path(sb: &Sandbox) -> PathBuf {
    let dir = sb.root.join("empty-path");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `spm <args>` with no `git` reachable, asserting failure; returns stderr.
fn fails_without_git(sb: &Sandbox, args: &[&str]) -> String {
    let out = sb
        .command(args)
        .env("PATH", empty_path(sb))
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "spm {args:?} unexpectedly succeeded: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr_of(&out)
}

/// The empty-`PATH` probe itself must be sound: with git unreachable, an
/// *allowed* add fails with a git error. Otherwise "policy error under an empty
/// PATH" would prove nothing about ordering.
#[test]
fn policy_probe_empty_path_really_blocks_git() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    set_project_policy(&sb, &["file://*"]);
    let err = fails_without_git(&sb, &["add", &sb.skill_url(), "--branch", "main"]);
    assert!(err.contains("git"), "{err}");
    assert!(!err.contains("not allowed"), "{err}");
}

/// Without any policy behavior is unchanged, and a project policy survives the
/// `ai.json` rewrite `add` performs.
#[test]
fn policy_absent_changes_nothing_and_add_preserves_allowed_sources() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--branch", "main", "--name", "a"]);
    assert!(!sb.read("ai.json").contains("allowedSources"));

    set_project_policy(&sb, &["file://*", "example.test/org/*"]);
    sb.ok(&["add", &sb.skill_url(), "--branch", "main", "--name", "b"]);
    let m: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    assert_eq!(
        m["allowedSources"],
        serde_json::json!(["file://*", "example.test/org/*"])
    );
    assert!(m["skills"]["b"].is_object(), "{m}");
}

/// `add` of a source outside the policy fails before any git call and leaves
/// `ai.json` byte-identical (and writes no `ai.lock`) — for every add variant.
#[test]
fn policy_denied_add_fails_before_git_and_leaves_manifest_untouched() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    set_project_policy(&sb, &["example.test/org/*"]);
    let before = sb.read("ai.json");
    let variants: &[&[&str]] = &[
        &["add", EVIL_URL, "--branch", "main"],
        &["add", EVIL_URL, "--tag", "v1"],
        &["add", EVIL_URL],
        &["add", EVIL_URL, "--plugin"],
        &["add", EVIL_URL, "--all", "--branch", "main"],
        &[
            "add",
            "git@evil.test:attacker/skill.git",
            "--branch",
            "main",
        ],
        &["add", "ssh://git@evil.test/attacker/skill.git"],
        // `?`/`#` end the authority: the host is `evil.test`, not the
        // allowed host after the `@`.
        &["add", "https://evil.test?@example.test/org/skill.git"],
        &["add", "https://evil.test#@example.test/org/skill.git"],
        // git decodes SSH URLs before splitting the host off: this is
        // `evil.test`, with the rest as its path.
        &["add", "ssh://evil.test%2Fgit@example.test/org/skill.git"],
        // For SSH a trailing space is part of the repository path git uses.
        &["add", "git@example.test:org/skill.git ", "--branch", "main"],
        // The shorthand's ref lookup would need git: the expanded URL is
        // checked first.
        &["add", "github.com/attacker/skill@v1"],
        &["add", "github.com/attacker/skill"],
    ];
    for args in variants {
        let err = fails_without_git(&sb, args);
        assert!(err.contains("is not allowed"), "{args:?}: {err}");
        assert!(err.contains("project policy"), "{args:?}: {err}");
        assert!(err.contains("`example.test/org/*`"), "{args:?}: {err}");
        assert_eq!(sb.read("ai.json"), before, "{args:?} touched ai.json");
        assert!(!sb.project.join("ai.lock").exists(), "{args:?}");
    }
    assert_eq!(sb.store_entries(), 0);
}

/// The shorthand expands to `https://github.com/...` and that identity is what
/// the policy judges: an allowed org works end to end.
#[test]
fn policy_judges_the_expanded_shorthand() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    set_project_policy(&sb, &["github.com/owner/*"]);
    let out = sb.spm_github(&["add", "github.com/owner/repo@v0.1.0", "--name", "greet"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(sb
        .read("ai.json")
        .contains("https://github.com/owner/repo.git"));
}

/// HTTPS, `ssh://` and scp-style spellings of one repository share an
/// identity, so one pattern allows all three.
#[test]
fn policy_allows_a_matching_remote_in_every_spelling() {
    for url in [
        HTTPS_URL,
        "https://user@EXAMPLE.test/org/skill",
        "ssh://git@example.test/org/skill.git",
        SSH_URL,
    ] {
        let sb = Sandbox::new();
        sb.ok(&["init", "--target", "claude"]);
        set_project_policy(&sb, &["example.test/org/*"]);
        let good = sb.skill_url();
        let out = sb.spm_remotes(
            &["add", url, "--branch", "main", "--name", "greet"],
            &[(url, &good)],
        );
        assert!(out.status.success(), "{url}: {}", stderr_of(&out));
        assert!(sb.read("ai.lock").contains(url), "{url}");
        // A re-install from the lock passes the policy too.
        let out = sb.spm_remotes(&["install", "--locked"], &[(url, &good)]);
        assert!(out.status.success(), "{url}: {}", stderr_of(&out));
    }
}

/// An exact pattern allows only that repository; paths are case-sensitive.
#[test]
fn policy_exact_pattern_and_path_case() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    set_project_policy(&sb, &["example.test/org/skill"]);
    let good = sb.skill_url();
    let other = "https://example.test/org/other.git";
    let upper = "https://example.test/ORG/skill.git";
    for denied in [other, upper] {
        let err = fails_without_git(&sb, &["add", denied, "--branch", "main"]);
        assert!(err.contains("is not allowed"), "{denied}: {err}");
    }
    let out = sb.spm_remotes(
        &["add", HTTPS_URL, "--branch", "main", "--name", "greet"],
        &[(HTTPS_URL, &good)],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
}

/// Local (`file://`/path) sources are denied by any policy unless it lists
/// `file://*`.
#[test]
fn policy_local_sources_need_file_star() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    set_project_policy(&sb, &["example.test/org/*"]);
    let err = fails_without_git(&sb, &["add", &sb.skill_url(), "--branch", "main"]);
    assert!(err.contains("a local source"), "{err}");
    assert!(err.contains("file://*"), "{err}");

    // `file://*` covers local paths only, never a remote that merely looks
    // like one: a backslash before the colon is still scp-style SSH to git.
    set_project_policy(&sb, &["file://*"]);
    let mut remotes = vec![r"domain\user@evil.test:attacker/skill.git"];
    // Off Windows git reads a drive-like `x:path` as SSH to host `x`.
    if cfg!(not(windows)) {
        remotes.push("x:attacker/skill.git");
    }
    for url in remotes {
        let err = fails_without_git(&sb, &["add", url, "--branch", "main"]);
        assert!(err.contains("is not allowed"), "{url}: {err}");
    }

    set_project_policy(&sb, &["example.test/org/*", "file://*"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
}

/// On Windows a drive path is a local source, allowed by `file://*`.
#[cfg(windows)]
#[test]
fn policy_windows_drive_paths_are_local() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    set_project_policy(&sb, &["file://*"]);
    let path = sb.skill_repo.display().to_string();
    assert_eq!(path.as_bytes()[1], b':', "{path}");
    sb.ok(&["add", &path, "--branch", "main", "--name", "greet"]);
    set_project_policy(&sb, &["example.test/org/*"]);
    let err = fails_without_git(&sb, &["install"]);
    assert!(err.contains("a local source"), "{err}");
}

/// An empty `allowedSources` is a policy that allows nothing.
#[test]
fn policy_empty_list_denies_everything() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    set_project_policy(&sb, &[]);
    let err = fails_without_git(&sb, &["add", &sb.skill_url(), "--branch", "main"]);
    assert!(err.contains("allows nothing"), "{err}");
}

/// `--protocol` rewrites (and `--protocol-fallback` retries) cannot turn a
/// denied source into an allowed one: the URL as given is judged, and so is
/// every URL actually contacted.
#[test]
fn policy_cannot_be_bypassed_with_protocol_flags() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    set_project_policy(&sb, &["example.test/org/*"]);
    let good = sb.skill_url();
    let cases: &[(&str, &[&str])] = &[
        ("git@evil.test:attacker/skill.git", &["--protocol", "https"]),
        (EVIL_URL, &["--protocol", "ssh"]),
        (EVIL_URL, &["--protocol-fallback"]),
    ];
    for (url, flags) in cases {
        let mut args = vec!["add", url, "--branch", "main"];
        args.extend_from_slice(flags);
        // Even with every spelling of the denied repo served, it is refused.
        let out = sb.spm_remotes(
            &args,
            &[
                ("https://evil.test/attacker/skill.git", &good),
                ("git@evil.test:attacker/skill.git", &good),
            ],
        );
        assert!(!out.status.success(), "{args:?}");
        let err = stderr_of(&out);
        assert!(err.contains("is not allowed"), "{args:?}: {err}");
    }
    assert!(!sb.project.join("ai.lock").exists());
}

/// A transitive (nested `ai.json`) source outside the policy fails the whole
/// install, naming the chain of skills that pulled it in, before it is fetched;
/// nothing is locked or materialized.
#[test]
fn policy_denies_transitive_sources_and_names_the_chain() {
    let sb = Sandbox::new();
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"helper":{{"git":"{EVIL_URL}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    let top_url = "https://example.test/org/top.git";
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"allowedSources":["example.test/org/*"],
                "skills":{{"top":{{"git":"{top_url}","branch":"main"}}}}}}"#
        ),
    );
    // `evil.test` is not served: reaching it would be a git error instead.
    let out = sb.spm_remotes(&["install"], &[(top_url, &top)]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("nested skill `helper` required by `top` (via top -> helper)"),
        "{err}"
    );
    assert!(
        err.contains(EVIL_URL) && err.contains("is not allowed"),
        "{err}"
    );
    assert!(!sb.project.join("ai.lock").exists());
    assert!(copilot_skill_dirs(&sb).is_empty());
}

/// A denied source two levels down reports the full chain.
#[test]
fn policy_denies_deep_transitive_sources_with_full_chain() {
    let sb = Sandbox::new();
    let mid_url = "https://example.test/org/mid.git";
    let mid = make_repo(
        &sb.root.join("mid"),
        &[
            ("SKILL.md", "---\nname: mid\n---\nMid.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"evil":{{"git":"{EVIL_URL}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"mid":{{"git":"{mid_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    let top_url = "https://example.test/org/top.git";
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"allowedSources":["example.test/org/*"],
                "skills":{{"top":{{"git":"{top_url}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm_remotes(&["install"], &[(top_url, &top), (mid_url, &mid)]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("nested skill `evil`"), "{err}");
    assert!(err.contains("(via top -> "), "{err}");
    assert!(err.contains(" -> evil)"), "{err}");
    assert!(!sb.project.join("ai.lock").exists());
}

/// Allowed transitive sources resolve normally under a policy.
#[test]
fn policy_allows_matching_transitive_sources() {
    let sb = Sandbox::new();
    let leaf_url = "https://example.test/org/leaf.git";
    let leaf = make_repo(
        &sb.root.join("leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")],
    );
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    let top_url = "https://example.test/org/top.git";
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"allowedSources":["example.test/org/*"],
                "skills":{{"top":{{"git":"{top_url}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm_remotes(&["install"], &[(top_url, &top), (leaf_url, &leaf)]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(copilot_skill_dirs(&sb)
        .iter()
        .any(|d| d.starts_with("top__leaf-")));
}

/// Only the root project sets the policy: a dependency's own `ai.json` that
/// declares `allowedSources` is rejected rather than silently honored.
#[test]
fn policy_in_a_dependency_manifest_is_rejected() {
    let sb = Sandbox::new();
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                r#"{"allowedSources":["evil.test/*"],"skills":{}}"#,
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    let err = fails(&sb, &["install"]);
    assert!(
        err.contains("`allowedSources` is not allowed in a dependency's manifest"),
        "{err}"
    );
}

/// Invalid patterns are a load error for every command reading `ai.json`.
#[test]
fn policy_invalid_project_patterns_are_rejected() {
    for bad in [
        "https://github.com/org/*",
        "github.com",
        "github.com/*/repo",
        "github.com/org*",
        "",
    ] {
        let sb = Sandbox::new();
        sb.ok(&["init", "--target", "claude"]);
        set_project_policy(&sb, &[bad]);
        let err = fails(&sb, &["install"]);
        assert!(err.contains("allowedSources"), "{bad:?}: {err}");
        assert!(
            err.contains("invalid source pattern") || err.contains("does not match"),
            "{bad:?}: {err}"
        );
    }
}

/// The machine policy (`SPM_POLICY` or `$SPM_HOME/policy.json`) applies even
/// with no project policy, and a project cannot widen it.
#[test]
fn policy_machine_layer_applies_and_cannot_be_widened() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let machine = sb.root.join("org-policy.json");
    write_policy_file(&machine, &["example.test/org/*"]);

    // Machine policy alone denies a local source.
    let out = sb
        .command(&["add", &sb.skill_url(), "--branch", "main"])
        .env("SPM_POLICY", &machine)
        .env("PATH", empty_path(&sb))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("machine policy"), "{err}");
    assert!(err.contains("(from SPM_POLICY)"), "{err}");

    // A wider project policy does not loosen it: both layers must allow.
    set_project_policy(&sb, &["example.test/*", "file://*"]);
    let wide = "https://example.test/other/skill.git";
    let out = sb
        .command(&["add", wide, "--branch", "main"])
        .env("SPM_POLICY", &machine)
        .env("PATH", empty_path(&sb))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("machine policy"), "{err}");
    assert!(
        !err.contains("project policy"),
        "only the machine layer denied: {err}"
    );

    // Allowed by both layers: works.
    let good = sb.skill_url();
    let out = sb
        .remote_command(
            &["add", HTTPS_URL, "--branch", "main", "--name", "greet"],
            &[(HTTPS_URL, &good)],
        )
        .env("SPM_POLICY", &machine)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr_of(&out));
}

/// A project policy can narrow the machine policy, and a source denied by both
/// layers names both.
#[test]
fn policy_project_narrows_machine_and_errors_name_every_denying_layer() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    write_policy_file(&sb.spm_home.join("policy.json"), &["example.test/*"]);
    set_project_policy(&sb, &["example.test/org/*"]);

    let err = fails_without_git(&sb, &["add", "https://example.test/other/x.git"]);
    assert!(err.contains("project policy"), "{err}");
    assert!(!err.contains("machine policy"), "{err}");

    let err = fails_without_git(&sb, &["add", EVIL_URL]);
    assert!(err.contains("machine policy"), "{err}");
    assert!(
        !err.contains("(from SPM_POLICY)"),
        "default location: {err}"
    );
    assert!(err.contains("and by the project policy"), "{err}");
}

/// The machine policy also governs the global (`-g`) scope.
#[test]
fn policy_machine_layer_applies_to_global_scope() {
    let sb = Sandbox::new();
    write_policy_file(&sb.spm_home.join("policy.json"), &["example.test/org/*"]);
    sb.ok(&["init", "-g", "--target", "copilot"]);
    let err = fails_without_git(&sb, &["add", "-g", EVIL_URL, "--branch", "main"]);
    assert!(err.contains("is not allowed"), "{err}");
    assert!(err.contains("machine policy"), "{err}");
}

/// A broken machine policy never fails open: a missing `SPM_POLICY` file, bad
/// JSON, an unknown key or an invalid pattern are all errors. An empty
/// `SPM_POLICY` is unset.
#[test]
fn policy_broken_machine_policy_fails_closed() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let run = |policy: &Path| {
        sb.command(&["add", &sb.skill_url(), "--branch", "main"])
            .env("SPM_POLICY", policy)
            .output()
            .unwrap()
    };

    let out = run(&sb.root.join("no-such-policy.json"));
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("reading the machine policy"),
        "{}",
        stderr_of(&out)
    );

    let cases = [
        ("bad.json", "{ not json", "parsing the machine policy"),
        (
            "typo.json",
            r#"{"allowedSource":["file://*"]}"#,
            "unknown field",
        ),
        ("missing.json", r#"{}"#, "missing field"),
        (
            "pattern.json",
            r#"{"allowedSources":["https://x.test/a/*"]}"#,
            "invalid source pattern",
        ),
    ];
    for (file, body, needle) in cases {
        let path = sb.root.join(file);
        std::fs::write(&path, body).unwrap();
        let out = run(&path);
        assert!(!out.status.success(), "{file}");
        let err = stderr_of(&out);
        assert!(err.contains(needle), "{file}: {err}");
        assert!(!sb.read("ai.json").contains("greet"), "{file}");
    }

    // `$schema` is accepted alongside the list.
    let path = sb.root.join("ok.json");
    std::fs::write(&path, r#"{"$schema":"x","allowedSources":["file://*"]}"#).unwrap();
    let out = run(&path);
    assert!(out.status.success(), "{}", stderr_of(&out));

    // Empty SPM_POLICY means "unset" (and there is no default file).
    let out = sb
        .command(&["install"])
        .env("SPM_POLICY", "")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr_of(&out));
}

/// The policy also governs installs from an existing `ai.lock` with a warm
/// store: `install`, `install --locked` and `update` refuse a pinned source
/// that is no longer allowed, without running git.
#[test]
fn policy_applies_to_installs_from_the_lock_and_cache() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    assert!(sb.store_entries() > 0);
    let lock = sb.read("ai.lock");

    write_policy_file(&sb.spm_home.join("policy.json"), &["example.test/org/*"]);
    for args in [
        &["install"][..],
        &["install", "--locked"],
        &["update"],
        &["update", "greet"],
    ] {
        let err = fails_without_git(&sb, args);
        assert!(
            err.contains("1 dependency is not allowed"),
            "{args:?}: {err}"
        );
        assert!(err.contains("skill `greet`"), "{args:?}: {err}");
        assert_eq!(sb.read("ai.lock"), lock, "{args:?} rewrote ai.lock");
    }

    // The same lock installs again once the policy allows the source.
    write_policy_file(&sb.spm_home.join("policy.json"), &["file://*"]);
    sb.ok(&["install", "--locked"]);
}

/// Every denied direct dependency is reported at once.
#[test]
fn policy_reports_all_denied_dependencies_together() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude"],"allowedSources":["example.test/org/*"],
                "skills":{{"a":{{"git":"{EVIL_URL}","branch":"main"}},
                          "b":{{"git":"git@evil.test:x/b.git","branch":"main"}}}},
                "plugins":{{"p":{{"git":"https://evil.test/x/p.git","branch":"main"}}}}}}"#
        ),
    );
    let err = fails_without_git(&sb, &["install"]);
    assert!(err.contains("3 dependencies are not allowed"), "{err}");
    for needle in ["skill `a`", "skill `b`", "plugin `p`"] {
        assert!(err.contains(needle), "{needle}: {err}");
    }
}

/// `outdated` never contacts a denied source: it reports an error row.
#[test]
fn policy_denied_source_is_an_outdated_error_row() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    write_policy_file(&sb.spm_home.join("policy.json"), &["example.test/org/*"]);
    let out = sb
        .command(&["outdated"])
        .env("PATH", empty_path(&sb))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        stderr_of(&out)
    );
    assert!(all.contains("is not allowed"), "{all}");
    assert!(all.contains("could not check 1"), "{all}");
}

/// A commit pin is never queried remotely, but a denied commit-pinned source
/// is still an error row (not `pinned`) — and no git runs.
#[test]
fn policy_denied_commit_pin_is_an_outdated_error_row() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let sha = skill_head(&sb.skill_repo);
    sb.ok(&["add", &sb.skill_url(), "--commit", &sha, "--name", "greet"]);
    write_policy_file(&sb.spm_home.join("policy.json"), &["example.test/org/*"]);
    let out = sb
        .command(&["outdated", "--json"])
        .env("PATH", empty_path(&sb))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let row = &report["skills"][0];
    assert_eq!(row["status"], "error", "{stdout}");
    assert!(
        row["error"].as_str().unwrap().contains("is not allowed"),
        "{stdout}"
    );
}

/// A dangling `$SPM_HOME/policy.json` symlink (a managed policy whose target
/// vanished) is a configured-but-broken policy, not an absent one.
#[cfg(unix)]
#[test]
fn policy_dangling_default_symlink_fails_closed() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    std::fs::create_dir_all(&sb.spm_home).unwrap();
    std::os::unix::fs::symlink(
        sb.root.join("managed/policy.json"),
        sb.spm_home.join("policy.json"),
    )
    .unwrap();
    let err = fails(&sb, &["add", &sb.skill_url(), "--branch", "main"]);
    assert!(err.contains("reading the machine policy"), "{err}");
    assert!(!sb.read("ai.json").contains("greet"));
}

/// Commands that never touch the network keep working under a policy that
/// denies the installed sources, so a user can still inspect and clean up.
#[test]
fn policy_does_not_block_offline_commands() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    write_policy_file(&sb.spm_home.join("policy.json"), &["example.test/org/*"]);
    let list = sb.ok(&["list"]);
    assert!(list.contains("greet"), "{list}");
    sb.ok(&["clean"]);
}

/// A hand-edited `ai.lock` can't smuggle in a denied source: the lock below
/// pins `greet` to a denied URL (with a valid store key and a warm cache, and
/// the denied URL even served), while `ai.json` names an allowed one. spm never
/// uses the locked URL: `install` re-resolves from the allowed spec, and
/// `install --locked` refuses the mismatch.
#[test]
fn policy_hand_edited_lock_cannot_bypass_it() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    let remotes = [(EVIL_URL, good.as_str()), (HTTPS_URL, good.as_str())];
    let add = |url: &str| {
        let out = sb.spm_remotes(
            &["add", url, "--branch", "main", "--name", "greet", "--force"],
            &remotes,
        );
        assert!(out.status.success(), "{}", stderr_of(&out));
    };
    // Produce a well-formed lock (and store entry) for the denied URL, then
    // point `ai.json` at the allowed one, leaving the lock as the tamper.
    add(EVIL_URL);
    let mut m: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    m["skills"]["greet"]["git"] = serde_json::json!(HTTPS_URL);
    m["allowedSources"] = serde_json::json!(["example.test/org/*"]);
    write_manifest(&sb, &serde_json::to_string_pretty(&m).unwrap());
    assert!(sb.read("ai.lock").contains(EVIL_URL));

    let out = sb.spm_remotes(&["install", "--locked"], &remotes);
    assert!(!out.status.success());
    assert!(
        sb.read("ai.lock").contains(EVIL_URL),
        "--locked never rewrites"
    );

    let out = sb.spm_remotes(&["install"], &remotes);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let lock = sb.read("ai.lock");
    assert!(
        lock.contains(HTTPS_URL) && !lock.contains(EVIL_URL),
        "{lock}"
    );
}

/// A bare name can be a configured git remote (`git ls-remote origin`
/// contacts `remote.origin.url`), so under a policy it is not a local source
/// even for `file://*`: the alias could point anywhere.
#[test]
fn policy_remote_alias_is_not_a_local_source() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    git_in(&sb.project, &["init", "-q"]);
    git_in(&sb.project, &["remote", "add", "origin", &sb.skill_url()]);
    set_project_policy(&sb, &["file://*"]);
    let before = sb.read("ai.json");
    for name in ["origin", "origin/x"] {
        let err = fails_without_git(&sb, &["add", name, "--branch", "main"]);
        assert!(err.contains("is not allowed"), "{name}: {err}");
        assert_eq!(sb.read("ai.json"), before, "{name}");
    }
    // An absolute path can't be a remote name, so it stays a local source.
    let abs = sb.skill_repo.display().to_string();
    sb.ok(&["add", &abs, "--branch", "main", "--name", "greet"]);
}

/// A minimal HTTP/1.1 server on 127.0.0.1 for hermetic transport tests: each
/// request path (query string included) goes to `respond(port, path)`, which
/// returns the status line, extra headers and body. Returns the port and every
/// request path seen.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

type Response = (&'static str, Vec<String>, Vec<u8>);

fn http_server(respond: impl Fn(u16, &str) -> Response + Send + 'static) -> (u16, Seen) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Default::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            loop {
                let mut header = String::new();
                match reader.read_line(&mut header) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if header.trim().is_empty() => break,
                    Ok(_) => {}
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            log.lock().unwrap().push(path.clone());
            let (status, headers, body) = respond(port, &path);
            let mut head = format!("HTTP/1.1 {status}\r\n");
            for h in headers {
                head.push_str(&h);
                head.push_str("\r\n");
            }
            head.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ));
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (port, seen)
}

/// Answers every request under `/allowed/` with a 301 to the same path under
/// `/denied/`, and 404s everything else.
fn redirecting_http_server() -> (u16, Seen) {
    http_server(|port, path| match path.strip_prefix("/allowed/") {
        Some(rest) => (
            "301 Moved Permanently",
            vec![format!("Location: http://127.0.0.1:{port}/denied/{rest}")],
            Vec::new(),
        ),
        None => ("404 Not Found", Vec::new(), Vec::new()),
    })
}

/// A dumb-HTTP static file server rooted at `root` (query strings ignored).
fn static_http_server(root: PathBuf) -> (u16, Seen) {
    http_server(move |_, path| {
        let rel = path.split('?').next().unwrap_or("").trim_start_matches('/');
        match std::fs::read(root.join(rel)) {
            Ok(body) if !rel.is_empty() => ("200 OK", Vec::new(), body),
            _ => ("404 Not Found", Vec::new(), Vec::new()),
        }
    })
}

/// The proxy-free command for a `127.0.0.1` HTTP test server.
fn local_http_command(sb: &Sandbox, args: &[&str]) -> Command {
    let mut cmd = sb.command(args);
    cmd.env("NO_PROXY", "*").env("no_proxy", "*");
    for var in ["http_proxy", "HTTP_PROXY", "all_proxy", "ALL_PROXY"] {
        cmd.env_remove(var);
    }
    cmd
}

/// Under a policy git must not follow an HTTP redirect from an allowed URL to
/// a destination the policy never judged; without a policy, redirects are
/// followed as before.
#[test]
fn policy_blocks_http_redirects_to_unchecked_destinations() {
    let (port, seen) = redirecting_http_server();
    let base = format!("http://127.0.0.1:{port}/allowed/");
    let url = format!("{base}skill.git");
    let hits = |prefix: &str| {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(prefix))
            .count()
    };
    let policy_sandbox = || {
        let sb = Sandbox::new();
        sb.ok(&["init", "--target", "claude"]);
        set_project_policy(&sb, &[&format!("127.0.0.1:{port}/allowed/*")]);
        sb
    };

    // With a policy allowing only `/allowed/*` the redirect is not followed,
    // even when the user's git config turns redirects on — generically or for
    // a URL prefix / the exact URL, which git would otherwise prefer over a
    // generic setting.
    for user_key in [
        None,
        Some("http.followRedirects".to_string()),
        Some(format!("http.{base}.followRedirects")),
        Some(format!("http.{url}.followRedirects")),
    ] {
        let sb = policy_sandbox();
        seen.lock().unwrap().clear();
        let mut cmd = local_http_command(&sb, &["add", &url, "--branch", "main"]);
        if let Some(key) = &user_key {
            with_user_git_config(&mut cmd, key, "true");
        }
        let out = cmd.output().unwrap();
        assert!(!out.status.success(), "{user_key:?}");
        assert!(
            hits("/allowed/") > 0,
            "{user_key:?}: the allowed URL was contacted: {}",
            stderr_of(&out)
        );
        assert_eq!(hits("/denied/"), 0, "{user_key:?}: {}", stderr_of(&out));
    }

    // A URL `-c` can't pin (it contains `=`): fine without a conflicting
    // setting, refused before any request when the user's config enables
    // redirects for it.
    let eq_url = format!("{base}a=b.git");
    let sb = policy_sandbox();
    seen.lock().unwrap().clear();
    let out = local_http_command(&sb, &["add", &eq_url, "--branch", "main"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(hits("/allowed/") > 0, "{}", stderr_of(&out));
    assert_eq!(hits("/denied/"), 0, "{}", stderr_of(&out));
    let sb = policy_sandbox();
    seen.lock().unwrap().clear();
    let mut cmd = local_http_command(&sb, &["add", &eq_url, "--branch", "main"]);
    with_user_git_config(&mut cmd, &format!("http.{base}.followRedirects"), "true");
    let out = cmd.output().unwrap();
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("refusing to contact") && err.contains("followRedirects"),
        "{err}"
    );
    assert!(seen.lock().unwrap().is_empty(), "{err}");

    // No policy: unchanged behavior, git follows the redirect.
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let out = local_http_command(&sb, &["add", &url, "--branch", "main"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "the redirect target is a 404");
    assert!(hits("/denied/") > 0, "{}", stderr_of(&out));
}

/// Simulate a setting in the user's own git config (environment config sits
/// above the global/system files, like a user's `~/.gitconfig` entry would).
fn with_user_git_config(cmd: &mut Command, key: &str, value: &str) {
    cmd.env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", key)
        .env("GIT_CONFIG_VALUE_0", value);
}

/// Dumb-HTTP `objects/info/http-alternates` are another way for a server to
/// send git elsewhere. git treats them like redirects, so under a policy
/// (`http.followRedirects=false`, which overrides user config) an allowed
/// repository can't pull objects from a denied alternate — even when the
/// user's own git config says `http.followRedirects=true`.
#[test]
fn policy_blocks_dumb_http_alternates_to_unchecked_destinations() {
    let sb = Sandbox::new();
    let srv = sb.root.join("srv");
    let allowed = srv.join("allowed/repo.git");
    let denied = srv.join("denied/repo.git");
    git_in(
        &sb.root,
        &[
            "clone",
            "-q",
            "--bare",
            "--no-local",
            sb.skill_repo.to_str().unwrap(),
            allowed.to_str().unwrap(),
        ],
    );
    // Unpack so objects are loose files the dumb walker requests one by one.
    for pack in std::fs::read_dir(allowed.join("objects/pack")).unwrap() {
        let pack = pack.unwrap().path();
        if pack.extension().is_some_and(|e| e == "pack") {
            let moved = sb.root.join("p.pack");
            std::fs::rename(&pack, &moved).unwrap();
            std::fs::remove_file(pack.with_extension("idx")).unwrap();
            // `--git-dir` keeps this working under `safe.bareRepository=explicit`.
            let out = Command::new("git")
                .arg("--git-dir")
                .arg(&allowed)
                .args(["unpack-objects", "-q"])
                .stdin(std::fs::File::open(&moved).unwrap())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(&allowed)
        .arg("update-server-info")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Every object lives only behind the alternate, on a denied path.
    std::fs::create_dir_all(&denied).unwrap();
    std::fs::rename(allowed.join("objects"), denied.join("objects")).unwrap();
    std::fs::create_dir_all(allowed.join("objects/info")).unwrap();
    std::fs::copy(
        denied.join("objects/info/packs"),
        allowed.join("objects/info/packs"),
    )
    .unwrap();

    let (port, seen) = static_http_server(srv);
    std::fs::write(
        allowed.join("objects/info/http-alternates"),
        format!("http://127.0.0.1:{port}/denied/repo.git/objects\n"),
    )
    .unwrap();
    let url = format!("http://127.0.0.1:{port}/allowed/repo.git");
    let run = |sb: &Sandbox, key: &str| {
        let mut cmd = local_http_command(sb, &["add", &url, "--branch", "main", "--name", "greet"]);
        with_user_git_config(&mut cmd, key, "true");
        cmd.output().unwrap()
    };
    let denied_hits = || {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with("/denied/"))
            .count()
    };

    // Sanity: with no policy the alternate is followed and the add succeeds.
    sb.ok(&["init", "--target", "claude"]);
    let out = run(&sb, "http.followRedirects");
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(denied_hits() > 0);

    // Under a policy allowing only `/allowed/*` the alternate is never used,
    // whether the user enables redirects generically or just for this URL.
    for key in [
        "http.followRedirects".to_string(),
        format!("http.http://127.0.0.1:{port}/allowed/.followRedirects"),
        format!("http.{url}.followRedirects"),
    ] {
        let sb = Sandbox::new();
        sb.ok(&["init", "--target", "claude"]);
        set_project_policy(&sb, &[&format!("127.0.0.1:{port}/allowed/*")]);
        seen.lock().unwrap().clear();
        let out = run(&sb, &key);
        assert!(!out.status.success(), "{key}");
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|p| p.starts_with("/allowed/")),
            "{key}: {}",
            stderr_of(&out)
        );
        assert_eq!(denied_hits(), 0, "{key}: {}", stderr_of(&out));
    }
}

/// A checkout filter selected by the repository's own `.gitattributes` (Git
/// LFS: endpoint from `.lfsconfig`, download URLs from its batch API) fetches
/// content spm can't check against the policy. Under a policy every configured
/// filter driver is disabled and required, so the checkout fails: the filter
/// never runs, and no unfiltered (pointer) file is materialized as a skill.
#[test]
fn policy_disables_checkout_filters() {
    // The driver is named `fakelfs` so the fixture commit never runs a real
    // `lfs` filter from the developer's own git config.
    let fixture = |sb: &Sandbox| {
        std::fs::create_dir_all(sb.skill_repo.join("assets")).unwrap();
        std::fs::write(sb.skill_repo.join("assets/logo.txt"), "pointer\n").unwrap();
        std::fs::write(
            sb.skill_repo.join(".gitattributes"),
            "assets/** filter=fakelfs\n",
        )
        .unwrap();
        sb.git(&["add", "-A"]);
        sb.git(&["commit", "-qm", "filtered asset"]);
    };
    // The fake driver records that it ran (where git-lfs would download).
    let run = |sb: &Sandbox, setting: &str| {
        let marker = sb.root.join("filter-ran");
        let marker_sh = marker.to_str().unwrap().replace('\\', "/");
        let mut cmd = sb.command(&[
            "add",
            &sb.skill_url(),
            "--branch",
            "main",
            "--name",
            "greet",
        ]);
        with_user_git_config(
            &mut cmd,
            &format!("filter.fakelfs.{setting}"),
            &format!("echo ran >> '{marker_sh}'; cat"),
        );
        (cmd.output().unwrap(), marker)
    };
    let installed = |sb: &Sandbox| {
        sb.project
            .join(".agents/skills/spm-managed-skills/greet")
            .exists()
    };

    // No policy: unchanged, the filter runs and the skill is installed.
    let sb = Sandbox::new();
    fixture(&sb);
    sb.ok(&["init", "--target", "copilot"]);
    let (out, marker) = run(&sb, "smudge");
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(marker.exists());
    assert!(installed(&sb));

    // Under a policy that allows the repository, neither a smudge nor a
    // long-running process filter runs, and nothing is materialized.
    for setting in ["smudge", "process"] {
        let sb = Sandbox::new();
        fixture(&sb);
        sb.ok(&["init", "--target", "copilot"]);
        set_project_policy(&sb, &["file://*"]);
        let (out, marker) = run(&sb, setting);
        assert!(!out.status.success(), "{setting}");
        let err = stderr_of(&out);
        assert!(
            err.contains("checkout filters such as Git LFS"),
            "{setting}: {err}"
        );
        assert!(!marker.exists(), "{setting}: the filter ran");
        assert!(!installed(&sb), "{setting}");
    }
}

/// The shared store must not launder a checkout made without a policy into a
/// policy-controlled install: its working tree may hold checkout-filter output
/// from an unchecked source. Under a policy only checkouts made under the
/// restricted rules are reused; any other is rebuilt (and a filtered one
/// fails, without the filter running).
#[test]
fn policy_rebuilds_cached_checkouts_made_without_a_policy() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.skill_repo.join(".gitattributes"),
        "*.md filter=fakelfs\n",
    )
    .unwrap();
    sb.git(&["add", "-A"]);
    sb.git(&["commit", "-qm", "filtered"]);
    sb.ok(&["init", "--target", "copilot"]);
    let marker = sb.root.join("filter-ran");
    let marker_sh = marker.to_str().unwrap().replace('\\', "/");
    let run = |args: &[&str]| {
        let mut cmd = sb.command(args);
        with_user_git_config(
            &mut cmd,
            "filter.fakelfs.smudge",
            &format!("echo ran >> '{marker_sh}'; cat"),
        );
        cmd.output().unwrap()
    };

    // Populate the store (and ai.lock) without a policy: the filter runs.
    let out = run(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(marker.exists());
    std::fs::remove_file(&marker).unwrap();

    // A machine policy that allows the source is now enforced.
    write_policy_file(&sb.spm_home.join("policy.json"), &["file://*"]);
    let out = run(&["install", "--locked"]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("checkout filters such as Git LFS"), "{err}");
    assert!(!marker.exists(), "the filter ran: {err}");
}

/// Under a policy an unverified cached checkout is rebuilt, never reused; one
/// made under the policy is reused (also without a policy) with no refetch.
#[test]
fn policy_reuses_only_cached_checkouts_made_under_a_policy() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    let away = sb.root.join("skill-away");
    let hide = || std::fs::rename(&sb.skill_repo, &away).unwrap();
    let restore = || std::fs::rename(&away, &sb.skill_repo).unwrap();
    write_policy_file(&sb.spm_home.join("policy.json"), &["file://*"]);

    // The no-policy checkout isn't reused: with the source gone, the rebuild fails.
    hide();
    let out = sb.spm(&["install", "--locked"]);
    assert!(!out.status.success(), "the unverified checkout was reused");
    restore();

    // Rebuilt under the policy...
    sb.ok(&["install", "--locked"]);
    // ...and from then on served from the store, with or without a policy.
    hide();
    sb.ok(&["install", "--locked"]);
    std::fs::remove_file(sb.spm_home.join("policy.json")).unwrap();
    sb.ok(&["install", "--locked"]);
    assert!(sb
        .project
        .join(".agents/skills/spm-managed-skills/greet/SKILL.md")
        .exists());
}

/// A filter driver whose name contains `=` can't be overridden with `-c` (the
/// `=` would end the key), so under a policy the checkout is refused outright.
#[test]
fn policy_refuses_checkout_when_a_filter_cant_be_disabled() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    set_project_policy(&sb, &["file://*"]);
    let mut cmd = sb.command(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    with_user_git_config(&mut cmd, "filter.a=b.smudge", "cat");
    let out = cmd.output().unwrap();
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("checkout filter `a=b` can't be disabled"),
        "{err}"
    );
    assert!(!sb
        .project
        .join(".agents/skills/spm-managed-skills/greet")
        .exists());
}
