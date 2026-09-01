use super::common::default_name;
use super::common::VersionArg;
use super::sync::sync;
use crate::manifest::{Manifest, SkillSpec};
use crate::scope::Scope;
use crate::{resolver, store};
use anyhow::{bail, Context, Result};

/// The parsed arguments for `spm add`, grouped so the handler takes one request
/// rather than a long positional argument list.
pub(super) struct AddRequest {
    pub(super) git: String,
    pub(super) version: VersionArg,
    pub(super) path: Option<String>,
    pub(super) name: Option<String>,
    pub(super) all: bool,
    pub(super) plugin: bool,
    pub(super) force: bool,
}

pub(super) fn add(scope: &Scope, req: AddRequest) -> Result<()> {
    let AddRequest {
        git,
        version,
        path,
        name,
        all,
        plugin,
        force,
    } = req;
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir)?;
    if let Some(sub) = &path {
        crate::manifest::validate_subpath(sub)?;
    }
    if all {
        add_all(scope, &mut manifest, git, version, path, force)?;
    } else {
        let name = name.unwrap_or_else(|| {
            // Prefer the `--path` basename, but fall back to the git URL when the
            // path yields nothing usable as a skill name (e.g. `.`, `foo/.`, ``).
            path.as_deref()
                .map(default_name)
                .filter(|n| crate::manifest::validate_skill_name(n).is_ok())
                .unwrap_or_else(|| default_name(&git))
        });
        crate::manifest::validate_skill_name(&name)?;
        // A dependency name lives in one flat namespace shared by `skills` and
        // `plugins` (see `Manifest::load`), so a name already taken by the
        // *other* map is always a hard error — even with --force, since --force
        // only authorizes re-pinning an entry of the *same* kind, never turning
        // a skill into a plugin (or vice versa) behind the user's back. Check
        // this before `manifest.save`, otherwise we'd persist an ai.json that
        // `sync`'s re-load immediately rejects, stranding the user with an
        // invalid manifest.
        let (this_map_has, other_map_has, other_kind) = if plugin {
            (
                manifest.plugins.contains_key(&name),
                manifest.skills.contains_key(&name),
                "skill",
            )
        } else {
            (
                manifest.skills.contains_key(&name),
                manifest.plugins.contains_key(&name),
                "plugin",
            )
        };
        if other_map_has {
            bail!(
                "a {other_kind} named `{name}` already exists in {}; dependency names \
                 must be unique across skills and plugins — pick a different name with \
                 `--name <other>` or `spm remove` the existing {other_kind} first",
                Manifest::path_in(&dir).display()
            );
        }
        // Never clobber an existing entry silently: adding a name that already
        // exists is an error unless the user opts in with --force (e.g. to
        // re-pin a version). A plugin and a skill share neither map nor the
        // vendor `<name>` namespace question here, so each `--plugin` add is
        // checked against the plugins map and each skill add against skills.
        let kind = if plugin { "plugin" } else { "skill" };
        let occupied = this_map_has;
        if !force && occupied {
            bail!(
                "a {kind} named `{name}` already exists in {}; either give this one \
                 a different name with `--name <other>`, pass --force to overwrite \
                 the existing entry, or `spm remove {}{name}` first",
                Manifest::path_in(&dir).display(),
                if plugin { "--plugin " } else { "" }
            );
        }
        let spec = SkillSpec {
            git,
            tag: version.tag,
            branch: version.branch,
            commit: version.commit,
            path,
        };
        spec.version()?; // validate exactly one selector
        if plugin {
            manifest.plugins.insert(name.clone(), spec);
        } else {
            manifest.skills.insert(name.clone(), spec);
        }
        manifest.save(&dir)?;
        sync(scope, false, None)?;
        println!("added {kind} {name}");
    }
    Ok(())
}

/// `spm add --all`: treat `path` as a *container* and add every skill inside it
/// (each immediate subdirectory carrying a `SKILL.md`) as its own manifest entry,
/// keyed by the subdirectory name. This is the one-shot equivalent of the
/// per-sub-skill `spm add … --path <sub> --name <sub>` commands that the
/// container warning suggests — so it enumerates sub-skills through the same
/// `skillcheck::child_skills` source of truth, guaranteeing the two never drift.
fn add_all(
    scope: &Scope,
    manifest: &mut Manifest,
    git: String,
    version: VersionArg,
    path: Option<String>,
    force: bool,
) -> Result<()> {
    let dir = scope.manifest_dir()?;
    // Version selector is validated once for the container; every derived entry
    // reuses it verbatim.
    let container = SkillSpec {
        git: git.clone(),
        tag: version.tag,
        branch: version.branch,
        commit: version.commit,
        path: path.clone(),
    };
    container.version()?;

    // Fetch the container into the store so we can enumerate its sub-skills.
    let locked =
        resolver::resolve(&container).context("resolving container to enumerate skills")?;
    let ensured = store::ensure(&locked).context("fetching container to enumerate skills")?;
    let subs = crate::skillcheck::child_skills(&ensured.path);
    if subs.is_empty() {
        bail!(
            "--path `{}` is not a container of skills (no immediate subdirectory has a SKILL.md). \
             Drop --all to add it as a single skill.",
            path.as_deref().unwrap_or(".")
        );
    }

    // Reject the whole batch on the first collision so the manifest is never
    // left half-populated — unless --force opts into overwriting existing
    // entries. Name validation still runs regardless.
    for sub in &subs {
        crate::manifest::validate_skill_name(sub)?;
        if !force && manifest.skills.contains_key(sub) {
            bail!(
                "a skill named `{sub}` already exists in {}; either pass --force to \
                 overwrite the existing entries, or `spm remove {sub}` first",
                Manifest::path_in(&dir).display()
            );
        }
    }

    for sub in &subs {
        let subpath = crate::skillcheck::join_subpath(path.as_deref(), sub);
        manifest.skills.insert(
            sub.clone(),
            SkillSpec {
                git: git.clone(),
                tag: container.tag.clone(),
                branch: container.branch.clone(),
                commit: container.commit.clone(),
                path: Some(subpath),
            },
        );
    }
    manifest.save(&dir)?;
    sync(scope, false, None)?;
    println!("added {} skill(s): {}", subs.len(), subs.join(", "));
    Ok(())
}
