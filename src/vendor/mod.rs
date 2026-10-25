mod claude;
mod dirskills;
mod shareddir;

use crate::ids::DepName;
use crate::materialized::{MaterializedPlugin, MaterializedSkill};
use crate::scope::Scope;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

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
    pub present: Vec<DepName>,
    /// Declared skills that are missing here (e.g. an uninstalled worktree).
    pub missing: Vec<DepName>,
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
pub fn classify(skills_dir: &Path, expected: &[DepName], detect_stale: bool) -> VendorStatus {
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
pub type Claim<'a> = &'a mut dyn FnMut(&DepName) -> Result<()>;

pub trait Vendor {
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
        previously_managed: &[DepName],
        claim: Claim,
    ) -> Result<()>;

    /// Remove everything this vendor generated for this `scope`: deletes each of
    /// [`clean_paths`](Vendor::clean_paths), then drops its
    /// [`registration`](Vendor::registration), if any. `managed` lists the skill
    /// names spm currently owns (from the lockfile).
    fn clean(&self, scope: &Scope, managed: &[DepName]) -> Result<()> {
        remove_paths(&self.clean_paths(scope, managed)?)?;
        match self.registration() {
            Some(r) => r.deregister(scope),
            None => Ok(()),
        }
    }

    /// The paths [`clean`](Vendor::clean) deletes for `scope`. Vendors sharing a
    /// dir with the user's own skills list only the `managed` entries in it.
    fn clean_paths(&self, scope: &Scope, managed: &[DepName]) -> Result<Vec<PathBuf>>;

    /// The paths this vendor loads the `managed` skills (and any metadata they
    /// need, e.g. a marketplace manifest) from in `scope`. Another target's
    /// [`clean_paths`](Vendor::clean_paths) must not delete these, an ancestor,
    /// anything inside them, or anything they reach through a symlink while this
    /// vendor is still configured — e.g. `codex` and `amp` share the project's
    /// `.agents/skills/<name>/`.
    fn skill_paths(&self, scope: &Scope, managed: &[DepName]) -> Result<Vec<PathBuf>>;

    /// The directory this vendor writes each skill into in `scope`, as
    /// `<dir>/<name>/` (also where [`status`](Vendor::status) looks).
    fn skills_dir(&self, scope: &Scope) -> Result<PathBuf>;

    /// The `.gitignore` entries a project-scope [`materialize`](Vendor::materialize)
    /// of `skills` ensures (each with this vendor's comment), so spm's output
    /// stays out of version control. The single source for both.
    fn gitignore_entries(&self, skills: &[DepName]) -> Vec<String>;

    /// Report what this vendor has materialized in the given `scope`, compared
    /// against the declared `expected` skill names (from `ai.lock`).
    fn status(&self, scope: &Scope, expected: &[DepName]) -> Result<VendorStatus>;

    /// The tool config this vendor registers its output in (e.g. Claude's
    /// settings), or `None` when the tool discovers it by location alone.
    fn registration(&self) -> Option<&dyn Registration>;

    /// Full-plugin support (agents, MCP servers, hooks, scripts), or `None` for
    /// a vendor that loads only skills. A plugin's *skills* are flattened into
    /// the ordinary [`materialize`](Vendor::materialize) list either way, so
    /// every vendor gets that subset; only richer targets (e.g. Claude) do more.
    fn plugins(&self) -> Option<&dyn PluginHost>;
}

/// A vendor that registers its output in the tool's own config.
pub trait Registration {
    /// Files this vendor edits *in place* in `scope` (e.g. Claude's settings),
    /// writing through a symlink at their path instead of replacing it — unlike
    /// [`Vendor::clean_paths`], whose final symlink is removed.
    fn patched_files(&self, scope: &Scope) -> Result<Vec<PathBuf>>;

    /// Drop the config pointing this vendor at spm's output (e.g. Claude's
    /// settings entries) without deleting the output itself.
    fn deregister(&self, scope: &Scope) -> Result<()>;
}

/// A vendor that registers full plugins, not just their skills.
pub trait PluginHost {
    /// Register full plugins for `scope`. `previously_managed` lists the plugin
    /// names spm registered on the last sync (from the prior lockfile),
    /// mirroring [`Vendor::materialize`].
    fn materialize_plugins(
        &self,
        scope: &Scope,
        project_id: &str,
        plugins: &[MaterializedPlugin],
        previously_managed: &[DepName],
    ) -> Result<()>;

    /// Report what full plugins this vendor has materialized in `scope`,
    /// compared against the declared plugin names (the `ai.json`/`ai.lock`
    /// plugin keys), so `spm status` can catch a deleted marketplace dir or a
    /// fresh worktree whose plugin registration was never installed here.
    fn status_plugins(&self, scope: &Scope, expected: &[DepName]) -> Result<VendorStatus>;

    /// Remove everything this vendor registered for full plugins in this
    /// `scope`: deletes each of [`plugin_clean_paths`](PluginHost::plugin_clean_paths),
    /// then [`deregister_plugins`](PluginHost::deregister_plugins).
    fn clean_plugins(&self, scope: &Scope, managed: &[DepName]) -> Result<()> {
        remove_paths(&self.plugin_clean_paths(scope, managed)?)?;
        self.deregister_plugins(scope)
    }

    /// The paths [`clean_plugins`](PluginHost::clean_plugins) deletes.
    fn plugin_clean_paths(&self, scope: &Scope, managed: &[DepName]) -> Result<Vec<PathBuf>>;

    /// The paths this vendor loads the `managed` full plugins (and their
    /// metadata) from, protected like [`Vendor::skill_paths`].
    fn plugin_paths(&self, scope: &Scope, managed: &[DepName]) -> Result<Vec<PathBuf>>;

    /// Drop the config registering this vendor's full plugins without deleting
    /// them.
    fn deregister_plugins(&self, scope: &Scope) -> Result<()>;
}

/// Delete each entry in `paths`, ignoring absent ones and leaving anything but
/// a directory or symlink alone. A symlink is removed itself, never followed.
pub fn remove_paths(paths: &[PathBuf]) -> Result<()> {
    for p in paths {
        crate::fsutil::remove_managed_path(p)
            .with_context(|| format!("removing {}", p.display()))?;
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
        "copilot" => Ok(Box::new(shareddir::copilot())),
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
    use crate::ids::dep;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p =
            std::env::temp_dir().join(format!("spm-vendor-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_skill(root: &Path, name: &str) -> MaterializedSkill {
        let src = root.join("src").join(name);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
        MaterializedSkill {
            name: DepName::try_from(name).unwrap(),
            path: src,
            root: root.to_path_buf(),
        }
    }

    /// Copilot owns its whole project dir: a materialize rebuilds it (dropping
    /// anything undeclared), it is ignored as one entry, status flags strays as
    /// stale, clean deletes the dir, and an empty set leaves nothing behind.
    #[test]
    fn copilot_owns_its_whole_project_dir() {
        let root = scratch("copilot-project");
        let scope = Scope::Project { root: root.clone() };
        let v = for_target("copilot").unwrap();
        let dir = v.skills_dir(&scope).unwrap();
        assert_eq!(dir, root.join(".agents/skills/spm-managed-skills"));
        std::fs::create_dir_all(dir.join("old")).unwrap();

        let greet = make_skill(&root, "greet");
        let mut claimed = Vec::new();
        v.materialize(&scope, "id", &[greet], &[], &mut |n| {
            claimed.push(n.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(claimed, [dep("greet")]);
        assert!(dir.join("greet/SKILL.md").is_file());
        assert!(!dir.join("old").exists(), "undeclared entry dropped");
        let ignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert!(ignore
            .lines()
            .any(|l| l == ".agents/skills/spm-managed-skills/"));
        assert_eq!(
            v.gitignore_entries(&[dep("greet")]),
            [".agents/skills/spm-managed-skills/"]
        );
        assert!(v.gitignore_entries(&[]).is_empty());

        std::fs::create_dir_all(dir.join("stray")).unwrap();
        let st = v.status(&scope, &[dep("greet"), dep("gone")]).unwrap();
        assert_eq!(
            (st.present, st.missing, st.stale),
            (
                vec![dep("greet")],
                vec![dep("gone")],
                vec!["stray".to_string()]
            )
        );

        assert_eq!(
            v.clean_paths(&scope, &[dep("greet")]).unwrap(),
            std::slice::from_ref(&dir)
        );
        assert_eq!(
            v.skill_paths(&scope, &[dep("greet")]).unwrap(),
            [dir.join("greet")]
        );
        v.materialize(&scope, "id", &[], &[dep("greet")], &mut |_| Ok(()))
            .unwrap();
        assert!(!dir.exists(), "an empty set leaves no dir");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The global Copilot dir is shared with the user's own skills, so only the
    /// managed entries in it are ever targeted, never the dir itself.
    #[test]
    fn copilot_never_targets_the_whole_global_dir() {
        let v = for_target("copilot").unwrap();
        let dir = crate::paths::home_dir().unwrap().join(".copilot/skills");
        assert_eq!(v.skills_dir(&Scope::Global).unwrap(), dir);
        let managed = [dep("greet")];
        assert_eq!(
            v.clean_paths(&Scope::Global, &managed).unwrap(),
            [dir.join("greet")]
        );
        assert_eq!(
            v.skill_paths(&Scope::Global, &managed).unwrap(),
            [dir.join("greet")]
        );
        assert!(v.status(&Scope::Global, &managed).unwrap().stale.is_empty());
    }

    /// Cleanup removes a managed entry that is a symlink, never its target.
    #[cfg(unix)]
    #[test]
    fn removing_a_symlinked_entry_keeps_its_target() {
        let tmp = scratch("symlink-remove");
        let target = tmp.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.md"), "keep\n").unwrap();
        let link = tmp.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        remove_paths(std::slice::from_ref(&link)).unwrap();

        assert!(std::fs::symlink_metadata(&link).is_err(), "link removed");
        assert!(target.join("keep.md").is_file(), "target kept");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

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
