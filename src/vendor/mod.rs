mod claude;
mod copilot;
mod dirskills;
mod shareddir;

use crate::scope::Scope;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// A resolved skill ready to be wired into a vendor: a name plus the absolute
/// path to its content in the global store.
pub struct MaterializedSkill {
    pub name: String,
    pub path: PathBuf,
    /// Canonical repo checkout root that bounds symlink following when this
    /// skill is copied into a vendor dir (see [`crate::store::Ensured::root`]).
    pub root: PathBuf,
}

/// A resolved full plugin ready to be registered with a vendor that supports
/// more than skills: a name (the manifest key) plus the absolute path to the
/// plugin root in the global store (the dir holding `.claude-plugin/plugin.json`,
/// `agents/`, `skills/`, scripts, hooks, …).
pub struct MaterializedPlugin {
    pub name: String,
    pub path: PathBuf,
    /// Canonical repo checkout root that bounds symlink following when this
    /// plugin is copied into a vendor dir (see [`crate::store::Ensured::root`]).
    pub root: PathBuf,
}

/// A per-target snapshot of what is materialized in the *current* checkout vs.
/// what `ai.lock` declares. Powers `spm status` and, in particular, the
/// "fresh worktree/clone not installed here" diagnostic: because spm materializes
/// skills into gitignored dirs, a new git worktree sees nothing until
/// `spm install` runs inside it.
#[derive(Serialize)]
pub struct VendorStatus {
    /// Where this vendor materializes skills in the project (project-local).
    #[serde(serialize_with = "crate::jsonutil::serialize_path")]
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
/// Records that spm is about to modify the named skill entry (see
/// [`Vendor::materialize`]).
pub type Claim<'a> = &'a mut dyn FnMut(&str) -> Result<()>;

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
    ///
    /// `claim(name)` must be called right before the vendor modifies (removes
    /// or writes) a skill entry `name` in a dir it shares with the user, so the
    /// ownership record (`crate::owned`) covers everything spm touched even if
    /// a later step fails — and nothing it never reached.
    fn materialize(
        &self,
        scope: &Scope,
        project_id: &str,
        skills: &[MaterializedSkill],
        previously_managed: &[String],
        claim: Claim,
    ) -> Result<()>;

    /// Remove everything this vendor generated for this `scope`: deletes each of
    /// [`clean_paths`](Vendor::clean_paths), then [`deregister`](Vendor::deregister)s.
    /// `managed` lists the skill names spm currently owns (from the lockfile).
    fn clean(&self, scope: &Scope, managed: &[String]) -> Result<()> {
        remove_paths(&self.clean_paths(scope, managed)?)?;
        self.deregister(scope)
    }

    /// The paths [`clean`](Vendor::clean) deletes for `scope`. Vendors sharing a
    /// dir with the user's own skills list only the `managed` entries in it.
    fn clean_paths(&self, scope: &Scope, managed: &[String]) -> Result<Vec<PathBuf>>;

    /// The paths this vendor loads the `managed` skills (and any metadata they
    /// need, e.g. a marketplace manifest) from in `scope`. Another target's
    /// [`clean_paths`](Vendor::clean_paths) must not delete these, an ancestor,
    /// anything inside them, or anything they reach through a symlink while this
    /// vendor is still configured — e.g. `codex` and `amp` share the project's
    /// `.agents/skills/<name>/`.
    fn skill_paths(&self, scope: &Scope, managed: &[String]) -> Result<Vec<PathBuf>>;

    /// The directory this vendor writes each skill into in `scope`, as
    /// `<dir>/<name>/` (also where [`status`](Vendor::status) looks).
    fn skills_dir(&self, scope: &Scope) -> Result<PathBuf>;

    /// Files this vendor edits *in place* in `scope` (e.g. Claude's settings),
    /// writing through a symlink at their path instead of replacing it — unlike
    /// [`clean_paths`](Vendor::clean_paths), whose final symlink is removed.
    /// Default: none.
    fn patched_files(&self, _scope: &Scope) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    /// Drop the config pointing this vendor at spm's output (e.g. Claude's
    /// settings entries) without deleting the output itself. Default: nothing to
    /// drop.
    fn deregister(&self, _scope: &Scope) -> Result<()> {
        Ok(())
    }

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
    /// `scope`: deletes each of [`plugin_clean_paths`](Vendor::plugin_clean_paths),
    /// then [`deregister_plugins`](Vendor::deregister_plugins). Counterpart to
    /// [`materialize_plugins`](Vendor::materialize_plugins).
    fn clean_plugins(&self, scope: &Scope, managed: &[String]) -> Result<()> {
        remove_paths(&self.plugin_clean_paths(scope, managed)?)?;
        self.deregister_plugins(scope)
    }

    /// The paths [`clean_plugins`](Vendor::clean_plugins) deletes. Default: none,
    /// for targets that never register full plugins.
    fn plugin_clean_paths(&self, _scope: &Scope, _managed: &[String]) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    /// The paths this vendor loads the `managed` full plugins (and their
    /// metadata) from, protected like [`skill_paths`](Vendor::skill_paths).
    /// Default: none.
    fn plugin_paths(&self, _scope: &Scope, _managed: &[String]) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    /// Drop the config registering this vendor's full plugins without deleting
    /// them. Default: nothing to drop.
    fn deregister_plugins(&self, _scope: &Scope) -> Result<()> {
        Ok(())
    }
}

/// Delete each directory in `paths`, ignoring absent ones. A symlink is removed
/// itself, never followed.
pub fn remove_paths(paths: &[PathBuf]) -> Result<()> {
    for p in paths {
        if p.is_dir() {
            std::fs::remove_dir_all(p).with_context(|| format!("removing {}", p.display()))?;
        }
    }
    Ok(())
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
