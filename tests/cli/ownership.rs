//! Ownership records: what each target installed, and partial-failure recovery.

use crate::common::*;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

/// Install codex with skill `base`, then `add --lock-only` a skill `fresh`
/// and make the next materialization fail part-way: the shared-dir vendor
/// copies the skills before it updates the (now unreadable) `.gitignore`.
fn partially_failed_install(sb: &Sandbox) -> (PathBuf, String) {
    let base = make_repo(
        &sb.root.join("base"),
        &[("SKILL.md", "---\nname: base\n---\nBase.\n")],
    );
    let fresh = make_repo(
        &sb.root.join("fresh"),
        &[("SKILL.md", "---\nname: fresh\n---\nFresh.\n")],
    );
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &base, "--branch", "main", "--name", "base"]);
    sb.ok(&[
        "add",
        &fresh,
        "--branch",
        "main",
        "--name",
        "fresh",
        "--lock-only",
    ]);
    let dir = sb.project.join(".agents/skills");
    std::fs::create_dir_all(dir.join("mine")).unwrap();
    std::fs::write(dir.join("mine/SKILL.md"), "Mine.\n").unwrap();
    corrupt_gitignore(sb);
    (dir, fresh)
}

/// What a failed install already wrote is recorded, so `clean` right after it
/// (no successful install in between) still removes it.
#[test]
fn clean_after_a_partially_failed_install_removes_what_it_wrote() {
    let sb = Sandbox::new();
    let (dir, _) = partially_failed_install(&sb);
    fails(&sb, &["install"]);
    assert!(dir.join("fresh/SKILL.md").exists(), "copied before failing");
    sb.ok(&["clean"]);
    for gone in ["base", "fresh"] {
        assert!(!dir.join(gone).exists(), "{gone} cleaned");
    }
    assert_eq!(sb.read(".agents/skills/mine/SKILL.md"), "Mine.\n");
}

/// Same for a target whose addition failed part-way: `target remove` right
/// after it removes what that addition wrote. Codex installs fine; Cursor
/// copies `base`, then fails on `fresh` (a file of the user's is in the way).
#[test]
fn target_remove_after_a_partially_failed_target_add_removes_what_it_wrote() {
    let sb = Sandbox::new();
    let (dir, _) = partially_failed_install(&sb);
    std::fs::remove_file(sb.project.join(".gitignore")).unwrap();
    sb.ok(&["install"]);
    let cursor = sb.project.join(".cursor/skills");
    std::fs::create_dir_all(cursor.join("mine")).unwrap();
    std::fs::write(cursor.join("mine/SKILL.md"), "Mine.\n").unwrap();
    std::fs::write(cursor.join("fresh"), "in the way\n").unwrap();

    let err = fails(&sb, &["target", "add", "cursor"]);
    assert!(err.contains("clearing") && err.contains("fresh"), "{err}");
    assert!(
        cursor.join("base/SKILL.md").exists(),
        "copied before failing"
    );
    sb.ok(&["target", "remove", "cursor"]);
    assert!(!cursor.join("base").exists(), "removed from cursor");
    assert_eq!(sb.read(".cursor/skills/mine/SKILL.md"), "Mine.\n");
    for kept in ["base", "fresh", "mine"] {
        assert!(
            dir.join(kept).join("SKILL.md").exists(),
            "codex keeps {kept}"
        );
    }
}

/// Targets sharing a physical skills dir (`codex` and `amp` both load
/// `.agents/skills/`) share what one of them wrote there: when an install fails
/// part-way — `codex` wrote `fresh`, `cursor` failed, `amp` was never reached —
/// removing `codex` keeps `fresh` for `amp` and hands its ownership over, so a
/// later `clean` still removes it.
#[test]
fn target_remove_after_a_partial_install_keeps_output_shared_with_a_remaining_target() {
    let sb = Sandbox::new();
    let base = make_repo(
        &sb.root.join("base"),
        &[("SKILL.md", "---\nname: base\n---\nBase.\n")],
    );
    let fresh = make_repo(
        &sb.root.join("fresh"),
        &[("SKILL.md", "---\nname: fresh\n---\nFresh.\n")],
    );
    sb.ok(&["init", "--target", "codex,cursor,amp"]);
    sb.ok(&["add", &base, "--branch", "main", "--name", "base"]);
    sb.ok(&[
        "add",
        &fresh,
        "--branch",
        "main",
        "--name",
        "fresh",
        "--lock-only",
    ]);
    let cursor = sb.project.join(".cursor/skills");
    std::fs::write(cursor.join("fresh"), "in the way\n").unwrap();
    let err = fails(&sb, &["install"]);
    assert!(err.contains("clearing") && err.contains("fresh"), "{err}");
    let shared = sb.project.join(".agents/skills");
    assert!(shared.join("fresh/SKILL.md").exists(), "codex wrote fresh");

    let out = sb.ok(&["target", "remove", "codex"]);
    assert!(out.contains("still used by target `amp`"), "{out}");
    for kept in ["base", "fresh"] {
        assert!(
            shared.join(kept).join("SKILL.md").exists(),
            "amp keeps {kept}"
        );
    }

    std::fs::remove_file(cursor.join("fresh")).unwrap();
    sb.ok(&["clean"]);
    for gone in ["base", "fresh"] {
        assert!(!shared.join(gone).exists(), "{gone} cleaned");
    }
}

/// Shared output nested inside a removed vendor's container: `.cursor/skills`
/// aliases Claude's marketplace, and a `target add codex cursor` that fails at
/// `codex` never reaches `cursor`, so nothing is recorded for it. Removing
/// Claude must still keep the marketplace `cursor` loads, and hand `cursor`
/// ownership of what it loads there so a later `clean` removes it.
#[test]
fn target_remove_after_a_partial_target_add_keeps_output_nested_in_its_dir() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let greet = sb.claude_market_dir().join("plugin/skills/greet/SKILL.md");
    assert!(greet.exists());
    std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
    symlink(
        "../.spm/claude/plugin/skills",
        sb.project.join(".cursor/skills"),
    );
    let blocker = sb.project.join(".agents/skills/greet");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, "in the way\n").unwrap();
    let err = fails(&sb, &["target", "add", "codex", "cursor"]);
    assert!(err.contains("clearing") && err.contains("greet"), "{err}");

    let out = sb.ok(&["target", "remove", "claude"]);
    assert!(out.contains("still used by target `cursor`"), "{out}");
    assert!(greet.exists(), "cursor still loads greet through the alias");

    std::fs::remove_file(&blocker).unwrap();
    sb.ok(&["clean"]);
    assert!(!greet.exists(), "cursor's greet cleaned");
}

/// An ownership record that can't be inspected stops cleanup before anything is
/// deleted, rather than falling back to the lock — which may name a user's own,
/// never-installed skill.
#[cfg(unix)]
#[test]
fn an_unreadable_ownership_record_stops_clean_before_deleting() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let mine = make_repo(
        &sb.root.join("mine"),
        &[("SKILL.md", "---\nname: mine\n---\nTheirs.\n")],
    );
    sb.ok(&["init", "--target", "cursor"]);
    let user = sb.project.join(".cursor/skills/mine");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("SKILL.md"), "Mine.\n").unwrap();
    sb.ok(&[
        "add",
        &mine,
        "--branch",
        "main",
        "--name",
        "mine",
        "--lock-only",
    ]);

    let spm = sb.project_owned().parent().unwrap().to_path_buf();
    assert!(sb.project_owned().exists(), "the record exists");
    std::fs::set_permissions(&spm, std::fs::Permissions::from_mode(0o000)).unwrap();
    let out = sb.spm(&["clean"]);
    std::fs::set_permissions(&spm, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success(), "clean must fail");
    assert_eq!(sb.read(".cursor/skills/mine/SKILL.md"), "Mine.\n");
}

/// Handover follows owned payloads through chained aliases: after a partial
/// `target add codex cursor` (`cursor` aliases Claude's skills dir), Claude's
/// `plugin/skills` is itself moved behind a symlink. Removing Claude keeps the
/// linked payload and hands it to `cursor`, so a later `clean` removes it.
#[test]
fn target_remove_hands_over_payload_reached_through_chained_aliases() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
    symlink(
        "../.spm/claude/plugin/skills",
        sb.project.join(".cursor/skills"),
    );
    let blocker = sb.project.join(".agents/skills/greet");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, "in the way\n").unwrap();
    fails(&sb, &["target", "add", "codex", "cursor"]);

    let skills = sb.claude_market_dir().join("plugin/skills");
    let moved = sb.project.join(".spm/shared-skills");
    std::fs::rename(&skills, &moved).unwrap();
    symlink("../../shared-skills", &skills);
    let greet = moved.join("greet/SKILL.md");

    let out = sb.ok(&["target", "remove", "claude"]);
    assert!(out.contains("still used by target `cursor`"), "{out}");
    assert!(
        greet.exists(),
        "cursor still loads greet through the aliases"
    );

    std::fs::remove_file(&blocker).unwrap();
    sb.ok(&["clean"]);
    assert!(!greet.exists(), "cursor's greet cleaned");
}

/// Claude claims each skill as it copies it into its marketplace too: when a
/// Claude install fails after copying (here: unparsable settings), a target
/// aliasing that marketplace inherits what was copied once Claude is removed,
/// so a later `clean` removes it.
#[test]
fn a_partial_claude_install_records_the_skills_it_copied() {
    let sb = Sandbox::new();
    let fresh = make_repo(
        &sb.root.join("fresh"),
        &[("SKILL.md", "---\nname: fresh\n---\nFresh.\n")],
    );
    std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
    symlink(
        "../.spm/claude/plugin/skills",
        sb.project.join(".cursor/skills"),
    );
    sb.ok(&["init", "--target", "claude,cursor"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    sb.ok(&[
        "add",
        &fresh,
        "--branch",
        "main",
        "--name",
        "fresh",
        "--lock-only",
    ]);
    let settings = sb.project.join(".claude/settings.local.json");
    let good = std::fs::read_to_string(&settings).unwrap();
    std::fs::write(&settings, "{").unwrap();
    fails(&sb, &["install"]);
    let copied = sb.claude_market_dir().join("plugin/skills/fresh/SKILL.md");
    assert!(copied.exists(), "copied before failing");

    std::fs::write(&settings, good).unwrap();
    let out = sb.ok(&["target", "remove", "claude"]);
    assert!(out.contains("still used by target `cursor`"), "{out}");
    sb.ok(&["clean"]);
    assert!(!copied.exists(), "fresh cleaned through cursor");
}

/// Ownership is claimed name by name as the vendor reaches each entry: when
/// removing an earlier entry fails (`a`, which can't be deleted), a later
/// requested name the vendor never reached — the user's own `mine` — is not
/// claimed, so a later `clean` leaves it alone.
#[cfg(unix)]
#[test]
fn a_failure_before_the_vendor_reaches_a_user_skill_does_not_claim_it() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let repo = |name: &str| {
        make_repo(
            &sb.root.join(name),
            &[("SKILL.md", &format!("---\nname: {name}\n---\n{name}.\n"))],
        )
    };
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &repo("base"), "--branch", "main", "--name", "base"]);
    let dir = sb.project.join(".agents/skills");
    std::fs::create_dir_all(dir.join("mine")).unwrap();
    std::fs::write(dir.join("mine/SKILL.md"), "Mine.\n").unwrap();
    let locked = dir.join("a/locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("f"), "x\n").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    for name in ["a", "mine"] {
        sb.ok(&[
            "add",
            &repo(name),
            "--branch",
            "main",
            "--name",
            name,
            "--lock-only",
        ]);
    }

    let err = fails(&sb, &["install"]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(err.contains("removing"), "{err}");
    sb.ok(&["clean"]);
    assert_eq!(sb.read(".agents/skills/mine/SKILL.md"), "Mine.\n");
    assert!(!dir.join("base").exists(), "installed `base` cleaned");
}

/// Point `ai.json`'s `targets` at `targets` without syncing (e.g. checking out
/// a branch that declares other targets).
fn set_targets(sb: &Sandbox, targets: &[&str]) {
    let mut doc: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    doc["targets"] = serde_json::json!(targets);
    write_manifest(sb, &serde_json::to_string_pretty(&doc).unwrap());
}

/// An install for a branch's targets keeps the record of a target that branch
/// omits, so returning to it still removes what spm installed there.
#[test]
fn install_keeps_ownership_of_a_target_the_manifest_omits() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let greet = sb.project.join(".agents/skills/greet");
    assert!(greet.join("SKILL.md").exists());
    set_targets(&sb, &["cursor"]);
    sb.ok(&["install"]);
    set_targets(&sb, &["codex"]);
    sb.ok(&["remove", "greet"]);
    assert!(!greet.exists(), "codex's greet removed");
}

/// `clean` forgets only the targets it cleaned: one the manifest omits keeps
/// its record, so cleaning it later still removes its output.
#[test]
fn clean_keeps_ownership_of_a_target_the_manifest_omits() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let greet = sb.project.join(".agents/skills/greet");
    assert!(greet.join("SKILL.md").exists());
    set_targets(&sb, &["cursor"]);
    sb.ok(&["clean"]);
    set_targets(&sb, &["codex"]);
    sb.ok(&["clean"]);
    assert!(!greet.exists(), "codex's greet cleaned");
}

/// `target remove` hands a remaining target only output the removed one
/// installed (or loads through a symlink), never a location a symlink nested
/// inside that output points at: there, the user's own skill stays theirs.
#[test]
fn target_remove_does_not_hand_over_through_a_nested_link() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let user = sb.project.join(".cursor/skills/greet");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("SKILL.md"), "Mine.\n").unwrap();
    symlink(
        "../../../.cursor/skills",
        sb.project.join(".agents/skills/greet/link"),
    );
    set_targets(&sb, &["codex", "cursor"]);
    sb.ok(&["target", "remove", "codex"]);
    sb.ok(&["clean"]);
    assert_eq!(sb.read(".cursor/skills/greet/SKILL.md"), "Mine.\n");
}

fn codex_with_plugin(sb: &Sandbox) {
    sb.add_plugin();
    sb.ok(&["init", "--target", "codex"]);
    write_manifest(
        sb,
        &format!(
            r#"{{"targets":["codex"],"plugins":{{"ds":{{"git":"{}","branch":"main","path":"pkg"}}}}}}"#,
            sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
}

/// A skill one target got from a linked plugin is that target's: adding
/// another target after the working copy dropped it leaves the new target's
/// same-named skill (the user's own) alone.
#[test]
fn target_add_keeps_a_users_skill_another_target_got_from_a_linked_plugin() {
    let sb = Sandbox::new();
    codex_with_plugin(&sb);
    let dev = dev_plugin(&sb);
    sb.ok(&["link", "ds", "../dev-plugin"]);
    let shared = sb.project.join(".agents/skills");
    assert!(shared.join("extra/SKILL.md").exists());
    std::fs::remove_dir_all(dev.join("skills/extra")).unwrap();
    let user = sb.project.join(".cursor/skills/extra");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("SKILL.md"), "Mine.\n").unwrap();
    sb.ok(&["target", "add", "cursor"]);
    assert_eq!(sb.read(".cursor/skills/extra/SKILL.md"), "Mine.\n");
    assert!(!shared.join("extra").exists(), "codex's extra purged");
}

/// Ownership follows what was materialized, not `ai.lock`: a locked bundled
/// skill a linked working copy dropped is never written to a new target, so a
/// same-named skill there stays the user's.
#[test]
fn a_linked_plugins_reduced_bundle_is_not_claimed_on_a_new_target() {
    let sb = Sandbox::new();
    codex_with_plugin(&sb);
    let dev = dev_plugin(&sb);
    std::fs::remove_dir_all(dev.join("skills/composer")).unwrap();
    sb.ok(&["link", "ds", "../dev-plugin"]);
    let user = sb.project.join(".cursor/skills/composer");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("SKILL.md"), "Mine.\n").unwrap();
    sb.ok(&["target", "add", "cursor"]);
    sb.ok(&["clean"]);
    assert_eq!(sb.read(".cursor/skills/composer/SKILL.md"), "Mine.\n");
    assert!(!sb.project.join(".cursor/skills/extra").exists());
}

/// Plugin names are checked against every locked plugin, not just the ones a
/// lock-only add fetched: `ds` next to a locked `Ds` is rejected before
/// anything is written, as without `--lock-only`.
#[test]
fn add_lock_only_rejects_a_plugin_differing_only_by_case_from_a_locked_one() {
    let sb = Sandbox::new();
    sb.add_plugin();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pkg",
        "--plugin",
        "--name",
        "Ds",
    ]);
    let other = make_repo(
        &sb.root.join("other"),
        &[
            (
                "pkg/.claude-plugin/plugin.json",
                r#"{"name":"other","version":"1.0.0","skills":"./skills/"}"#,
            ),
            ("pkg/skills/tool/SKILL.md", "---\nname: tool\n---\nTool.\n"),
        ],
    );
    let (manifest, lock) = (sb.read("ai.json"), sb.read("ai.lock"));
    for lock_only in [false, true] {
        let mut args = vec![
            "add", &other, "--branch", "main", "--path", "pkg", "--plugin", "--name", "ds",
        ];
        if lock_only {
            args.push("--lock-only");
        }
        let out = sb.spm(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "lock_only={lock_only}: must fail");
        assert!(err.contains("differ only by case"), "{err}");
        assert_eq!(sb.read("ai.json"), manifest);
        assert_eq!(sb.read("ai.lock"), lock);
    }
}

/// Targets sharing one skills dir (codex and amp both use `.agents/skills/`)
/// share what spm installed there: switching a branch from one to the other
/// still lets `remove` take out what the first installed — also after an
/// install recorded it for the second and the branch switched back.
#[test]
fn a_target_sharing_a_skills_dir_removes_what_another_installed() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let greet = sb.project.join(".agents/skills/greet");
    assert!(greet.join("SKILL.md").exists());
    set_targets(&sb, &["amp"]);
    sb.ok(&["remove", "greet"]);
    assert!(!greet.exists(), "amp removed codex's greet");

    set_targets(&sb, &["codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    set_targets(&sb, &["amp"]);
    sb.ok(&["install"]);
    set_targets(&sb, &["codex"]);
    sb.ok(&["remove", "greet"]);
    assert!(!greet.exists(), "codex removed the greet amp took over");
}

/// `clean` of a target removes what another target installed into the skills
/// dir they share.
#[test]
fn clean_removes_what_a_target_sharing_the_skills_dir_installed() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let greet = sb.project.join(".agents/skills/greet");
    assert!(greet.join("SKILL.md").exists());
    set_targets(&sb, &["amp"]);
    sb.ok(&["clean"]);
    assert!(!greet.exists(), "amp's clean removed codex's greet");
}

/// What spm recorded installing is bound to the directory it wrote: once a
/// target's skills dir is replaced (here by a symlink to the user's own skills),
/// the names recorded for the old dir claim nothing in the new one.
#[test]
fn ownership_does_not_follow_a_retargeted_skills_dir() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
    let agents = sb.project.join(".agents");
    assert!(agents.join("skills/old/SKILL.md").exists());
    std::fs::rename(agents.join("skills"), agents.join("skills-moved")).unwrap();
    let mine = sb.root.join("personal/old");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(
        mine.join("SKILL.md"),
        "---\nname: old\ndescription: Mine.\n---\n",
    )
    .unwrap();
    symlink(sb.root.join("personal"), agents.join("skills"));
    sb.ok(&["remove", "old"]);
    assert!(
        mine.join("SKILL.md").exists(),
        "remove deleted the user's skill"
    );
    sb.ok(&["clean"]);
    assert!(
        mine.join("SKILL.md").exists(),
        "clean deleted the user's skill"
    );
}

/// The working-copy preflight checks each target's own output: a name only
/// another target recorded doesn't make this target's same-named entry (here a
/// linked working copy) count as output.
#[test]
fn preflight_checks_each_targets_own_recorded_names() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
    set_targets(&sb, &["cursor"]);
    sb.ok(&["remove", "old"]);
    set_targets(&sb, &["codex", "cursor"]);
    let wc = sb.project.join(".cursor/skills/old");
    std::fs::create_dir_all(&wc).unwrap();
    std::fs::write(
        wc.join("SKILL.md"),
        "---\nname: old\ndescription: Say hello nicely.\n---\nWIP\n",
    )
    .unwrap();
    let other = sb.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    for args in [
        &["init", "--target", "claude"][..],
        &["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"],
        &["link", "old", wc.to_str().unwrap()],
    ] {
        let out = sb.command(args).current_dir(&other).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", stderr_of(&out));
    }
    sb.ok(&["install"]);
    assert!(read_path(&wc.join("SKILL.md")).contains("WIP"), "wiped");
    assert!(
        !sb.project.join(".agents/skills/old").exists(),
        "codex's old removed"
    );
}

/// A first install from an existing lock (no ownership record yet: a fresh
/// clone) binds what it copies to the dir it copied into as it goes, so when
/// it fails part-way (here: `.gitignore` is a directory) a later `clean`
/// still removes what was copied.
#[test]
fn a_failed_first_install_from_a_lock_records_where_it_copied() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let greet = sb.project.join(".agents/skills/greet");
    std::fs::remove_file(sb.project_owned()).unwrap();
    std::fs::remove_dir_all(sb.project.join(".agents")).unwrap();
    let gitignore = sb.project.join(".gitignore");
    let ignored = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let _ = std::fs::remove_file(&gitignore);
    std::fs::create_dir(&gitignore).unwrap();
    fails(&sb, &["install"]);
    assert!(greet.join("SKILL.md").exists(), "copied before failing");
    std::fs::remove_dir(&gitignore).unwrap();
    std::fs::write(&gitignore, ignored).unwrap();
    sb.ok(&["clean"]);
    assert!(!greet.exists(), "greet cleaned");
}

/// A Claude reinstall recreates the marketplace, so even when it copies only
/// names it already owned, it rebinds them to the new dir as it claims them:
/// failing part-way (unparsable settings) keeps them owned, so a target
/// aliasing the marketplace inherits them and a later `clean` removes them.
#[test]
fn a_failed_claude_reinstall_rebinds_the_names_it_copied() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
    symlink(
        "../.spm/claude/plugin/skills",
        sb.project.join(".cursor/skills"),
    );
    sb.ok(&["init", "--target", "claude,cursor"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let settings = sb.project.join(".claude/settings.local.json");
    let good = std::fs::read_to_string(&settings).unwrap();
    std::fs::write(&settings, "{").unwrap();
    fails(&sb, &["install"]);
    let copied = sb.claude_market_dir().join("plugin/skills/greet/SKILL.md");
    assert!(copied.exists(), "copied before failing");

    std::fs::write(&settings, good).unwrap();
    let out = sb.ok(&["target", "remove", "claude"]);
    assert!(out.contains("still used by target `cursor`"), "{out}");
    assert!(copied.exists(), "cursor still loads greet");
    sb.ok(&["clean"]);
    assert!(!copied.exists(), "greet cleaned through cursor");
}

/// A lock-only update writes only `ai.json` and `ai.lock`: a `.gitignore` or
/// links file it leaves untouched may lead into a linked working copy.
#[test]
fn lock_only_ignores_untouched_files_leading_into_a_working_copy() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    let wc = sb.root.join("wc");
    std::fs::create_dir_all(&wc).unwrap();
    std::fs::write(
        wc.join("SKILL.md"),
        "---\nname: greet\ndescription: Say hello nicely.\n---\nWIP\n",
    )
    .unwrap();
    sb.ok(&["link", "greet", wc.to_str().unwrap()]);
    let gitignore = sb.project.join(".gitignore");
    std::fs::rename(&gitignore, wc.join("ignored")).unwrap();
    symlink(wc.join("ignored"), &gitignore);
    sb.ok(&["update", "--lock-only"]);
    assert!(read_path(&wc.join("SKILL.md")).contains("WIP"), "wiped");
}

/// The binding is to the directory itself, not its path: a new directory
/// created where the old one was (the old moved aside) is the user's.
#[test]
fn ownership_does_not_carry_over_to_a_replacement_skills_dir() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
    let skills = sb.project.join(".agents/skills");
    assert!(skills.join("old/SKILL.md").exists());
    std::fs::rename(&skills, sb.project.join(".agents/skills-moved")).unwrap();
    let mine = skills.join("old");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(
        mine.join("SKILL.md"),
        "---\nname: old\ndescription: Mine.\n---\n",
    )
    .unwrap();
    sb.ok(&["clean"]);
    assert!(
        mine.join("SKILL.md").exists(),
        "clean deleted the user's skill"
    );
}

/// Nor does it carry over to a directory created after deleting the original,
/// even one the filesystem hands the old one's inode (ext4 does, readily): on
/// such filesystems the new dir is recreated until it gets that inode, setting
/// the others aside so the allocator moves on.
#[test]
fn ownership_does_not_carry_over_to_a_recreated_skills_dir() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
    let skills = sb.project.join(".agents/skills");
    assert!(skills.join("old/SKILL.md").exists());
    #[cfg(unix)]
    let ino = |p: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap());
    #[cfg(unix)]
    let original = ino(&skills);
    std::fs::remove_dir_all(&skills).unwrap();
    std::fs::create_dir(&skills).unwrap();
    #[cfg(unix)]
    for held in 0..64 {
        if ino(&skills) == original {
            break;
        }
        std::fs::rename(&skills, sb.root.join(format!("held-{held}"))).unwrap();
        std::fs::create_dir(&skills).unwrap();
    }
    let mine = skills.join("old");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(
        mine.join("SKILL.md"),
        "---\nname: old\ndescription: Mine.\n---\n",
    )
    .unwrap();
    sb.ok(&["clean"]);
    assert!(
        mine.join("SKILL.md").exists(),
        "clean deleted the user's skill"
    );
}

/// `target remove` hands over only what the removed target's own entry is:
/// an entry that is a symlink to the user's skill in another target's dir
/// (cleanup removes just the symlink) does not make that skill spm's.
#[test]
fn target_remove_does_not_hand_over_through_an_entry_symlink() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let entry = sb.project.join(".agents/skills/greet");
    std::fs::remove_dir_all(&entry).unwrap();
    let mine = sb.project.join(".cursor/skills/greet");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(
        mine.join("SKILL.md"),
        "---\nname: greet\ndescription: Mine.\n---\n",
    )
    .unwrap();
    symlink("../../.cursor/skills/greet", &entry);
    set_targets(&sb, &["codex", "cursor"]);
    sb.ok(&["target", "remove", "codex"]);
    sb.ok(&["clean"]);
    assert!(
        mine.join("SKILL.md").exists(),
        "clean deleted the user's skill"
    );
}

/// The ownership record is the only proof of which skills spm installed: with
/// `$SPM_HOME/owned` redirected into this project's output, a command that
/// would rebuild or delete that output refuses rather than wipe the record
/// (after which cleanup would trust a newer lock and delete skills it never
/// installed).
#[test]
fn ownership_records_are_guarded_like_links_files() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot,codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let record = sb.project_owned();
    assert!(record.exists(), "install recorded what it wrote");
    let owned_dir = record.parent().unwrap().to_path_buf();
    let state = sb.project.join(".agents/skills/spm-managed-skills/owned");
    std::fs::rename(&owned_dir, &state).unwrap();
    symlink(&state, &owned_dir);
    let before = read_path(&record);

    for args in [
        &["install"][..],
        &["update"],
        &["clean"],
        &["target", "remove", "copilot"],
    ] {
        let err = fails(&sb, args);
        assert!(err.contains("the ownership record"), "{args:?}: {err}");
        assert_eq!(read_path(&record), before, "{args:?}");
    }
}

/// Saving the ownership record replaces the file at its path: with
/// `$SPM_HOME/owned` moved into a linked working copy (and symlinked back),
/// every command that would save it refuses before changing anything — an
/// install, `clean`, `target remove`, and a lock-only sync freezing a legacy
/// assumption into a record.
#[test]
fn ownership_records_are_never_written_into_a_working_copy() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot,codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let dev = local_skill_checkout(&sb, "dev", "LOCAL");
    sb.ok(&["link", "greet", "../dev"]);
    let record = sb.project_owned();
    let owned_dir = record.parent().unwrap().to_path_buf();
    let inside = dev.join("owned");
    std::fs::rename(&owned_dir, &inside).unwrap();
    symlink(&inside, &owned_dir);
    let before = read_path(&record);
    let lock = read_path(&sb.project.join("ai.lock"));

    for args in [
        &["install"][..],
        &["clean"],
        &["target", "remove", "copilot"],
    ] {
        let err = fails(&sb, args);
        assert!(err.contains("linked directory `../dev`"), "{args:?}: {err}");
        assert_eq!(read_path(&record), before, "{args:?}");
    }
    // No record yet (a legacy install): a lock-only sync would write one.
    std::fs::remove_file(&record).unwrap();
    let err = fails(&sb, &["update", "--lock-only"]);
    assert!(err.contains("linked directory `../dev`"), "{err}");
    assert!(!record.exists(), "no record written");
    assert_eq!(read_path(&sb.project.join("ai.lock")), lock);
}

/// Before the first install there is no ownership record (nor links file) to
/// find, yet the install creates one: with `$SPM_HOME/owned` pointing into a
/// target's output, it refuses rather than write the record where rebuilding
/// that output deletes it.
#[test]
fn a_first_install_never_records_ownership_inside_target_output() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    let state = sb.project.join(".agents/skills/spm-managed-skills/owned");
    std::fs::create_dir_all(&state).unwrap();
    let owned_dir = sb.project_owned().parent().unwrap().to_path_buf();
    std::fs::create_dir_all(owned_dir.parent().unwrap()).unwrap();
    symlink(&state, &owned_dir);
    let err = fails(
        &sb,
        &["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"],
    );
    assert!(err.contains("the ownership record"), "{err}");
    assert!(!sb.project_owned().exists(), "no record written");
}

/// spm's own state writes must not land on one another: with
/// `$SPM_HOME/owned` aliasing `$SPM_HOME/links` before either file exists,
/// saving the ownership record would write it as the links file. Every
/// command that would save it refuses, writing neither.
#[test]
fn ownership_records_never_overwrite_an_aliased_links_file() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex,cursor"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    // Both name the project's file alike, each in its own dir.
    let record = sb.project_owned();
    let owned_dir = record.parent().unwrap();
    let links_dir = sb.spm_home.join("links");
    let links_file = links_dir.join(record.file_name().unwrap());
    let links_dir = links_dir.as_path();
    std::fs::remove_dir_all(owned_dir).unwrap();
    std::fs::create_dir_all(links_dir).unwrap();
    symlink(links_dir, owned_dir);

    for args in [
        &["update", "--lock-only"][..],
        &["install"],
        &["clean"],
        &["target", "remove", "codex"],
    ] {
        let err = fails(&sb, args);
        assert!(err.contains("the links file"), "{args:?}: {err}");
        assert!(!links_file.exists(), "{args:?} wrote state");
    }
}

/// Targets sharing a skills dir (codex and amp: `.agents/skills/`) hold one
/// physical set: once one of them removes a name from it, the other no longer
/// owns that name there either — even when the install fails before reaching
/// it. Otherwise a later `clean` would delete the user's own skill created
/// under that name since.
#[test]
fn a_shared_dir_rename_failing_part_way_leaves_no_stale_alias_claim() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex,cursor,amp"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
    // Rename it in ai.json; cursor then fails on a file already at `fresh`.
    let mut doc: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    let spec = doc["skills"]["old"].take();
    doc["skills"] = serde_json::json!({ "fresh": spec });
    write_manifest(&sb, &serde_json::to_string_pretty(&doc).unwrap());
    std::fs::write(sb.project.join(".cursor/skills/fresh"), "mine").unwrap();
    fails(&sb, &["install"]);
    let old = sb.project.join(".agents/skills/old");
    assert!(!old.exists(), "codex removed `old` from the shared dir");

    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(
        old.join("SKILL.md"),
        "---\nname: old\ndescription: Mine.\n---\n",
    )
    .unwrap();
    std::fs::remove_file(sb.project.join(".cursor/skills/fresh")).unwrap();
    sb.ok(&["clean"]);
    assert!(
        old.join("SKILL.md").exists(),
        "clean deleted the user's skill"
    );
}

/// Create the user's own skill at `.agents/skills/<name>`, run `spm clean`,
/// and assert it survived.
fn assert_user_skill_survives_clean(sb: &Sandbox, name: &str) {
    let dir = sb.project.join(".agents/skills").join(name);
    assert!(!dir.exists(), "spm removed `{name}`");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Mine.\n---\n"),
    )
    .unwrap();
    sb.ok(&["clean"]);
    assert!(
        dir.join("SKILL.md").exists(),
        "clean deleted the user's `{name}`"
    );
}

/// An install that removed a skill's entry and then failed (here on a
/// `.gitignore` that isn't UTF-8) no longer records it: the user's own skill
/// created under that name since survives `clean`.
#[test]
fn an_install_failing_after_a_removal_releases_the_removed_name() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
    let mut doc: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    let spec = doc["skills"]["old"].take();
    doc["skills"] = serde_json::json!({ "fresh": spec });
    write_manifest(&sb, &serde_json::to_string_pretty(&doc).unwrap());
    let gitignore = sb.project.join(".gitignore");
    let kept = std::fs::read(&gitignore).unwrap();
    std::fs::write(&gitignore, b"\xff\xfe\n").unwrap();
    fails(&sb, &["install"]);
    std::fs::write(&gitignore, kept).unwrap();
    assert_user_skill_survives_clean(&sb, "old");
}

/// A `clean` that removed codex's skills and then failed on Claude (whose
/// settings can't be parsed) no longer records them for codex.
#[test]
fn a_clean_failing_part_way_releases_what_it_removed() {
    // With a record, and without one yet (installed by an older spm).
    for legacy in [false, true] {
        let sb = Sandbox::new();
        sb.ok(&["init", "--target", "codex,claude"]);
        sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
        if legacy {
            std::fs::remove_file(sb.project_owned()).unwrap();
        }
        let settings = sb.project.join(".claude/settings.local.json");
        let kept = std::fs::read(&settings).unwrap();
        std::fs::write(&settings, "not json").unwrap();
        fails(&sb, &["clean"]);
        std::fs::write(&settings, kept).unwrap();
        assert_user_skill_survives_clean(&sb, "old");
    }
}

/// Likewise for `target remove` failing after removing one target's output.
#[test]
fn a_target_remove_failing_part_way_releases_what_it_removed() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex,claude,cursor"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
    let settings = sb.project.join(".claude/settings.local.json");
    let kept = std::fs::read(&settings).unwrap();
    std::fs::write(&settings, "not json").unwrap();
    fails(&sb, &["target", "remove", "codex,claude"]);
    std::fs::write(&settings, kept).unwrap();
    assert_user_skill_survives_clean(&sb, "old");
}

/// A command that removes entries releases them in the ownership record
/// before deleting anything: when the record can't be written (here
/// `$SPM_HOME/owned` is read-only), `clean` and `target remove` delete nothing,
/// so no record can outlive what they removed.
#[cfg(unix)]
#[test]
fn an_unwritable_ownership_record_stops_removals_before_they_start() {
    use std::os::unix::fs::PermissionsExt;
    for args in [&["clean"][..], &["target", "remove", "codex"]] {
        let sb = Sandbox::new();
        sb.ok(&["init", "--target", "codex,cursor"]);
        sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "old"]);
        let owned_dir = sb.project_owned().parent().unwrap().to_path_buf();
        std::fs::set_permissions(&owned_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let out = sb.command(args).output().unwrap();
        std::fs::set_permissions(&owned_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!out.status.success(), "{args:?} succeeded");
        assert!(
            sb.project.join(".agents/skills/old/SKILL.md").exists(),
            "{args:?} deleted before recording it"
        );
    }
}
