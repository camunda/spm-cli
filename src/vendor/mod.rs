mod claude;
mod copilot;
mod dirskills;
mod shareddir;

use crate::scope::Scope;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// A resolved skill ready to be wired into a vendor: a name plus the absolute
/// path to its content in the global store.
pub struct MaterializedSkill {
    pub name: String,
    pub path: PathBuf,
}

/// A resolved full plugin ready to be registered with a vendor that supports
/// more than skills: a name (the manifest key) plus the absolute path to the
/// plugin root in the global store (the dir holding `.claude-plugin/plugin.json`,
/// `agents/`, `skills/`, scripts, hooks, …).
pub struct MaterializedPlugin {
    pub name: String,
    pub path: PathBuf,
}

/// A per-target snapshot of what is materialized in the *current* checkout vs.
/// what `ai.lock` declares. Powers `spm status` and, in particular, the
/// "fresh worktree/clone not installed here" diagnostic: because spm materializes
/// skills into gitignored dirs, a new git worktree sees nothing until
/// `spm install` runs inside it.
pub struct VendorStatus {
    /// Where this vendor materializes skills in the project (project-local).
    pub location: PathBuf,
    /// Declared skills that are present on disk in this checkout.
    pub present: Vec<String>,
    /// Declared skills that are missing here (e.g. an uninstalled worktree).
    pub missing: Vec<String>,
    /// Materialized skills that are no longer declared in `ai.lock` (stale).
    pub stale: Vec<String>,
    /// Extra human-readable notes (e.g. a registration pointer problem).
    pub notes: Vec<String>,
}

/// Compare the declared (`expected`) skill names against the subdirectories
/// actually materialized under `skills_dir`. Shared by the vendor adapters,
/// whose skills all live one directory deep under a vendor-specific root.
///
/// `detect_stale` controls whether directories present on disk but not in
/// `expected` are reported as stale. Project-scope vendor dirs are 100%
/// spm-owned, so stale detection is meaningful there; user-global dirs (e.g.
/// `~/.copilot/skills`) are *shared* with the user's own hand-authored skills,
/// so global-scope callers pass `false` to avoid mislabeling those as stale.
pub fn classify(skills_dir: &Path, expected: &[String], detect_stale: bool) -> VendorStatus {
    let (mut present, mut missing) = (Vec::new(), Vec::new());
    for name in expected {
        if skills_dir.join(name).is_dir() {
            present.push(name.clone());
        } else {
            missing.push(name.clone());
        }
    }
    let mut stale = Vec::new();
    if detect_stale {
        if let Ok(entries) = std::fs::read_dir(skills_dir) {
            for e in entries.flatten() {
                if e.path().is_dir() {
                    if let Ok(n) = e.file_name().into_string() {
                        if !expected.iter().any(|x| x == &n) {
                            stale.push(n);
                        }
                    }
                }
            }
        }
    }
    stale.sort();
    VendorStatus {
        location: skills_dir.to_path_buf(),
        present,
        missing,
        stale,
        notes: Vec::new(),
    }
}

/// A target tool (Claude, Copilot, ...) that materializes skills from the
/// global fetch cache into the location where the tool discovers them. In
/// [`Scope::Project`](crate::scope::Scope) that is a project-local, gitignored
/// directory; in [`Scope::Global`](crate::scope::Scope) it is a user-global
/// location shared across every project.
pub trait Vendor {
    #[allow(dead_code)] // part of the adapter contract; not all call sites use it yet
    fn name(&self) -> &'static str;

    /// Generate/refresh whatever config makes this vendor load `skills` in the
    /// given `scope`. `project_id` is the stable, path-independent id from the
    /// lockfile.
    ///
    /// `previously_managed` lists the skill names spm materialized on the last
    /// sync (from the prior lockfile). Project vendors ignore it — they own
    /// their whole dir and rebuild it from scratch — but global vendors that
    /// share a directory with the user's own skills use it to remove only the
    /// entries spm previously owned but that were since dropped from the
    /// manifest, never touching the user's hand-authored skills.
    fn materialize(
        &self,
        scope: &Scope,
        project_id: &str,
        skills: &[MaterializedSkill],
        previously_managed: &[String],
    ) -> Result<()>;

    /// Remove everything this vendor generated for this `scope`. `managed` lists
    /// the skill names spm currently owns (from the lockfile); global vendors
    /// sharing a directory with the user's skills remove only those.
    fn clean(&self, scope: &Scope, project_id: &str, managed: &[String]) -> Result<()>;

    /// Report what this vendor has materialized in the given `scope`, compared
    /// against the declared `expected` skill names (from `ai.lock`).
    fn status(&self, scope: &Scope, expected: &[String]) -> Result<VendorStatus>;

    /// Register full plugins (agents, MCP servers, hooks, scripts) for this
    /// vendor. Only targets that can load more than a `SKILL.md` override this;
    /// the default is a no-op because a plugin's *skills* are already flattened
    /// into the ordinary [`materialize`](Vendor::materialize) list, so every
    /// vendor gets the skills-only subset for free and only richer targets (e.g.
    /// Claude) need to do more here.
    ///
    /// `previously_managed` lists the plugin names spm registered on the last
    /// sync (from the prior lockfile), mirroring
    /// [`materialize`](Vendor::materialize).
    fn materialize_plugins(
        &self,
        _scope: &Scope,
        _project_id: &str,
        _plugins: &[MaterializedPlugin],
        _previously_managed: &[String],
    ) -> Result<()> {
        Ok(())
    }

    /// Report what full plugins this vendor has materialized in `scope`,
    /// compared against the declared plugin names (the `ai.json`/`ai.lock`
    /// plugin keys). Returns `None` for vendors that don't register full plugins
    /// (their plugins contribute only *skills*, already covered by
    /// [`status`](Vendor::status)); richer targets (e.g. Claude) return `Some`
    /// so `spm status` can catch a deleted marketplace dir or a fresh worktree
    /// whose plugin registration was never installed here.
    fn status_plugins(&self, _scope: &Scope, _expected: &[String]) -> Result<Option<VendorStatus>> {
        Ok(None)
    }

    /// Remove everything this vendor registered for full plugins in this
    /// `scope`. Counterpart to [`materialize_plugins`](Vendor::materialize_plugins);
    /// the default is a no-op for targets that never registered any.
    fn clean_plugins(&self, _scope: &Scope, _project_id: &str, _managed: &[String]) -> Result<()> {
        Ok(())
    }
}

/// Every target vendor spm knows how to materialize. Single source of truth for
/// the `for_target` dispatch, the CLI's interactive target picker, and the
/// "supported targets" error text — so those never drift apart. Kept in sync with
/// the `targets` enum in `schema/ai.schema.json` by
/// [`all_targets_match_schema_enum`](tests::all_targets_match_schema_enum).
pub const ALL_TARGETS: &[&str] = &[
    "amp", "claude", "cline", "codex", "copilot", "cursor", "gemini", "windsurf",
];

pub fn for_target(target: &str) -> Result<Box<dyn Vendor>> {
    match target {
        "amp" => Ok(Box::new(shareddir::amp())),
        "claude" => Ok(Box::new(claude::Claude)),
        "cline" => Ok(Box::new(shareddir::cline())),
        "codex" => Ok(Box::new(shareddir::codex())),
        "copilot" => Ok(Box::new(copilot::Copilot)),
        "cursor" => Ok(Box::new(shareddir::cursor())),
        "gemini" => Ok(Box::new(shareddir::gemini())),
        "windsurf" => Ok(Box::new(shareddir::windsurf())),
        other => bail!(
            "unknown target `{other}` (supported: {})",
            ALL_TARGETS.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard the drift surface: every advertised target must actually resolve to
    /// a vendor, and every resolvable name must be advertised. If someone adds a
    /// match arm to `for_target` without listing it in `ALL_TARGETS` (or vice
    /// versa) this fails.
    #[test]
    fn all_targets_resolve_and_report_their_own_name() {
        for t in ALL_TARGETS {
            let v = for_target(t).expect("advertised target must resolve");
            assert_eq!(&v.name(), t, "vendor name must match its target key");
        }
    }

    /// Close the drift surface between the runtime target list and the manifest
    /// schema: the `targets` enum in `schema/ai.schema.json` must be exactly
    /// `ALL_TARGETS`. Adding a vendor without updating the schema (or vice versa)
    /// fails here instead of silently letting `ai.json` accept/reject the wrong
    /// set of targets.
    #[test]
    fn all_targets_match_schema_enum() {
        let schema: serde_json::Value =
            serde_json::from_str(crate::schema::SOURCE).expect("schema is valid JSON");
        let enum_vals = schema["properties"]["targets"]["items"]["enum"]
            .as_array()
            .expect("targets.items.enum must be an array");
        let schema_targets: Vec<&str> = enum_vals
            .iter()
            .map(|v| v.as_str().expect("enum entries are strings"))
            .collect();
        assert_eq!(
            schema_targets, ALL_TARGETS,
            "schema targets enum must match ALL_TARGETS (order included)"
        );
    }
}
