//! Claude plugins: install, clean, collisions and bundled skills.

use crate::common::*;

/// Installing a full-plugin dependency registers the plugin's agents/MCP/scripts
/// for Claude via a dedicated `spm-plugins` marketplace, and flattens the
/// plugin's bundled skill into every target's skills dir (skills-only
/// degradation for Copilot). Verified against a `camunda-design-system`-shaped
/// plugin bundling `dev`/`validator` agents and a `composer` skill.
#[test]
fn plugin_install_materializes_agents_and_degrades_skills() {
    let sb = Sandbox::new();
    let plugin_name = sb.add_plugin();
    sb.ok(&["init", "--target", "claude", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude","copilot"],"plugins":{{"ds":{{"git":"{}","branch":"main","path":"pkg"}}}}}}"#,
            sb.skill_url()
        ),
    );

    let out = sb.ok(&["install"]);
    assert!(
        out.contains("plugin"),
        "install should report the plugin: {out}"
    );

    // Claude: the full plugin is copied into the spm-plugins marketplace, with
    // its agents and scripts intact.
    let pdir = sb.project.join(".spm/claude-plugins/ds");
    assert!(
        pdir.join("agents/dev.md").exists(),
        "dev agent materialized"
    );
    assert!(
        pdir.join("agents/validator.md").exists(),
        "validator agent materialized"
    );
    assert!(
        pdir.join("scripts/mcp.mjs").exists(),
        "plugin scripts materialized"
    );

    // The copied plugin.json keeps its name/mcpServers but has the `skills`
    // pointer stripped (skills are served via the spm skills marketplace).
    let pj: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(pdir.join(".claude-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(pj["name"], plugin_name);
    assert!(pj.get("mcpServers").is_some(), "mcpServers preserved: {pj}");
    assert!(pj.get("skills").is_none(), "skills pointer stripped: {pj}");

    // The spm-plugins marketplace lists the plugin under its own internal name.
    let market: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            sb.project
                .join(".spm/claude-plugins/.claude-plugin/marketplace.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(market["name"], "spm-plugins");
    assert_eq!(market["plugins"][0]["name"], plugin_name);
    assert_eq!(market["plugins"][0]["source"], "./ds");

    // Claude settings register + enable the plugin marketplace.
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sb.project.join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    assert!(settings["extraKnownMarketplaces"]["spm-plugins"].is_object());
    assert_eq!(
        settings["enabledPlugins"][format!("{plugin_name}@spm-plugins")],
        serde_json::Value::Bool(true)
    );

    // Skills-only degradation: the bundled `composer` skill lands in Copilot's
    // skills dir and in Claude's skills marketplace.
    assert!(
        sb.project
            .join(".agents/skills/spm-managed-skills/composer/SKILL.md")
            .exists(),
        "bundled skill degraded into Copilot skills dir"
    );
    assert!(
        sb.project
            .join(".spm/claude/plugin/skills/composer/SKILL.md")
            .exists(),
        "bundled skill served via Claude skills marketplace"
    );

    // ai.lock records the plugin's pinned commit and its bundled component set.
    let lock = sb.read("ai.lock");
    assert!(lock.contains("\"ds\""), "plugin locked: {lock}");
    assert!(
        lock.contains("bundled_skills"),
        "bundled skills recorded: {lock}"
    );
    assert!(lock.contains("composer"), "composer recorded: {lock}");
}

/// Acceptance test for the camunda/design-system case (issue #84): a plugin
/// whose `agents/`, `knowledge/`, and `skills/` are symlinks to repo-root
/// directories (outside the declared plugin subdir but inside the checkout)
/// must have that symlinked content materialized, not silently skipped.
#[cfg(unix)]
#[test]
fn plugin_with_in_checkout_symlinked_dirs_materializes_linked_content() {
    let sb = Sandbox::new();
    let plugin_name = sb.add_symlinked_plugin();
    sb.ok(&["init", "--target", "claude", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude","copilot"],"plugins":{{"cds":{{"git":"{}","branch":"main","path":"plugins/cds"}}}}}}"#,
            sb.skill_url()
        ),
    );

    sb.ok(&["install"]);

    let pdir = sb.project.join(".spm/claude-plugins/cds");
    // All five agents reached via the `agents -> ../../agents` symlink land.
    for agent in ["dev", "migration", "spec-author", "ui-builder", "validator"] {
        assert!(
            pdir.join(format!("agents/{agent}.md")).exists(),
            "symlinked agent `{agent}` must be materialized"
        );
    }
    // The knowledge dir (also symlinked) is materialized too.
    assert!(
        pdir.join("knowledge/guide.md").exists(),
        "symlinked knowledge dir must be materialized"
    );
    // Real (non-symlinked) files still land as before.
    assert!(
        pdir.join("scripts/run.mjs").exists(),
        "real plugin scripts materialized"
    );
    // The bundled skill reached through the `skills -> ../../skills` symlink is
    // enumerated and degraded into the skills marketplaces.
    assert!(
        sb.project
            .join(".agents/skills/spm-managed-skills/composer/SKILL.md")
            .exists(),
        "bundled skill (via symlinked skills dir) degraded into Copilot skills dir"
    );

    let market: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            sb.project
                .join(".spm/claude-plugins/.claude-plugin/marketplace.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(market["plugins"][0]["name"], plugin_name);
}

#[test]
fn plugin_bundled_skill_name_collision_is_rejected() {
    let sb = Sandbox::new();
    sb.add_plugin();
    sb.ok(&["init", "--target", "copilot"]);
    // Standalone skill named `composer` collides with the plugin's bundled one.
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"skills":{{"composer":{{"git":"{url}","tag":"v0.1.0"}}}},"plugins":{{"ds":{{"git":"{url}","branch":"main","path":"pkg"}}}}}}"#,
            url = sb.skill_url()
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "collision must fail the install");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("collision"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `spm clean` removes the plugins marketplace dir and de-registers it from
/// Claude settings.
#[test]
fn plugin_clean_removes_marketplace_and_registration() {
    let sb = Sandbox::new();
    let plugin_name = sb.add_plugin();
    sb.ok(&["init", "--target", "claude"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude"],"plugins":{{"ds":{{"git":"{}","branch":"main","path":"pkg"}}}}}}"#,
            sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    assert!(sb.project.join(".spm/claude-plugins/ds").exists());

    sb.ok(&["clean"]);
    assert!(
        !sb.project.join(".spm/claude-plugins").exists(),
        "clean removes the plugins marketplace dir"
    );
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sb.project.join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    assert!(
        settings
            .get("extraKnownMarketplaces")
            .and_then(|m| m.get("spm-plugins"))
            .is_none(),
        "spm-plugins marketplace de-registered: {settings}"
    );
    assert!(
        settings
            .get("enabledPlugins")
            .and_then(|e| e.get(format!("{plugin_name}@spm-plugins")))
            .is_none(),
        "plugin de-enabled: {settings}"
    );
}

/// `spm add --plugin` declares a full-plugin dependency in `ai.json` and
/// materializes it (agents registered for Claude); `spm remove --plugin` tears
/// it back down.
#[test]
fn add_and_remove_plugin_via_cli() {
    let sb = Sandbox::new();
    let plugin_name = sb.add_plugin();
    sb.ok(&["init", "--target", "claude"]);

    let out = sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pkg",
        "--plugin",
        "--name",
        "ds",
    ]);
    assert!(out.contains("added plugin ds"), "{out}");

    // Declared under `plugins` (not `skills`) in ai.json.
    let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    assert!(manifest["plugins"]["ds"].is_object(), "{manifest}");
    assert!(manifest.get("skills").is_none_or(|s| s.get("ds").is_none()));

    // Materialized: the plugin's agents are registered for Claude.
    assert!(sb
        .project
        .join(".spm/claude-plugins/ds/agents/dev.md")
        .exists());
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sb.project.join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        settings["enabledPlugins"][format!("{plugin_name}@spm-plugins")],
        serde_json::Value::Bool(true)
    );

    // `spm list` shows it as a plugin.
    let listed = sb.ok(&["list"]);
    assert!(
        listed.contains("ds") && listed.contains("plugin"),
        "{listed}"
    );

    // Remove it again.
    let out = sb.ok(&["remove", "ds", "--plugin"]);
    assert!(out.contains("removed plugin ds"), "{out}");
    let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    assert!(manifest
        .get("plugins")
        .is_none_or(|p| p.get("ds").is_none()));
    assert!(
        !sb.project.join(".spm/claude-plugins/ds").exists(),
        "plugin dir removed after remove --plugin"
    );
}

/// Adding a plugin whose name already names a *skill* (or vice versa) is a hard
/// error that must be caught *before* ai.json is rewritten — otherwise the
/// manifest would be persisted with a cross-map collision that every later
/// command rejects, stranding the user with an invalid ai.json. `--force` must
/// not override this (it only authorizes re-pinning a same-kind entry).
#[test]
fn add_rejects_cross_map_name_collision() {
    let sb = Sandbox::new();
    sb.add_plugin();
    sb.ok(&["init", "--target", "claude"]);
    // A standalone skill named `ds`.
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "ds"]);
    let before = sb.read("ai.json");

    // Adding a plugin also named `ds` must fail — even with --force.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pkg",
        "--plugin",
        "--name",
        "ds",
        "--force",
    ]);
    assert!(
        !out.status.success(),
        "cross-map name collision must fail even with --force"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unique across skills and plugins"),
        "stderr: {stderr}"
    );
    // ai.json must be untouched — the collision was caught before saving.
    assert_eq!(
        sb.read("ai.json"),
        before,
        "ai.json must not be rewritten when the add is rejected"
    );
    let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    assert!(
        manifest
            .get("plugins")
            .is_none_or(|p| p.get("ds").is_none()),
        "no plugin `ds` should have been persisted: {manifest}"
    );
}

/// Removing a non-existent plugin (or a skill via `--plugin`) is a clear error,
/// not a silent success.
#[test]
fn remove_plugin_unknown_errors() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    // A skill exists, but `--plugin` must not match it.
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let out = sb.spm(&["remove", "greet", "--plugin"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no plugin named `greet`"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `spm add --plugin --all` is rejected: a plugin is a single unit, not a
/// container of skills.
#[test]
fn add_plugin_conflicts_with_all() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--plugin",
        "--all",
    ]);
    assert!(!out.status.success(), "--plugin --all must conflict");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot be used with"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `spm status` must not falsely report success when a plugin's marketplace has
/// been removed (e.g. a deleted `.spm/claude-plugins` or a fresh worktree). The
/// plugin's *bundled skills* still sit in the skills marketplace, so a
/// skills-only check would wrongly pass — status must verify the full plugin
/// materialization too.
#[test]
fn status_fails_when_plugin_marketplace_deleted() {
    let sb = Sandbox::new();
    sb.add_plugin();
    sb.ok(&["init", "--target", "claude"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude"],"plugins":{{"ds":{{"git":"{}","branch":"main","path":"pkg"}}}}}}"#,
            sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    // Sanity: a clean install reports success.
    sb.ok(&["status"]);

    // Simulate a fresh worktree / accidental deletion: the full-plugin
    // marketplace is gone, but the bundled skill copy survives.
    std::fs::remove_dir_all(sb.project.join(".spm/claude-plugins")).unwrap();
    assert!(sb
        .project
        .join(".spm/claude/plugin/skills/composer/SKILL.md")
        .exists());

    let out = sb.spm(&["status"]);
    assert!(
        !out.status.success(),
        "status must fail when the plugin marketplace is missing"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("MISSING"),
        "should mark plugin MISSING: {text}"
    );
    assert!(
        text.contains("spm install"),
        "should tell the user to run `spm install`: {text}"
    );
}

/// `spm status` must fail when `ai.json` declares a plugin that `ai.lock` never
/// pinned (a partial/hand-edited lock), instructing the user to run
/// `spm install` rather than reporting a false success.
#[test]
fn status_fails_when_plugin_declared_but_not_locked() {
    let sb = Sandbox::new();
    sb.add_plugin();
    sb.ok(&["init", "--target", "claude"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude"],"plugins":{{"ds":{{"git":"{}","branch":"main","path":"pkg"}}}}}}"#,
            sb.skill_url()
        ),
    );
    sb.ok(&["install"]);

    // Drop the plugin from ai.lock (leaving ai.json declaring it) to model a
    // partial lock. This also clears the only source of bundled skills, so
    // `expected` is empty — exactly the branch that used to report success.
    let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    let mut lock = lock.as_object().unwrap().clone();
    lock.remove("plugins");
    std::fs::write(
        sb.project.join("ai.lock"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    let out = sb.spm(&["status"]);
    assert!(
        !out.status.success(),
        "status must fail when a declared plugin is absent from ai.lock"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("not in ai.lock") && text.contains("spm install"),
        "should flag the unlocked plugin and point at `spm install`: {text}"
    );
}

/// Removing a plugin must purge its bundled skills from a *shared* global skills
/// dir. Those skills are flattened into the same dir the vendor shares with the
/// user's own skills, so the previous-managed set fed to `materialize` must
/// include plugin-bundled skills — otherwise they orphan on removal.
#[cfg(unix)]
#[test]
fn removing_plugin_purges_bundled_skills_from_shared_global_dir() {
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
    // The plugin's bundled `composer` skill lands in the shared global dir.
    let composer = sb.copilot_global_skills().join("composer");
    assert!(
        composer.join("SKILL.md").exists(),
        "bundled skill materialized in shared global dir"
    );

    // A skill the user authored by hand in the same shared dir must survive.
    let user = sb.copilot_global_skills().join("user-skill");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("SKILL.md"), "mine\n").unwrap();

    sb.ok(&["remove", "-g", "ds", "--plugin"]);
    assert!(
        !composer.exists(),
        "bundled skill must be purged from the shared global dir on plugin removal"
    );
    assert!(
        user.join("SKILL.md").exists(),
        "the user's own skill in the shared dir must be preserved"
    );
}
