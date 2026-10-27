//! The contract every [`Vendor`] must honor, run against each [`ALL_TARGETS`]
//! entry (via [`for_target`]) in every scope — so a new target, e.g. another
//! `SharedDirVendor` preset, is covered without being listed here.
//!
//! Each run gets its own sandbox: the project root, the user's home and
//! `$SPM_HOME` all live under it (the global ones redirected per thread by
//! [`crate::paths::sandbox`]), and the whole sandbox is snapshotted around every
//! step so any write outside the vendor's declared outputs is caught.
#![cfg(test)]

use super::{for_target, Vendor, ALL_TARGETS};
use crate::ids::{dep, DepName};
use crate::materialized::{MaterializedPlugin, MaterializedSkill};
use crate::scope::Scope;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
enum Entry {
    Dir,
    File(Vec<u8>),
    Symlink(PathBuf),
}

type Snapshot = BTreeMap<PathBuf, Entry>;

fn snapshot(root: &Path) -> Snapshot {
    fn walk(dir: &Path, out: &mut Snapshot) {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let entry = if meta.file_type().is_symlink() {
                Entry::Symlink(std::fs::read_link(&path).unwrap())
            } else if meta.is_dir() {
                walk(&path, out);
                Entry::Dir
            } else {
                Entry::File(std::fs::read(&path).unwrap())
            };
            out.insert(path, entry);
        }
    }
    let mut out = Snapshot::new();
    walk(root, &mut out);
    out
}

/// Paths added, removed or modified between two snapshots.
fn changed(before: &Snapshot, after: &Snapshot) -> BTreeSet<PathBuf> {
    before
        .keys()
        .chain(after.keys())
        .filter(|p| before.get(*p) != after.get(*p))
        .cloned()
        .collect()
}

/// A scratch tree holding the project, home and `$SPM_HOME` of one run.
struct Sandbox {
    root: PathBuf,
    project: PathBuf,
    home: PathBuf,
    store: PathBuf,
    _redirect: crate::paths::sandbox::Guard,
}

impl Sandbox {
    fn new(target: &str, scope: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "spm-vendor-contract-{target}-{scope}-{}-{nanos}",
            std::process::id()
        ));
        let (project, home, spm_home) = (root.join("project"), root.join("home"), root.join("spm"));
        let store = spm_home.join("store");
        for d in [&project, &home, &store] {
            std::fs::create_dir_all(d).unwrap();
        }
        let _redirect = crate::paths::sandbox::Guard::enter(&home, &spm_home);
        Sandbox {
            root,
            project,
            home,
            store,
            _redirect,
        }
    }

    fn write(&self, path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// A store skill with a nested file, so copies are checked as whole trees.
    fn skill(&self, name: &str) -> MaterializedSkill {
        let path = self.store.join("skills").join(name);
        self.write(
            &path.join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: {name}\n---\n"),
        );
        self.write(
            &path.join("references/notes.md"),
            &format!("{name} notes\n"),
        );
        MaterializedSkill {
            name: dep(name),
            path,
            root: self.store.clone(),
        }
    }

    /// A store plugin carrying agents and a bundled skill.
    fn plugin(&self, name: &str) -> MaterializedPlugin {
        let path = self.store.join("plugins").join(name);
        self.write(
            &path.join(".claude-plugin/plugin.json"),
            &json!({ "name": name, "skills": "./skills" }).to_string(),
        );
        self.write(&path.join("agents/helper.md"), "agent\n");
        self.write(
            &path.join("skills/inner/SKILL.md"),
            "---\nname: inner\n---\n",
        );
        MaterializedPlugin {
            name: dep(name),
            path,
            root: self.store.clone(),
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// User config already present in each file a [`Registration`](super::Registration)
/// vendor patches, nested into the objects such vendors merge their entries into.
fn user_config() -> Value {
    json!({
        "userKey": "keep",
        "enabledPlugins": { "user-plugin@user-market": true },
        "extraKnownMarketplaces": {
            "user-market": { "source": { "source": "github", "repo": "user/market" } }
        },
    })
}

const USER_GITIGNORE: &str = "# user\nnode_modules/\n";

fn names(v: &[MaterializedSkill]) -> Vec<DepName> {
    v.iter().map(|s| s.name.clone()).collect()
}

/// Every path the vendor declares it may write to in `scope` for `skills` and
/// `plugins`.
fn declared_outputs(
    v: &dyn Vendor,
    scope: &Scope,
    skills: &[DepName],
    plugins: &[DepName],
) -> Vec<PathBuf> {
    let mut out = v.clean_paths(scope, skills).unwrap();
    out.extend(v.skill_paths(scope, skills).unwrap());
    if let Some(r) = v.registration() {
        out.extend(r.patched_files(scope).unwrap());
    }
    if let Some(p) = v.plugins() {
        out.extend(p.plugin_clean_paths(scope, plugins).unwrap());
        out.extend(p.plugin_paths(scope, plugins).unwrap());
    }
    if let Scope::Project { root } = scope {
        if !v.gitignore_entries(skills).is_empty() {
            out.push(root.join(".gitignore"));
        }
    }
    out
}

/// Inside a declared output, or a directory created on the way to one.
fn is_declared(path: &Path, after: &Snapshot, declared: &[PathBuf]) -> bool {
    declared.iter().any(|d| {
        path.starts_with(d) || (d.starts_with(path) && after.get(path) == Some(&Entry::Dir))
    })
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Every leaf of `user` is present, unchanged, in `got`.
fn assert_contains_user_config(got: &Value, user: &Value, ctx: &str) {
    match user {
        Value::Object(m) => {
            for (k, v) in m {
                assert_contains_user_config(&got[k], v, &format!("{ctx}.{k}"));
            }
        }
        leaf => assert_eq!(got, leaf, "user config clobbered at {ctx}"),
    }
}

fn materialize(
    v: &dyn Vendor,
    scope: &Scope,
    skills: &[MaterializedSkill],
    plugins: &[MaterializedPlugin],
    previous: (&[DepName], &[DepName]),
) -> BTreeSet<DepName> {
    let mut claimed = BTreeSet::new();
    v.materialize(scope, "spm-contract", skills, previous.0, &mut |n| {
        claimed.insert(n.clone());
        Ok(())
    })
    .unwrap();
    if let Some(p) = v.plugins() {
        p.materialize_plugins(scope, "spm-contract", plugins, previous.1)
            .unwrap();
    }
    claimed
}

fn clean(v: &dyn Vendor, scope: &Scope, skills: &[DepName], plugins: &[DepName]) {
    v.clean(scope, skills).unwrap();
    if let Some(p) = v.plugins() {
        p.clean_plugins(scope, plugins).unwrap();
    }
}

/// Whether the files a [`Registration`](super::Registration) vendor patches
/// already hold [`user_config`] before the first materialize.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Settings {
    Seeded,
    Absent,
}

fn check_contract(target: &str, global: bool, settings: Settings) {
    let label = if global { "global" } else { "project" };
    let ctx = match settings {
        Settings::Seeded => format!("[{target} / {label}]"),
        Settings::Absent => format!("[{target} / {label} / no settings]"),
    };
    let sb = Sandbox::new(target, &format!("{label}-{settings:?}"));
    let scope = Scope::new(global, sb.project.clone());
    let v = for_target(target).unwrap();
    let v = v.as_ref();

    let skills = [sb.skill("alpha"), sb.skill("beta")];
    let plugins: Vec<_> = v.plugins().map(|_| sb.plugin("plug")).into_iter().collect();
    let skill_names = names(&skills);
    let plugin_names: Vec<DepName> = plugins.iter().map(|p| p.name.clone()).collect();
    let declared = declared_outputs(v, &scope, &skill_names, &plugin_names);

    // The user's own content, which spm must never touch. A hand-authored
    // skill survives unless spm owns the whole skills dir.
    let skills_dir = v.skills_dir(&scope).unwrap();
    let clean_paths = v.clean_paths(&scope, &skill_names).unwrap();
    let mut owned = clean_paths.clone();
    if let Some(p) = v.plugins() {
        owned.extend(p.plugin_clean_paths(&scope, &plugin_names).unwrap());
    }
    let user_skill = skills_dir.join("user-skill");
    sb.write(&user_skill.join("SKILL.md"), "user\n");
    let patched = v
        .registration()
        .map(|r| r.patched_files(&scope).unwrap())
        .unwrap_or_default();
    if settings == Settings::Seeded {
        for f in &patched {
            sb.write(f, &serde_json::to_string_pretty(&user_config()).unwrap());
        }
    }
    sb.write(&sb.project.join(".gitignore"), USER_GITIGNORE);
    sb.write(&sb.project.join("README.md"), "user\n");
    sb.write(&sb.home.join(".config/user-tool/config"), "user\n");
    let before = snapshot(&sb.root);

    // Materialize writes only declared outputs and claims exactly its skills.
    let claimed = materialize(v, &scope, &skills, &plugins, (&[], &[]));
    assert_eq!(
        claimed,
        skill_names.iter().cloned().collect(),
        "{ctx} claims"
    );
    let materialized = snapshot(&sb.root);
    let shared = user_skill.exists();
    if global && skills_dir.starts_with(&sb.home) {
        assert!(shared, "{ctx} a skills dir in the user's home is shared");
    }
    if !shared {
        assert!(
            clean_paths.iter().any(|c| skills_dir.starts_with(c)),
            "{ctx} dropped a user skill from a dir it does not clean as a whole"
        );
    }
    for p in changed(&before, &materialized) {
        assert!(
            is_declared(&p, &materialized, &declared),
            "{ctx} materialize touched undeclared {}",
            p.display()
        );
    }
    if settings == Settings::Seeded {
        for f in &patched {
            assert_contains_user_config(&read_json(f), &user_config(), &format!("{ctx} {f:?}"));
        }
    }
    let gitignore = std::fs::read_to_string(sb.project.join(".gitignore")).unwrap();
    assert!(gitignore.starts_with(USER_GITIGNORE), "{ctx} {gitignore}");
    if let Scope::Project { root } = &scope {
        let entries = v.gitignore_entries(&skill_names);
        let missing = crate::gitignore::missing(root, &entries).unwrap();
        assert!(missing.is_empty(), "{ctx} not gitignored: {missing:?}");
    } else {
        assert_eq!(
            gitignore, USER_GITIGNORE,
            "{ctx} global writes no .gitignore"
        );
    }

    // Status reports exactly what was just materialized.
    let st = v.status(&scope, &skill_names).unwrap();
    assert_eq!(st.location, skills_dir, "{ctx}");
    assert_eq!(st.present, skill_names, "{ctx} present");
    assert!(st.missing.is_empty(), "{ctx} missing: {:?}", st.missing);
    assert!(st.stale.is_empty(), "{ctx} stale: {:?}", st.stale);
    assert!(st.notes.is_empty(), "{ctx} notes: {:?}", st.notes);
    if let Some(p) = v.plugins() {
        let st = p.status_plugins(&scope, &plugin_names).unwrap();
        assert_eq!(st.present, plugin_names, "{ctx} plugins present");
        assert!(st.missing.is_empty(), "{ctx} plugins missing");
        assert!(st.stale.is_empty(), "{ctx} plugins stale: {:?}", st.stale);
        assert!(st.notes.is_empty(), "{ctx} plugins notes: {:?}", st.notes);
    }

    // Re-materializing the same set is byte-for-byte stable.
    materialize(v, &scope, &skills, &plugins, (&skill_names, &plugin_names));
    assert_eq!(
        changed(&materialized, &snapshot(&sb.root)),
        BTreeSet::new(),
        "{ctx} re-materialize changed the tree"
    );

    // Clean removes everything spm wrote and restores the user's config; only
    // the directories created on the way, the patched files and `.gitignore`
    // (left on purpose, with spm's entries) may remain.
    clean(v, &scope, &skill_names, &plugin_names);
    let cleaned = snapshot(&sb.root);
    for p in &owned {
        assert!(
            std::fs::symlink_metadata(p).is_err(),
            "{ctx} clean left {}",
            p.display()
        );
    }
    // A patched file spm created is left behind as an empty object: every
    // entry it registered, and the now-empty objects holding them, are gone.
    let restored = match settings {
        Settings::Seeded => user_config(),
        Settings::Absent => json!({}),
    };
    for f in &patched {
        assert_eq!(read_json(f), restored, "{ctx} {f:?} after clean");
    }
    assert_eq!(
        std::fs::read_to_string(sb.project.join(".gitignore")).unwrap(),
        gitignore,
        "{ctx} clean changed .gitignore"
    );
    if shared {
        assert!(
            user_skill.join("SKILL.md").is_file(),
            "{ctx} user skill cleaned"
        );
    }
    for p in changed(&before, &cleaned) {
        let left_dir = cleaned.get(&p) == Some(&Entry::Dir) && !before.contains_key(&p);
        let removed_owned = !cleaned.contains_key(&p) && owned.iter().any(|o| p.starts_with(o));
        let ok = (left_dir && is_declared(&p, &cleaned, &declared))
            || removed_owned
            || patched.contains(&p)
            || p == sb.project.join(".gitignore") && cleaned.get(&p) == materialized.get(&p);
        assert!(ok, "{ctx} clean left a change at {}", p.display());
    }
    let st = v.status(&scope, &skill_names).unwrap();
    assert!(st.present.is_empty(), "{ctx} present after clean");
    assert_eq!(st.missing, skill_names, "{ctx} missing after clean");
    if let Some(p) = v.plugins() {
        let st = p.status_plugins(&scope, &plugin_names).unwrap();
        assert_eq!(
            st.missing, plugin_names,
            "{ctx} plugins missing after clean"
        );
    }

    // Clean is idempotent.
    clean(v, &scope, &skill_names, &plugin_names);
    assert_eq!(
        changed(&cleaned, &snapshot(&sb.root)),
        BTreeSet::new(),
        "{ctx} second clean changed the tree"
    );
}

#[test]
fn every_vendor_honors_the_contract_in_project_scope() {
    assert!(!ALL_TARGETS.is_empty());
    for target in ALL_TARGETS {
        check_contract(target, false, Settings::Seeded);
    }
}

#[test]
fn every_vendor_honors_the_contract_in_global_scope() {
    assert!(!ALL_TARGETS.is_empty());
    for target in ALL_TARGETS {
        check_contract(target, true, Settings::Seeded);
    }
}

/// Without pre-existing settings, clean must also drop the containers spm's
/// registration created, not just its own entries.
#[test]
fn every_registration_vendor_cleans_settings_it_created() {
    let registering: Vec<_> = ALL_TARGETS
        .iter()
        .filter(|t| for_target(t).unwrap().registration().is_some())
        .collect();
    assert!(!registering.is_empty());
    for target in registering {
        for global in [false, true] {
            check_contract(target, global, Settings::Absent);
        }
    }
}
