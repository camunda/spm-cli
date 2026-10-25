//! `spm install --locked`.

use crate::common::*;

/// A matching lock installs the *locked* SHA even after the branch moved
/// upstream (proving no `ls-remote` re-resolution), and `ai.lock` is
/// byte-identical afterwards.
#[test]
fn install_locked_uses_pinned_sha_after_branch_moved_and_never_writes_lock() {
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
    let pinned = skill_head(&sb.skill_repo);
    let lock_before = sb.read("ai.lock");

    std::fs::write(
        sb.skill_repo.join("SKILL.md"),
        "---\nname: greet\ndescription: Moved.\n---\nUPSTREAM MOVED\n",
    )
    .unwrap();
    sb.git(&["commit", "-qam", "move main"]);
    assert_ne!(skill_head(&sb.skill_repo), pinned);
    std::fs::remove_dir_all(sb.project.join(".agents")).unwrap();

    let out = sb.ok(&["install", "--locked"]);
    assert!(out.contains("(locked)"), "{out}");
    assert_eq!(
        lock_before,
        sb.read("ai.lock"),
        "--locked must not write ai.lock"
    );
    let installed = sb.read(".agents/skills/spm-managed-skills/greet/SKILL.md");
    assert!(!installed.contains("UPSTREAM MOVED"), "{installed}");
}

/// Every kind of direct mismatch — a missing skill, a changed tag/commit/branch,
/// git and path, and a stale lock entry — is reported in a single error that
/// ends with the fix hint. Nothing is written. Plain `install` (no --locked)
/// then repairs the lock as it always has, after which `--locked` passes.
#[test]
fn install_locked_reports_every_direct_mismatch_at_once() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.add_plugin();
    let other = make_repo(
        &sb.root.join("other"),
        &[("pack/beta/SKILL.md", "---\nname: beta\n---\n")],
    );
    let url = sb.skill_url();
    let tagged = skill_head(&sb.skill_repo);
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],
               "skills":{{
                 "greet":{{"git":"{url}","tag":"v0.1.0"}},
                 "alpha":{{"git":"{url}","branch":"main","path":"pack/alpha"}},
                 "beta":{{"git":"{url}","branch":"main","path":"pack/beta"}},
                 "gamma":{{"git":"{url}","branch":"main"}}}},
               "plugins":{{"ds":{{"git":"{url}","branch":"main","path":"pkg"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    sb.ok(&["install", "--locked"]);
    let lock_before = sb.read("ai.lock");

    // greet: tag -> commit; alpha: path; beta: git; gamma: branch -> tag;
    // newbie: not in the lock; plugin `ds`: no longer declared (stale).
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],
               "skills":{{
                 "greet":{{"git":"{url}","commit":"{tagged}"}},
                 "alpha":{{"git":"{url}","branch":"main","path":"pack/beta"}},
                 "beta":{{"git":"{other}","branch":"main","path":"pack/beta"}},
                 "gamma":{{"git":"{url}","tag":"v0.1.0"}},
                 "newbie":{{"git":"{url}","tag":"v0.1.0"}}}}}}"#
        ),
    );
    let err = fails(&sb, &["install", "--locked"]);
    for needle in [
        "out of sync with ai.json (6 problem(s))",
        "skill `greet`: changed since ai.lock was written (`tag:v0.1.0` -> `commit:",
        "skill `alpha`: changed since ai.lock was written (path `pack/alpha` -> `pack/beta`)",
        "skill `beta`: changed since ai.lock was written (git `",
        "skill `gamma`: changed since ai.lock was written (`branch:main` -> `tag:v0.1.0`)",
        "skill `newbie`: missing from ai.lock",
        "plugin `ds`: in ai.lock but not declared in ai.json",
    ] {
        assert!(err.contains(needle), "missing `{needle}` in:\n{err}");
    }
    assert!(
        err.trim_end()
            .ends_with("run `spm install` (without --locked) and commit ai.lock"),
        "{err}"
    );
    assert_eq!(
        lock_before,
        sb.read("ai.lock"),
        "a failed --locked must not write"
    );

    // Without --locked, install behaves as before: it re-resolves and rewrites.
    sb.ok(&["install"]);
    assert_ne!(lock_before, sb.read("ai.lock"));
    sb.ok(&["install", "--locked"]);
}

/// `--locked` reports a changed and an unpinned plugin with the same wording
/// as skills, alongside skill mismatches, and writes nothing.
#[test]
fn install_locked_reports_plugin_mismatches() {
    let sb = Sandbox::new();
    sb.add_plugin();
    let url = sb.skill_url();
    let head = skill_head(&sb.skill_repo);
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],
               "skills":{{"greet":{{"git":"{url}","tag":"v0.1.0"}}}},
               "plugins":{{"ds":{{"git":"{url}","branch":"main","path":"pkg"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let lock_before = sb.read("ai.lock");

    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],
               "skills":{{"greet":{{"git":"{url}","branch":"main"}}}},
               "plugins":{{
                 "ds":{{"git":"{url}","commit":"{head}","path":"pkg"}},
                 "kit":{{"git":"{url}","branch":"main","path":"pkg"}}}}}}"#
        ),
    );
    let err = fails(&sb, &["install", "--locked"]);
    for needle in [
        "out of sync with ai.json (3 problem(s))",
        "skill `greet`: changed since ai.lock was written (`tag:v0.1.0` -> `branch:main`)",
        &format!(
            "plugin `ds`: changed since ai.lock was written (`branch:main` -> `commit:{head}`)"
        ),
        "plugin `kit`: missing from ai.lock",
    ] {
        assert!(err.contains(needle), "missing `{needle}` in:\n{err}");
    }
    let skill = err.find("skill `greet`").unwrap();
    let plugin = err.find("plugin `ds`").unwrap();
    assert!(skill < plugin, "skills are reported before plugins:\n{err}");
    assert_eq!(lock_before, sb.read("ai.lock"));
}

/// A transitive skill whose synthesized name is taken by a directly-declared
/// plugin is rejected: skills and plugins share one name namespace in
/// `ai.lock` too.
#[test]
fn transitive_skill_colliding_with_a_direct_plugin_is_rejected() {
    let sb = Sandbox::new();
    sb.add_plugin();
    let url = sb.skill_url();
    let leaf = make_repo(
        &sb.root.join("leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf skill.\n")],
    );
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop skill.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    let top_skill = format!(r#""top":{{"git":"{top}","branch":"main"}}"#);
    write_manifest(
        &sb,
        &format!(r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{{top_skill}}}}}"#),
    );
    sb.ok(&["install"]);
    let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    let synth = lock["skills"]
        .as_object()
        .unwrap()
        .keys()
        .find(|k| k.starts_with("top__leaf-"))
        .unwrap()
        .clone();

    let manifest = format!(
        r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{{top_skill}}},
           "plugins":{{"{synth}":{{"git":"{url}","branch":"main","path":"pkg"}}}}}}"#
    );
    write_manifest(&sb, &manifest);
    let err = fails(&sb, &["install"]);
    assert!(
        err.contains(&format!(
            "transitive skill name collision: `{synth}` clashes with a directly-declared \
             skill or plugin"
        )),
        "{err}"
    );
}

/// A stale plugin pin left in `ai.lock` is still judged by the source policy:
/// doctor names it as a locked plugin.
#[test]
fn doctor_checks_policy_for_stale_locked_plugins() {
    let sb = Sandbox::new();
    sb.add_plugin();
    let url = sb.skill_url();
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"plugins":{{"ds":{{"git":"{url}","branch":"main","path":"pkg"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    write_manifest(&sb, r#"{"targets":["copilot"]}"#);
    set_project_policy(&sb, &["example.test/*"]);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok, "{doc:#}");
    let c = check(&doc, "policy", "error");
    assert!(
        c["details"].to_string().contains("locked plugin `ds`"),
        "{c}"
    );
}

/// A direct skill entry left in `ai.lock` after it was dropped from `ai.json`
/// is stale and fails `--locked`.
#[test]
fn install_locked_rejects_stale_skill_entry() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    write_manifest(&sb, r#"{"targets":["copilot"],"skills":{}}"#);
    let err = fails(&sb, &["install", "--locked"]);
    assert!(
        err.contains("skill `greet`: in ai.lock but not declared in ai.json"),
        "{err}"
    );
}

/// `--locked` without any `ai.lock` is an error, not a fresh resolution.
#[test]
fn install_locked_requires_a_lockfile() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    let err = fails(&sb, &["install", "--locked"]);
    assert!(err.contains("ai.lock not found"), "{err}");
    assert!(
        err.contains("run `spm install` (without --locked)"),
        "{err}"
    );
    assert!(!sb.project.join("ai.lock").exists());
}

/// A hand-edited lock whose derived data (a plugin's bundled skills) no longer
/// matches what spm would write is caught by the whole-lock backstop.
#[test]
fn install_locked_rejects_lock_spm_would_rewrite() {
    let sb = Sandbox::new();
    sb.add_plugin();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--plugin",
        "--branch",
        "main",
        "--path",
        "pkg",
        "--name",
        "ds",
    ]);
    let mut lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    lock["plugins"]["ds"]["bundled_skills"] = serde_json::json!([]);
    std::fs::write(
        sb.project.join("ai.lock"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();
    let err = fails(&sb, &["install", "--locked"]);
    assert!(
        err.contains("ai.lock differs from what spm would write"),
        "{err}"
    );
}

/// With `resolveTransitive`, a transitive child missing from `ai.lock` fails
/// `--locked` (it is never resolved), and a transitive pin no longer reached
/// is stale.
#[test]
fn install_locked_checks_transitive_children() {
    let sb = Sandbox::new();
    let leaf = make_repo(
        &sb.root.join("leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf skill.\n")],
    );
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop skill.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    let manifest = |transitive: bool| {
        format!(
            r#"{{"targets":["copilot"],"resolveTransitive":{transitive},"skills":{{"top":{{"git":"{top}","branch":"main"}}}}}}"#
        )
    };
    write_manifest(&sb, &manifest(true));
    sb.ok(&["install"]);
    let good_lock = sb.read("ai.lock");
    sb.ok(&["install", "--locked"]);
    assert_eq!(good_lock, sb.read("ai.lock"));

    // Missing transitive child.
    let mut lock: serde_json::Value = serde_json::from_str(&good_lock).unwrap();
    let skills = lock["skills"].as_object_mut().unwrap();
    let leaf_name = skills
        .keys()
        .find(|k| k.starts_with("top__leaf-"))
        .unwrap()
        .clone();
    skills.remove(&leaf_name);
    let broken = serde_json::to_string_pretty(&lock).unwrap();
    std::fs::write(sb.project.join("ai.lock"), &broken).unwrap();
    let err = fails(&sb, &["install", "--locked"]);
    assert!(
        err.contains(&format!(
            "transitive skill `{leaf_name}` (`leaf` required by `top`): missing from ai.lock"
        )),
        "{err}"
    );
    assert_eq!(broken, sb.read("ai.lock"));

    // Stale transitive pin once the walk no longer reaches it.
    std::fs::write(sb.project.join("ai.lock"), &good_lock).unwrap();
    write_manifest(&sb, &manifest(false));
    let err = fails(&sb, &["install", "--locked"]);
    assert!(
        err.contains(&format!(
            "transitive skill `{leaf_name}`: in ai.lock but no longer required"
        )),
        "{err}"
    );
}

/// `--locked` works on the global scope, with a `-g`-aware fix hint.
#[cfg(unix)]
#[test]
fn install_locked_works_with_global_scope() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "copilot"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    let lock_path = sb.spm_home.join("ai.lock");
    let lock_before = std::fs::read_to_string(&lock_path).unwrap();
    std::fs::write(
        sb.skill_repo.join("SKILL.md"),
        "---\nname: greet\n---\nMOVED\n",
    )
    .unwrap();
    sb.git(&["commit", "-qam", "move main"]);

    sb.ok(&["install", "-g", "--locked"]);
    assert_eq!(lock_before, std::fs::read_to_string(&lock_path).unwrap());
    let installed =
        std::fs::read_to_string(sb.copilot_global_skills().join("greet/SKILL.md")).unwrap();
    assert!(!installed.contains("MOVED"), "{installed}");

    let manifest_path = sb.spm_home.join("ai.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["skills"]["newbie"] = serde_json::json!({"git": sb.skill_url(), "tag": "v0.1.0"});
    std::fs::write(&manifest_path, manifest.to_string()).unwrap();
    let err = fails(&sb, &["install", "-g", "--locked"]);
    assert!(
        err.contains("skill `newbie`: missing from ai.lock"),
        "{err}"
    );
    assert!(
        err.contains("run `spm install -g` (without --locked) and commit ai.lock"),
        "{err}"
    );
    assert_eq!(lock_before, std::fs::read_to_string(&lock_path).unwrap());
}

/// Two edges requesting one shared skill by *different* selectors that resolve
/// to the same commit dedup to one lock entry. Plain `install` records the
/// second selector as pin evidence, so `--locked` accepts the lock unchanged
/// (it used to report the deduplicated edge as missing).
#[test]
fn install_locked_accepts_mixed_selector_transitive_diamond() {
    let sb = Sandbox::new();
    let shared_dir = sb.root.join("shared");
    let shared = make_repo(
        &shared_dir,
        &[("SKILL.md", "---\nname: shared\n---\nShared.\n")],
    );
    git_in(&shared_dir, &["tag", "v1"]);
    let left = make_repo(
        &sb.root.join("left"),
        &[
            ("SKILL.md", "---\nname: left\n---\nLeft.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"shared":{{"git":"{shared}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    let right = make_repo(
        &sb.root.join("right"),
        &[
            ("SKILL.md", "---\nname: right\n---\nRight.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"shared":{{"git":"{shared}","tag":"v1"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"left":{{"git":"{left}","branch":"main"}},"right":{{"git":"{right}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let lock_before = sb.read("ai.lock");
    assert!(lock_before.contains("\"also_satisfies\""), "{lock_before}");
    sb.ok(&["install", "--locked"]);
    assert_eq!(lock_before, sb.read("ai.lock"));
    // Plain install is stable too: the evidence round-trips without churn.
    sb.ok(&["install"]);
    assert_eq!(lock_before, sb.read("ai.lock"));
}

/// A nested edge onto a *direct root* by a different selector (same commit) is
/// likewise recorded on the root's entry and accepted by `--locked`.
#[test]
fn install_locked_accepts_differing_selector_edge_onto_direct_root() {
    let sb = Sandbox::new();
    let shared_dir = sb.root.join("shared");
    let shared = make_repo(
        &shared_dir,
        &[("SKILL.md", "---\nname: shared\n---\nShared.\n")],
    );
    git_in(&shared_dir, &["tag", "v1"]);
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"shared":{{"git":"{shared}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"shared":{{"git":"{shared}","tag":"v1"}},"top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let lock_before = sb.read("ai.lock");
    let lock: serde_json::Value = serde_json::from_str(&lock_before).unwrap();
    assert_eq!(
        lock["skills"]["shared"]["also_satisfies"],
        serde_json::json!(["branch:main"]),
        "{lock_before}"
    );
    sb.ok(&["install", "--locked"]);
    assert_eq!(lock_before, sb.read("ai.lock"));
}

/// Stale transitive pins are reported alongside unrelated mismatches (a stale
/// plugin, a removed root) rather than being suppressed by them — only a
/// subtree that was actually skipped (a root that failed its own pin check)
/// holds back judgement on its children.
#[test]
fn install_locked_reports_stale_transitive_pins_with_other_mismatches() {
    let sb = Sandbox::new();
    sb.add_plugin();
    let url = sb.skill_url();
    let leaf = make_repo(
        &sb.root.join("leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")],
    );
    let top_dir = sb.root.join("top");
    let top = make_repo(
        &top_dir,
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    git_in(&top_dir, &["tag", "v1"]);
    let manifest = |transitive: bool, top_sel: Option<&str>, plugin: bool| {
        let top_entry = top_sel
            .map(|sel| format!(r#""top":{{"git":"{top}",{sel}}}"#))
            .unwrap_or_default();
        let plugins = if plugin {
            format!(r#","plugins":{{"ds":{{"git":"{url}","branch":"main","path":"pkg"}}}}"#)
        } else {
            String::new()
        };
        format!(
            r#"{{"targets":["copilot"],"resolveTransitive":{transitive},"skills":{{{top_entry}}}{plugins}}}"#
        )
    };
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(&sb, &manifest(true, Some(r#""branch":"main""#), true));
    sb.ok(&["install"]);
    let good_lock = sb.read("ai.lock");
    let lock: serde_json::Value = serde_json::from_str(&good_lock).unwrap();
    let leaf_name = lock["skills"]
        .as_object()
        .unwrap()
        .keys()
        .find(|k| k.starts_with("top__leaf-"))
        .unwrap()
        .clone();
    let stale_leaf = format!("transitive skill `{leaf_name}`: in ai.lock but no longer required");

    // Plugin removed + resolveTransitive disabled: both reported together.
    write_manifest(&sb, &manifest(false, Some(r#""branch":"main""#), false));
    let err = fails(&sb, &["install", "--locked"]);
    assert!(
        err.contains("plugin `ds`: in ai.lock but not declared"),
        "{err}"
    );
    assert!(err.contains(&stale_leaf), "{err}");

    // Direct root removed: the root and its now-unused child are both stale.
    write_manifest(&sb, &manifest(true, None, true));
    let err = fails(&sb, &["install", "--locked"]);
    assert!(
        err.contains("skill `top`: in ai.lock but not declared"),
        "{err}"
    );
    assert!(err.contains(&stale_leaf), "{err}");

    // Root's own pin changed: its subtree is skipped, so the child is not
    // misreported as stale.
    write_manifest(&sb, &manifest(true, Some(r#""tag":"v1""#), true));
    let err = fails(&sb, &["install", "--locked"]);
    assert!(err.contains("skill `top`: changed since ai.lock"), "{err}");
    assert!(!err.contains("no longer required"), "{err}");
    assert_eq!(good_lock, sb.read("ai.lock"));
}

/// `spm update <name>` re-resolving a direct root to the *same* commit keeps
/// its `also_satisfies` evidence: a nested edge onto that root by another
/// (since-moved) selector must not be re-resolved and reported as a conflict —
/// a named update does not cascade into the transitive frontier.
#[test]
fn named_update_keeps_selector_evidence_for_unchanged_root() {
    let sb = Sandbox::new();
    let shared_dir = sb.root.join("shared");
    let shared = make_repo(
        &shared_dir,
        &[("SKILL.md", "---\nname: shared\n---\nShared.\n")],
    );
    git_in(&shared_dir, &["tag", "v1"]);
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"shared":{{"git":"{shared}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"shared":{{"git":"{shared}","tag":"v1"}},"top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let lock_before = sb.read("ai.lock");

    std::fs::write(
        shared_dir.join("SKILL.md"),
        "---\nname: shared\n---\nMoved.\n",
    )
    .unwrap();
    git_in(&shared_dir, &["commit", "-qam", "move main"]);

    sb.ok(&["update", "shared"]);
    assert_eq!(lock_before, sb.read("ai.lock"));
    sb.ok(&["install", "--locked"]);
}
