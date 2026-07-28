use super::{MaterializedSkill, Vendor};
use crate::{fsutil, jsonutil, paths};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::Path;
use std::process::Command;

/// GitHub Copilot CLI adapter.
///
/// Copilot CLI consumes skills through a plugin marketplace (`marketplace.json`
/// → `plugin.json` → `skills/<name>/SKILL.md`), the same shape as Claude. spm
/// assembles a self-contained marketplace in `~/.spm/vendors/copilot/<project>/`
/// (outside the repo) and registers it by shelling out to the `copilot` CLI.
///
/// Unlike Claude, Copilot CLI marketplaces/plugins are **user-global** and
/// command-driven — there is no project-local config file to write, so wiring
/// mutates global `copilot` state. A per-project marketplace/plugin id keeps
/// projects from colliding.
pub struct Copilot;

impl Vendor for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn materialize(&self, project_root: &Path, skills: &[MaterializedSkill]) -> Result<()> {
        let dir = paths::vendor_project_dir("copilot", project_root)?;
        let id = market_id(project_root);

        // No skills: tear down any prior registration and remove the dir.
        if skills.is_empty() {
            unregister(&id);
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
            }
            return Ok(());
        }

        // Rebuild the marketplace from scratch so removed skills disappear.
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        let plugin_dir = dir.join("plugin");
        let skills_dir = plugin_dir.join("skills");
        std::fs::create_dir_all(&skills_dir)?;

        for s in skills {
            let dest = skills_dir.join(&s.name);
            fsutil::copy_tree(&s.path, &dest)
                .with_context(|| format!("copying skill `{}` into plugin", s.name))?;
            if !dest.join("SKILL.md").exists() {
                eprintln!(
                    "warning: skill `{}` has no SKILL.md at its root — Copilot may ignore it",
                    s.name
                );
            }
        }

        jsonutil::write(&plugin_dir.join("plugin.json"), &json!({ "name": id }))?;
        jsonutil::write(
            &dir.join(".github/plugin/marketplace.json"),
            &json!({
                "name": id,
                "owner": { "name": "spm", "email": "spm@example.com" },
                "metadata": { "description": "spm-managed skills", "version": "0.0.0" },
                "plugins": [{
                    "name": id,
                    "description": "spm-managed skills",
                    "version": "0.0.0",
                    "source": "plugin"
                }],
            }),
        )?;

        register(&id, &dir)
    }

    fn clean(&self, project_root: &Path) -> Result<()> {
        let id = market_id(project_root);
        unregister(&id);
        let dir = paths::vendor_project_dir("copilot", project_root)?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

/// Register (or refresh) the generated marketplace with the copilot CLI.
fn register(id: &str, dir: &Path) -> Result<()> {
    if !copilot_available() {
        eprintln!(
            "warning: `copilot` CLI not found — skills assembled at {} but not registered.\n  \
             Register manually:\n    copilot plugin marketplace add {}\n    copilot plugin install {id}@{id}",
            dir.display(),
            dir.display()
        );
        return Ok(());
    }
    // Refresh: drop any prior registration, then re-add and install.
    run(&["plugin", "uninstall", id], true)?;
    run(&["plugin", "marketplace", "remove", id], true)?;
    run(
        &["plugin", "marketplace", "add", &dir.to_string_lossy()],
        false,
    )?;
    run(&["plugin", "install", &format!("{id}@{id}")], false)?;
    Ok(())
}

/// Best-effort teardown of a project's copilot registration (used by clean/empty).
fn unregister(id: &str) {
    if copilot_available() {
        let _ = run(&["plugin", "uninstall", id], true);
        let _ = run(&["plugin", "marketplace", "remove", id], true);
    }
}

/// The copilot binary, overridable via `SPM_COPILOT_BIN` (used by tests).
fn copilot_bin() -> String {
    std::env::var("SPM_COPILOT_BIN").unwrap_or_else(|_| "copilot".to_string())
}

fn copilot_available() -> bool {
    Command::new(copilot_bin())
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a copilot subcommand. When `ignore_err`, a non-zero exit is swallowed
/// (e.g. removing something that isn't registered yet).
fn run(args: &[&str], ignore_err: bool) -> Result<()> {
    let out = Command::new(copilot_bin())
        .args(args)
        .output()
        .with_context(|| format!("failed to run `copilot {}`", args.join(" ")))?;
    if !out.status.success() && !ignore_err {
        bail!(
            "copilot {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Stable, kebab-safe per-project marketplace/plugin id derived from the project
/// path (FNV-1a). Keeps globally-registered marketplaces from colliding.
fn market_id(project_root: &Path) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in project_root.to_string_lossy().bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("spm-{h:08x}")
}
