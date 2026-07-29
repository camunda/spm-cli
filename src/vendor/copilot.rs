use super::{MaterializedSkill, Vendor};
use crate::{fsutil, jsonutil, paths};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

/// GitHub Copilot CLI adapter.
///
/// Copilot CLI consumes skills through a plugin marketplace (`marketplace.json`
/// → `plugin.json` → `skills/<name>/SKILL.md`), the same shape as Claude. spm
/// assembles a self-contained marketplace under `~/.spm/vendors/copilot/<id>/`
/// (outside the repo) and registers it by shelling out to the `copilot` CLI.
///
/// Unlike Claude, Copilot CLI marketplaces/plugins are **user-global** and
/// command-driven — there is no project-local config file to write, so wiring
/// mutates global `copilot` state. The registration is named by the stable,
/// path-independent project id from the lockfile, so re-running from a moved or
/// re-cloned checkout re-registers the *same* entry instead of leaving an
/// orphaned duplicate. Orphans from earlier runs are pruned on every sync.
pub struct Copilot;

impl Vendor for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn materialize(
        &self,
        _project_root: &Path,
        project_id: &str,
        skills: &[MaterializedSkill],
    ) -> Result<()> {
        let dir = market_dir(project_id)?;
        prune_orphans();

        // No skills: tear down this project's registration and remove the dir.
        if skills.is_empty() {
            unregister(project_id);
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

        jsonutil::write(
            &plugin_dir.join("plugin.json"),
            &json!({ "name": project_id }),
        )?;
        jsonutil::write(
            &dir.join(".github/plugin/marketplace.json"),
            &json!({
                "name": project_id,
                "owner": { "name": "spm", "email": "spm@example.com" },
                "metadata": { "description": "spm-managed skills", "version": "0.0.0" },
                "plugins": [{
                    "name": project_id,
                    "description": "spm-managed skills",
                    "version": "0.0.0",
                    "source": "plugin"
                }],
            }),
        )?;

        register(project_id, &dir)
    }

    fn clean(&self, _project_root: &Path, project_id: &str) -> Result<()> {
        unregister(project_id);
        prune_orphans();
        let dir = market_dir(project_id)?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

/// This project's marketplace dir, keyed by the stable id (not the path).
/// Rejects an empty/invalid id: `market_dir("")` would otherwise resolve to the
/// shared `~/.spm/vendors/copilot` root, which `clean` recursively deletes —
/// wiping every project's registration.
fn market_dir(project_id: &str) -> Result<PathBuf> {
    crate::lockfile::validate_project_id(project_id)?;
    Ok(paths::vendors_dir()?.join("copilot").join(project_id))
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

/// Remove spm-managed marketplaces whose local directory no longer exists —
/// leftovers from deleted projects or the old path-derived naming scheme.
fn prune_orphans() {
    if !copilot_available() {
        return;
    }
    let Ok(out) = Command::new(copilot_bin())
        .args(["plugin", "marketplace", "list"])
        .output()
    else {
        return;
    };
    let listing = String::from_utf8_lossy(&out.stdout);
    for (name, local) in parse_local_marketplaces(&listing) {
        if name.starts_with("spm") && !Path::new(&local).exists() {
            let _ = run(&["plugin", "uninstall", &name], true);
            let _ = run(&["plugin", "marketplace", "remove", &name], true);
        }
    }
}

/// Parse `copilot plugin marketplace list` lines of the form
/// `  • <name> (Local: <path>)`, returning (name, path) for local marketplaces.
fn parse_local_marketplaces(listing: &str) -> Vec<(String, String)> {
    listing
        .lines()
        .filter_map(|line| {
            let (left, rest) = line.split_once(" (Local: ")?;
            let name = left
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .trim();
            let path = rest.trim_end().trim_end_matches(')');
            (!name.is_empty()).then(|| (name.to_string(), path.to_string()))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::parse_local_marketplaces;

    #[test]
    fn parses_local_marketplaces_only() {
        let listing = "Included with GitHub Copilot:\n  \u{25c6} copilot-plugins (GitHub: github/copilot-plugins)\n  \u{2022} spm-24918f1d (Local: /home/u/.spm/vendors/copilot/spm-24918f1d)\n";
        let got = parse_local_marketplaces(listing);
        assert_eq!(
            got,
            vec![(
                "spm-24918f1d".to_string(),
                "/home/u/.spm/vendors/copilot/spm-24918f1d".to_string()
            )]
        );
    }
}
