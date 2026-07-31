use super::{MaterializedSkill, Vendor};
use crate::{fsutil, gitignore, jsonutil};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Name used for both the generated marketplace and the single wrapper plugin.
/// Skills end up invocable as `/spm:<skill-name>`.
const MARKETPLACE: &str = "spm";

/// Project-local (gitignored) directory holding the generated Claude
/// marketplace. Deliberately **outside** `.agents/skills/` so Copilot's
/// `.agents/skills/**/SKILL.md` scanner never discovers these marketplace
/// skills.
const MANAGED_DIR: [&str; 2] = [".spm", "claude"];

/// `.gitignore` block that keeps the whole project-local spm dir out of VCS.
const GITIGNORE_ENTRY: &str = ".spm/";
const GITIGNORE_COMMENT: &str =
    "# spm-managed Claude marketplace — materialized locally by `spm`, not committed.";

pub struct Claude;

impl Vendor for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    /// Claude can only load skills that physically sit inside a plugin dir, so we
    /// assemble a self-contained marketplace in the project-local, gitignored
    /// `.spm/claude/` dir and register it via the project's gitignored
    /// `.claude/settings.local.json`. The materialized skills stay truly local
    /// and are never committed.
    // Claude registration is per-project (a pointer in the repo's gitignored
    // settings.local.json), so the marketplace name can stay fixed and the
    // stable project_id is unused here.
    fn materialize(
        &self,
        project_root: &Path,
        _project_id: &str,
        skills: &[MaterializedSkill],
    ) -> Result<()> {
        let market_dir = managed_dir(project_root);
        // Rebuild from scratch so removed skills disappear.
        if market_dir.exists() {
            std::fs::remove_dir_all(&market_dir)?;
        }
        let plugin_dir = market_dir.join("plugin");
        let skills_dir = plugin_dir.join("skills");
        std::fs::create_dir_all(&skills_dir)?;

        for s in skills {
            let dest = skills_dir.join(&s.name);
            fsutil::copy_tree(&s.path, &dest)
                .with_context(|| format!("copying skill `{}` into plugin", s.name))?;
            if !dest.join("SKILL.md").exists() {
                eprintln!(
                    "warning: skill `{}` has no SKILL.md at its root — Claude may ignore it",
                    s.name
                );
            }
        }

        jsonutil::write(
            &plugin_dir.join(".claude-plugin/plugin.json"),
            &json!({ "name": MARKETPLACE }),
        )?;
        jsonutil::write(
            &market_dir.join(".claude-plugin/marketplace.json"),
            &json!({
                "name": MARKETPLACE,
                "owner": { "name": "spm" },
                "plugins": [ { "name": MARKETPLACE, "source": "./plugin" } ],
            }),
        )?;

        patch_settings(project_root, &market_dir, skills)?;
        gitignore::ensure(project_root, GITIGNORE_COMMENT, GITIGNORE_ENTRY)?;
        Ok(())
    }

    fn clean(&self, project_root: &Path, _project_id: &str) -> Result<()> {
        let market_dir = managed_dir(project_root);
        if market_dir.exists() {
            std::fs::remove_dir_all(&market_dir)?;
        }
        let path = settings_path(project_root);
        if path.exists() {
            let mut root = jsonutil::read_object(&path)?;
            jsonutil::remove_nested(&mut root, "extraKnownMarketplaces", MARKETPLACE);
            jsonutil::remove_nested(&mut root, "enabledPlugins", &plugin_key());
            jsonutil::write(&path, &root)?;
        }
        gitignore::remove(project_root, GITIGNORE_COMMENT, GITIGNORE_ENTRY)
    }
}

/// Absolute path of the project-local generated marketplace dir.
fn managed_dir(project_root: &Path) -> PathBuf {
    MANAGED_DIR
        .iter()
        .fold(project_root.to_path_buf(), |p, seg| p.join(seg))
}

/// Merge spm's marketplace + enabled-plugin entries into settings.local.json,
/// preserving any keys the user already has.
fn patch_settings(
    project_root: &Path,
    market_dir: &Path,
    skills: &[MaterializedSkill],
) -> Result<()> {
    let path = settings_path(project_root);
    let mut root = jsonutil::read_object(&path)?;

    jsonutil::object_mut(&mut root, "extraKnownMarketplaces").insert(
        MARKETPLACE.to_string(),
        json!({ "source": { "source": "directory", "path": market_dir.to_string_lossy() } }),
    );

    let enabled = jsonutil::object_mut(&mut root, "enabledPlugins");
    // Only enable the wrapper plugin if there is at least one skill.
    if skills.is_empty() {
        enabled.remove(&plugin_key());
    } else {
        enabled.insert(plugin_key(), Value::Bool(true));
    }

    jsonutil::write(&path, &root)
}

fn plugin_key() -> String {
    format!("{MARKETPLACE}@{MARKETPLACE}")
}

fn settings_path(project_root: &Path) -> PathBuf {
    project_root.join(".claude").join("settings.local.json")
}
