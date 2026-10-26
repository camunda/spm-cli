//! Core commands: per-target `add`, aliases, `init`, manifest loading and
//! validation, `status`, `list`, `update`, `prune`.

use crate::common::*;

#[test]
fn claude_add_resolves_tag_to_commit_and_wires_marketplace() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // ai.lock pins the annotated tag to the underlying commit (not the tag object).
    let lock = sb.read("ai.lock");
    assert!(lock.contains("\"reference\": \"tag:v0.1.0\""), "{lock}");
    assert!(
        lock.contains(&skill_head(&sb.skill_repo)),
        "lock should pin repo HEAD commit: {lock}"
    );

    // Skill physically copied into the plugin dir.
    let skill_md = sb.claude_market_dir().join("plugin/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());

    // Project pointer written to the gitignored local settings file.
    let settings = sb.read(".claude/settings.local.json");
    assert!(settings.contains("\"spm@spm\": true"), "{settings}");
    assert!(settings.contains("extraKnownMarketplaces"), "{settings}");

    // The project-local marketplace dir is gitignored (with an explanatory
    // comment) so the copied skills are never committed.
    let gitignore = sb.read(".gitignore");
    assert!(gitignore.contains(".spm/"), "{gitignore}");
    assert!(gitignore.contains("spm-managed Claude"), "{gitignore}");

    // Nothing landed in the global vendors area.
    assert!(!sb.spm_home.join("vendors").exists());
    // Nothing leaked into the project tree beyond ai.json/ai.lock/.claude/.spm.
    assert!(!sb.project.join("skills").exists());
}

#[test]
fn copilot_add_materializes_project_local_skills() {
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

    // Skills are copied into a project-local dir, not a user-global marketplace.
    let skill_md = sb
        .project
        .join(".agents/skills/spm-managed-skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());
    // Nothing lands in the global vendors area anymore.
    assert!(!sb.spm_home.join("vendors/copilot").exists());

    // The managed dir is gitignored (with an explanatory comment) so the copied
    // skills are never committed.
    let gitignore = sb.read(".gitignore");
    assert!(
        gitignore.contains(".agents/skills/spm-managed-skills/"),
        "{gitignore}"
    );
    assert!(gitignore.contains("spm-managed"), "{gitignore}");
}

#[test]
fn gemini_add_materializes_project_local_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "gemini"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);

    // Skills are copied one level deep under the tool-native .gemini/skills dir.
    let skill_md = sb.project.join(".gemini/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());
    // The shared skills dir is not nested under an spm-owned subdir.
    assert!(!sb
        .project
        .join(".gemini/skills/spm-managed-skills")
        .exists());

    // Only the spm-managed skill subdir is gitignored (not the whole shared dir,
    // which may hold the user's own committed skills), with a comment.
    let gitignore = sb.read(".gitignore");
    assert!(gitignore.contains(".gemini/skills/greet/"), "{gitignore}");
    assert!(gitignore.contains("spm-managed Gemini"), "{gitignore}");
}

#[test]
fn codex_add_materializes_agents_skills() {
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

    // Codex reads the cross-tool `.agents/skills` alias one level deep.
    let skill_md = sb.project.join(".agents/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());

    // Only the spm-managed skill subdir is gitignored (not the whole shared dir),
    // with a comment.
    let gitignore = sb.read(".gitignore");
    assert!(gitignore.contains(".agents/skills/greet/"), "{gitignore}");
    assert!(gitignore.contains("spm-managed Codex"), "{gitignore}");
}

#[test]
fn cursor_add_materializes_cursor_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "cursor"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);

    // Cursor auto-discovers skills one level deep under its native .cursor/skills dir.
    let skill_md = sb.project.join(".cursor/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());
    // The shared skills dir is not nested under an spm-owned subdir.
    assert!(!sb
        .project
        .join(".cursor/skills/spm-managed-skills")
        .exists());

    // Only the spm-managed skill subdir is gitignored (not the whole shared dir,
    // which may hold the user's own committed skills), with a comment.
    let gitignore = sb.read(".gitignore");
    assert!(gitignore.contains(".cursor/skills/greet/"), "{gitignore}");
    assert!(gitignore.contains("spm-managed Cursor"), "{gitignore}");
}

#[test]
fn cline_add_materializes_cline_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "cline"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);

    // Cline auto-discovers skills one level deep under its native .cline/skills dir.
    let skill_md = sb.project.join(".cline/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());
    assert!(!sb.project.join(".cline/skills/spm-managed-skills").exists());

    let gitignore = sb.read(".gitignore");
    assert!(gitignore.contains(".cline/skills/greet/"), "{gitignore}");
    assert!(gitignore.contains("spm-managed Cline"), "{gitignore}");
}

#[test]
fn windsurf_add_materializes_windsurf_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "windsurf"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);

    // Windsurf (Cascade) auto-discovers skills one level deep under .windsurf/skills.
    let skill_md = sb.project.join(".windsurf/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());

    let gitignore = sb.read(".gitignore");
    assert!(gitignore.contains(".windsurf/skills/greet/"), "{gitignore}");
    assert!(gitignore.contains("spm-managed Windsurf"), "{gitignore}");
}

#[test]
fn amp_add_materializes_agents_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "amp"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);

    // Amp installs into the cross-tool `.agents/skills` alias one level deep.
    let skill_md = sb.project.join(".agents/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());

    let gitignore = sb.read(".gitignore");
    assert!(gitignore.contains(".agents/skills/greet/"), "{gitignore}");
    assert!(gitignore.contains("spm-managed Amp"), "{gitignore}");
}

#[test]
fn add_without_name_keys_manifest_by_path_basename() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.ok(&["init", "--target", "claude"]);

    // No `--name`: the manifest key must come from the `--path` basename
    // (`alpha`), not the git URL basename (`skill`).
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pack/alpha",
    ]);

    let manifest = sb.read("ai.json");
    assert!(
        manifest.contains("\"alpha\""),
        "manifest should be keyed by path basename `alpha`: {manifest}"
    );
    assert!(
        !manifest.contains("\"skill\""),
        "manifest must not be keyed by the git URL basename `skill`: {manifest}"
    );
}

#[test]
fn add_with_dot_path_falls_back_to_git_url_basename() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);

    // `--path .` yields "." which is not a valid skill name, so the default
    // must fall back to the git URL basename (`skill`) instead of erroring.
    sb.ok(&["add", &sb.skill_url(), "--branch", "main", "--path", "."]);

    let manifest = sb.read("ai.json");
    assert!(
        manifest.contains("\"skill\""),
        "manifest should fall back to git URL basename `skill`: {manifest}"
    );
}

#[test]
fn remove_prunes_skill() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    sb.ok(&["remove", "greet"]);

    assert!(!sb.read("ai.json").contains("greet"));
    let skills_dir = sb.claude_market_dir().join("plugin/skills");
    assert!(std::fs::read_dir(&skills_dir).unwrap().next().is_none());
}

#[test]
fn rm_alias_behaves_like_remove() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    sb.ok(&["rm", "greet"]);

    assert!(!sb.read("ai.json").contains("greet"));
    let skills_dir = sb.claude_market_dir().join("plugin/skills");
    assert!(std::fs::read_dir(&skills_dir).unwrap().next().is_none());
}

#[test]
fn i_alias_behaves_like_install() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let lock_before = sb.read("ai.lock");

    assert_eq!(sb.ok(&["i"]), sb.ok(&["install"]));
    assert_eq!(lock_before, sb.read("ai.lock"));
}

#[test]
fn ls_alias_behaves_like_list() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    let out = sb.ok(&["ls"]);
    assert!(out.contains("greet"), "{out}");
    assert_eq!(out, sb.ok(&["list"]));
}

#[cfg(unix)]
#[test]
fn aliases_accept_the_global_flag() {
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

    let listed = sb.ok(&["ls", "-g"]);
    assert!(listed.contains("greet"), "{listed}");
    assert_eq!(listed, sb.ok(&["list", "-g"]));

    assert_eq!(sb.ok(&["i", "-g"]), sb.ok(&["install", "-g"]));

    sb.ok(&["rm", "-g", "greet"]);
    assert!(!sb.copilot_global_skills().join("greet").exists());
}

#[test]
fn aliases_are_shown_in_help() {
    let sb = Sandbox::new();
    let help = sb.ok(&["--help"]);
    for (cmd, alias) in [("install", "i"), ("remove", "rm"), ("list", "ls")] {
        let row = help
            .lines()
            .find(|l| l.trim_start().starts_with(cmd))
            .unwrap_or_else(|| panic!("no `{cmd}` row in help: {help}"));
        assert!(
            row.contains(&format!("[alias: {alias}]")),
            "`{cmd}` row should advertise `{alias}`: {row}"
        );
    }
}

#[test]
fn install_is_idempotent_from_lock() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let lock_before = sb.read("ai.lock");

    sb.ok(&["install"]);
    let lock_after = sb.read("ai.lock");
    assert_eq!(lock_before, lock_after, "install must not change the lock");
}

#[test]
fn clean_removes_generated_config() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    sb.ok(&["clean"]);

    let settings = sb.read(".claude/settings.local.json");
    assert!(!settings.contains("spm@spm"), "{settings}");
    // The project-local marketplace dir is removed after clean.
    assert!(
        !sb.claude_market_dir().exists(),
        "marketplace dir should be gone after clean"
    );
    // `.gitignore` is left untouched by clean: the entry spm added stays put
    // rather than risk clobbering a user-owned file.
    let gitignore = sb.project.join(".gitignore");
    assert!(
        gitignore.exists(),
        ".gitignore must not be removed by clean"
    );
    let gi = std::fs::read_to_string(&gitignore).unwrap();
    assert!(gi.contains(".spm/"), "{gi}");
}

#[test]
fn multi_target_wires_both_vendors() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    assert!(sb
        .claude_market_dir()
        .join("plugin/skills/greet/SKILL.md")
        .exists());
    assert!(sb
        .project
        .join(".agents/skills/spm-managed-skills/greet/SKILL.md")
        .exists());
    assert!(sb.read(".claude/settings.local.json").contains("spm@spm"));
}

#[test]
fn init_is_idempotent_and_preserves_manifest() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    // Mutate the manifest so we can prove the second init leaves it untouched.
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let before = sb.read("ai.json");

    // Re-running init succeeds (no longer an error) with a different target,
    // and must not overwrite the existing manifest.
    let out = sb.ok(&["init", "--target", "copilot"]);
    assert!(out.contains("already exists"), "{out}");
    assert_eq!(
        sb.read("ai.json"),
        before,
        "init must not touch the manifest"
    );

    // A bogus target is still rejected even when the project is already
    // initialized — the idempotent early-return must not swallow typos.
    let bad = sb.spm(&["init", "--target", "nonsense"]);
    assert!(
        !bad.status.success(),
        "bogus target must fail even when ai.json exists"
    );
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown target"));
    assert_eq!(
        sb.read("ai.json"),
        before,
        "rejected init must not touch the manifest"
    );
}

#[test]
fn unknown_target_is_rejected() {
    let sb = Sandbox::new();
    let out = sb.spm(&["init", "--target", "nonsense"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown target"));
}

/// Every command that loads the manifest, run with no `ai.json` present.
fn manifest_loading_commands(sb: &Sandbox) -> Vec<Vec<String>> {
    let url = sb.skill_url();
    [
        vec!["list"],
        vec!["status"],
        vec!["install"],
        vec!["update"],
        vec!["clean"],
        vec!["remove", "greet"],
        vec!["add", &url, "--tag", "v0.1.0"],
    ]
    .into_iter()
    .map(|args| args.into_iter().map(String::from).collect())
    .collect()
}

#[test]
fn missing_manifest_hints_spm_init() {
    let sb = Sandbox::new();
    let mut commands = manifest_loading_commands(&sb);
    commands.push(vec!["target".into(), "add".into(), "claude".into()]);
    for args in commands {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = sb.spm(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "spm {args:?}: {err}");
        // The original message is preserved for scripts that match on it.
        assert!(
            err.starts_with("error: no ai.json found at "),
            "spm {args:?}: {err}"
        );
        assert!(
            err.trim_end()
                .ends_with("\nhint: run `spm init` to create one"),
            "spm {args:?}: {err}"
        );
    }
}

#[test]
fn missing_global_manifest_hints_spm_init_global() {
    let sb = Sandbox::new();
    for args in manifest_loading_commands(&sb) {
        let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
        args.push("--global");
        let out = sb.spm(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "spm {args:?}: {err}");
        assert!(
            err.starts_with("error: no ai.json found at "),
            "spm {args:?}: {err}"
        );
        assert!(
            err.trim_end()
                .ends_with("\nhint: run `spm init --global` to create one"),
            "spm {args:?}: {err}"
        );
    }
}

#[test]
fn malformed_manifest_does_not_hint_spm_init() {
    let sb = Sandbox::new();
    std::fs::write(sb.project.join("ai.json"), "{ not json").unwrap();
    let out = sb.spm(&["list"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("parsing "), "{err}");
    assert!(!err.contains("hint"), "{err}");
    assert!(!err.contains("spm init"), "{err}");
}

#[test]
fn schema_invalid_manifest_does_not_hint_spm_init() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["bogus"],"skills":{}}"#,
    )
    .unwrap();
    let out = sb.spm(&["list"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("does not match schema"), "{err}");
    assert!(!err.contains("hint"), "{err}");
}

/// A manifest that exists but cannot be read (here: `ai.json` is a directory,
/// which fails with a non-`NotFound` error on every platform and, unlike a
/// chmod, also fails when the tests run as root) must not suggest `spm init`.
#[test]
fn unreadable_manifest_does_not_hint_spm_init() {
    let sb = Sandbox::new();
    std::fs::create_dir(sb.project.join("ai.json")).unwrap();
    let out = sb.spm(&["list"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("no ai.json found at "), "{err}");
    assert!(!err.contains("hint"), "{err}");
}

#[test]
fn schema_rejects_unknown_target_value() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["bogus"],"skills":{}}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not match schema"), "{err}");
}

#[test]
fn schema_rejects_skill_without_version_selector() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["claude"],"skills":{"x":{"git":"u"}}}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("oneOf"));
}

#[test]
fn rejects_path_traversal_in_skill_path() {
    let sb = Sandbox::new();
    // A hostile `path` escaping the fetched repo must be refused, not fetched.
    // Rejection happens at manifest load, before any git URL is touched.
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["claude"],"skills":{"evil":{"git":"u","tag":"v0.1.0","path":"../../../../../../etc"}}}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "traversal path must be rejected");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`..`"), "{err}");
}

#[test]
fn rejects_absolute_skill_path() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["claude"],"skills":{"evil":{"git":"u","tag":"v0.1.0","path":"/etc"}}}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "absolute path must be rejected");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("relative to the repo root"), "{err}");
}

#[test]
fn rejects_path_traversal_in_skill_name() {
    let sb = Sandbox::new();
    // A skill name containing path separators would let the vendor write outside
    // its skills/ directory. Reject it at the manifest layer.
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["claude"],"skills":{"../../evil":{"git":"u","tag":"v0.1.0"}}}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "traversal name must be rejected");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("invalid skill name"), "{err}");
}

#[test]
fn rejects_traversal_name_on_add() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--tag",
        "v0.1.0",
        "--name",
        "../escape",
    ]);
    assert!(!out.status.success(), "add must reject a traversal name");
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid skill name"));
    // A rejected add must not leave the bad name persisted in ai.json.
    assert!(!sb.read("ai.json").contains("escape"));
}

#[test]
fn rejects_forged_absolute_store_in_lock() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["claude"],"skills":{}}"#,
    )
    .unwrap();
    // A committed ai.lock is untrusted: an absolute `store` must never be acted on.
    std::fs::write(
        sb.project.join("ai.lock"),
        r#"{"id":"spm-deadbeef","skills":{"evil":{"git":"u","reference":"branch:main","commit":"0000000000000000000000000000000000000000","store":"/home/victim/.config/autostart"}}}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "forged store must be rejected");
    assert!(String::from_utf8_lossy(&out.stderr).contains("store key"));
}

#[test]
fn rejects_forged_project_id_in_lock() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["copilot"],"skills":{}}"#,
    )
    .unwrap();
    std::fs::write(
        sb.project.join("ai.lock"),
        r#"{"id":"../../evil","skills":{}}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "forged id must be rejected");
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid project id"));
}

#[test]
fn schema_rejects_abbreviated_commit() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["claude"],"skills":{"x":{"git":"u","commit":"abc1234"}}}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "abbreviated commit must be rejected");
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not match schema"));
}

#[test]
fn copilot_clean_removes_project_local_dir_but_keeps_gitignore_entry() {
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

    let managed = sb.project.join(".agents/skills/spm-managed-skills");
    assert!(
        managed.exists(),
        "skills should be materialized before clean"
    );
    assert!(sb
        .read(".gitignore")
        .contains(".agents/skills/spm-managed-skills/"));

    sb.ok(&["clean"]);

    assert!(
        !managed.exists(),
        "clean must remove the managed skills dir"
    );
    assert!(
        sb.read(".gitignore")
            .contains(".agents/skills/spm-managed-skills/"),
        "clean must leave the gitignore entry untouched"
    );
}

#[test]
fn uppercase_commit_pin_is_not_refetched() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    // Pin the real HEAD as an UPPERCASE 40-hex SHA. It must resolve, and a second
    // install must reuse the cached checkout rather than deleting and refetching.
    let head = skill_head(&sb.skill_repo).to_uppercase();
    sb.ok(&["add", &sb.skill_url(), "--commit", &head, "--name", "greet"]);
    let second = sb.ok(&["install"]);
    assert!(
        second.contains("cached") && !second.contains("fetched"),
        "second install should be cached, got: {second}"
    );
    // The lock stores the normalized lowercase SHA.
    assert!(sb.read("ai.lock").contains(&head.to_lowercase()));
}

#[test]
fn container_path_warns_once_and_suggests_subskills() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.ok(&["init", "--target", "claude,copilot"]);

    // `pack/` is a container of skills (alpha, beta), not a skill itself. The add
    // still succeeds (warning only), but must guide the user to the sub-skills.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pack",
        "--name",
        "pack",
    ]);
    assert!(
        out.status.success(),
        "add should succeed with a warning: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // Even with two vendors configured, the check runs once.
    assert_eq!(
        err.matches("has no SKILL.md at its root").count(),
        1,
        "warning must be emitted once, not per-vendor: {err}"
    );
    // Sorted, copy-pasteable suggestions for each discovered sub-skill — and
    // they must carry the version selector so they run as-is.
    assert!(
        err.contains("--branch main --path pack/alpha --name alpha"),
        "should suggest a runnable alpha command with selector: {err}"
    );
    assert!(
        err.contains("--branch main --path pack/beta --name beta"),
        "should suggest a runnable beta command with selector: {err}"
    );
}

#[test]
fn missing_skill_md_without_subskills_warns_once_generically() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.ok(&["init", "--target", "claude,copilot"]);

    // `bare/` has no SKILL.md and no sub-skills: a single generic warning.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "bare",
        "--name",
        "bare",
    ]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.matches("has no SKILL.md at its root").count(),
        1,
        "generic warning must be emitted once: {err}"
    );
    assert!(err.contains("agents may ignore it"), "{err}");
    assert!(
        !err.contains("Did you mean"),
        "must not offer sub-skill suggestions when there are none: {err}"
    );
}

#[cfg(unix)]
#[test]
fn in_checkout_symlinked_skill_md_is_materialized() {
    let sb = Sandbox::new();
    sb.add_symlinked_skill();
    sb.ok(&["init", "--target", "copilot"]);

    // `linked/SKILL.md` is a symlink to a sibling file inside the same checkout,
    // so copy_tree now follows it: the SKILL.md is materialized and no warning
    // fires.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "linked",
        "--name",
        "linked",
    ]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("has no SKILL.md at its root"),
        "in-checkout symlinked SKILL.md must be treated as present: {err}"
    );
    let materialized = sb
        .project
        .join(".agents/skills/spm-managed-skills/linked/SKILL.md");
    assert!(
        materialized.exists(),
        "symlinked SKILL.md must be materialized"
    );
    assert_eq!(
        std::fs::read_to_string(&materialized).unwrap(),
        "---\nname: linked\n---\n"
    );
}

#[cfg(unix)]
#[test]
fn escaping_symlinked_skill_md_is_skipped_and_warns() {
    let sb = Sandbox::new();
    sb.add_escaping_symlinked_skill();
    sb.ok(&["init", "--target", "copilot"]);

    // `escaping/SKILL.md` points outside the store checkout: copy_tree must not
    // follow it. The vendor dir ends up with no SKILL.md (so spm warns) and the
    // secret never lands there — the exfiltration boundary is preserved.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "escaping",
        "--name",
        "escaping",
    ]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("has no SKILL.md at its root"),
        "escaping symlinked SKILL.md must still warn: {err}"
    );
    let materialized = sb
        .project
        .join(".agents/skills/spm-managed-skills/escaping/SKILL.md");
    assert!(
        !materialized.exists(),
        "an escaping symlink must never be materialized"
    );
}

#[test]
fn status_reports_materialized_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // With everything installed, `spm status` succeeds and reports the skill as
    // present for both targets — no MISSING markers.
    let out = sb.ok(&["status"]);
    assert!(out.contains("greet"), "{out}");
    assert!(out.contains("claude"), "{out}");
    assert!(out.contains("copilot"), "{out}");
    assert!(!out.contains("MISSING"), "nothing should be missing: {out}");
}

/// A skill declared directly in `ai.json` is a direct dependency even if a
/// stale or hand-edited `ai.lock` leaves a non-empty `requested_by` on its
/// entry (lock validation permits that). `spm status` must not mislabel it
/// `transitive; via ...` — matching `spm list`, which excludes manifest skills
/// from the transitive section.
#[test]
fn status_does_not_label_a_direct_skill_as_transitive() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // Hand-edit the lock to smuggle a requester onto the direct `greet` entry.
    let mut lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    lock["skills"]["greet"]["requested_by"] = serde_json::json!(["phantom-parent"]);
    std::fs::write(
        sb.project.join("ai.lock"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    let out = sb.ok(&["status"]);
    assert!(out.contains("greet"), "{out}");
    assert!(
        !out.contains("transitive; via"),
        "a direct manifest skill must not be labeled transitive: {out}"
    );
}

#[test]
fn status_flags_uninstalled_worktree() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // Simulate a fresh worktree/clone: ai.json + ai.lock are committed and
    // present, but the gitignored materialized skills are absent.
    std::fs::remove_dir_all(sb.project.join(".agents")).unwrap();

    let out = sb.spm(&["status"]);
    assert!(
        !out.status.success(),
        "status must fail when declared skills are not materialized here"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("MISSING"),
        "should mark greet MISSING: {text}"
    );
    assert!(
        text.contains("spm install"),
        "should tell the user to run `spm install` here: {text}"
    );
}

#[test]
fn status_succeeds_when_no_skills_are_declared() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);

    let out = sb.ok(&["status"]);
    assert!(out.contains("no skills or plugins declared"), "{out}");
    assert!(
        !out.contains("all declared skills are materialized"),
        "must not claim materialization when nothing is declared: {out}"
    );
}

#[test]
fn status_fails_when_declared_skills_are_not_locked() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // ai.json still declares `greet`, but ai.lock is gone — resolution never
    // happened here, so status must not report a green "all materialized".
    std::fs::remove_file(sb.project.join("ai.lock")).unwrap();

    let out = sb.spm(&["status"]);
    assert!(
        !out.status.success(),
        "status must fail when ai.json declares skills that ai.lock does not"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("ai.lock has none"), "{text}");
    assert!(text.contains("spm install"), "{text}");
}

#[test]
fn status_flags_claude_pointer_to_other_checkout() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // Rewrite the settings pointer to a different absolute path, mimicking a
    // worktree that inherited the main checkout's registration (issue #28).
    // Mutate the JSON structurally: a textual replace would not match on Windows,
    // where the serialized path has its backslashes JSON-escaped.
    let sp = sb.project.join(".claude/settings.local.json");
    let other = sb
        .root
        .join("some-other-checkout")
        .join(".spm")
        .join("claude");
    let mut settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    settings["extraKnownMarketplaces"]["spm"]["source"]["path"] =
        serde_json::Value::String(other.to_string_lossy().into_owned());
    std::fs::write(&sp, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

    let out = sb.spm(&["status"]);
    assert!(
        !out.status.success(),
        "status must fail when the Claude marketplace points at another checkout"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("some-other-checkout"),
        "should surface the mismatched registered path: {text}"
    );
    assert!(
        text.contains("spm install"),
        "should tell the user to run `spm install` here: {text}"
    );
}

#[test]
fn prune_wipes_the_global_store() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    assert!(sb.store_entries() > 0, "add should populate the store");

    let out = sb.ok(&["prune", "--yes"]);
    assert!(out.contains("pruned"), "{out}");
    assert_eq!(sb.store_entries(), 0, "store should be empty after prune");

    // The store is a pure cache: install re-fetches everything on demand.
    sb.ok(&["install"]);
    assert!(
        sb.store_entries() > 0,
        "install should re-populate the store after a prune"
    );
}

#[test]
fn prune_ignores_stray_non_directory_entries() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    // A stray file at the store root (e.g. a macOS `.DS_Store`) must not be
    // counted as a cached checkout — only the one real checkout directory is.
    std::fs::write(sb.spm_home.join("store").join(".DS_Store"), b"junk").unwrap();

    let out = sb.ok(&["prune", "--yes"]);
    assert!(out.contains("1 cached checkout"), "{out}");
    assert_eq!(sb.store_entries(), 0);
}

#[test]
fn prune_removes_a_store_holding_only_stray_files() {
    let sb = Sandbox::new();
    // A store root with no checkout directories, only a stray file. prune must
    // NOT report it as empty, and must remove the file (honors "everything").
    let store = sb.spm_home.join("store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join(".DS_Store"), b"junk").unwrap();

    let out = sb.ok(&["prune", "--yes"]);
    assert!(!out.contains("nothing to prune"), "{out}");
    assert!(out.contains("pruned"), "{out}");
    assert!(
        !store.join(".DS_Store").exists(),
        "stray file must be removed"
    );
    assert_eq!(sb.store_entries(), 0);
}

#[test]
fn prune_empty_store_is_a_noop() {
    let sb = Sandbox::new();
    let out = sb.ok(&["prune", "--yes"]);
    assert!(out.contains("nothing to prune"), "{out}");
    assert_eq!(sb.store_entries(), 0);
}

#[test]
fn prune_prompt_aborts_on_negative_answer() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let before = sb.store_entries();
    assert!(before > 0);

    let out = sb.spm_stdin(&["prune"], "n\n");
    assert!(out.status.success(), "abort is a success, not an error");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("aborted"), "{stdout}");
    assert_eq!(
        sb.store_entries(),
        before,
        "store must be untouched when the user declines"
    );
}

#[test]
fn list_reports_declared_skills_and_pins() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);

    // No skills declared yet.
    let out = sb.ok(&["list"]);
    assert!(out.contains("no skills or plugins declared"), "{out}");

    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let out = sb.ok(&["list"]);
    assert!(out.contains("greet"), "{out}");
    assert!(out.contains(&sb.skill_url()), "{out}");
    assert!(out.contains("tag:v0.1.0"), "{out}");
}

#[test]
fn update_with_no_name_refreshes_all_and_with_name_refreshes_one() {
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

    // `update` with no name re-resolves every branch/tag skill.
    let out = sb.ok(&["update"]);
    assert!(out.contains("updated"), "{out}");

    // `update <name>` refreshes just that one skill; an unknown name is
    // simply a no-op filter (nothing matches `only`), not an error.
    let out = sb.ok(&["update", "greet"]);
    assert!(out.contains("updated"), "{out}");
    let out = sb.ok(&["update", "does-not-exist"]);
    assert!(out.contains("updated"), "{out}");
}

#[test]
fn remove_unknown_skill_errors() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let out = sb.spm(&["remove", "nope"]);
    assert!(!out.status.success(), "removing an unknown skill must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no skill named `nope`"), "{err}");
}

#[test]
fn target_add_interactive_rejects_empty_input() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    // Hitting enter with nothing typed must fail cleanly, not hang or panic.
    let out = sb.spm_stdin(&["target", "add"], "\n");
    assert!(!out.status.success(), "empty selection must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no target selected"), "{err}");
    assert!(!sb.read("ai.json").contains("copilot"));
}

#[test]
fn target_add_interactive_accepts_all() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    // `all` picks every not-yet-configured vendor (here amp, cline, codex,
    // copilot, cursor, gemini, windsurf).
    let out = sb.spm_stdin(&["target", "add"], "all\n");
    assert!(
        out.status.success(),
        "`all` must be accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("added target(s):"), "{stdout}");
    for vendor in [
        "amp", "cline", "codex", "copilot", "cursor", "gemini", "windsurf",
    ] {
        assert!(
            stdout.contains(vendor),
            "picker `all` must add {vendor}: {stdout}"
        );
        assert!(
            sb.read("ai.json").contains(&format!("\"{vendor}\"")),
            "ai.json must contain {vendor}"
        );
    }
}

#[test]
fn status_flags_stale_materialized_skill_not_in_lock() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // Drop an extra, undeclared directory into the materialized skills dir —
    // simulating a skill that was removed from ai.json/ai.lock but whose
    // on-disk copy was never cleaned up some other way.
    let stray = sb
        .project
        .join(".agents/skills/spm-managed-skills/leftover");
    std::fs::create_dir_all(&stray).unwrap();

    let out = sb.ok(&["status"]);
    assert!(
        out.contains("leftover") && out.contains("stale (not in ai.lock)"),
        "{out}"
    );
}

#[test]
fn claude_clean_is_a_noop_when_nothing_was_ever_materialized() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);

    // Nothing was ever `add`ed: clean must succeed without touching anything
    // that was never created.
    let out = sb.ok(&["clean"]);
    assert!(out.contains("cleaned"), "{out}");
    assert!(!sb.claude_market_dir().exists());
    assert!(!sb.project.join(".claude/settings.local.json").exists());
}

#[test]
fn claude_status_flags_missing_marketplace_registration() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // Simulate a checkout where the marketplace pointer was never (re-)written
    // here — e.g. a gitignored settings.local.json that got wiped some other
    // way — while the materialized plugin dir is left in place.
    std::fs::remove_file(sb.project.join(".claude/settings.local.json")).unwrap();

    let out = sb.spm(&["status"]);
    assert!(!out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("no spm marketplace registered"), "{text}");
}

#[test]
fn copilot_second_add_rebuilds_managed_dir_from_scratch() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    assert!(sb
        .project
        .join(".agents/skills/spm-managed-skills/greet/SKILL.md")
        .exists());

    // A second `add` re-runs `sync`, which re-materializes for copilot into a
    // managed dir that already exists from the first add — exercising the
    // "rebuild from scratch" removal path, not just first-time creation.
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet2",
    ]);
    assert!(sb
        .project
        .join(".agents/skills/spm-managed-skills/greet/SKILL.md")
        .exists());
    assert!(sb
        .project
        .join(".agents/skills/spm-managed-skills/greet2/SKILL.md")
        .exists());
}

#[test]
fn add_path_pointing_at_a_plain_file_warns_generically() {
    let sb = Sandbox::new();
    // A top-level plain file (not a directory) at the given --path: SKILL.md
    // can't possibly live "at its root" and there's nothing to recurse into
    // for sub-skill detection, so `child_skills` must handle `read_dir` on a
    // non-directory gracefully (empty result, no sub-skill suggestions) rather
    // than propagating an error from the check itself. The overall `add` still
    // fails later when it tries to materialize a *file* as a skill directory —
    // that's expected; what this test guards is the no-SKILL.md warning path.
    std::fs::write(sb.skill_repo.join("notes.txt"), "not a skill").unwrap();
    sb.git(&["add", "-A"]);
    sb.git(&["commit", "-qm", "add plain file"]);
    sb.ok(&["init", "--target", "copilot"]);

    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "notes.txt",
        "--name",
        "notes",
    ]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("has no SKILL.md at its root"), "{err}");
    assert!(
        !err.contains("Did you mean"),
        "a plain file has no sub-skills to suggest: {err}"
    );
}

#[test]
fn prune_prompt_removes_on_yes() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    assert!(sb.store_entries() > 0);

    let out = sb.spm_stdin(&["prune"], "y\n");
    assert!(out.status.success());
    assert_eq!(
        sb.store_entries(),
        0,
        "store should be empty after a confirmed prune"
    );
}

#[test]
fn claude_settings_with_non_object_entries_fail_cleanly() {
    // A hand-edited settings file whose root, or a key spm merges into, is not
    // a JSON object must be reported as an error, never panic.
    for settings in [r#"{"enabledPlugins": "oops"}"#, "[]"] {
        let sb = Sandbox::new();
        sb.ok(&["init", "--target", "claude"]);
        std::fs::create_dir_all(sb.project.join(".claude")).unwrap();
        std::fs::write(sb.project.join(".claude/settings.local.json"), settings).unwrap();

        let out = sb.spm(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
        let err = stderr_of(&out);
        assert_eq!(out.status.code(), Some(1), "{settings}: {err}");
        assert!(!err.contains("panicked"), "{settings}: {err}");
        assert!(err.contains("settings.local.json"), "{settings}: {err}");
    }
}
