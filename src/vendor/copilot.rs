use super::{MaterializedSkill, Vendor};
use crate::{jsonutil, paths};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// VS Code setting that lists extra folders scanned for `*.instructions.md`.
const LOCATIONS_KEY: &str = "chat.instructionsFilesLocations";

/// GitHub Copilot (VS Code) adapter.
///
/// Copilot has no plugin/skill-bundle concept — the closest primitive is a
/// custom-instruction file. Each skill's `SKILL.md` is rewritten as a
/// `<name>.instructions.md` in `~/.spm/vendors/copilot/<project>/instructions`
/// (outside the repo). The folder is registered via the `~/`-relative form of
/// `chat.instructionsFilesLocations` in `.vscode/settings.json`.
///
/// Caveat: Copilot has no gitignored-settings convention like Claude's
/// `settings.local.json`, so `.vscode/settings.json` should be gitignored or
/// treated as machine-local — `spm install` regenerates it.
pub struct Copilot;

impl Vendor for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn materialize(&self, project_root: &Path, skills: &[MaterializedSkill]) -> Result<()> {
        let base = paths::vendor_project_dir("copilot", project_root)?;
        let instr_dir = base.join("instructions");
        // Rebuild from scratch so removed skills disappear.
        if base.exists() {
            std::fs::remove_dir_all(&base)?;
        }
        std::fs::create_dir_all(&instr_dir)?;

        for s in skills {
            let src = s.path.join("SKILL.md");
            if !src.exists() {
                eprintln!(
                    "warning: skill `{}` has no SKILL.md — skipped for copilot",
                    s.name
                );
                continue;
            }
            let md = std::fs::read_to_string(&src)
                .with_context(|| format!("reading {}", src.display()))?;
            let out = to_instruction(&md);
            std::fs::write(instr_dir.join(format!("{}.instructions.md", s.name)), out)?;
        }

        patch_settings(project_root, &instr_dir)
    }

    fn clean(&self, project_root: &Path) -> Result<()> {
        let base = paths::vendor_project_dir("copilot", project_root)?;
        if base.exists() {
            std::fs::remove_dir_all(&base)?;
        }
        let path = settings_path(project_root);
        if !path.exists() {
            return Ok(());
        }
        let location = paths::tildify(&base.join("instructions"));
        let mut root = jsonutil::read_object(&path)?;
        jsonutil::remove_nested(&mut root, LOCATIONS_KEY, &location);
        jsonutil::write(&path, &root)
    }
}

fn settings_path(project_root: &Path) -> PathBuf {
    project_root.join(".vscode").join("settings.json")
}

fn patch_settings(project_root: &Path, instr_dir: &Path) -> Result<()> {
    let path = settings_path(project_root);
    let mut root = jsonutil::read_object(&path)?;
    jsonutil::object_mut(&mut root, LOCATIONS_KEY)
        .insert(paths::tildify(instr_dir), Value::Bool(true));
    jsonutil::write(&path, &root)
}

/// Rewrite a SKILL.md as a Copilot instruction file: force `applyTo: "**"` so it
/// always applies, carry over `description` if present, keep the body.
fn to_instruction(md: &str) -> String {
    let (frontmatter, body) = split_frontmatter(md);
    let description = frontmatter.lines().find_map(|line| {
        line.strip_prefix("description:")
            .map(|v| v.trim().trim_matches('"').to_string())
    });

    let mut out = String::from("---\napplyTo: \"**\"\n");
    if let Some(d) = description {
        out.push_str(&format!("description: {d}\n"));
    }
    out.push_str("---\n\n");
    out.push_str(body.trim_start());
    out
}

/// Split a markdown file into (frontmatter-body, content-body). If there is no
/// leading `---` frontmatter block, returns ("", whole input).
fn split_frontmatter(md: &str) -> (&str, &str) {
    let Some(rest) = md.strip_prefix("---\n") else {
        return ("", md);
    };
    match rest.find("\n---") {
        Some(end) => {
            let frontmatter = &rest[..end];
            // Skip the rest of the closing `---` line; body starts after its newline.
            let after = &rest[end + 1..];
            let body_start = after.find('\n').map(|i| i + 1).unwrap_or(after.len());
            (frontmatter, &after[body_start..])
        }
        None => ("", md),
    }
}
