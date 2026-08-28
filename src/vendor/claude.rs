use super::{MaterializedPlugin, MaterializedSkill, Vendor};
use crate::scope::Scope;
use crate::{fsutil, gitignore, jsonutil, paths};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Claude adapter.
///
/// Claude can only load skills that physically sit inside a plugin dir, so spm
/// assembles a self-contained marketplace and registers it in Claude's
/// settings. This works in two scopes:
///
/// - **Project** ([`Scope::Project`]) — the marketplace is built in the
///   project-local, gitignored `.spm/claude/` dir and registered in the repo's
///   gitignored `.claude/settings.local.json` under the marketplace name `spm`
///   (skills invoked as `/spm:<name>`).
///
/// - **Global** ([`Scope::Global`]) — the marketplace is built in the spm-owned
///   `$SPM_HOME/claude-global/` dir and registered in the user-global
///   `~/.claude/settings.json` under the marketplace name `spm-global` (skills
///   invoked as `/spm-global:<name>`). A distinct name is required because
///   Claude merges the global `settings.json` with a project's
///   `settings.local.json`; reusing `spm` for both scopes would collide.
///
/// Both scopes' marketplace dirs are entirely spm-owned, so each materialize
/// rebuilds from scratch.
pub struct Claude;

/// Marketplace/plugin name per scope. Skills end up invocable as
/// `/<name>:<skill-name>`.
const MARKETPLACE_PROJECT: &str = "spm";
const MARKETPLACE_GLOBAL: &str = "spm-global";

/// Marketplace name for **full plugins** per scope. Kept separate from the
/// skills-wrapper marketplace above so the two registrations never fight over a
/// single `marketplace.json`: skills flow through `materialize`, full plugins
/// (agents/MCP/hooks/scripts) through `materialize_plugins`.
const MARKETPLACE_PLUGINS_PROJECT: &str = "spm-plugins";
const MARKETPLACE_PLUGINS_GLOBAL: &str = "spm-plugins-global";

/// Project-scope managed dir segments (relative to the project root).
const PROJECT_DIR: [&str; 2] = [".spm", "claude"];
/// Project-scope full-plugin marketplace dir segments (relative to the root).
const PROJECT_PLUGINS_DIR: [&str; 2] = [".spm", "claude-plugins"];
/// Global-scope managed dir name (under `$SPM_HOME`).
const GLOBAL_DIR: &str = "claude-global";
/// Global-scope full-plugin marketplace dir name (under `$SPM_HOME`).
const GLOBAL_PLUGINS_DIR: &str = "claude-plugins-global";

const GITIGNORE_ENTRY: &str = ".spm/";
const GITIGNORE_COMMENT: &str =
    "# spm-managed Claude marketplace — materialized locally by `spm`, not committed.";

impl Vendor for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn materialize(
        &self,
        scope: &Scope,
        _project_id: &str,
        skills: &[MaterializedSkill],
        _previously_managed: &[String],
    ) -> Result<()> {
        let market = marketplace(scope);
        let market_dir = managed_dir(scope)?;
        // Rebuild from scratch so removed skills disappear (dir is spm-owned).
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
        }

        jsonutil::write(
            &plugin_dir.join(".claude-plugin/plugin.json"),
            &json!({ "name": market }),
        )?;
        jsonutil::write(
            &market_dir.join(".claude-plugin/marketplace.json"),
            &json!({
                "name": market,
                "owner": { "name": "spm" },
                "plugins": [ { "name": market, "source": "./plugin" } ],
            }),
        )?;

        patch_settings(scope, &market_dir, skills)?;
        if let Scope::Project { root } = scope {
            gitignore::ensure(root, GITIGNORE_COMMENT, GITIGNORE_ENTRY)?;
        }
        Ok(())
    }

    fn clean(&self, scope: &Scope, _project_id: &str, _managed: &[String]) -> Result<()> {
        let market_dir = managed_dir(scope)?;
        if market_dir.exists() {
            std::fs::remove_dir_all(&market_dir)?;
        }
        let path = settings_path(scope)?;
        if path.exists() {
            let mut root = jsonutil::read_object(&path)?;
            jsonutil::remove_nested(&mut root, "extraKnownMarketplaces", marketplace(scope));
            jsonutil::remove_nested(&mut root, "enabledPlugins", &plugin_key(scope));
            jsonutil::write(&path, &root)?;
        }
        // `.gitignore` is intentionally left untouched (project scope only): the
        // entry spm added is harmless once the materialized dir is gone.
        Ok(())
    }

    fn status(&self, scope: &Scope, expected: &[String]) -> Result<super::VendorStatus> {
        let market_dir = managed_dir(scope)?;
        let skills_dir = market_dir.join("plugin").join("skills");
        // The marketplace dir is spm-owned in both scopes, so stale detection is
        // always meaningful here.
        let mut st = super::classify(&skills_dir, expected, true);
        // Diagnose the settings marketplace pointer. A fresh worktree may have
        // inherited another checkout's absolute path (issue #28), so flag when it
        // is missing or points at a different dir.
        if !expected.is_empty() {
            match registered_marketplace_path(scope)? {
                None => st.notes.push(format!(
                    "no spm marketplace registered in {}",
                    settings_path(scope)?.display()
                )),
                Some(p) if Path::new(&p) != market_dir => st.notes.push(format!(
                    "{} marketplace points at {p}, not this location ({})",
                    settings_path(scope)?.display(),
                    market_dir.display()
                )),
                Some(_) => {}
            }
        }
        Ok(st)
    }

    fn materialize_plugins(
        &self,
        scope: &Scope,
        _project_id: &str,
        plugins: &[MaterializedPlugin],
        _previously_managed: &[String],
    ) -> Result<()> {
        let market = plugins_marketplace(scope);
        let market_dir = plugins_managed_dir(scope)?;
        // The plugins marketplace dir is entirely spm-owned: rebuild from scratch
        // so removed plugins disappear.
        if market_dir.exists() {
            std::fs::remove_dir_all(&market_dir)?;
        }

        // Deregister first, then re-add: this drops stale marketplace/enabled
        // entries in one pass and cleanly handles the "no plugins left" case.
        let settings = settings_path(scope)?;
        let mut root = jsonutil::read_object(&settings)?;
        jsonutil::remove_nested(&mut root, "extraKnownMarketplaces", market);
        jsonutil::remove_nested_by_suffix(&mut root, "enabledPlugins", &format!("@{market}"));

        if plugins.is_empty() {
            jsonutil::write(&settings, &root)?;
            return Ok(());
        }

        std::fs::create_dir_all(&market_dir)?;
        let mut entries: Vec<Value> = Vec::new();
        for p in plugins {
            let dest = market_dir.join(&p.name);
            fsutil::copy_tree(&p.path, &dest)
                .with_context(|| format!("copying plugin `{}` into marketplace", p.name))?;
            // The plugin's bundled skills are already served through the `spm`
            // skills marketplace (flattened into the ordinary skills list), so
            // strip the `skills` pointer from this copy to avoid exposing them
            // twice — this registration contributes only agents/MCP/hooks/scripts.
            strip_bundled_skills(&dest)?;

            let plugin_name = crate::plugin::plugin_name(&dest, &p.name)?;
            entries.push(json!({ "name": plugin_name, "source": format!("./{}", p.name) }));
            jsonutil::object_mut(&mut root, "enabledPlugins")
                .insert(format!("{plugin_name}@{market}"), Value::Bool(true));
        }

        jsonutil::write(
            &market_dir.join(".claude-plugin/marketplace.json"),
            &json!({
                "name": market,
                "owner": { "name": "spm" },
                "plugins": entries,
            }),
        )?;
        jsonutil::object_mut(&mut root, "extraKnownMarketplaces").insert(
            market.to_string(),
            json!({ "source": { "source": "directory", "path": market_dir.to_string_lossy() } }),
        );
        jsonutil::write(&settings, &root)?;

        if let Scope::Project { root } = scope {
            gitignore::ensure(root, GITIGNORE_COMMENT, GITIGNORE_ENTRY)?;
        }
        Ok(())
    }

    fn status_plugins(
        &self,
        scope: &Scope,
        expected: &[String],
    ) -> Result<Option<super::VendorStatus>> {
        let market_dir = plugins_managed_dir(scope)?;
        // Each plugin is copied verbatim to `<market_dir>/<manifest-name>/`.
        // Stale detection is disabled: `.claude-plugin/` (the marketplace
        // manifest) lives directly under `market_dir` and is not a plugin.
        let mut st = super::classify(&market_dir, expected, false);
        // Verify the settings registration, mirroring the skills marketplace
        // check: a deleted `.spm/claude-plugins` or a fresh worktree that
        // inherited another checkout's absolute path both surface here.
        if !expected.is_empty() {
            match registered_plugins_marketplace_path(scope)? {
                None => st.notes.push(format!(
                    "no spm plugins marketplace registered in {}",
                    settings_path(scope)?.display()
                )),
                Some(p) if Path::new(&p) != market_dir => st.notes.push(format!(
                    "{} plugins marketplace points at {p}, not this location ({})",
                    settings_path(scope)?.display(),
                    market_dir.display()
                )),
                Some(_) => {}
            }
        }
        Ok(Some(st))
    }

    fn clean_plugins(&self, scope: &Scope, _project_id: &str, _managed: &[String]) -> Result<()> {
        let market = plugins_marketplace(scope);
        let market_dir = plugins_managed_dir(scope)?;
        if market_dir.exists() {
            std::fs::remove_dir_all(&market_dir)?;
        }
        let settings = settings_path(scope)?;
        if settings.exists() {
            let mut root = jsonutil::read_object(&settings)?;
            jsonutil::remove_nested(&mut root, "extraKnownMarketplaces", market);
            jsonutil::remove_nested_by_suffix(&mut root, "enabledPlugins", &format!("@{market}"));
            jsonutil::write(&settings, &root)?;
        }
        Ok(())
    }
}

/// Full-plugin marketplace name for a scope.
fn plugins_marketplace(scope: &Scope) -> &'static str {
    match scope {
        Scope::Project { .. } => MARKETPLACE_PLUGINS_PROJECT,
        Scope::Global => MARKETPLACE_PLUGINS_GLOBAL,
    }
}

/// Absolute path of the generated full-plugin marketplace dir for a scope.
fn plugins_managed_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Project { root } => Ok(PROJECT_PLUGINS_DIR
            .iter()
            .fold(root.clone(), |p, seg| p.join(seg))),
        Scope::Global => Ok(paths::spm_home()?.join(GLOBAL_PLUGINS_DIR)),
    }
}

/// Drop the `skills` pointer from a copied plugin's `plugin.json` (if present),
/// leaving every other field intact. spm serves plugin-bundled skills through
/// its own skills marketplace, so a plugin registered for its agents/MCP/hooks
/// must not also advertise those same skills.
fn strip_bundled_skills(plugin_dir: &Path) -> Result<()> {
    let pj = plugin_dir.join(".claude-plugin").join("plugin.json");
    if !pj.exists() {
        return Ok(());
    }
    let mut v = jsonutil::read_object(&pj)?;
    if let Some(obj) = v.as_object_mut() {
        obj.remove("skills");
    }
    jsonutil::write(&pj, &v)
}

/// Marketplace/plugin name for a scope.
fn marketplace(scope: &Scope) -> &'static str {
    match scope {
        Scope::Project { .. } => MARKETPLACE_PROJECT,
        Scope::Global => MARKETPLACE_GLOBAL,
    }
}

/// Read the directory path spm registered for its **skills** marketplace, if present.
fn registered_marketplace_path(scope: &Scope) -> Result<Option<String>> {
    let root = jsonutil::read_object(&settings_path(scope)?)?;
    Ok(root
        .get("extraKnownMarketplaces")
        .and_then(|v| v.get(marketplace(scope)))
        .and_then(|v| v.get("source"))
        .and_then(|v| v.get("path"))
        .and_then(|v| v.as_str())
        .map(str::to_owned))
}

/// Read the directory path spm registered for its **full-plugin** marketplace,
/// if present. Mirrors [`registered_marketplace_path`] for the `spm-plugins`
/// registration.
fn registered_plugins_marketplace_path(scope: &Scope) -> Result<Option<String>> {
    let root = jsonutil::read_object(&settings_path(scope)?)?;
    Ok(root
        .get("extraKnownMarketplaces")
        .and_then(|v| v.get(plugins_marketplace(scope)))
        .and_then(|v| v.get("source"))
        .and_then(|v| v.get("path"))
        .and_then(|v| v.as_str())
        .map(str::to_owned))
}

/// Absolute path of the generated marketplace dir for a scope.
fn managed_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Project { root } => Ok(PROJECT_DIR.iter().fold(root.clone(), |p, seg| p.join(seg))),
        Scope::Global => Ok(paths::spm_home()?.join(GLOBAL_DIR)),
    }
}

/// Merge spm's marketplace + enabled-plugin entries into the settings file,
/// preserving any keys the user already has.
fn patch_settings(scope: &Scope, market_dir: &Path, skills: &[MaterializedSkill]) -> Result<()> {
    let path = settings_path(scope)?;
    let market = marketplace(scope);
    let mut root = jsonutil::read_object(&path)?;

    jsonutil::object_mut(&mut root, "extraKnownMarketplaces").insert(
        market.to_string(),
        json!({ "source": { "source": "directory", "path": market_dir.to_string_lossy() } }),
    );

    let key = plugin_key(scope);
    let enabled = jsonutil::object_mut(&mut root, "enabledPlugins");
    // Only enable the wrapper plugin if there is at least one skill.
    if skills.is_empty() {
        enabled.remove(&key);
    } else {
        enabled.insert(key, Value::Bool(true));
    }

    jsonutil::write(&path, &root)
}

fn plugin_key(scope: &Scope) -> String {
    let m = marketplace(scope);
    format!("{m}@{m}")
}

/// Path of the Claude settings file spm writes its registration into.
fn settings_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Project { root } => Ok(root.join(".claude").join("settings.local.json")),
        Scope::Global => Ok(paths::home_dir()?.join(".claude").join("settings.json")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spm-claude-{name}-{}-{nanos}", std::process::id()))
    }

    fn make_skill(root: &Path, name: &str) -> MaterializedSkill {
        let src = root.join("src").join(name);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
        MaterializedSkill {
            name: name.to_string(),
            path: src,
        }
    }

    #[test]
    fn project_and_global_use_distinct_marketplace_names() {
        assert_eq!(marketplace(&Scope::Global), "spm-global");
        assert_eq!(plugin_key(&Scope::Global), "spm-global@spm-global");
        let p = Scope::Project {
            root: PathBuf::from("/tmp/p"),
        };
        assert_eq!(marketplace(&p), "spm");
        assert_eq!(plugin_key(&p), "spm@spm");
    }

    /// Project materialize builds the marketplace and registers it in the repo's
    /// gitignored settings.local.json (with a .gitignore block).
    #[test]
    fn project_materialize_builds_marketplace_and_registers() {
        let tmp = scratch("proj-mat");
        std::fs::create_dir_all(&tmp).unwrap();
        let scope = Scope::Project { root: tmp.clone() };
        let s = make_skill(&tmp, "greet");
        Claude.materialize(&scope, "spm-1234", &[s], &[]).unwrap();

        assert!(tmp
            .join(".spm/claude/plugin/skills/greet/SKILL.md")
            .exists());
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert!(settings["extraKnownMarketplaces"]["spm"].is_object());
        assert_eq!(settings["enabledPlugins"]["spm@spm"], Value::Bool(true));
        assert!(std::fs::read_to_string(tmp.join(".gitignore"))
            .unwrap()
            .contains(".spm/"));

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Clean removes the marketplace dir and de-registers the plugin/marketplace.
    #[test]
    fn project_clean_removes_dir_and_registration() {
        let tmp = scratch("proj-clean");
        std::fs::create_dir_all(&tmp).unwrap();
        let scope = Scope::Project { root: tmp.clone() };
        let s = make_skill(&tmp, "greet");
        Claude.materialize(&scope, "spm-1234", &[s], &[]).unwrap();
        Claude.clean(&scope, "spm-1234", &["greet".into()]).unwrap();

        assert!(!tmp.join(".spm/claude").exists(), "marketplace dir removed");
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert!(settings.get("extraKnownMarketplaces").is_none());
        assert!(settings.get("enabledPlugins").is_none());

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
