//! Reading Claude Code plugin metadata from a materialized plugin checkout.
//!
//! A full-plugin dependency (see the `plugins` map in `ai.json`) points at a
//! Claude Code plugin root — a directory holding `.claude-plugin/plugin.json`
//! plus `agents/`, `skills/`, `scripts/`, hooks, etc. spm needs two things from
//! that tree:
//!
//! 1. The plugin's own declared **name** (for the Claude marketplace entry and
//!    the `<name>@<marketplace>` enabled-plugins key).
//! 2. The plugin's **bundled skills**, extracted as ordinary
//!    [`MaterializedSkill`]s so that *every* target — even the ones that only
//!    understand `SKILL.md` and cannot register agents/MCP/hooks — still gets
//!    the plugin's skills (a graceful, skills-only degradation).

use crate::manifest::{validate_skill_name, validate_subpath};
use crate::vendor::MaterializedSkill;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The subset of `.claude-plugin/plugin.json` spm cares about.
#[derive(Debug, Default, Deserialize)]
struct PluginJson {
    #[serde(default)]
    name: Option<String>,
    /// Relative path to the plugin's skills dir; Claude defaults this to
    /// `./skills` when absent.
    #[serde(default)]
    skills: Option<String>,
}

/// Read `<root>/.claude-plugin/plugin.json`, tolerating its absence (returns the
/// default) so a non-Claude-shaped checkout still degrades gracefully instead of
/// erroring.
fn read_plugin_json(root: &Path) -> Result<PluginJson> {
    let path = root.join(".claude-plugin").join("plugin.json");
    if !path.exists() {
        return Ok(PluginJson::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Whether `root` looks like a Claude Code plugin, i.e. carries a
/// `.claude-plugin/plugin.json`. Used to warn when a `--plugin` dependency
/// points at something that isn't actually a plugin.
pub fn looks_like_plugin(root: &Path) -> bool {
    root.join(".claude-plugin").join("plugin.json").is_file()
}

/// The plugin's declared name (`plugin.json` → `name`), falling back to
/// `fallback` (the manifest key) when the plugin omits it. The returned value
/// feeds the Claude marketplace/enabled-plugins keys, so it is validated to be a
/// safe single token.
pub fn plugin_name(root: &Path, fallback: &str) -> Result<String> {
    let declared = read_plugin_json(root)?.name;
    match declared {
        Some(n) if validate_skill_name(&n).is_ok() => Ok(n),
        _ => Ok(fallback.to_string()),
    }
}

/// Enumerate the plugin's bundled skills as [`MaterializedSkill`]s: every
/// immediate subdirectory of the plugin's skills dir that contains a `SKILL.md`.
///
/// The skills dir is taken from `plugin.json`'s `skills` field (default
/// `skills`) and validated to stay inside the plugin root. Returns an empty vec
/// when the plugin declares no skills dir or it holds none.
pub fn plugin_skills(root: &Path) -> Result<Vec<MaterializedSkill>> {
    let meta = read_plugin_json(root)?;
    // Normalize the declared skills path (strip a leading `./`, trailing `/`).
    let rel = meta.skills.as_deref().unwrap_or("skills");
    let rel = rel.trim_start_matches("./").trim_end_matches('/');
    let rel = if rel.is_empty() { "skills" } else { rel };
    validate_subpath(rel).with_context(|| format!("plugin skills dir `{rel}`"))?;

    let skills_dir = root.join(rel);
    if !skills_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&skills_dir)
        .with_context(|| format!("reading {}", skills_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if !path.is_dir() || !path.join("SKILL.md").is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .with_context(|| format!("non-UTF-8 skill dir name under {}", skills_dir.display()))?;
        // The subdir name becomes a path component in every vendor's skills dir,
        // so it must pass the same escape checks as a manifest skill name.
        validate_skill_name(&name)
            .with_context(|| format!("bundled skill in plugin at {}", root.display()))?;
        out.push(MaterializedSkill { name, path });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spm-plugin-{name}-{}-{nanos}", std::process::id()))
    }

    /// Build a plugin root with the given plugin.json body and a `skills/`
    /// tree holding `names` (each with a SKILL.md) plus a non-skill dir.
    fn make_plugin(root: &Path, plugin_json: &str, names: &[&str]) {
        let cp = root.join(".claude-plugin");
        std::fs::create_dir_all(&cp).unwrap();
        std::fs::write(cp.join("plugin.json"), plugin_json).unwrap();
        for n in names {
            let d = root.join("skills").join(n);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), format!("---\nname: {n}\n---\n")).unwrap();
        }
        // A non-skill dir (no SKILL.md) must be ignored.
        std::fs::create_dir_all(root.join("skills").join("notaskill")).unwrap();
        std::fs::write(root.join("skills").join("notaskill").join("x.md"), "x").unwrap();
    }

    #[test]
    fn plugin_name_prefers_declared_then_falls_back() {
        let root = scratch("name");
        make_plugin(&root, r#"{"name":"declared-name"}"#, &[]);
        assert_eq!(plugin_name(&root, "fallback").unwrap(), "declared-name");
        std::fs::remove_dir_all(&root).unwrap();

        let root2 = scratch("name-missing");
        make_plugin(&root2, r#"{}"#, &[]);
        assert_eq!(plugin_name(&root2, "fallback").unwrap(), "fallback");
        std::fs::remove_dir_all(&root2).unwrap();
    }

    #[test]
    fn plugin_skills_enumerates_only_skill_dirs() {
        let root = scratch("skills");
        make_plugin(&root, r#"{"name":"p","skills":"./skills/"}"#, &["a", "b"]);
        let mut got: Vec<String> = plugin_skills(&root)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        got.sort();
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn plugin_skills_defaults_dir_and_tolerates_missing() {
        let root = scratch("default-dir");
        // No `skills` key → defaults to `skills/`.
        make_plugin(&root, r#"{"name":"p"}"#, &["only"]);
        let got = plugin_skills(&root).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "only");
        std::fs::remove_dir_all(&root).unwrap();

        // A plugin with no skills dir yields an empty list, not an error.
        let bare = scratch("bare");
        std::fs::create_dir_all(bare.join(".claude-plugin")).unwrap();
        std::fs::write(bare.join(".claude-plugin/plugin.json"), r#"{"name":"p"}"#).unwrap();
        assert!(plugin_skills(&bare).unwrap().is_empty());
        std::fs::remove_dir_all(&bare).unwrap();
    }
}
