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
        let g = |args: &[&str]| {
            let ok = Command::new("git")
                // Pin identity and disable signing so the harness is hermetic
                // regardless of the developer's global git config.
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
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["add", "-A"]);
        g(&["commit", "-qm", "initial"]);
        g(&["tag", "-a", "v0.1.0", "-m", "v0.1.0"]);
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
            // Stub the copilot CLI with a no-op so tests never touch (or require)
            // the real, user-global copilot installation.
            .env("SPM_COPILOT_BIN", "true")
            .output()
            .unwrap()
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

    /// The single generated marketplace/instruction dir for a vendor.
    fn vendor_dir(&self, vendor: &str) -> PathBuf {
        let parent = self.spm_home.join("vendors").join(vendor);
        std::fs::read_dir(&parent)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
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
    let skill_md = sb.vendor_dir("claude").join("plugin/skills/greet/SKILL.md");
    assert!(skill_md.exists(), "missing {}", skill_md.display());

    // Project pointer written to the gitignored local settings file.
    let settings = sb.read(".claude/settings.local.json");
    assert!(settings.contains("\"spm@spm\": true"), "{settings}");
    assert!(settings.contains("extraKnownMarketplaces"), "{settings}");

    // Nothing leaked into the project tree beyond ai.json/ai.lock/.claude.
    assert!(!sb.project.join("skills").exists());
}

#[test]
fn copilot_add_assembles_plugin_marketplace() {
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

    let dir = sb.vendor_dir("copilot");
    // Self-contained marketplace: manifest, plugin manifest, and the skill inside.
    let marketplace = dir.join(".github/plugin/marketplace.json");
    let manifest = std::fs::read_to_string(&marketplace).unwrap();
    assert!(manifest.contains("\"source\": \"plugin\""), "{manifest}");
    assert!(dir.join("plugin/plugin.json").exists());
    assert!(dir.join("plugin/skills/greet/SKILL.md").exists());
    // No VS Code instruction files anymore.
    assert!(!sb.project.join(".vscode").exists());

    // The marketplace dir is named by the stable, path-independent id in ai.lock
    // (so moved/re-cloned checkouts re-register the same entry, not a duplicate).
    let lock = sb.read("ai.lock");
    let id = dir.file_name().unwrap().to_string_lossy().to_string();
    assert!(id.starts_with("spm-"), "dir id: {id}");
    assert!(lock.contains(&format!("\"id\": \"{id}\"")), "{lock}");
}

#[test]
fn remove_prunes_skill() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    sb.ok(&["remove", "greet"]);

    assert!(!sb.read("ai.json").contains("greet"));
    let skills_dir = sb.vendor_dir("claude").join("plugin/skills");
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
    assert!(
        !sb.spm_home
            .join("vendors/claude")
            .read_dir()
            .is_ok_and(|mut d| d.next().is_some()),
        "vendor dir should be empty after clean"
    );
}

#[test]
fn multi_target_wires_both_vendors() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    assert!(sb
        .vendor_dir("claude")
        .join("plugin/skills/greet/SKILL.md")
        .exists());
    assert!(sb
        .vendor_dir("copilot")
        .join("plugin/skills/greet/SKILL.md")
        .exists());
    assert!(sb.read(".claude/settings.local.json").contains("spm@spm"));
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
