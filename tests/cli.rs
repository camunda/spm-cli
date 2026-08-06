//! End-to-end tests that drive the real `spm` binary against throwaway git repos.
//! Each test gets an isolated scratch dir and its own `SPM_HOME`, so the global
//! store/vendor areas never leak between tests or into the developer's home.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A sandbox: unique scratch root holding a fake skill repo, a project dir, and
/// an isolated SPM_HOME.
struct Sandbox {
    root: PathBuf,
    skill_repo: PathBuf,
    project: PathBuf,
    spm_home: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("spm-test-{}-{nanos}-{n}", std::process::id()));
        let sb = Sandbox {
            skill_repo: root.join("skill"),
            project: root.join("project"),
            spm_home: root.join("home"),
            root,
        };
        std::fs::create_dir_all(&sb.project).unwrap();
        sb.init_skill_repo();
        sb
    }

    /// Build a git repo containing one skill, an annotated tag `v0.1.0`, and a
    /// `main` branch — exercising both tag (with `^{}` deref) and branch resolution.
    fn init_skill_repo(&self) {
        std::fs::create_dir_all(&self.skill_repo).unwrap();
        std::fs::write(
            self.skill_repo.join("SKILL.md"),
            "---\nname: greet\ndescription: Say hello nicely.\n---\nGreet warmly.\n",
        )
        .unwrap();
        self.git(&["init", "-q", "-b", "main"]);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", "initial"]);
        self.git(&["tag", "-a", "v0.1.0", "-m", "v0.1.0"]);
    }

    /// Run `git <args>` in the skill repo with a pinned, hermetic identity
    /// (signing disabled) so the harness is independent of the developer's
    /// global git config.
    fn git(&self, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgSign=false",
            ])
            .args(args)
            .current_dir(&self.skill_repo)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// Add, on `main`, a `pack/` directory that is a *container* of skills
    /// (`pack/alpha/SKILL.md`, `pack/beta/SKILL.md`) with no `SKILL.md` at its
    /// own root, plus a `bare/` directory with no skill at all. Used to exercise
    /// the container-detection and generic no-SKILL.md warnings.
    fn add_skill_pack(&self) {
        for sub in ["alpha", "beta"] {
            let dir = self.skill_repo.join("pack").join(sub);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("---\nname: {sub}\n---\n")).unwrap();
        }
        let bare = self.skill_repo.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(bare.join("README.md"), "not a skill\n").unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", "add pack"]);
    }

    /// Add, on `main`, a `linked/` skill dir whose `SKILL.md` is a **symlink**
    /// (to a sibling regular file). Since `copy_tree` skips symlinks, the
    /// materialized skill ends up with no `SKILL.md`, so spm must still warn.
    #[cfg(unix)]
    fn add_symlinked_skill(&self) {
        let dir = self.skill_repo.join("linked");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("real.md"), "---\nname: linked\n---\n").unwrap();
        std::os::unix::fs::symlink("real.md", dir.join("SKILL.md")).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", "add symlinked skill"]);
    }

    fn skill_url(&self) -> String {
        format!("file://{}", self.skill_repo.display())
    }

    /// Run `spm <args>` in the project dir with the sandboxed SPM_HOME.
    fn spm(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_spm"))
            .args(args)
            .current_dir(&self.project)
            .env("SPM_HOME", &self.spm_home)
            .output()
            .unwrap()
    }

    /// Run `spm <args>` feeding `input` on stdin — drives the interactive
    /// target picker (a plain line read, so piped input works without a tty).
    fn spm_stdin(&self, args: &[&str], input: &str) -> std::process::Output {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_spm"))
            .args(args)
            .current_dir(&self.project)
            .env("SPM_HOME", &self.spm_home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Tolerate ONLY BrokenPipe: a command that short-circuits before reading
        // stdin (e.g. "all targets already configured") closes the pipe first.
        // Any other write error is a real harness failure and must surface, not
        // be masked.
        if let Err(e) = child.stdin.take().unwrap().write_all(input.as_bytes()) {
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe,
                "unexpected stdin write error: {e}"
            );
        }
        child.wait_with_output().unwrap()
    }

    /// Run `spm`, asserting success and returning stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.spm(args);
        assert!(
            out.status.success(),
            "spm {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.project.join(rel)).unwrap()
    }

    /// The generated Claude marketplace dir, now project-local (gitignored).
    fn claude_market_dir(&self) -> PathBuf {
        self.project.join(".spm/claude")
    }

    /// Number of top-level entries in the sandboxed global store. A missing
    /// store reads as empty — the state after a `prune`. Any other error (an
    /// unreadable path, or a non-directory where the store should be) is a real
    /// harness fault and panics loudly rather than masquerading as an empty
    /// store and letting an assertion pass on a misleading `0`.
    fn store_entries(&self) -> usize {
        let dir = self.spm_home.join("store");
        match std::fs::read_dir(&dir) {
            Ok(rd) => rd.count(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => panic!("reading store dir {}: {e}", dir.display()),
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn skill_head(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

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
    // The gitignore block spm added is removed too.
    let gitignore = sb.project.join(".gitignore");
    if gitignore.exists() {
        let gi = std::fs::read_to_string(&gitignore).unwrap();
        assert!(!gi.contains(".spm/"), "{gi}");
    }
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
fn copilot_clean_removes_project_local_dir_and_gitignore_entry() {
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
        !sb.read(".gitignore")
            .contains(".agents/skills/spm-managed-skills/"),
        "clean must drop the gitignore entry"
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
fn symlinked_skill_md_still_warns() {
    let sb = Sandbox::new();
    sb.add_symlinked_skill();
    sb.ok(&["init", "--target", "copilot"]);

    // `linked/SKILL.md` is a symlink, which copy_tree skips — so the vendor dir
    // ends up with no SKILL.md. The check must not be fooled into silence by the
    // symlink resolving to a file in the store.
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
        err.contains("has no SKILL.md at its root"),
        "symlinked SKILL.md must still warn: {err}"
    );
    // And nothing landed in the materialized dir root.
    assert!(!sb
        .project
        .join(".agents/skills/spm-managed-skills/linked/SKILL.md")
        .exists());
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
    assert!(out.contains("no skills declared"), "{out}");
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
fn add_all_expands_container_into_one_entry_per_subskill() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.ok(&["init", "--target", "claude,copilot"]);

    // `--all` over the `pack/` container adds every sub-skill in one shot, each
    // as its own manifest entry keyed by the subdirectory name.
    let out = sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pack",
        "--all",
    ]);
    assert!(out.contains("alpha") && out.contains("beta"), "{out}");

    // ai.json gained one derived entry per sub-skill, each pinned under its own
    // path — and no bare `pack` container entry.
    let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    let skills = &manifest["skills"];
    assert_eq!(skills["alpha"]["path"], "pack/alpha", "{manifest}");
    assert_eq!(skills["beta"]["path"], "pack/beta", "{manifest}");
    assert_eq!(skills["alpha"]["branch"], "main", "{manifest}");
    assert!(
        skills.get("pack").is_none(),
        "no container entry: {manifest}"
    );

    // Both sub-skills are materialized for both vendors, with their SKILL.md.
    for name in ["alpha", "beta"] {
        assert!(
            sb.claude_market_dir()
                .join(format!("plugin/skills/{name}/SKILL.md"))
                .exists(),
            "claude missing {name}"
        );
        assert!(
            sb.project
                .join(format!(".agents/skills/spm-managed-skills/{name}/SKILL.md"))
                .exists(),
            "copilot missing {name}"
        );
    }
}

#[test]
fn add_all_rejects_collision_without_force_but_force_overwrites() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.ok(&["init", "--target", "claude"]);

    // Seed `alpha` as a standalone entry so the container batch collides on it.
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "alpha"]);

    // Without --force the whole batch is rejected on the collision.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pack",
        "--all",
    ]);
    assert!(!out.status.success(), "colliding --all must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");
    assert!(err.contains("--force"), "should suggest --force: {err}");

    // With --force the batch overwrites the colliding entry and completes.
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pack",
        "--all",
        "--force",
    ]);
    let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    // `alpha` is now the container-derived entry (pinned under its subpath),
    // not the original standalone tag.
    assert_eq!(
        manifest["skills"]["alpha"]["path"], "pack/alpha",
        "{manifest}"
    );
    assert_eq!(
        manifest["skills"]["beta"]["path"], "pack/beta",
        "{manifest}"
    );
}

#[test]
fn add_all_on_non_container_path_errors() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.ok(&["init", "--target", "copilot"]);

    // `bare/` has no SKILL.md and no sub-skills: `--all` has nothing to add and
    // must fail loudly rather than silently create zero entries.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "bare",
        "--all",
    ]);
    assert!(
        !out.status.success(),
        "add --all on a non-container must fail"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("not a container of skills"),
        "should explain the path is not a container: {err}"
    );
    // Nothing was written to the manifest.
    assert!(
        !sb.read("ai.json").contains("bare"),
        "no entry should be created on failure"
    );
}

#[test]
fn add_rejects_duplicate_name_without_force() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let before = sb.read("ai.json");

    // Re-adding the same name must fail loudly rather than silently clobber the
    // existing entry.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    assert!(!out.status.success(), "duplicate add must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");
    assert!(
        err.contains("--force"),
        "error should mention --force: {err}"
    );
    assert!(
        err.contains("--name"),
        "error should mention renaming with --name: {err}"
    );
    assert_eq!(
        sb.read("ai.json"),
        before,
        "a rejected add must not touch the manifest"
    );
}

#[test]
fn add_force_overwrites_existing_name() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    // --force re-pins the same name to a different selector.
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
        "--force",
    ]);
    let manifest = sb.read("ai.json");
    assert!(manifest.contains("\"branch\""), "{manifest}");
    assert!(
        !manifest.contains("v0.1.0"),
        "old selector should be gone: {manifest}"
    );
}

#[test]
fn load_rejects_duplicate_skill_keys() {
    let sb = Sandbox::new();
    // Hand-written manifest with two entries under the same skill name. A plain
    // map would silently keep the last; spm must reject it instead of dropping
    // the first without warning.
    std::fs::write(
        sb.project.join("ai.json"),
        r#"{"targets":["claude"],"skills":{
            "greet":{"git":"https://example.com/a.git","tag":"v1"},
            "greet":{"git":"https://example.com/b.git","tag":"v2"}
        }}"#,
    )
    .unwrap();
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "duplicate keys must be rejected");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("duplicate skill name `greet`"),
        "should name the duplicate: {err}"
    );
}

#[test]
fn add_all_rejects_name_flag() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.ok(&["init", "--target", "copilot"]);

    // `--name` names a single skill; with `--all` names are derived per
    // sub-skill, so combining them is a usage error caught by the CLI parser.
    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pack",
        "--all",
        "--name",
        "whatever",
    ]);
    assert!(!out.status.success(), "--all with --name must be rejected");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("cannot be used with") || err.contains("conflicts"),
        "should report the --all/--name conflict: {err}"
    );
}

#[test]
fn add_all_rejects_collision_with_existing_skill() {
    let sb = Sandbox::new();
    sb.add_skill_pack();
    sb.ok(&["init", "--target", "copilot"]);

    // Pre-declare a skill named `alpha` (a name the container also yields).
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pack/alpha",
        "--name",
        "alpha",
    ]);

    let out = sb.spm(&[
        "add",
        &sb.skill_url(),
        "--branch",
        "main",
        "--path",
        "pack",
        "--all",
    ]);
    assert!(
        !out.status.success(),
        "colliding sub-skill name must abort the whole batch"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");

    // The pre-existing `beta` was NOT partially added — the batch is atomic.
    assert!(
        !sb.read("ai.json").contains("beta"),
        "no partial insertion on collision: {}",
        sb.read("ai.json")
    );
}

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

#[test]
fn target_add_interactive_picks_unconfigured_vendor_from_list() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);

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
    sb.ok(&["init", "--target", "claude,copilot"]);

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
