//! A generic vendor for tools that auto-discover `SKILL.md` skills **one
//! directory deep** under a skills root the user also authors into — i.e. a
//! *shared*, potentially version-controlled dir (Gemini CLI's `.gemini/skills`,
//! Codex CLI's `.agents/skills`, …).
//!
//! These adapters differ only in *where* the skills root is per scope and the
//! `.gitignore` comment, so they are expressed as [`SharedDirVendor`] config
//! rather than duplicated per tool (see the "Derivation Over Duplication" rule
//! in `AGENTS.md`). The mechanics are identical to the Copilot adapter's
//! *global* branch, applied to **both** scopes:
//!
//! - The dir is shared with the user's own hand-authored skills, so spm never
//!   wipes it; it removes only the entries it previously managed (∪ the ones it
//!   is about to write) and copies the current set.
//! - spm's materialized skills must still stay out of VCS without hiding the
//!   user's own committed skills in the same dir, so in [`Scope::Project`] each
//!   spm-managed skill subdir is added to `.gitignore` individually.
//! - In [`Scope::Global`] there is no repo, so no `.gitignore` is written.

use super::dirskills::{copy_skills_into, remove_managed};
use super::{MaterializedSkill, Vendor};
use crate::scope::Scope;
use crate::{gitignore, paths};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Config-driven adapter for a "shared skills dir, one level deep" tool.
pub struct SharedDirVendor {
    /// Target key (e.g. `"gemini"`, `"codex"`).
    name: &'static str,
    /// Project-scope skills dir segments, relative to the project root.
    project_segs: &'static [&'static str],
    /// Global-scope skills dir segments, relative to the user's home dir.
    global_segs: &'static [&'static str],
    /// Comment written above each spm-managed `.gitignore` entry.
    gitignore_comment: &'static str,
}

/// Gemini CLI: workspace `.gemini/skills/` + user `~/.gemini/skills/`.
pub fn gemini() -> SharedDirVendor {
    SharedDirVendor {
        name: "gemini",
        project_segs: &[".gemini", "skills"],
        global_segs: &[".gemini", "skills"],
        gitignore_comment:
            "# spm-managed Gemini skills — materialized locally by `spm`, not committed.",
    }
}

/// Codex CLI: the cross-tool `.agents/skills/` alias (repo) + `~/.agents/skills/`
/// (user). Codex has no private `.codex/skills` dir — `.agents/skills` is its
/// documented location, shared with any other tool that reads the alias.
pub fn codex() -> SharedDirVendor {
    SharedDirVendor {
        name: "codex",
        project_segs: &[".agents", "skills"],
        global_segs: &[".agents", "skills"],
        gitignore_comment:
            "# spm-managed Codex skills — materialized locally by `spm`, not committed.",
    }
}

/// Cursor: workspace `.cursor/skills/` + user `~/.cursor/skills/`. Per the
/// official docs Cursor auto-discovers skills one level deep from its own
/// `.cursor/skills` dir (and the `.agents/skills` alias) in *both* scopes, and
/// treats that dir as version-controlled/shared — the same shared-dir contract
/// as Gemini. We target the tool-native `.cursor/skills` so a skill added for
/// `cursor` alone lands in Cursor's own dir without colliding with the
/// `.agents/skills` alias other targets use.
pub fn cursor() -> SharedDirVendor {
    SharedDirVendor {
        name: "cursor",
        project_segs: &[".cursor", "skills"],
        global_segs: &[".cursor", "skills"],
        gitignore_comment:
            "# spm-managed Cursor skills — materialized locally by `spm`, not committed.",
    }
}

/// Cline: workspace `.cline/skills/` + user `~/.cline/skills/` (verified against
/// docs.cline.bot). Skills live one level deep; the workspace dir is committed
/// with the repo, so spm shares it surgically like the other shared-dir tools.
pub fn cline() -> SharedDirVendor {
    SharedDirVendor {
        name: "cline",
        project_segs: &[".cline", "skills"],
        global_segs: &[".cline", "skills"],
        gitignore_comment:
            "# spm-managed Cline skills — materialized locally by `spm`, not committed.",
    }
}

/// Windsurf (Cascade): workspace `.windsurf/skills/` + user
/// `~/.codeium/windsurf/skills/` (verified against docs.windsurf.com). Note the
/// **asymmetric** dirs — the global scope lives under `~/.codeium/windsurf`, not
/// `~/.windsurf` — which is exactly why [`SharedDirVendor`] keeps `project_segs`
/// and `global_segs` separate. The workspace dir is committed with the repo, so
/// spm shares it surgically.
pub fn windsurf() -> SharedDirVendor {
    SharedDirVendor {
        name: "windsurf",
        project_segs: &[".windsurf", "skills"],
        global_segs: &[".codeium", "windsurf", "skills"],
        gitignore_comment:
            "# spm-managed Windsurf skills — materialized locally by `spm`, not committed.",
    }
}

/// Amp: workspace `.agents/skills/` (its documented default) + user
/// `~/.config/agents/skills/` (verified against ampcode.com). Amp installs into
/// the cross-tool `.agents/skills` alias in the workspace — the same dir Codex
/// reads — so targeting both `amp` and `codex` writes to one shared workspace
/// dir; the user-scope dirs differ. Amp has no private `.amp/skills` dir.
pub fn amp() -> SharedDirVendor {
    SharedDirVendor {
        name: "amp",
        project_segs: &[".agents", "skills"],
        global_segs: &[".config", "agents", "skills"],
        gitignore_comment:
            "# spm-managed Amp skills — materialized locally by `spm`, not committed.",
    }
}

impl SharedDirVendor {
    /// Absolute path of the managed skills directory for a scope.
    fn skills_dir(&self, scope: &Scope) -> Result<PathBuf> {
        match scope {
            Scope::Project { root } => Ok(join_all(root, self.project_segs)),
            Scope::Global => Ok(join_all(&paths::home_dir()?, self.global_segs)),
        }
    }

    /// `.gitignore` entry for a single spm-managed skill subdir (project scope).
    fn gitignore_entry(&self, skill: &str) -> String {
        format!("{}/{skill}/", self.project_segs.join("/"))
    }
}

impl Vendor for SharedDirVendor {
    fn name(&self) -> &'static str {
        self.name
    }

    fn materialize(
        &self,
        scope: &Scope,
        _project_id: &str,
        skills: &[MaterializedSkill],
        previously_managed: &[String],
    ) -> Result<()> {
        let dir = self.skills_dir(scope)?;
        // Shared dir: remove only the entries spm owns (previously-managed ∪
        // about-to-write), then copy the current set. Never wipe the whole dir.
        let mut owned: BTreeSet<&str> = previously_managed.iter().map(String::as_str).collect();
        owned.extend(skills.iter().map(|s| s.name.as_str()));
        remove_managed(&dir, owned.iter().copied())?;
        if !skills.is_empty() {
            copy_skills_into(&dir, skills)?;
        }
        // Keep spm's skills out of VCS without ignoring the user's own committed
        // skills in the same shared dir: ignore each managed subdir by name.
        if let Scope::Project { root } = scope {
            for s in skills {
                gitignore::ensure(root, self.gitignore_comment, &self.gitignore_entry(&s.name))?;
            }
        }
        Ok(())
    }

    fn clean(&self, scope: &Scope, _project_id: &str, managed: &[String]) -> Result<()> {
        // Shared dir: remove only the skills spm owns, never the whole dir.
        // `.gitignore` is intentionally left untouched: the per-skill entries spm
        // added are harmless once the materialized dirs are gone, and rewriting a
        // user-owned file on `clean` risks clobbering it.
        remove_managed(&self.skills_dir(scope)?, managed.iter().map(String::as_str))
    }

    fn status(&self, scope: &Scope, expected: &[String]) -> Result<super::VendorStatus> {
        // The dir is shared with the user's own skills in *both* scopes, so stale
        // detection would mislabel those — never enable it here.
        Ok(super::classify(&self.skills_dir(scope)?, expected, false))
    }
}

fn join_all(base: &Path, segs: &[&str]) -> PathBuf {
    segs.iter().fold(base.to_path_buf(), |p, seg| p.join(seg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "spm-shareddir-{name}-{}-{nanos}",
            std::process::id()
        ))
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
    fn presets_report_expected_names_and_dirs() {
        assert_eq!(gemini().name(), "gemini");
        assert_eq!(gemini().gitignore_entry("greet"), ".gemini/skills/greet/");
        assert_eq!(codex().name(), "codex");
        assert_eq!(codex().gitignore_entry("greet"), ".agents/skills/greet/");
        assert_eq!(cursor().name(), "cursor");
        assert_eq!(cursor().gitignore_entry("greet"), ".cursor/skills/greet/");
        assert_eq!(cline().name(), "cline");
        assert_eq!(cline().gitignore_entry("greet"), ".cline/skills/greet/");
        assert_eq!(windsurf().name(), "windsurf");
        assert_eq!(
            windsurf().gitignore_entry("greet"),
            ".windsurf/skills/greet/"
        );
        assert_eq!(amp().name(), "amp");
        assert_eq!(amp().gitignore_entry("greet"), ".agents/skills/greet/");
    }

    /// Guards the *asymmetric* scopes: Windsurf and Amp materialize into
    /// different dirs per scope (global lives under `~/.codeium/windsurf` and
    /// `~/.config/agents` respectively, not a `~/.<tool>` mirror of the project
    /// dir), so `skills_dir` must honor `global_segs` independently.
    #[test]
    fn asymmetric_presets_resolve_distinct_global_dirs() {
        let home = std::path::Path::new("/home/u");
        let ws = windsurf();
        assert_eq!(
            join_all(home, ws.global_segs),
            home.join(".codeium").join("windsurf").join("skills")
        );
        let a = amp();
        assert_eq!(
            join_all(home, a.global_segs),
            home.join(".config").join("agents").join("skills")
        );
    }

    /// Project materialize drops skills one level deep under the tool's skills
    /// dir and gitignores each managed subdir, without touching a user's own
    /// skill in the same shared dir, and purges a previously-managed-but-dropped
    /// skill.
    #[test]
    fn project_materialize_preserves_user_skills_gitignores_and_purges() {
        let tmp = scratch("proj-mat");
        std::fs::create_dir_all(&tmp).unwrap();
        let v = codex();
        let dir = tmp.join(".agents").join("skills");
        std::fs::create_dir_all(&dir).unwrap();

        // A skill the user authored & committed by hand — spm must never touch it.
        let user = dir.join("user-skill");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("SKILL.md"), "user\n").unwrap();

        // A skill spm managed last time that is now dropped from the manifest.
        let dropped = dir.join("old-skill");
        std::fs::create_dir_all(&dropped).unwrap();
        std::fs::write(dropped.join("SKILL.md"), "old\n").unwrap();

        let scope = Scope::Project { root: tmp.clone() };
        let greet = make_skill(&tmp, "greet");
        v.materialize(&scope, "spm-1234", &[greet], &["old-skill".to_string()])
            .unwrap();

        assert!(
            dir.join("greet").join("SKILL.md").exists(),
            "greet copied one level deep"
        );
        assert!(!dir.join("old-skill").exists(), "dropped skill purged");
        assert!(user.join("SKILL.md").exists(), "user's own skill preserved");
        let gi = std::fs::read_to_string(tmp.join(".gitignore")).unwrap();
        assert!(gi.contains(".agents/skills/greet/"), "{gi}");
        assert!(!gi.contains("user-skill"), "user's skill not ignored: {gi}");

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Clean removes only spm-managed entries, leaving the user's intact.
    #[test]
    fn clean_removes_only_managed() {
        let tmp = scratch("clean");
        let dir = tmp.join("skills");
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["greet", "user-skill"] {
            std::fs::create_dir_all(dir.join(n)).unwrap();
            std::fs::write(dir.join(n).join("SKILL.md"), "x\n").unwrap();
        }
        remove_managed(&dir, ["greet"].into_iter()).unwrap();
        assert!(!dir.join("greet").exists(), "managed skill removed");
        assert!(dir.join("user-skill").exists(), "user skill preserved");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
