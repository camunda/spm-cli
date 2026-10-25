//! `--lock-only` updates.

use crate::common::*;
use std::path::{Path, PathBuf};

/// `--lock-only` fixture: two branch skills from their own repos (`gone`, `ok`)
/// and a branch plugin `kit` (bundling `composer`) from the sandbox repo, all
/// installed for copilot. Returns `ok`'s repo dir.
fn lock_only_fixture(sb: &Sandbox) -> PathBuf {
    let gone = make_repo(
        &sb.root.join("gone"),
        &[("SKILL.md", "---\nname: gone\n---\nGone.\n")],
    );
    let ok_dir = sb.root.join("ok");
    let ok = make_repo(&ok_dir, &[("SKILL.md", "---\nname: ok\n---\nOk v1.\n")]);
    sb.add_plugin();
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        sb,
        &format!(
            r#"{{"targets":["copilot"],"skills":{{
                "gone":{{"git":"{gone}","branch":"main"}},
                "ok":{{"git":"{ok}","branch":"main"}}}},
              "plugins":{{"kit":{{"git":"{u}","branch":"main","path":"pkg"}}}}}}"#,
            u = sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    ok_dir
}

/// Commit a new `SKILL.md` body to `dir` on `main` and return the new tip.
fn commit_skill(dir: &Path, body: &str) -> String {
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
    git_in(dir, &["commit", "-aqm", "change"]);
    skill_head(dir)
}

/// Make `gone` and `kit` unfetchable — remotes moved away and the store wiped,
/// as on a fresh CI runner. `ok` stays reachable. Undone by [`restore_unrelated`].
fn break_unrelated(sb: &Sandbox) {
    std::fs::rename(sb.root.join("gone"), sb.root.join("gone-moved")).unwrap();
    std::fs::rename(&sb.skill_repo, sb.root.join("skill-moved")).unwrap();
    std::fs::remove_dir_all(sb.spm_home.join("store")).unwrap();
}

fn restore_unrelated(sb: &Sandbox) {
    std::fs::rename(sb.root.join("gone-moved"), sb.root.join("gone")).unwrap();
    std::fs::rename(sb.root.join("skill-moved"), &sb.skill_repo).unwrap();
}

/// Assert `after` is `before` except for the listed `(map, name)` entries,
/// which must come from `after` — every other entry (and the project `id`) is
/// carried over exactly, `bundled_skills`/`requested_by` included.
fn assert_only_entries_changed(
    before: &serde_json::Value,
    after: &serde_json::Value,
    changed: &[(&str, &str)],
) {
    let mut expected = before.clone();
    for (map, name) in changed {
        let entry = after[*map][*name].clone();
        assert!(!entry.is_null(), "{map}.{name} missing from {after}");
        expected[*map][*name] = entry;
    }
    assert_eq!(after, &expected, "only {changed:?} may change in ai.lock");
}

fn vendor_tree(sb: &Sandbox) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    tree_snapshot(&sb.project.join(".agents"))
}

/// Issue #126: `update <name> --lock-only` re-resolves and fetches only that
/// dependency, so an unrelated one whose remote is gone (and whose store entry
/// is missing) neither blocks it nor changes in ai.lock. Nothing is installed,
/// and the lock it writes is exactly what a full `update <name>` writes.
#[test]
fn update_lock_only_skips_unrelated_unreachable_dependencies() {
    let sb = Sandbox::new();
    let ok_dir = lock_only_fixture(&sb);
    let before = lock_doc(&sb);
    assert_eq!(
        before["plugins"]["kit"]["bundled_skills"],
        serde_json::json!(["composer"])
    );
    let vendor_before = vendor_tree(&sb);
    let tip = commit_skill(&ok_dir, "---\nname: ok\n---\nOk v2.\n");
    break_unrelated(&sb);

    let out = sb.ok(&["update", "ok", "--lock-only"]);
    assert!(out.contains("wrote ai.lock only"), "{out}");
    assert!(out.contains("run `spm install`"), "{out}");
    let after = lock_doc(&sb);
    assert_eq!(after["skills"]["ok"]["commit"], tip);
    assert_only_entries_changed(&before, &after, &[("skills", "ok")]);
    assert_eq!(vendor_tree(&sb), vendor_before, "nothing is materialized");
    assert_eq!(sb.store_entries(), 1, "only `ok` is fetched");
    // Without --lock-only the unrelated, unreachable pins block the update.
    let err = fails(&sb, &["update", "ok"]);
    assert!(err.contains("fetching"), "{err}");

    // The lock is complete: `install --locked` accepts it, and a full update of
    // the same dependency writes the very same bytes.
    restore_unrelated(&sb);
    let lock_only_bytes = snapshot(&sb, "ai.lock");
    sb.ok(&["install", "--locked"]);
    sb.ok(&["update", "ok"]);
    assert_eq!(snapshot(&sb, "ai.lock"), lock_only_bytes);
    assert!(sb
        .read(".agents/skills/spm-managed-skills/ok/SKILL.md")
        .contains("Ok v2."));
}

/// Every form of `add --lock-only` writes ai.json + ai.lock for the new or
/// re-pinned entry alone, without fetching unrelated, unreachable entries or
/// installing anything.
#[test]
fn add_lock_only_skips_unrelated_unreachable_dependencies() {
    struct Case {
        label: &'static str,
        files: &'static [(&'static str, &'static str)],
        args: &'static [&'static str],
        changed: &'static [(&'static str, &'static str)],
    }
    let cases = [
        Case {
            label: "new skill",
            files: &[("SKILL.md", "---\nname: fresh\n---\nFresh.\n")],
            args: &["--branch", "main", "--name", "fresh"],
            changed: &[("skills", "fresh")],
        },
        Case {
            label: "new plugin",
            files: &[
                (
                    "pkg/.claude-plugin/plugin.json",
                    r#"{"name":"plug","skills":"./skills/"}"#,
                ),
                (
                    "pkg/skills/other/SKILL.md",
                    "---\nname: other\n---\nOther.\n",
                ),
            ],
            args: &[
                "--branch", "main", "--path", "pkg", "--plugin", "--name", "plug",
            ],
            changed: &[("plugins", "plug")],
        },
        Case {
            label: "--all",
            files: &[
                ("pack/alpha/SKILL.md", "---\nname: alpha\n---\n"),
                ("pack/beta/SKILL.md", "---\nname: beta\n---\n"),
            ],
            args: &["--branch", "main", "--path", "pack", "--all"],
            changed: &[("skills", "alpha"), ("skills", "beta")],
        },
    ];
    for case in cases {
        let label = case.label;
        let sb = Sandbox::new();
        lock_only_fixture(&sb);
        let source = make_repo(&sb.root.join("new"), case.files);
        let before = lock_doc(&sb);
        let manifest_before: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
        let vendor_before = vendor_tree(&sb);
        break_unrelated(&sb);

        let mut args = vec!["add", source.as_str(), "--lock-only"];
        args.extend_from_slice(case.args);
        let out = sb.ok(&args);
        assert!(out.contains("wrote ai.lock only"), "{label}: {out}");
        let after = lock_doc(&sb);
        assert_only_entries_changed(&before, &after, case.changed);
        let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
        let mut expected = manifest_before.clone();
        for (map, name) in case.changed {
            assert!(manifest[*map][*name].is_object(), "{label}: {manifest}");
            expected[*map][*name] = manifest[*map][*name].clone();
        }
        assert_eq!(
            manifest, expected,
            "{label}: only the new entries are added"
        );
        if label == "new plugin" {
            assert_eq!(
                after["plugins"]["plug"]["bundled_skills"],
                serde_json::json!(["other"])
            );
        }
        assert_eq!(
            vendor_tree(&sb),
            vendor_before,
            "{label}: nothing is materialized"
        );

        restore_unrelated(&sb);
        sb.ok(&["install", "--locked"]);
    }
}

/// The update action's re-tag: `add --force --tag <new> --lock-only` re-pins an
/// existing entry without fetching unrelated, unreachable ones.
#[test]
fn add_force_lock_only_repins_without_fetching_unrelated_dependencies() {
    let sb = Sandbox::new();
    let ok_dir = lock_only_fixture(&sb);
    let ok = sb.root.join("ok").display().to_string().replace('\\', "/");
    let tip = commit_skill(&ok_dir, "---\nname: ok\n---\nOk v2.\n");
    git_in(&ok_dir, &["tag", "v2.0.0"]);
    let before = lock_doc(&sb);
    let vendor_before = vendor_tree(&sb);
    break_unrelated(&sb);

    sb.ok(&[
        "add",
        "--tag=v2.0.0",
        "--name=ok",
        "--force",
        "--lock-only",
        "--",
        &format!("file://{ok}"),
    ]);
    let after = lock_doc(&sb);
    assert_eq!(after["skills"]["ok"]["commit"], tip);
    assert_eq!(after["skills"]["ok"]["reference"], "tag:v2.0.0");
    assert_only_entries_changed(&before, &after, &[("skills", "ok")]);
    let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    assert_eq!(manifest["skills"]["ok"]["tag"], "v2.0.0");
    assert_eq!(vendor_tree(&sb), vendor_before);
}

/// A bare `update --lock-only` refreshes every entry (so fetches them all) but
/// still installs nothing.
#[test]
fn bare_update_lock_only_refreshes_everything_without_installing() {
    let sb = Sandbox::new();
    let ok_dir = lock_only_fixture(&sb);
    let ok_tip = commit_skill(&ok_dir, "---\nname: ok\n---\nOk v2.\n");
    let gone_tip = commit_skill(&sb.root.join("gone"), "---\nname: gone\n---\nGone v2.\n");
    let vendor_before = vendor_tree(&sb);

    sb.ok(&["update", "--lock-only"]);
    let after = lock_doc(&sb);
    assert_eq!(after["skills"]["ok"]["commit"], ok_tip);
    assert_eq!(after["skills"]["gone"]["commit"], gone_tip);
    assert_eq!(vendor_tree(&sb), vendor_before);
    sb.ok(&["install", "--locked"]);
}

/// `ai.lock` never depends on `spm link` overrides and `--lock-only` writes
/// nothing else, so it ignores them: a linked directory that has since
/// disappeared (which fails every installing sync) does not block it, and the
/// linked output and link state are left as they were.
#[test]
fn update_lock_only_ignores_links() {
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
    let dev = local_skill_checkout(&sb, "dev", "LOCAL EDIT");
    sb.ok(&["link", "greet", "../dev"]);
    let links = read_path(&sb.project_links());
    let installed = sb
        .project
        .join(".agents/skills/spm-managed-skills/greet/SKILL.md");
    std::fs::remove_dir_all(&dev).unwrap();
    fails(&sb, &["update", "greet"]);

    let tip = commit_skill(&sb.skill_repo, "---\nname: greet\n---\nGreet v2.\n");
    let out = sb.ok(&["update", "greet", "--lock-only"]);
    assert!(out.contains("wrote ai.lock only"), "{out}");
    assert_eq!(lock_doc(&sb)["skills"]["greet"]["commit"], tip);
    assert!(read_path(&installed).contains("LOCAL EDIT"));
    assert_eq!(read_path(&sb.project_links()), links);
}

/// An unrelated entry whose ai.json request no longer matches its pin can't be
/// carried over: `--lock-only` re-resolves it like any sync, and fails when its
/// remote is gone rather than keep a pin ai.json no longer asks for.
#[test]
fn lock_only_still_resolves_an_unrelated_entry_whose_request_changed() {
    let sb = Sandbox::new();
    lock_only_fixture(&sb);
    break_unrelated(&sb);
    let manifest = sb
        .read("ai.json")
        .replacen(r#""branch":"main"}"#, r#""branch":"dev"}"#, 1);
    assert!(manifest.contains(r#""branch":"dev"}"#));
    write_manifest(&sb, &manifest);
    let lock_before = snapshot(&sb, "ai.lock");
    let err = fails(&sb, &["update", "ok", "--lock-only"]);
    assert!(err.contains("resolving skill `gone`"), "{err}");
    assert_eq!(snapshot(&sb, "ai.lock"), lock_before);
}

/// What `--lock-only` does fetch still goes through the content scan: a
/// blocked entry is never pinned.
#[test]
fn add_lock_only_still_scans_what_it_fetches() {
    let sb = Sandbox::new();
    sb.add_malicious_skill();
    sb.ok(&["init", "--target", "copilot"]);
    let manifest_before = snapshot(&sb, "ai.json");
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "malicious",
        "--lock-only",
    ]);
    let err = assert_failed_and_unchanged(&sb, &out, &manifest_before, &None);
    assert!(err.contains("scanning skill `malicious`"), "{err}");
}

/// With `resolveTransitive`, `update <name> --lock-only` keeps every other
/// root's transitive pins exactly, even when their branches moved — but it
/// still walks those roots to read their nested `ai.json`, so it needs them
/// (store or remote) and fails rather than write an unverified lock.
#[test]
fn update_lock_only_with_transitive_keeps_other_transitive_pins() {
    let sb = Sandbox::new();
    let leaf_dir = sb.root.join("leaf");
    let leaf = make_repo(&leaf_dir, &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")]);
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
    let ok_dir = sb.root.join("ok");
    let ok = make_repo(&ok_dir, &[("SKILL.md", "---\nname: ok\n---\nOk.\n")]);
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{
                "top":{{"git":"{top}","branch":"main"}},
                "ok":{{"git":"{ok}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let before = lock_doc(&sb);
    let leaf_key = before["skills"]
        .as_object()
        .unwrap()
        .keys()
        .find(|k| k.starts_with("top__leaf-"))
        .unwrap()
        .clone();
    let vendor_before = vendor_tree(&sb);
    commit_skill(&leaf_dir, "---\nname: leaf\n---\nLeaf v2.\n");
    commit_skill(&top_dir, "---\nname: top\n---\nTop v2.\n");
    let tip = commit_skill(&ok_dir, "---\nname: ok\n---\nOk v2.\n");

    sb.ok(&["update", "ok", "--lock-only"]);
    let after = lock_doc(&sb);
    assert_eq!(after["skills"]["ok"]["commit"], tip);
    assert_only_entries_changed(&before, &after, &[("skills", "ok")]);
    assert_eq!(after["skills"][&leaf_key], before["skills"][&leaf_key]);
    assert_eq!(vendor_tree(&sb), vendor_before);

    std::fs::rename(&top_dir, sb.root.join("top-moved")).unwrap();
    std::fs::remove_dir_all(sb.spm_home.join("store")).unwrap();
    let lock_before = snapshot(&sb, "ai.lock");
    let err = fails(&sb, &["update", "ok", "--lock-only"]);
    assert!(err.contains("fetching skill `top`"), "{err}");
    assert_eq!(snapshot(&sb, "ai.lock"), lock_before);
}

/// A skills dir shared with the user's own skills (Codex in a project, Copilot
/// globally), where spm may only remove what it owns.
fn shared_skills_dir(sb: &Sandbox, global: bool) -> PathBuf {
    if global {
        sb.home.join(".copilot").join("skills")
    } else {
        sb.project.join(".agents").join("skills")
    }
}

/// Install plugin `kit` (bundling `old`) and transitive root `top` (pulling in
/// `leaf`) into a shared skills dir next to a user-authored `mine`, then make
/// lock-only updates drop `old` (now `new`) and `leaf` from ai.lock. Returns
/// the shared dir and `leaf`'s lock key.
fn lock_only_drops_installed_skills(sb: &Sandbox, global: bool) -> (PathBuf, String) {
    let g: &[&str] = if global { &["-g"] } else { &[] };
    let run = |args: &[&str]| sb.ok(&[args, g].concat());
    let kit_dir = sb.root.join("kit");
    let kit = make_repo(
        &kit_dir,
        &[
            (
                "pkg/.claude-plugin/plugin.json",
                r#"{"name":"kit","version":"1.0.0","skills":"./skills/"}"#,
            ),
            ("pkg/skills/old/SKILL.md", "---\nname: old\n---\nOld.\n"),
        ],
    );
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
    let target = if global { "copilot" } else { "codex" };
    run(&["init", "--target", target]);
    let manifest_dir = if global { &sb.spm_home } else { &sb.project };
    std::fs::write(
        manifest_dir.join("ai.json"),
        format!(
            r#"{{"targets":["{target}"],"resolveTransitive":true,
              "skills":{{"top":{{"git":"{top}","branch":"main"}}}},
              "plugins":{{"kit":{{"git":"{kit}","branch":"main","path":"pkg"}}}}}}"#
        ),
    )
    .unwrap();
    let dir = shared_skills_dir(sb, global);
    std::fs::create_dir_all(dir.join("mine")).unwrap();
    std::fs::write(dir.join("mine/SKILL.md"), "---\nname: mine\n---\nMine.\n").unwrap();
    run(&["install"]);
    let lock: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest_dir.join("ai.lock")).unwrap())
            .unwrap();
    let leaf_key = lock["skills"]
        .as_object()
        .unwrap()
        .keys()
        .find(|k| k.starts_with("top__leaf-"))
        .unwrap()
        .clone();
    for name in ["old", "top", leaf_key.as_str(), "mine"] {
        assert!(dir.join(name).join("SKILL.md").exists(), "{name} installed");
    }

    git_in(&kit_dir, &["mv", "pkg/skills/old", "pkg/skills/new"]);
    std::fs::write(
        kit_dir.join("pkg/skills/new/SKILL.md"),
        "---\nname: new\n---\nNew.\n",
    )
    .unwrap();
    git_in(&kit_dir, &["commit", "-aqm", "rename old to new"]);
    git_in(&top_dir, &["rm", "-q", "ai.json"]);
    git_in(&top_dir, &["commit", "-qm", "drop leaf"]);
    run(&["update", "kit", "--lock-only"]);
    run(&["update", "top", "--lock-only"]);
    let lock = std::fs::read_to_string(manifest_dir.join("ai.lock")).unwrap();
    assert!(
        !lock.contains(&leaf_key) && !lock.contains("\"old\""),
        "{lock}"
    );
    for name in ["old", leaf_key.as_str()] {
        assert!(dir.join(name).exists(), "lock-only leaves {name} installed");
    }
    (dir, leaf_key)
}

/// A skill a lock-only update drops from ai.lock (a plugin's bundled skill or a
/// transitive pin) is still spm's: the next `install` purges it from the
/// shared target, and the user's own skills there are left alone.
#[test]
fn install_after_lock_only_update_removes_the_skills_it_dropped() {
    let sb = Sandbox::new();
    let (dir, leaf_key) = lock_only_drops_installed_skills(&sb, false);
    sb.ok(&["install"]);
    for gone in ["old", leaf_key.as_str()] {
        assert!(!dir.join(gone).exists(), "{gone} purged");
    }
    for kept in ["new", "top", "mine"] {
        assert!(dir.join(kept).join("SKILL.md").exists(), "{kept} kept");
    }
    // Ownership now matches the lock again: removing `kit` purges `new`.
    sb.ok(&["remove", "--plugin", "kit"]);
    assert!(!dir.join("new").exists());
    assert!(dir.join("mine/SKILL.md").exists());
    // The record is machine-local state, kept out of the checkout.
    assert!(sb.project_owned().exists());
    assert!(!sb.project.join(".spm").exists());
}

/// Same for `clean`: it removes what a lock-only update dropped from ai.lock,
/// then records that spm owns nothing.
#[test]
fn clean_after_lock_only_update_removes_the_skills_it_dropped() {
    let sb = Sandbox::new();
    let (dir, leaf_key) = lock_only_drops_installed_skills(&sb, false);
    sb.ok(&["clean"]);
    for gone in ["old", "top", leaf_key.as_str()] {
        assert!(!dir.join(gone).exists(), "{gone} cleaned");
    }
    assert!(dir.join("mine/SKILL.md").exists(), "user skill kept");
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sb.project_owned()).unwrap()).unwrap();
    assert_eq!(
        record,
        serde_json::json!({"targets": {}}),
        "nothing owned now"
    );
}

/// The global scope keeps its own record (under SPM_HOME) for Copilot's flat
/// `~/.copilot/skills`, which is shared with the user's personal skills.
#[cfg(unix)]
#[test]
fn global_install_and_clean_after_lock_only_update_remove_the_skills_it_dropped() {
    for finish in ["install", "clean"] {
        let sb = Sandbox::new();
        let (dir, leaf_key) = lock_only_drops_installed_skills(&sb, true);
        assert_eq!(dir, sb.copilot_global_skills());
        assert!(sb.spm_home.join("owned-global.json").exists());
        assert!(!sb.project.join(".spm").exists(), "{finish}");
        sb.ok(&[finish, "-g"]);
        for gone in ["old", leaf_key.as_str()] {
            assert!(!dir.join(gone).exists(), "{finish}: {gone} removed");
        }
        assert!(
            dir.join("mine/SKILL.md").exists(),
            "{finish}: user skill kept"
        );
        assert_eq!(dir.join("new").exists(), finish == "install");
        let record: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(sb.spm_home.join("owned-global.json")).unwrap(),
        )
        .unwrap();
        let skills = &record["targets"]["copilot"]["skills"];
        assert_eq!(skills.is_null(), finish == "clean", "{finish}: {record}");
    }
}

/// Ownership is what spm installed, not what intermediate locks named: after
/// successive lock-only updates (the first bundling a name the user also
/// authored, the next renaming it away), install and clean leave the user's
/// skill alone and still remove what was really installed.
#[test]
fn successive_lock_only_updates_do_not_claim_never_installed_names() {
    for finish in ["install", "clean"] {
        let sb = Sandbox::new();
        let (dir, _) = lock_only_drops_installed_skills(&sb, false);
        let kit_dir = sb.root.join("kit");
        // Intermediate lock: `kit` bundles `mine`, a name the user authored.
        git_in(&kit_dir, &["mv", "pkg/skills/new", "pkg/skills/mine"]);
        git_in(&kit_dir, &["commit", "-qm", "bundle mine"]);
        sb.ok(&["update", "kit", "--lock-only"]);
        assert!(sb.read("ai.lock").contains("\"mine\""));
        git_in(&kit_dir, &["mv", "pkg/skills/mine", "pkg/skills/new"]);
        git_in(&kit_dir, &["commit", "-qm", "bundle new"]);
        sb.ok(&["update", "kit", "--lock-only"]);

        sb.ok(&[finish]);
        assert_eq!(
            std::fs::read_to_string(dir.join("mine/SKILL.md")).unwrap(),
            "---\nname: mine\n---\nMine.\n",
            "{finish}: user skill untouched"
        );
        assert!(
            !dir.join("old").exists(),
            "{finish}: installed `old` removed"
        );
    }
}

/// A target added after a lock-only update owns nothing yet: installing into it
/// must not delete a user's same-named skill that spm only ever installed into
/// another target (and the lock no longer requests). The original target is
/// still purged.
#[test]
fn target_added_after_lock_only_update_does_not_purge_other_targets_names() {
    let sb = Sandbox::new();
    let (dir, _) = lock_only_drops_installed_skills(&sb, false);
    let cursor = sb.project.join(".cursor/skills");
    std::fs::create_dir_all(cursor.join("old")).unwrap();
    std::fs::write(cursor.join("old/SKILL.md"), "Mine.\n").unwrap();

    sb.ok(&["target", "add", "cursor"]);
    assert_eq!(
        std::fs::read_to_string(cursor.join("old/SKILL.md")).unwrap(),
        "Mine.\n"
    );
    assert!(cursor.join("new/SKILL.md").exists());
    assert!(!dir.join("old").exists(), "codex's own `old` purged");

    // Removing a target forgets it, so its output is never claimed again.
    sb.ok(&["target", "remove", "cursor"]);
    assert!(!cursor.join("new").exists());
    assert!(cursor.join("old/SKILL.md").exists());
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sb.project_owned()).unwrap()).unwrap();
    assert!(record["targets"]["cursor"].is_null(), "{record}");
    assert!(!record["targets"]["codex"].is_null(), "{record}");
}
