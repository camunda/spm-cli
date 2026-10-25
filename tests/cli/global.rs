//! Global scope (`-g/--global`).

use crate::common::*;

// ---------------------------------------------------------------------------
// Global scope (`-g/--global`).
//
// Global installs materialize into user-global vendor locations (`~/.copilot/
// skills`, `~/.claude/settings.json`) resolved from HOME. The harness overrides
// HOME to the sandbox, which `directories` honors on Unix — so these are
// Unix-gated; the cross-platform path/copy logic is covered by the vendor unit
// tests.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn global_copilot_add_materializes_into_home_and_stores_manifest_under_spm_home() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "copilot"]);

    // The global manifest lives under SPM_HOME, not the project.
    assert!(
        sb.spm_home.join("ai.json").exists(),
        "global ai.json missing"
    );
    assert!(
        !sb.project.join("ai.json").exists(),
        "global init must not touch the project"
    );

    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "greet",
    ]);

    // Skill copied flat into ~/.copilot/skills/greet/ (Copilot's global dir).
    let skill_md = sb.copilot_global_skills().join("greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());
    // No project-local materialization happened.
    assert!(
        !sb.project.join(".agents").exists(),
        "global add must not materialize into the project"
    );
    // Reuses the shared store (no separate global cache).
    assert!(
        sb.store_entries() > 0,
        "global add should populate the store"
    );
    // Global lock is under SPM_HOME.
    assert!(
        sb.spm_home.join("ai.lock").exists(),
        "global ai.lock missing"
    );
}

#[cfg(unix)]
#[test]
fn global_copilot_preserves_user_authored_skills_on_add_and_remove() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "copilot"]);

    // A skill the user authored by hand in the shared global dir.
    let mine = sb.copilot_global_skills().join("mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("SKILL.md"), "---\nname: mine\n---\n").unwrap();

    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "greet",
    ]);
    assert!(sb.copilot_global_skills().join("greet/SKILL.md").exists());
    assert!(
        mine.join("SKILL.md").exists(),
        "user's own skill must survive a global add"
    );

    sb.ok(&["remove", "-g", "greet"]);
    assert!(
        !sb.copilot_global_skills().join("greet").exists(),
        "removed skill should be gone"
    );
    assert!(
        mine.join("SKILL.md").exists(),
        "user's own skill must survive a global remove"
    );
}

#[cfg(unix)]
#[test]
fn global_cursor_materializes_into_home_and_preserves_user_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "cursor"]);

    // The global manifest lives under SPM_HOME, not the project.
    assert!(
        sb.spm_home.join("ai.json").exists(),
        "global ai.json missing"
    );

    // A skill the user authored by hand in the shared global dir.
    let cursor_global = sb.home.join(".cursor/skills");
    let mine = cursor_global.join("mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("SKILL.md"), "---\nname: mine\n---\n").unwrap();

    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "greet",
    ]);

    // Skill copied one level deep into ~/.cursor/skills/greet/ (Cursor's global dir).
    assert!(cursor_global.join("greet/SKILL.md").exists());
    // No project-local materialization happened.
    assert!(
        !sb.project.join(".cursor").exists(),
        "global add must not materialize into the project"
    );
    // The user's own skill in the shared dir survives.
    assert!(
        mine.join("SKILL.md").exists(),
        "user's own skill must survive a global add"
    );

    sb.ok(&["remove", "-g", "greet"]);
    assert!(
        !cursor_global.join("greet").exists(),
        "removed skill should be gone"
    );
    assert!(
        mine.join("SKILL.md").exists(),
        "user's own skill must survive a global remove"
    );
}

#[cfg(unix)]
#[test]
fn global_windsurf_materializes_into_asymmetric_codeium_dir() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "windsurf"]);

    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "greet",
    ]);

    // Windsurf's *global* dir is ~/.codeium/windsurf/skills (NOT ~/.windsurf/skills):
    // this guards the asymmetric project/global segments.
    let global_dir = sb.home.join(".codeium/windsurf/skills");
    assert!(
        global_dir.join("greet/SKILL.md").exists(),
        "missing {}",
        global_dir.join("greet/SKILL.md").display()
    );
    // Must not fall back to a ~/.windsurf mirror of the project path.
    assert!(
        !sb.home.join(".windsurf/skills/greet").exists(),
        "global windsurf must not use ~/.windsurf/skills"
    );
    // No project-local materialization happened.
    assert!(
        !sb.project.join(".windsurf").exists(),
        "global add must not materialize into the project"
    );

    sb.ok(&["remove", "-g", "greet"]);
    assert!(
        !global_dir.join("greet").exists(),
        "removed skill should be gone"
    );
}

#[cfg(unix)]
#[test]
fn global_claude_registers_spm_global_marketplace_in_user_settings() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "claude"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "greet",
    ]);

    // Marketplace built in the spm-owned global dir under SPM_HOME.
    let skill_md = sb
        .claude_global_market_dir()
        .join("plugin/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());

    // Registered under the distinct `spm-global` name in ~/.claude/settings.json.
    let settings = std::fs::read_to_string(sb.claude_global_settings()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&settings).unwrap();
    assert!(
        v["extraKnownMarketplaces"]["spm-global"].is_object(),
        "{settings}"
    );
    assert_eq!(
        v["enabledPlugins"]["spm-global@spm-global"],
        serde_json::Value::Bool(true),
        "{settings}"
    );
}

#[cfg(unix)]
#[test]
fn global_and_project_scopes_are_independent() {
    let sb = Sandbox::new();
    // Project scope.
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    // Global scope.
    sb.ok(&["init", "-g", "--target", "claude"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);

    // Two separate manifests/locks.
    assert!(sb.project.join("ai.lock").exists());
    assert!(sb.spm_home.join("ai.lock").exists());
    // The project pinned the tag; the global pinned the branch — independent.
    assert!(sb.read("ai.lock").contains("\"reference\": \"tag:v0.1.0\""));
    let global_lock = std::fs::read_to_string(sb.spm_home.join("ai.lock")).unwrap();
    assert!(
        global_lock.contains("\"reference\": \"branch:main\""),
        "{global_lock}"
    );

    // Both materialized in their own locations.
    assert!(sb
        .claude_market_dir()
        .join("plugin/skills/greet/SKILL.md")
        .exists());
    assert!(sb
        .claude_global_market_dir()
        .join("plugin/skills/greet/SKILL.md")
        .exists());
}

#[cfg(unix)]
#[test]
fn status_warns_when_a_skill_is_installed_in_both_scopes() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    sb.ok(&["init", "-g", "--target", "copilot"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "greet",
    ]);

    // Project status should flag the global shadow.
    let out = sb.ok(&["status"]);
    assert!(
        out.contains("also installed in the global scope"),
        "expected a shadow warning, got: {out}"
    );

    // And global status should flag the project shadow.
    let out = sb.ok(&["status", "-g"]);
    assert!(
        out.contains("also installed in the project scope"),
        "expected a shadow warning, got: {out}"
    );
}

#[cfg(unix)]
#[test]
fn status_warns_when_a_plugin_bundled_skill_shadows_the_other_scope() {
    let sb = Sandbox::new();
    // Project scope: a standalone skill named `composer` (matching the
    // plugin's bundled skill name added below in the global scope).
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "composer",
    ]);

    // Global scope: a plugin whose bundled skill is also named `composer`.
    sb.add_plugin();
    sb.ok(&["init", "-g", "--target", "copilot"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pkg",
        "--plugin",
        "--name",
        "ds",
    ]);

    // Project status must flag the collision even though the global entry is
    // a plugin-bundled skill, not a standalone one.
    let out = sb.ok(&["status"]);
    assert!(
        out.contains("also installed in the global scope"),
        "expected a shadow warning for the plugin-bundled skill, got: {out}"
    );
}

#[cfg(unix)]
#[test]
fn global_list_and_clean() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "copilot"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "greet",
    ]);

    let listed = sb.ok(&["list", "-g"]);
    assert!(listed.contains("greet"), "{listed}");

    sb.ok(&["clean", "-g"]);
    assert!(
        !sb.copilot_global_skills().join("greet").exists(),
        "clean -g should remove the materialized global skill"
    );
}

#[cfg(unix)]
#[test]
fn clean_purges_plugin_bundled_skills_from_shared_global_dir() {
    let sb = Sandbox::new();
    sb.add_plugin();
    sb.ok(&["init", "-g", "--target", "copilot"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pkg",
        "--plugin",
        "--name",
        "ds",
    ]);
    // The plugin's bundled `composer` skill lands in the shared global dir,
    // same as a standalone skill would.
    let composer = sb.copilot_global_skills().join("composer");
    assert!(
        composer.join("SKILL.md").exists(),
        "bundled skill materialized in shared global dir"
    );

    // A skill the user authored by hand in the same shared dir must survive.
    let user = sb.copilot_global_skills().join("user-skill");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("SKILL.md"), "mine\n").unwrap();

    sb.ok(&["clean", "-g"]);
    assert!(
        !composer.exists(),
        "clean must purge plugin-bundled skills from the shared global dir, \
         not just top-level skills"
    );
    assert!(
        user.join("SKILL.md").exists(),
        "the user's own skill in the shared dir must be preserved by clean"
    );
}
