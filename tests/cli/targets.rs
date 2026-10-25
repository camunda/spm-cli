//! `spm target add` / `spm target remove`.

use crate::common::*;
#[cfg(unix)]
use std::process::Command;

#[test]
fn target_add_appends_vendor_and_materializes_existing_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // Add copilot after the fact via an explicit vendor argument.
    let out = sb.ok(&["target", "add", "copilot"]);
    assert!(out.contains("added target(s): copilot"), "{out}");

    // ai.json now declares both targets, in insertion order.
    let manifest = sb.read("ai.json");
    assert!(manifest.contains("\"claude\""), "{manifest}");
    assert!(manifest.contains("\"copilot\""), "{manifest}");

    // The already-declared skill is materialized into the newly-added vendor
    // without a separate `spm install`.
    let skill_md = sb
        .project
        .join(".agents/skills/spm-managed-skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());
}

#[test]
fn target_add_splits_comma_separated_vendors_into_multiple_tokens() {
    let sb = Sandbox::new();
    // Only two vendors exist and `init` forces at least one, so at most one is
    // ever unconfigured — a single call can't *add* two. Instead prove the
    // `value_delimiter` split by passing BOTH vendors as one comma-separated
    // argument against a copilot-seeded manifest: the arg must parse into two
    // distinct tokens, so `claude` is added and `copilot` is recognized as
    // already-configured (a skip). A no-split parse would treat the whole string
    // as one unknown vendor and error instead.
    sb.ok(&["init", "--target", "copilot"]);
    let out = sb.ok(&["target", "add", "claude,copilot"]);
    assert!(out.contains("added target(s): claude"), "{out}");
    assert!(
        out.contains("`copilot` already configured"),
        "second comma-separated token must be parsed and skipped: {out}"
    );
    let manifest = sb.read("ai.json");
    assert!(manifest.contains("\"claude\""), "{manifest}");
    // copilot present exactly once — the skip must not duplicate it.
    assert_eq!(manifest.matches("\"copilot\"").count(), 1, "{manifest}");
}

#[test]
fn target_add_accepts_repeated_vendor_arguments() {
    let sb = Sandbox::new();
    // The space-separated (repeatable positional) form parses the same way as the
    // comma form: each token is validated independently.
    sb.ok(&["init", "--target", "copilot"]);
    let out = sb.ok(&["target", "add", "claude", "copilot"]);
    assert!(out.contains("added target(s): claude"), "{out}");
    assert!(out.contains("`copilot` already configured"), "{out}");
}

#[test]
fn target_add_already_configured_is_a_skip_not_an_error() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);

    // Re-adding claude succeeds (exit 0) and reports the skip.
    let out = sb.spm(&["target", "add", "claude"]);
    assert!(
        out.status.success(),
        "re-adding a configured target must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("already configured"), "{stdout}");
    assert!(stdout.contains("no new targets added"), "{stdout}");

    // ai.json still lists claude exactly once.
    assert_eq!(
        sb.read("ai.json").matches("\"claude\"").count(),
        1,
        "target must not be duplicated"
    );
}

#[test]
fn target_add_rejects_unknown_vendor() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let out = sb.spm(&["target", "add", "bogus"]);
    assert!(!out.status.success(), "unknown vendor must be rejected");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown target"), "{err}");
    // The manifest is untouched when validation fails.
    assert!(
        !sb.read("ai.json").contains("bogus"),
        "{}",
        sb.read("ai.json")
    );
}

fn manifest_targets(sb: &Sandbox) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    v["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn target_remove_cleans_managed_skills_and_keeps_user_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,cursor"]);
    let mine = sb.project.join(".cursor/skills/mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("SKILL.md"), "---\nname: mine\n---\n").unwrap();
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    assert!(sb.project.join(".cursor/skills/greet/SKILL.md").exists());

    let out = sb.ok(&["target", "remove", "cursor"]);
    assert!(out.contains("removed target(s): cursor"), "{out}");
    assert_eq!(manifest_targets(&sb), ["claude"]);
    assert!(
        !sb.project.join(".cursor/skills/greet").exists(),
        "spm-managed skill must be cleaned"
    );
    assert!(
        mine.join("SKILL.md").exists(),
        "hand-authored skill must be left untouched"
    );
    // The remaining target is untouched.
    assert!(sb
        .claude_market_dir()
        .join("plugin/skills/greet/SKILL.md")
        .exists());
}

#[test]
fn target_remove_claude_removes_marketplaces_and_spm_keys_only() {
    let sb = Sandbox::new();
    let plugin_name = sb.add_plugin();
    sb.ok(&["init", "--target", "claude"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude","copilot"],"skills":{{"greet":{{"git":"{url}","tag":"v0.1.0"}}}},"plugins":{{"ds":{{"git":"{url}","branch":"main","path":"pkg"}}}}}}"#,
            url = sb.skill_url()
        ),
    );
    std::fs::create_dir_all(sb.project.join(".claude")).unwrap();
    std::fs::write(
        sb.project.join(".claude/settings.local.json"),
        r#"{"permissions":{"allow":["Bash(ls)"]}}"#,
    )
    .unwrap();
    sb.ok(&["install"]);
    assert!(sb.claude_market_dir().exists());
    assert!(sb.project.join(".spm/claude-plugins/ds").exists());

    // `rm` alias, matching `spm remove`.
    sb.ok(&["target", "rm", "claude"]);
    assert_eq!(manifest_targets(&sb), ["copilot"]);
    assert!(
        !sb.claude_market_dir().exists(),
        "skills marketplace removed"
    );
    assert!(
        !sb.project.join(".spm/claude-plugins").exists(),
        "plugins marketplace removed"
    );
    let settings: serde_json::Value =
        serde_json::from_str(&sb.read(".claude/settings.local.json")).unwrap();
    assert_eq!(
        settings["permissions"]["allow"][0], "Bash(ls)",
        "user keys kept: {settings}"
    );
    let markets = settings.get("extraKnownMarketplaces");
    assert!(
        markets.and_then(|m| m.get("spm")).is_none()
            && markets.and_then(|m| m.get("spm-plugins")).is_none(),
        "spm marketplaces de-registered: {settings}"
    );
    let enabled = settings.get("enabledPlugins");
    assert!(
        enabled.and_then(|e| e.get("spm@spm")).is_none()
            && enabled
                .and_then(|e| e.get(format!("{plugin_name}@spm-plugins")))
                .is_none(),
        "spm plugins de-enabled: {settings}"
    );
    // Copilot keeps its skills.
    assert!(sb
        .project
        .join(".agents/skills/spm-managed-skills/greet/SKILL.md")
        .exists());
}

/// `codex` and `amp` both materialize into the project's `.agents/skills`, so
/// removing either while the other stays configured must keep those entries.
#[test]
fn target_remove_keeps_skills_dir_shared_with_a_remaining_target() {
    for (removed, kept) in [("codex", "amp"), ("amp", "codex")] {
        let sb = Sandbox::new();
        sb.ok(&["init", "--target", &format!("{removed},{kept}")]);
        sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
        let greet = sb.project.join(".agents/skills/greet/SKILL.md");
        assert!(greet.exists());

        let out = sb.ok(&["target", "remove", removed]);
        assert!(
            out.contains(&format!("still used by target `{kept}`")),
            "{out}"
        );
        assert_eq!(manifest_targets(&sb), [kept]);
        assert!(
            greet.exists(),
            "removing {removed} must not clean the dir {kept} still uses"
        );
    }
}

/// Shared-dir ownership compares resolved dirs, not path spellings: a vendor dir
/// symlinked onto another target's dir is still shared.
#[test]
fn target_remove_keeps_skills_dir_aliased_by_symlink_to_a_remaining_target() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.project.join(".agents/skills")).unwrap();
    std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
    symlink("../.agents/skills", sb.project.join(".cursor/skills"));
    sb.ok(&["init", "--target", "cursor,codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let greet = sb.project.join(".agents/skills/greet/SKILL.md");
    assert!(greet.exists());

    let out = sb.ok(&["target", "remove", "cursor"]);
    assert!(out.contains("still used by target `codex`"), "{out}");
    assert!(greet.exists(), "codex's skills must survive via the alias");
}

/// Nested aliasing: `.cursor/skills` symlinked into Claude's marketplace payload
/// dir. Neither removal may delete skills the other target still loads, and
/// removing Claude must still deregister it from settings.
#[test]
fn target_remove_keeps_output_nested_in_another_targets_dir() {
    for (removed, kept) in [("cursor", "claude"), ("claude", "cursor")] {
        let sb = Sandbox::new();
        std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
        symlink(
            "../.spm/claude/plugin/skills",
            sb.project.join(".cursor/skills"),
        );
        sb.ok(&["init", "--target", "claude,cursor"]);
        sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
        let greet = sb.claude_market_dir().join("plugin/skills/greet/SKILL.md");
        assert!(greet.exists());

        let out = sb.ok(&["target", "remove", removed]);
        assert!(
            out.contains(&format!("still used by target `{kept}`")),
            "{out}"
        );
        assert!(
            greet.exists(),
            "removing {removed} must keep the skill {kept} still loads"
        );
        let settings = sb.read(".claude/settings.local.json");
        assert_eq!(
            settings.contains("spm@spm"),
            removed == "cursor",
            "claude is deregistered only when removed: {settings}"
        );
    }
}

/// Plugins too: `.cursor/skills` symlinked into Claude's installed plugin copy.
/// Neither removal may delete the bundled skill the other target loads, and
/// removing Claude must still deregister its plugins marketplace.
#[test]
fn target_remove_keeps_output_nested_in_another_targets_plugin() {
    for (removed, kept) in [("claude", "cursor"), ("cursor", "claude")] {
        let sb = Sandbox::new();
        sb.add_plugin();
        std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
        symlink(
            "../.spm/claude-plugins/ds/skills",
            sb.project.join(".cursor/skills"),
        );
        sb.ok(&["init", "--target", "claude"]);
        write_manifest(
            &sb,
            &format!(
                r#"{{"targets":["claude","cursor"],"plugins":{{"ds":{{"git":"{}","branch":"main","path":"pkg"}}}}}}"#,
                sb.skill_url()
            ),
        );
        sb.ok(&["install"]);
        let composer = sb
            .project
            .join(".spm/claude-plugins/ds/skills/composer/SKILL.md");
        assert!(composer.exists());

        let out = sb.ok(&["target", "remove", removed]);
        assert!(
            out.contains(&format!("still used by target `{kept}`")),
            "{out}"
        );
        assert!(
            composer.exists(),
            "removing {removed} must keep the bundled skill {kept} still loads"
        );
        let settings = sb.read(".claude/settings.local.json");
        assert_eq!(
            settings.contains("spm-plugins"),
            removed == "cursor",
            "claude plugins deregistered only when claude is removed: {settings}"
        );
    }
}

/// Chained aliasing: `.cursor/skills` -> Claude's payload dir, which itself is a
/// symlink elsewhere. Removing Claude must keep the intermediate link Cursor
/// reaches its skills through, not just the final files.
#[test]
fn target_remove_keeps_intermediate_symlink_a_remaining_target_traverses() {
    for (removed, kept) in [("claude", "cursor"), ("cursor", "claude")] {
        let sb = Sandbox::new();
        std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
        symlink(
            "../.spm/claude/plugin/skills",
            sb.project.join(".cursor/skills"),
        );
        sb.ok(&["init", "--target", "claude,cursor"]);
        sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
        let payload = sb.claude_market_dir().join("plugin/skills");
        let shared = sb.project.join("shared-skills");
        std::fs::rename(&payload, &shared).unwrap();
        symlink(&shared, &payload);
        let via_cursor = sb.project.join(".cursor/skills/greet/SKILL.md");
        assert!(via_cursor.exists());

        let out = sb.ok(&["target", "remove", removed]);
        assert!(
            out.contains(&format!("still used by target `{kept}`")),
            "{out}"
        );
        assert!(
            via_cursor.exists(),
            "removing {removed} must keep every link {kept} loads through"
        );
        assert!(shared.join("greet/SKILL.md").exists());
    }
}

/// Claude needs its marketplace metadata, not just the skill payloads: removing
/// another target must not delete metadata Claude reaches through a symlink.
#[test]
fn target_remove_keeps_metadata_a_remaining_target_loads_through_a_symlink() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let meta = sb.claude_market_dir().join(".claude-plugin");
    let moved = sb
        .project
        .join(".agents/skills/spm-managed-skills/claude-meta");
    std::fs::rename(&meta, &moved).unwrap();
    symlink(&moved, &meta);
    let manifest = meta.join("marketplace.json");
    assert!(manifest.exists());

    let out = sb.ok(&["target", "remove", "copilot"]);
    assert!(out.contains("still used by target `claude`"), "{out}");
    assert!(manifest.exists(), "claude's marketplace.json must survive");
}

/// A link spelled differently from spm's own paths — canonical form, i.e. on
/// Windows a `\\?\` prefix and long names where the temp dir is spelled with
/// 8.3 short names — must still be recognised as the same location.
#[test]
fn target_remove_keeps_metadata_linked_by_a_differently_spelled_path() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let meta = sb.claude_market_dir().join(".claude-plugin");
    let moved = sb
        .project
        .join(".agents/skills/spm-managed-skills/claude-meta");
    std::fs::rename(&meta, &moved).unwrap();
    symlink(std::fs::canonicalize(&moved).unwrap(), &meta);
    let manifest = meta.join("marketplace.json");
    assert!(manifest.exists());

    let out = sb.ok(&["target", "remove", "copilot"]);
    assert!(out.contains("still used by target `claude`"), "{out}");
    assert!(manifest.exists(), "claude's marketplace.json must survive");
}

/// Claude's settings file registers its marketplace: removing another target
/// must not delete it when it is a symlink into that target's output.
#[test]
fn target_remove_keeps_settings_a_remaining_target_loads_through_a_symlink() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let settings = sb.project.join(".claude/settings.local.json");
    let moved = sb
        .project
        .join(".agents/skills/spm-managed-skills/claude-settings.json");
    std::fs::rename(&settings, &moved).unwrap();
    symlink(&moved, &settings);

    let out = sb.ok(&["target", "remove", "copilot"]);
    assert!(out.contains("still used by target `claude`"), "{out}");
    let kept = sb.read(".claude/settings.local.json");
    assert!(kept.contains("spm@spm"), "claude registration kept: {kept}");
}

/// On a case-insensitive filesystem `.AGENTS` and `.agents` are one dir: a
/// remaining target's output must be recognised however it is spelled.
#[test]
fn target_remove_keeps_output_aliased_by_case_on_case_insensitive_fs() {
    let probe = Sandbox::new();
    std::fs::create_dir_all(probe.project.join("case-probe")).unwrap();
    if !probe.project.join("CASE-PROBE").exists() {
        eprintln!("skipping: filesystem is case-sensitive");
        return;
    }
    for (removed, kept) in [("cursor", "codex"), ("codex", "cursor")] {
        let sb = Sandbox::new();
        std::fs::create_dir_all(sb.project.join(".AGENTS/skills")).unwrap();
        std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
        symlink("../.AGENTS/skills", sb.project.join(".cursor/skills"));
        sb.ok(&["init", "--target", "cursor,codex"]);
        sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
        let skill = sb.project.join(".agents/skills/greet/SKILL.md");
        assert!(skill.exists());

        let out = sb.ok(&["target", "remove", removed]);
        assert!(
            out.contains(&format!("still used by target `{kept}`")),
            "{out}"
        );
        assert!(
            skill.exists(),
            "removing {removed} must keep {kept}'s skill"
        );
    }
}

/// A relative `SPM_HOME` yields relative Claude global paths; they must still be
/// compared against other vendors' absolute paths for the same output. Unix
/// only: on Windows the home dir can't be redirected into the sandbox.
#[cfg(unix)]
#[test]
fn target_remove_global_with_relative_spm_home_keeps_shared_output() {
    for (removed, kept) in [("claude", "cursor"), ("cursor", "claude")] {
        let sb = Sandbox::new();
        let run = |args: &[&str]| {
            let out = Command::new(env!("CARGO_BIN_EXE_spm"))
                .args(args)
                .current_dir(&sb.project)
                .env("SPM_HOME", "rel-home")
                .env("HOME", &sb.home)
                .env("USERPROFILE", &sb.home)
                .env_remove("SPM_POLICY")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "spm {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let payload = sb.project.join("rel-home/claude-global/plugin/skills");
        std::fs::create_dir_all(sb.home.join(".cursor")).unwrap();
        symlink(&payload, sb.home.join(".cursor/skills"));
        run(&["init", "-g", "--target", "claude,cursor"]);
        run(&[
            "add",
            "-g",
            &sb.skill_url(),
            "--tag",
            "v0.1.0",
            "--name",
            "greet",
        ]);
        let greet = payload.join("greet/SKILL.md");
        assert!(greet.exists());

        let out = run(&["target", "remove", "-g", removed]);
        assert!(
            out.contains(&format!("still used by target `{kept}`")),
            "{out}"
        );
        assert!(
            greet.exists(),
            "removing {removed} must keep the skill {kept} still loads"
        );
    }
}

/// A dir a remaining target's alias passes through before a `..` must survive,
/// even though the skills themselves live elsewhere.
#[test]
fn target_remove_keeps_dir_traversed_before_dotdot_by_a_remaining_target() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.project.join(".spm/shared-skills")).unwrap();
    std::fs::create_dir_all(sb.project.join(".cursor")).unwrap();
    symlink(
        "../.spm/claude/../shared-skills",
        sb.project.join(".cursor/skills"),
    );
    sb.ok(&["init", "--target", "claude,cursor"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let via_cursor = sb.project.join(".cursor/skills/greet/SKILL.md");
    assert!(via_cursor.exists());

    let out = sb.ok(&["target", "remove", "claude"]);
    assert!(out.contains("still used by target `cursor`"), "{out}");
    assert!(
        via_cursor.exists(),
        "cursor's alias must still resolve after removing claude"
    );
    let settings = sb.read(".claude/settings.local.json");
    assert!(
        !settings.contains("spm@spm"),
        "claude deregistered: {settings}"
    );
}

#[test]
fn target_remove_unconfigured_is_a_skip_and_unknown_changes_nothing() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,cursor"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    let out = sb.ok(&["target", "remove", "gemini"]);
    assert!(out.contains("`gemini` not configured"), "{out}");
    assert!(out.contains("no targets removed"), "{out}");
    assert_eq!(manifest_targets(&sb), ["claude", "cursor"]);

    // A typo aborts the whole call, even alongside a valid configured vendor.
    let out = sb.spm(&["target", "remove", "cursor,bogus"]);
    assert!(!out.status.success(), "unknown vendor must be rejected");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown target `bogus`"), "{err}");
    assert_eq!(manifest_targets(&sb), ["claude", "cursor"]);
    assert!(sb.project.join(".cursor/skills/greet/SKILL.md").exists());
}

#[test]
fn target_remove_refuses_to_remove_the_last_target() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,cursor"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    let out = sb.spm(&["target", "remove", "claude,cursor"]);
    assert!(!out.status.success(), "removing every target must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("last target") && err.contains("spm clean"),
        "{err}"
    );
    assert_eq!(manifest_targets(&sb), ["claude", "cursor"]);
    assert!(sb.project.join(".cursor/skills/greet/SKILL.md").exists());
    assert!(sb.claude_market_dir().exists());
}

#[cfg(unix)]
#[test]
fn target_add_and_remove_global_retarget_the_global_manifest() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "cursor"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "greet",
    ]);
    let mine = sb.home.join(".cursor/skills/mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("SKILL.md"), "---\nname: mine\n---\n").unwrap();

    let out = sb.ok(&["target", "add", "-g", "copilot"]);
    assert!(out.contains("added target(s): copilot"), "{out}");
    assert!(sb.copilot_global_skills().join("greet/SKILL.md").exists());

    let out = sb.ok(&["target", "remove", "--global", "cursor"]);
    assert!(out.contains("removed target(s): cursor"), "{out}");
    assert!(!sb.home.join(".cursor/skills/greet").exists());
    assert!(mine.join("SKILL.md").exists(), "user's global skill kept");
    assert!(sb.copilot_global_skills().join("greet/SKILL.md").exists());

    let global: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sb.spm_home.join("ai.json")).unwrap())
            .unwrap();
    assert_eq!(global["targets"], serde_json::json!(["copilot"]));
    assert!(
        !sb.project.join("ai.json").exists(),
        "global retargeting must not touch the project"
    );
}

#[test]
fn target_add_interactive_picks_unconfigured_vendor_from_list() {
    let sb = Sandbox::new();
    // Configure every vendor except copilot so it is the sole unconfigured one
    // (option 1), independent of the global target ordering.
    sb.ok(&[
        "init",
        "--target",
        "amp,claude,cline,codex,cursor,gemini,windsurf",
    ]);

    // No vendor arg → interactive numbered picker over the unconfigured vendors
    // (here just `copilot`, option 1). Piped stdin drives it.
    let out = sb.spm_stdin(&["target", "add"], "1\n");
    assert!(
        out.status.success(),
        "interactive add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("1) copilot"),
        "picker must list copilot: {stdout}"
    );
    assert!(stdout.contains("added target(s): copilot"), "{stdout}");
    assert!(sb.read("ai.json").contains("\"copilot\""));
}

#[test]
fn target_add_interactive_reports_when_all_configured() {
    let sb = Sandbox::new();
    // Init with every supported vendor so the "nothing to pick" path is exercised
    // regardless of how many targets exist.
    sb.ok(&[
        "init",
        "--target",
        "amp,claude,cline,codex,copilot,cursor,gemini,windsurf",
    ]);

    // Every supported vendor is already configured: nothing to pick, and the
    // command short-circuits with a message (no stdin consumed).
    let out = sb.spm_stdin(&["target", "add"], "");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("all supported targets already configured"),
        "{stdout}"
    );
}

#[test]
fn target_add_interactive_rejects_out_of_range_selection() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let out = sb.spm_stdin(&["target", "add"], "9\n");
    assert!(!out.status.success(), "out-of-range pick must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("invalid selection"), "{err}");
    // Manifest unchanged on a bad pick.
    assert!(!sb.read("ai.json").contains("copilot"));
}
