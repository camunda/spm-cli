//! `spm doctor`.

use crate::common::*;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

/// Every file under `dir` with its bytes (empty when `dir` is missing).
fn tree(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                files.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    files
}

/// A healthy install passes every check, in text and JSON, and doctor writes
/// nothing: not the project, not its targets, not the store.
#[test]
fn doctor_passes_a_healthy_project_and_writes_nothing() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,copilot,gemini"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let (project, store) = (tree(&sb.project), tree(&sb.spm_home.join("store")));

    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["scope"], "project");
    assert_eq!(doc["errors"], 0);
    assert_eq!(doc["online"], false);
    for id in [
        "git",
        "store",
        "manifest",
        "targets",
        "machine-policy",
        "policy",
        "lockfile",
        "materialized",
        "gitignore",
        "skills",
    ] {
        check(&doc, id, "ok");
    }
    assert!(checks(&doc, "remotes").is_empty(), "offline by default");

    let text = sb.ok(&["doctor"]);
    assert!(text.contains("✓ ai.lock matches ai.json"), "{text}");
    assert!(text.contains("0 errors"), "{text}");

    assert_eq!(project, tree(&sb.project), "doctor changed the project");
    assert_eq!(
        store,
        tree(&sb.spm_home.join("store")),
        "doctor changed the store"
    );
}

#[test]
fn doctor_without_manifest_points_at_init_for_the_scope() {
    let sb = Sandbox::new();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    assert_eq!(check(&doc, "manifest", "error")["fix"], "spm init");
    check(&doc, "git", "ok");

    let (ok, doc) = doctor(&sb, &["-g"]);
    assert!(!ok);
    assert_eq!(doc["scope"], "global");
    assert_eq!(check(&doc, "manifest", "error")["fix"], "spm init --global");

    let out = sb.spm(&["doctor"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("✗ no ai.json"), "{text}");
    assert!(text.contains("fix: spm init"), "{text}");
    assert!(stderr_of(&out).contains("spm doctor found 1 problem"));
}

#[test]
fn doctor_reports_an_invalid_manifest() {
    let sb = Sandbox::new();
    write_manifest(&sb, r#"{"targets":["nope"],"skills":{}}"#);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    let c = check(&doc, "manifest", "error");
    assert!(c["message"].as_str().unwrap().contains("nope"), "{c}");
}

/// The lock verdict is exactly `install --locked`'s: same mismatch text.
#[test]
fn doctor_flags_a_lock_out_of_sync_and_a_missing_lock() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let manifest = sb.read("ai.json").replace("v0.1.0", "v0.2.0");
    write_manifest(&sb, &manifest);

    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    let c = check(&doc, "lockfile", "error");
    assert_eq!(c["fix"], "spm install and commit ai.lock");
    let detail = c["details"][0].as_str().unwrap();
    let err = fails(&sb, &["install", "--locked"]);
    assert!(err.contains(detail), "{detail} vs {err}");

    std::fs::remove_file(sb.project.join("ai.lock")).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    let c = check(&doc, "lockfile", "error");
    assert!(c["message"].as_str().unwrap().contains("no ai.lock"), "{c}");
}

/// Doctor and `install --locked` render one verdict: for every class of lock
/// problem, both reject the lock and doctor details exactly the problems
/// `install --locked` lists.
#[test]
fn doctor_and_locked_install_report_the_same_lock_problem_class() {
    fn greet(sb: &Sandbox) {
        sb.ok(&["init", "--target", "copilot"]);
        sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    }
    fn edit_lock(sb: &Sandbox, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
        edit(&mut lock);
        std::fs::write(sb.project.join("ai.lock"), lock.to_string()).unwrap();
    }
    // Each case drifts a fresh project and returns the problem it expects.
    type Drift = fn(&Sandbox) -> String;
    let cases: [(&str, Drift); 5] = [
        ("missing id", |sb| {
            greet(sb);
            edit_lock(sb, |l| {
                l.as_object_mut().unwrap().remove("id");
            });
            "ai.lock has no project `id`".into()
        }),
        ("drifted selector", |sb| {
            greet(sb);
            write_manifest(sb, &sb.read("ai.json").replace("v0.1.0", "v0.2.0"));
            "skill `greet`: changed since ai.lock was written".into()
        }),
        ("stale entry", |sb| {
            greet(sb);
            write_manifest(sb, r#"{"targets":["copilot"]}"#);
            "skill `greet`: in ai.lock but not declared in ai.json".into()
        }),
        ("unresolved transitive", |sb| {
            let leaf = make_repo(
                &sb.root.join("leaf"),
                &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")],
            );
            let top = make_repo(
                &sb.root.join("top"),
                &[
                    ("SKILL.md", "---\nname: top\n---\nTop.\n"),
                    (
                        "ai.json",
                        &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#),
                    ),
                ],
            );
            sb.ok(&["init", "--target", "copilot"]);
            write_manifest(
                sb,
                &format!(
                    r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"top":{{"git":"{top}","branch":"main"}}}}}}"#
                ),
            );
            sb.ok(&["install"]);
            let mut leaf_name = String::new();
            edit_lock(sb, |l| {
                let skills = l["skills"].as_object_mut().unwrap();
                leaf_name = skills
                    .keys()
                    .find(|k| k.starts_with("top__leaf-"))
                    .unwrap()
                    .clone();
                skills.remove(&leaf_name);
            });
            format!(
                "transitive skill `{leaf_name}` (`leaf` required by `top`): missing from ai.lock"
            )
        }),
        ("lock spm would rewrite", |sb| {
            sb.add_plugin();
            sb.ok(&["init", "--target", "copilot"]);
            sb.ok(&[
                "add",
                &sb.skill_url(),
                "--plugin",
                "--branch",
                "main",
                "--path",
                "pkg",
                "--name",
                "ds",
            ]);
            edit_lock(sb, |l| {
                l["plugins"]["ds"]["bundled_skills"] = serde_json::json!([])
            });
            "ai.lock differs from what spm would write".into()
        }),
    ];
    for (class, drift) in cases {
        let sb = Sandbox::new();
        let expected = drift(&sb);
        let err = fails(&sb, &["install", "--locked"]);
        let (ok, doc) = doctor(&sb, &[]);
        assert!(!ok, "{class}: {doc:#}");
        let c = check(&doc, "lockfile", "error");
        let details: Vec<&str> = c["details"]
            .as_array()
            .unwrap_or_else(|| panic!("{class}: {c}"))
            .iter()
            .map(|d| d.as_str().unwrap())
            .collect();
        assert!(
            details.iter().any(|d| d.starts_with(&expected)),
            "{class}: {details:?}"
        );
        assert!(
            err.contains(&format!(
                "--locked: ai.lock is out of sync with ai.json ({} problem(s)):",
                details.len()
            )),
            "{class}: {err}"
        );
        for d in &details {
            assert!(err.contains(&format!("  - {d}\n")), "{class}: {d} vs {err}");
        }
    }

    // A missing lock is its own class.
    let sb = Sandbox::new();
    greet(&sb);
    std::fs::remove_file(sb.project.join("ai.lock")).unwrap();
    let err = fails(&sb, &["install", "--locked"]);
    assert!(err.contains("ai.lock not found"), "{err}");
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    assert_eq!(
        check_message(&doc, "lockfile", "error"),
        "no ai.lock: declared dependencies are not pinned"
    );
}

/// Offline, doctor never fetches: a dependency missing from the store is
/// reported as unverified (a warning), and the store stays empty.
#[test]
fn doctor_never_fetches_and_reports_what_it_could_not_verify() {
    let sb = Sandbox::new();
    let leaf = make_repo(
        &sb.root.join("leaf"),
        &[("SKILL.md", "---\nname: leaf\ndescription: Leaf.\n---\n")],
    );
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\ndescription: Top.\n---\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    check(&doc, "lockfile", "ok");

    sb.ok(&["prune", "--yes"]);
    assert_eq!(sb.store_entries(), 0);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "unverified is a warning, not an error: {doc:#}");
    check(&doc, "lockfile", "ok");
    let c = check(&doc, "lockfile", "warn");
    assert_eq!(c["details"], serde_json::json!(["top"]), "{c}");
    assert_eq!(sb.store_entries(), 0, "doctor must not fetch");
}

/// A shared transitive child is named after the first root that reaches it.
/// Offline, a root missing from the store can't be walked, so a later root
/// reaches the child "first" under another name: that is not drift, and doctor
/// must not call it an error that `install --locked` would never raise.
#[test]
fn doctor_does_not_flag_a_diamond_whose_first_root_is_not_in_the_store() {
    let sb = Sandbox::new();
    let shared = make_repo(
        &sb.root.join("shared"),
        &[("SKILL.md", "---\nname: shared\ndescription: S.\n---\n")],
    );
    let dep_json = format!(r#"{{"skills":{{"shared":{{"git":"{shared}","branch":"main"}}}}}}"#);
    let root = |name: &str| {
        make_repo(
            &sb.root.join(name),
            &[
                (
                    "SKILL.md",
                    &format!("---\nname: {name}\ndescription: R.\n---\n"),
                ),
                ("ai.json", &dep_json),
            ],
        )
    };
    let (left, right) = (root("left"), root("right"));
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"left":{{"git":"{left}","branch":"main"}},"right":{{"git":"{right}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    remove_store_checkout(&sb, "left-");

    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    check(&doc, "lockfile", "ok");
    assert_eq!(
        check(&doc, "lockfile", "warn")["details"],
        serde_json::json!(["left"])
    );
    sb.ok(&["install", "--locked"]);
}

/// Write permission for *someone* is not enough: a store the invoking user
/// can't write (here group-writable, owner read-only) fails `spm install`, so
/// doctor must flag it rather than read the mode bits.
#[cfg(unix)]
#[test]
fn doctor_flags_a_store_the_current_user_cannot_write() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let store = sb.spm_home.join("store");
    let set = |mode| std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode));
    set(0o575).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    set(0o755).unwrap();
    assert!(!ok, "{doc:#}");
    let c = check(&doc, "store", "error");
    assert!(
        c["message"].as_str().unwrap().contains("not writable"),
        "{c}"
    );
}

/// A missing store is fine only if `spm install` can create it.
#[cfg(unix)]
#[test]
fn doctor_flags_a_missing_store_that_cannot_be_created() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    std::fs::remove_dir_all(sb.spm_home.join("store")).ok();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    check(&doc, "store", "ok");

    let set = |mode| std::fs::set_permissions(&sb.spm_home, std::fs::Permissions::from_mode(mode));
    set(0o555).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    set(0o755).unwrap();
    assert!(!ok, "{doc:#}");
    let c = check(&doc, "store", "error");
    assert!(
        c["message"].as_str().unwrap().contains("can't be created"),
        "{c}"
    );
}

/// A relative `SPM_HOME` resolves against the working directory, so a
/// missing one is only creatable if that directory is writable.
#[cfg(unix)]
#[test]
fn doctor_flags_a_missing_relative_store_in_a_read_only_cwd() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    let set = |mode| std::fs::set_permissions(&sb.project, std::fs::Permissions::from_mode(mode));
    set(0o555).unwrap();
    let mut cmd = sb.command(&["doctor", "--json"]);
    cmd.env("SPM_HOME", "cache");
    let (_, doc) = doctor_json(cmd);
    set(0o755).unwrap();
    let c = check(&doc, "store", "error");
    assert!(
        c["message"].as_str().unwrap().contains("can't be created"),
        "{c}"
    );
}

/// Without the state lock, a concurrent command may be rewriting ai.json,
/// ai.lock, links or target output, so doctor must not diagnose them; checks
/// that read none of that still run.
#[test]
fn doctor_skips_state_checks_when_the_state_lock_fails() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let lock = sb.spm_home.join("links.lock");
    std::fs::remove_file(&lock).ok();
    std::fs::create_dir_all(&lock).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok, "{doc:#}");
    check(&doc, "state-lock", "error");
    check(&doc, "machine-policy", "ok");
    for id in [
        "manifest",
        "lockfile",
        "links",
        "materialized",
        "gitignore",
        "skills",
    ] {
        assert!(checks(&doc, id).is_empty(), "{id} ran unlocked: {doc:#}");
    }
}

/// A dangling `store` symlink occupies the path, so install can't create the
/// store there even when its parent is writable.
#[cfg(unix)]
#[test]
fn doctor_flags_a_dangling_store_symlink() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    let store = sb.spm_home.join("store");
    std::fs::remove_dir_all(&store).ok();
    std::fs::create_dir_all(&sb.spm_home).unwrap();
    std::os::unix::fs::symlink(sb.root.join("gone"), &store).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok, "{doc:#}");
    let c = check(&doc, "store", "error");
    assert!(c["fix"].as_str().unwrap().contains("remove"), "{c}");
    let out = sb.spm(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    assert!(!out.status.success());

    // A dangling ancestor (here SPM_HOME itself) blocks creating the store too.
    std::fs::remove_dir_all(&sb.spm_home).unwrap();
    std::os::unix::fs::symlink(sb.root.join("gone"), &sb.spm_home).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok, "{doc:#}");
    let c = check(&doc, "store", "error");
    assert!(c["fix"].as_str().unwrap().contains("remove"), "{c}");
    let out = sb.spm(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    assert!(!out.status.success());
}

/// A regular file as SPM_HOME can't hold a store, however permissive.
#[cfg(unix)]
#[test]
fn doctor_flags_an_spm_home_that_is_not_a_directory() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    std::fs::remove_dir_all(&sb.spm_home).ok();
    std::fs::write(&sb.spm_home, "").unwrap();
    std::fs::set_permissions(&sb.spm_home, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok, "{doc:#}");
    let c = check(&doc, "store", "error");
    assert!(
        c["message"].as_str().unwrap().contains("not a directory"),
        "{c}"
    );
}

/// Deletes the store checkout whose directory name starts with `prefix`.
fn remove_store_checkout(sb: &Sandbox, prefix: &str) {
    let entry = std::fs::read_dir(sb.spm_home.join("store"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix))
        .unwrap();
    std::fs::remove_dir_all(entry).unwrap();
}

/// A root skill repo `name` whose nested `ai.json` depends on its own
/// `<name>-dep` repo, both on `main`; returns the root's URL.
fn transitive_root(sb: &Sandbox, name: &str) -> String {
    let dep = make_repo(
        &sb.root.join(format!("{name}-dep")),
        &[(
            "SKILL.md",
            &format!("---\nname: {name}-dep\ndescription: D.\n---\n"),
        )],
    );
    make_repo(
        &sb.root.join(name),
        &[
            (
                "SKILL.md",
                &format!("---\nname: {name}\ndescription: R.\n---\n"),
            ),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"{name}-dep":{{"git":"{dep}","branch":"main"}}}}}}"#),
            ),
        ],
    )
}

fn edit_lock(sb: &Sandbox, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = sb.project.join("ai.lock");
    let mut lock: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut lock);
    std::fs::write(&path, serde_json::to_string_pretty(&lock).unwrap()).unwrap();
}

/// A root missing from the store leaves only *its* subtree unverified: drift in
/// an unrelated root's child pin (removed, or its selector changed) is still
/// proven, so doctor must flag it, as `install --locked` does.
#[test]
fn doctor_keeps_lock_drift_of_an_independent_root_when_another_is_not_in_the_store() {
    let sb = Sandbox::new();
    let (alpha, beta) = (transitive_root(&sb, "alpha"), transitive_root(&sb, "beta"));
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"alpha":{{"git":"{alpha}","branch":"main"}},"beta":{{"git":"{beta}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let installed = sb.read("ai.lock");
    let lock: serde_json::Value = serde_json::from_str(&installed).unwrap();
    let beta_child = lock["skills"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, l)| l["requested_by"] == serde_json::json!(["beta"]))
        .map(|(n, _)| n.clone())
        .unwrap();
    remove_store_checkout(&sb, "alpha-");

    // A skipped subtree never adds requesters to a direct root: provenance
    // on a direct pin is always cleared.
    let direct = "skill `beta`".to_string();
    for (change, culprit) in [
        ("selector", &beta_child),
        ("removal", &beta_child),
        ("direct requester", &direct),
    ] {
        std::fs::write(sb.project.join("ai.lock"), &installed).unwrap();
        edit_lock(&sb, |l| match change {
            "selector" => l["skills"][&beta_child]["reference"] = "branch:dev".into(),
            "removal" => {
                l["skills"].as_object_mut().unwrap().remove(&beta_child);
            }
            _ => l["skills"]["beta"]["requested_by"] = serde_json::json!(["alpha"]),
        });
        let (ok, doc) = doctor(&sb, &[]);
        assert!(!ok, "{change}: {doc:#}");
        let c = check(&doc, "lockfile", "error");
        assert!(c["details"].to_string().contains(culprit.as_str()), "{c}");
        assert!(!sb.spm(&["install", "--locked"]).status.success());
        remove_store_checkout(&sb, "alpha-");
    }
}

/// A missing checkout leaves only what it contributes unverified: a cached
/// plugin's hand-edited `bundled_skills` is still drift the whole-lock
/// backstop of `install --locked` rejects, so doctor must flag it too.
/// Two roots share a dependency at different selectors: the lock pins it
/// under the first root's edge and records the second selector as
/// `also_satisfies` evidence. With the first root not in the store, the
/// second root's miss is only uncertain while that evidence still covers its
/// selector; without it `install --locked` fails, so doctor must too.
#[test]
fn doctor_keeps_a_diamond_miss_whose_selector_the_unreached_pin_does_not_cover() {
    let sb = Sandbox::new();
    let shared_dir = sb.root.join("shared");
    let shared = make_repo(
        &shared_dir,
        &[("SKILL.md", "---\nname: shared\ndescription: S.\n---\n")],
    );
    git_in(&shared_dir, &["tag", "v1"]);
    let root = |name: &str, selector: &str| {
        make_repo(
            &sb.root.join(name),
            &[
                (
                    "SKILL.md",
                    &format!("---\nname: {name}\ndescription: R.\n---\n"),
                ),
                (
                    "ai.json",
                    &format!(r#"{{"skills":{{"shared":{{"git":"{shared}",{selector}}}}}}}"#),
                ),
            ],
        )
    };
    let (alpha, beta) = (
        root("alpha", r#""branch":"main""#),
        root("beta", r#""tag":"v1""#),
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"alpha":{{"git":"{alpha}","branch":"main"}},"beta":{{"git":"{beta}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let installed = sb.read("ai.lock");
    let lock: serde_json::Value = serde_json::from_str(&installed).unwrap();
    let pin = lock["skills"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, l)| l["requested_by"] == serde_json::json!(["alpha", "beta"]))
        .map(|(n, _)| n.clone())
        .unwrap();
    assert_eq!(
        lock["skills"][&pin]["also_satisfies"],
        serde_json::json!(["tag:v1"]),
        "{lock:#}"
    );
    remove_store_checkout(&sb, "alpha-");
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "evidence covers beta's selector: {doc:#}");

    edit_lock(&sb, |l| {
        l["skills"][&pin]
            .as_object_mut()
            .unwrap()
            .remove("also_satisfies");
    });
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok, "{doc:#}");
    check(&doc, "lockfile", "error");
    assert!(!sb.spm(&["install", "--locked"]).status.success());
}

#[test]
fn doctor_compares_verified_lock_entries_when_another_root_is_not_in_the_store() {
    let sb = Sandbox::new();
    sb.add_plugin();
    let alpha = transitive_root(&sb, "alpha");
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"alpha":{{"git":"{alpha}","branch":"main"}}}},"plugins":{{"ds":{{"git":"{}","branch":"main","path":"pkg"}}}}}}"#,
            sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    let installed = sb.read("ai.lock");
    remove_store_checkout(&sb, "alpha-");
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "only unverified: {doc:#}");

    // Plugins are never walked, so no unwalked subtree explains a difference.
    for (key, value) in [
        ("bundled_skills", serde_json::json!([])),
        ("also_satisfies", serde_json::json!(["branch:dev"])),
    ] {
        std::fs::write(sb.project.join("ai.lock"), &installed).unwrap();
        edit_lock(&sb, |l| l["plugins"]["ds"][key] = value);
        let (ok, doc) = doctor(&sb, &[]);
        assert!(!ok, "{key}: {doc:#}");
        let c = check(&doc, "lockfile", "error");
        assert!(c["details"].to_string().contains("`ds`"), "{c}");
        assert!(!sb.spm(&["install", "--locked"]).status.success());
        remove_store_checkout(&sb, "alpha-");
    }
}

/// A plugin is never walked, so a plugin missing from the store leaves no
/// skill subtree unwalked: every skill entry is still compared exactly.
#[test]
fn doctor_compares_skill_entries_exactly_when_only_a_plugin_is_not_in_the_store() {
    let sb = Sandbox::new();
    sb.add_plugin();
    let alpha = transitive_root(&sb, "alpha");
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"alpha":{{"git":"{alpha}","branch":"main"}}}},"plugins":{{"ds":{{"git":"{}","branch":"main","path":"pkg"}}}}}}"#,
            sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    let installed = sb.read("ai.lock");
    let lock: serde_json::Value = serde_json::from_str(&installed).unwrap();
    let leaf = lock["skills"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, l)| l["requested_by"] == serde_json::json!(["alpha"]))
        .map(|(n, _)| n.clone())
        .unwrap();
    remove_store_checkout(&sb, "skill-");
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "only unverified: {doc:#}");
    let c = check(&doc, "lockfile", "warn");
    assert!(c["details"].to_string().contains("ds"), "{c}");

    for (key, value) in [
        ("also_satisfies", serde_json::json!(["branch:dev"])),
        ("requested_by", serde_json::json!(["alpha", "ds"])),
    ] {
        std::fs::write(sb.project.join("ai.lock"), &installed).unwrap();
        edit_lock(&sb, |l| l["skills"][leaf.as_str()][key] = value);
        let (ok, doc) = doctor(&sb, &[]);
        assert!(!ok, "{key}: {doc:#}");
        let c = check(&doc, "lockfile", "error");
        assert!(c["details"].to_string().contains("ai.lock differs"), "{c}");
        assert!(!sb.spm(&["install", "--locked"]).status.success());
        remove_store_checkout(&sb, "skill-");
    }
}

/// An unwalked subtree can only add to entries, never change the lock's outer
/// shape, so the partial comparison must still reject a stray top-level field.
#[test]
fn doctor_rejects_extra_top_level_lock_fields_when_a_root_is_not_in_the_store() {
    let sb = Sandbox::new();
    let alpha = transitive_root(&sb, "alpha");
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"alpha":{{"git":"{alpha}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    remove_store_checkout(&sb, "alpha-");
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "only unverified: {doc:#}");
    check(&doc, "lockfile", "warn");

    let installed = sb.read("ai.lock");
    // Plugins are never walked, so their map (even an empty one spm omits)
    // is compared exactly too.
    for (key, value) in [
        ("metadata", serde_json::json!(true)),
        ("plugins", serde_json::json!({})),
    ] {
        std::fs::write(sb.project.join("ai.lock"), &installed).unwrap();
        edit_lock(&sb, |l| l[key] = value);
        let (ok, doc) = doctor(&sb, &[]);
        assert!(!ok, "{key}: {doc:#}");
        let c = check(&doc, "lockfile", "error");
        assert!(c["details"].to_string().contains(key), "{c}");
        assert!(!sb.spm(&["install", "--locked"]).status.success());
        remove_store_checkout(&sb, "alpha-");
    }
}

/// The walk writes provenance lists sorted and deduplicated, so a
/// non-canonical list on a walked entry is drift even while another root's
/// subtree is unwalked.
#[test]
fn doctor_rejects_non_canonical_lists_when_another_root_is_not_in_the_store() {
    // A duplicated requester, and an explicitly empty list (spm omits empty
    // lists), on the transitive child and on the cached direct root.
    let cases: [(&str, &str, serde_json::Value); 3] = [
        ("child", "requested_by", serde_json::json!(["beta", "beta"])),
        ("child", "also_satisfies", serde_json::json!([])),
        ("beta", "also_satisfies", serde_json::json!([])),
    ];
    for (target, key, value) in cases {
        let sb = Sandbox::new();
        let (alpha, beta) = (transitive_root(&sb, "alpha"), transitive_root(&sb, "beta"));
        sb.ok(&["init", "--target", "copilot"]);
        write_manifest(
            &sb,
            &format!(
                r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"alpha":{{"git":"{alpha}","branch":"main"}},"beta":{{"git":"{beta}","branch":"main"}}}}}}"#
            ),
        );
        sb.ok(&["install"]);
        let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
        let child = lock["skills"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, l)| l["requested_by"] == serde_json::json!(["beta"]))
            .map(|(n, _)| n.clone())
            .unwrap();
        let name = if target == "child" {
            child.as_str()
        } else {
            target
        };
        remove_store_checkout(&sb, "alpha-");
        edit_lock(&sb, |l| l["skills"][name][key] = value.clone());
        let (ok, doc) = doctor(&sb, &[]);
        assert!(!ok, "{name}.{key}: {doc:#}");
        check(&doc, "lockfile", "error");
        assert!(
            !sb.spm(&["install", "--locked"]).status.success(),
            "{name}.{key}"
        );
    }
}

#[test]
fn doctor_flags_missing_stale_and_unloadable_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let skills = sb.project.join(".agents/skills/spm-managed-skills");

    std::fs::remove_dir_all(sb.project.join(".agents")).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    let c = check(&doc, "materialized", "error");
    assert_eq!(c["fix"], "spm install");
    assert!(c["details"][0].as_str().unwrap().contains("greet"), "{c}");

    sb.ok(&["install"]);
    std::fs::create_dir_all(skills.join("old")).unwrap();
    std::fs::write(skills.join("greet/SKILL.md"), "# Greet\n").unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    let stale = check(&doc, "materialized", "warn");
    assert!(
        stale["details"][0].as_str().unwrap().contains("old"),
        "{stale}"
    );
    let c = check(&doc, "skills", "warn");
    assert!(
        c["details"][0]
            .as_str()
            .unwrap()
            .contains("no YAML frontmatter"),
        "{c}"
    );
}

#[test]
fn doctor_flags_a_stale_claude_pointer() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let sp = sb.project.join(".claude/settings.local.json");
    let mut settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    settings["extraKnownMarketplaces"]["spm"]["source"]["path"] =
        serde_json::Value::String(sb.root.join("elsewhere").to_string_lossy().into_owned());
    std::fs::write(&sp, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    assert_eq!(check(&doc, "materialized", "error")["fix"], "spm install");
}

/// The expected entries are each vendor's own (`gitignore_entries`).
#[test]
fn doctor_flags_missing_gitignore_entries() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude,gemini"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    std::fs::write(sb.project.join(".gitignore"), "target/\n").unwrap();

    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "a missing entry is a warning: {doc:#}");
    let c = check(&doc, "gitignore", "warn");
    assert_eq!(
        c["details"],
        serde_json::json!(["claude: .spm/", "gemini: .gemini/skills/greet/"]),
        "{c}"
    );
    sb.ok(&["install"]);
    let (_, doc) = doctor(&sb, &[]);
    check(&doc, "gitignore", "ok");
}

/// An install with nothing declared still writes Claude's marketplace, so its
/// ignore entry matters even though no skill is expected; a never-installed
/// project has no output to ignore.
#[test]
fn doctor_checks_gitignore_for_an_empty_claude_install() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    std::fs::write(sb.project.join(".gitignore"), "target/\n").unwrap();
    std::fs::remove_dir_all(sb.project.join(".spm")).ok();
    let (_, doc) = doctor(&sb, &[]);
    assert!(checks(&doc, "gitignore").is_empty(), "{doc:#}");

    sb.ok(&["install"]);
    assert!(sb.project.join(".spm").exists());
    std::fs::write(sb.project.join(".gitignore"), "target/\n").unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    let c = check(&doc, "gitignore", "warn");
    assert_eq!(c["details"], serde_json::json!(["claude: .spm/"]), "{c}");
}

/// The entries being present isn't enough: a later re-include rule, or a
/// managed file committed anyway, still exposes spm's output to git — and
/// `spm install` can't fix either.
#[test]
fn doctor_flags_spm_output_that_git_would_still_commit() {
    let sb = Sandbox::new();
    git_in(&sb.project, &["init", "-q"]);
    sb.ok(&["init", "--target", "claude,gemini"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let ignored = sb.read(".gitignore") + ".claude/settings.local.json\n";
    std::fs::write(sb.project.join(".gitignore"), &ignored).unwrap();
    let all_ok =
        |doc: &serde_json::Value| checks(doc, "gitignore").iter().all(|c| c["status"] == "ok");
    let (_, doc) = doctor(&sb, &[]);
    assert!(all_ok(&doc), "{doc:#}");

    std::fs::write(sb.project.join(".gitignore"), ignored.clone() + "!.spm/\n").unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    let c = check(&doc, "gitignore", "warn");
    assert!(c["details"].to_string().contains("claude: .spm"), "{c}");
    assert!(c["fix"].as_str().unwrap().contains(".gitignore"), "{c}");
    std::fs::write(sb.project.join(".gitignore"), &ignored).unwrap();

    git_in(&sb.project, &["add", "-f", ".gemini/skills/greet/SKILL.md"]);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    let c = check(&doc, "gitignore", "warn");
    assert!(
        c["details"]
            .to_string()
            .contains("gemini: .gemini/skills/greet"),
        "{c}"
    );
    assert_eq!(
        c["fix"], "git --literal-pathspecs rm -r --cached -- .gemini/skills/greet",
        "{c}"
    );
    git_in(
        &sb.project,
        &["rm", "-r", "-q", "--cached", ".gemini/skills/greet"],
    );
    let (_, doc) = doctor(&sb, &[]);
    assert!(all_ok(&doc), "{doc:#}");
}

/// A fix is meant to be pasted into a shell, so names with spaces or shell
/// metacharacters must come out as one safely quoted argument each.
#[cfg(unix)]
#[test]
fn doctor_fix_commands_quote_names_for_the_shell() {
    let sb = Sandbox::new();
    git_in(&sb.project, &["init", "-q"]);
    sb.ok(&["init", "--target", "gemini"]);
    let name = "it's my;skill";
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", name]);
    git_in(
        &sb.project,
        &["add", "-f", &format!(".gemini/skills/{name}/SKILL.md")],
    );
    let (_, doc) = doctor(&sb, &[]);
    let fix = check(&doc, "gitignore", "warn")["fix"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        fix,
        r#"git --literal-pathspecs rm -r --cached -- '.gemini/skills/it'\''s my;skill'"#
    );
    run_fix(&sb, &fix);
    let (_, doc) = doctor(&sb, &[]);
    check(&doc, "gitignore", "ok");
}

/// Run a suggested `fix` through `sh` in the project, with this build's `spm`
/// first on `PATH` and the sandbox's environment.
#[cfg(unix)]
fn run_fix(sb: &Sandbox, fix: &str) {
    let bin = Path::new(env!("CARGO_BIN_EXE_spm")).parent().unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Command::new("sh")
        .args(["-c", fix])
        .current_dir(&sb.project)
        .env("PATH", path)
        .env("SPM_HOME", &sb.spm_home)
        .env("HOME", &sb.home)
        .env_remove("SPM_POLICY")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{fix}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Git treats paths as glob pathspecs and `.gitignore` lines as patterns, so a
/// bracket in a name (`g[1]`) must not be taken to mean the user's `g1`.
#[cfg(unix)]
#[test]
fn doctor_treats_bracket_names_literally_in_git() {
    let sb = Sandbox::new();
    git_in(&sb.project, &["init", "-q"]);
    sb.ok(&["init", "--target", "gemini"]);
    let user = sb.project.join(".gemini/skills/g1");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.join("SKILL.md"),
        "---\nname: g1\ndescription: U.\n---\n",
    )
    .unwrap();
    git_in(&sb.project, &["add", ".gemini/skills/g1/SKILL.md"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "g[1]"]);
    let (_, doc) = doctor(&sb, &[]);
    check(&doc, "gitignore", "ok");

    git_in(
        &sb.project,
        &[
            "--literal-pathspecs",
            "add",
            "-f",
            ".gemini/skills/g[1]/SKILL.md",
        ],
    );
    let (_, doc) = doctor(&sb, &[]);
    let c = check(&doc, "gitignore", "warn");
    assert_eq!(
        c["fix"], "git --literal-pathspecs rm -r --cached -- '.gemini/skills/g[1]'",
        "{c}"
    );
    run_fix(&sb, c["fix"].as_str().unwrap());
    let tracked = Command::new("git")
        .args(["ls-files"])
        .current_dir(&sb.project)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&tracked.stdout),
        ".gemini/skills/g1/SKILL.md\n"
    );
    let (_, doc) = doctor(&sb, &[]);
    check(&doc, "gitignore", "ok");
}

/// A name that looks like an option must reach `spm` as an operand: the fix
/// for a stale `--all` link must not drop every link.
#[cfg(unix)]
#[test]
fn doctor_fix_commands_separate_dash_names_from_options() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name=--all"]);
    local_skill_checkout(&sb, "dev", "LOCAL");
    sb.ok(&["link", "greet", "../dev"]);
    sb.ok(&["link", "--", "--all", "../dev"]);
    sb.ok(&["remove", "--", "--all"]);
    let (_, doc) = doctor(&sb, &[]);
    let fix = checks(&doc, "links")
        .into_iter()
        .find(|c| c["message"].as_str().unwrap().contains("not declared"))
        .unwrap()["fix"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(fix, "spm unlink -- --all");
    run_fix(&sb, &fix);
    let (_, doc) = doctor(&sb, &[]);
    let links = checks(&doc, "links");
    assert_eq!(links.len(), 1, "{doc:#}");
    assert!(
        links[0]["message"].as_str().unwrap().contains("`greet`"),
        "{doc:#}"
    );
}

/// Claude's settings.local.json holds a machine-specific path: flag it when
/// git would commit it.
#[test]
fn doctor_warns_when_claude_local_settings_are_not_gitignored() {
    let sb = Sandbox::new();
    git_in(&sb.project, &["init", "-q"]);
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok);
    let c = check(&doc, "gitignore", "warn");
    assert!(
        c["message"]
            .as_str()
            .unwrap()
            .contains(".claude/settings.local.json"),
        "{c}"
    );

    let gi = sb.read(".gitignore") + ".claude/settings.local.json\n";
    std::fs::write(sb.project.join(".gitignore"), gi).unwrap();
    let (_, doc) = doctor(&sb, &[]);
    assert!(
        checks(&doc, "gitignore")
            .iter()
            .all(|c| c["status"] == "ok"),
        "{doc:#}"
    );

    // Committed before the rule existed: the rule doesn't apply to it, so
    // the fix is to untrack it.
    git_in(&sb.project, &["add", "-f", ".claude/settings.local.json"]);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok);
    assert_eq!(
        check(&doc, "gitignore", "warn")["fix"],
        "git rm --cached .claude/settings.local.json"
    );
    git_in(
        &sb.project,
        &["rm", "-q", "--cached", ".claude/settings.local.json"],
    );
    let (_, doc) = doctor(&sb, &[]);
    assert!(
        checks(&doc, "gitignore")
            .iter()
            .all(|c| c["status"] == "ok"),
        "{doc:#}"
    );
}

#[test]
fn doctor_reports_policy_problems() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);

    let bad = sb.root.join("bad-policy.json");
    std::fs::write(&bad, "not json").unwrap();
    let mut cmd = sb.command(&["doctor", "--json"]);
    cmd.env("SPM_POLICY", &bad);
    let (ok, doc) = doctor_json(cmd);
    assert!(!ok);
    check(&doc, "machine-policy", "error");

    let machine = sb.root.join("policy.json");
    write_policy_file(&machine, &["file://*"]);
    set_project_policy(&sb, &["example.test/*"]);
    let mut cmd = sb.command(&["doctor", "--json"]);
    cmd.env("SPM_POLICY", &machine);
    let (ok, doc) = doctor_json(cmd);
    assert!(!ok);
    let layers = &check(&doc, "machine-policy", "ok")["details"];
    assert!(layers[0].as_str().unwrap().contains("file://*"), "{layers}");
    let c = check(&doc, "policy", "error");
    assert!(
        c["details"][0]
            .as_str()
            .unwrap()
            .starts_with("skill `greet`"),
        "{c}"
    );
}

#[test]
fn doctor_reports_links() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let dev = local_skill_checkout(&sb, "dev", "LOCAL");
    sb.ok(&["link", "greet", "../dev"]);

    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "an active link is a warning: {doc:#}");
    let c = check(&doc, "links", "warn");
    assert_eq!(c["fix"], "spm unlink greet when done");

    std::fs::remove_dir_all(&dev).unwrap();
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    assert_eq!(check(&doc, "links", "error")["fix"], "spm unlink greet");
}

/// After switching to a branch that declares and locks nothing, skills left
/// in spm's own Copilot dir are still discoverable: doctor must see them even
/// though `spm status` has nothing to compare.
#[test]
fn doctor_flags_leftover_skills_when_nothing_is_declared() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    write_manifest(&sb, r#"{"targets":["copilot"]}"#);
    std::fs::remove_file(sb.project.join("ai.lock")).unwrap();

    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    let c = check(&doc, "materialized", "warn");
    assert!(c["details"].to_string().contains("greet"), "{c}");
    check(&doc, "gitignore", "ok");

    // That branch's .gitignore may lack the entry: the leftovers still need it.
    std::fs::write(sb.project.join(".gitignore"), "target/\n").unwrap();
    let (_, doc) = doctor(&sb, &[]);
    let c = check(&doc, "gitignore", "warn");
    assert!(c["details"].to_string().contains("copilot"), "{c}");
}

/// Likewise a full plugin left registered after switching to a branch that
/// declares no plugins is still loaded by Claude.
#[test]
fn doctor_flags_leftover_plugins_when_none_are_declared() {
    let sb = Sandbox::new();
    install_skill_and_plugin(&sb);
    write_manifest(&sb, r#"{"targets":["claude"]}"#);
    std::fs::remove_file(sb.project.join("ai.lock")).unwrap();

    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    let c = check(&doc, "materialized", "warn");
    assert!(c["details"].to_string().contains("ds"), "{c}");
    assert!(!c["details"].to_string().contains(".claude-plugin"), "{c}");
    let out = sb.spm(&["status", "--json"]);
    assert!(json_doc(&out)["targets"][0]["plugins"].is_null());

    // Only the plugin tree left, unignored: it still needs `.spm/`.
    std::fs::remove_dir_all(sb.project.join(".spm/claude")).unwrap();
    std::fs::write(sb.project.join(".gitignore"), "target/\n").unwrap();
    let (_, doc) = doctor(&sb, &[]);
    let c = check(&doc, "gitignore", "warn");
    assert_eq!(c["details"], serde_json::json!(["claude: .spm/"]), "{c}");

    sb.ok(&["install"]);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    check(&doc, "materialized", "ok");
}

/// A shared skills dir (Codex's `.agents/skills/`) also holds the user's own
/// skills, so leftovers there are only what spm recorded installing.
#[test]
fn doctor_flags_leftover_shared_dir_skills_but_not_user_skills() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "codex"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let mine = sb.project.join(".agents/skills/mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(
        mine.join("SKILL.md"),
        "---\nname: mine\ndescription: M.\n---\n",
    )
    .unwrap();
    write_manifest(&sb, r#"{"targets":["codex"]}"#);
    std::fs::remove_file(sb.project.join("ai.lock")).unwrap();
    std::fs::write(sb.project.join(".gitignore"), "target/\n").unwrap();

    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    let c = check(&doc, "materialized", "warn");
    assert_eq!(
        c["details"],
        serde_json::json!(["codex: no longer declared: greet"]),
        "{c}"
    );
    let c = check(&doc, "gitignore", "warn");
    assert_eq!(
        c["details"],
        serde_json::json!(["codex: .agents/skills/greet/"]),
        "{c}"
    );

    sb.ok(&["install"]);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    assert!(
        checks(&doc, "materialized")
            .iter()
            .all(|c| c["status"] == "ok"),
        "{doc:#}"
    );
    assert!(mine.exists());
}

/// A link to a name no longer declared is ignored by every sync, so its
/// directory being gone is no error: it is only reported as dropped-able.
#[test]
fn doctor_warns_about_an_undeclared_link_whose_dir_is_gone() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let dev = local_skill_checkout(&sb, "dev", "LOCAL");
    sb.ok(&["link", "greet", "../dev"]);
    write_manifest(&sb, r#"{"targets":["copilot"]}"#);
    std::fs::remove_dir_all(&dev).unwrap();
    sb.ok(&["install"]);

    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
    let c = check(&doc, "links", "warn");
    assert!(
        c["message"].as_str().unwrap().contains("not declared"),
        "{c}"
    );
    assert_eq!(c["fix"], "spm unlink greet");
}

/// A linked directory that still resolves but can't be read fails `spm
/// install`, so it is an error, not just an active link.
#[cfg(unix)]
#[test]
fn doctor_flags_an_unreadable_linked_dir() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let dev = local_skill_checkout(&sb, "dev", "LOCAL");
    sb.ok(&["link", "greet", "../dev"]);
    let set = |mode| std::fs::set_permissions(&dev, std::fs::Permissions::from_mode(mode));
    // 0400 can be listed but not searched, so its files can't be read.
    for mode in [0o000, 0o400] {
        set(mode).unwrap();
        let (ok, doc) = doctor(&sb, &[]);
        let install_ok = sb.spm(&["install"]).status.success();
        set(0o755).unwrap();
        assert!(!install_ok, "{mode:o}");
        assert!(!ok, "{mode:o}: {doc:#}");
        let c = check(&doc, "links", "error");
        assert!(
            c["message"].as_str().unwrap().contains("can't be read"),
            "{c}"
        );
    }
    // A readable link can still hold content install can't snapshot.
    std::fs::create_dir_all(dev.join("nested")).unwrap();
    std::fs::write(dev.join("nested/notes.md"), "n").unwrap();
    std::fs::write(dev.join("secret.md"), "s").unwrap();
    for path in [dev.join("secret.md"), dev.join("nested")] {
        let set = |mode| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode));
        set(0o000).unwrap();
        let (ok, doc) = doctor(&sb, &[]);
        let install_ok = sb.spm(&["install"]).status.success();
        set(0o755).unwrap();
        assert!(!install_ok, "{}", path.display());
        assert!(!ok, "{}: {doc:#}", path.display());
        let c = check(&doc, "links", "error");
        assert!(
            c["message"].as_str().unwrap().contains("can't be read"),
            "{c}"
        );
    }
    let (ok, doc) = doctor(&sb, &[]);
    assert!(ok, "{doc:#}");
}

/// A locked transitive child is fetched by `install --locked` once its root
/// is, so policy must cover it even when doctor can't walk the root.
#[test]
fn doctor_checks_policy_for_locked_transitive_sources() {
    const DEP_URL: &str = "https://example.test/org/dep.git";
    let sb = Sandbox::new();
    let dep = make_repo(
        &sb.root.join("dep"),
        &[("SKILL.md", "---\nname: dep\ndescription: D.\n---\n")],
    );
    let root = make_repo(
        &sb.root.join("alpha"),
        &[
            ("SKILL.md", "---\nname: alpha\ndescription: R.\n---\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"dep":{{"git":"{DEP_URL}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    let remotes = [(DEP_URL, dep.as_str())];
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"alpha":{{"git":"{root}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm_remotes(&["install"], &remotes);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    remove_store_checkout(&sb, "alpha-");
    set_project_policy(&sb, &["file://*"]);
    let out = sb.spm_remotes(&["install", "--locked"], &remotes);
    assert!(!out.status.success());
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok, "{doc:#}");
    let c = check(&doc, "policy", "error");
    let details = c["details"].to_string();
    assert!(details.contains(DEP_URL), "{c}");
    assert!(!details.contains("skill `alpha`"), "{c}");
}

/// The message of the single check with id `id` and status `status`.
fn check_message<'a>(doc: &'a serde_json::Value, id: &str, status: &str) -> &'a str {
    check(doc, id, status)["message"].as_str().unwrap()
}

/// A lock without a project `id` is out of sync: `install --locked` rejects it
/// with the whole problem list and fix, and doctor reports the same problem.
#[test]
fn locked_install_and_doctor_reject_a_lock_without_a_project_id() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let mut lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    lock.as_object_mut().unwrap().remove("id").unwrap();
    std::fs::write(sb.project.join("ai.lock"), lock.to_string()).unwrap();

    let err = fails(&sb, &["install", "--locked"]);
    assert!(
        err.contains(
            "--locked: ai.lock is out of sync with ai.json (1 problem(s)):\n  \
             - ai.lock has no project `id`\n\
             run `spm install` (without --locked) and commit ai.lock"
        ),
        "{err}"
    );
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    let c = check(&doc, "lockfile", "error");
    assert_eq!(
        c["details"],
        serde_json::json!(["ai.lock has no project `id`"])
    );
}

/// An unreadable `ai.lock` fails the lock and materialization checks, each
/// with the load error, without hiding the checks that don't read it — and
/// the lock check still reports a problem it meets before reading the lock.
#[test]
fn doctor_reports_an_unreadable_lock_without_hiding_other_checks() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    std::fs::write(sb.project.join("ai.lock"), "not json").unwrap();

    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    check(&doc, "policy", "ok");
    let lock = check_message(&doc, "lockfile", "error");
    let err = lock
        .strip_prefix("ai.lock can't be checked: ")
        .unwrap_or_else(|| panic!("{lock}"));
    assert!(err.contains("ai.lock"), "{err}");
    assert_eq!(
        check_message(&doc, "materialized", "error"),
        format!("can't inspect the targets: {err}")
    );

    let out = sb.spm(&["doctor"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(&format!("✗ {lock}")), "{text}");
    assert!(
        text.contains("    fix: spm install and commit ai.lock"),
        "{text}"
    );

    set_project_policy(&sb, &["example.test/*"]);
    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    let lock = check_message(&doc, "lockfile", "error");
    assert!(lock.contains("not allowed by the source policy"), "{lock}");
    let text = String::from_utf8_lossy(&sb.spm(&["doctor"]).stdout).into_owned();
    assert!(
        text.contains("\n    skill `greet`"),
        "denied source details: {text}"
    );
}

/// An unreadable links file fails the links check, and every check that reads
/// link state reports it; the policy check (which needs the links) is skipped.
/// The lock check reads links before `ai.lock`, the materialization check
/// `ai.lock` first.
#[test]
fn doctor_reports_an_unreadable_links_file() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    local_skill_checkout(&sb, "dev", "LOCAL");
    sb.ok(&["link", "greet", "../dev"]);
    std::fs::write(sb.project_links(), "not json").unwrap();

    let (ok, doc) = doctor(&sb, &[]);
    assert!(!ok);
    assert!(checks(&doc, "policy").is_empty(), "{doc:#}");
    let msg = check_message(&doc, "links", "error");
    let err = msg
        .strip_prefix("links file is unreadable: ")
        .unwrap_or_else(|| panic!("{msg}"));
    assert_eq!(
        check_message(&doc, "lockfile", "error"),
        format!("ai.lock can't be checked: {err}")
    );
    assert_eq!(
        check_message(&doc, "materialized", "error"),
        format!("can't inspect the targets: {err}")
    );

    std::fs::write(sb.project.join("ai.lock"), "not json").unwrap();
    let (_, doc) = doctor(&sb, &[]);
    assert_eq!(
        check_message(&doc, "lockfile", "error"),
        format!("ai.lock can't be checked: {err}")
    );
    let targets = check_message(&doc, "materialized", "error");
    assert!(!targets.contains(err), "{targets}");
    assert!(targets.contains("ai.lock"), "{targets}");
}

#[test]
fn doctor_text_marks_warnings() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    let out = sb
        .command(&["doctor"])
        .env("SPM_ALLOW_SUSPICIOUS", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("! SPM_ALLOW_SUSPICIOUS=1: suspicious skill content"),
        "{text}"
    );
    assert!(text.contains("0 errors, 1 warning\n"), "{text}");
}

#[test]
fn doctor_warns_when_suspicious_content_is_allowed() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    let mut cmd = sb.command(&["doctor", "--json"]);
    cmd.env("SPM_ALLOW_SUSPICIOUS", "1");
    let (ok, doc) = doctor_json(cmd);
    assert!(ok, "{doc:#}");
    assert_eq!(
        check(&doc, "scan", "warn")["fix"],
        "unset SPM_ALLOW_SUSPICIOUS"
    );
    let (_, doc) = doctor(&sb, &[]);
    assert!(checks(&doc, "scan").is_empty());
}

#[cfg(unix)]
#[test]
fn doctor_warns_about_ssh_remotes_without_an_agent() {
    let sb = Sandbox::new();
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"skills":{{"greet":{{"git":"{SSH_URL}","branch":"main"}}}}}}"#
        ),
    );
    let mut cmd = sb.command(&["doctor", "--json"]);
    cmd.env_remove("SSH_AUTH_SOCK");
    let (_, doc) = doctor_json(cmd);
    assert_eq!(
        check(&doc, "ssh", "warn")["details"],
        serde_json::json!([SSH_URL])
    );

    let mut cmd = sb.command(&["doctor", "--json"]);
    cmd.env("SSH_AUTH_SOCK", "/tmp/agent.sock");
    let (_, doc) = doctor_json(cmd);
    assert!(checks(&doc, "ssh").is_empty(), "{doc:#}");
}

/// `--online` contacts nothing the source policy denies — even with no ai.lock
/// yet, when no lock check has run.
#[test]
fn doctor_online_never_contacts_a_denied_source() {
    let sb = Sandbox::new();
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"allowedSources":["example.test/*"],"skills":{{"greet":{{"git":"{}","branch":"main"}}}}}}"#,
            sb.skill_url()
        ),
    );
    let (ok, doc) = doctor(&sb, &["--online"]);
    assert!(!ok);
    check(&doc, "policy", "error");
    let c = check(&doc, "remotes", "error");
    assert!(
        c["details"][0].as_str().unwrap().contains("not allowed"),
        "{c}"
    );
}

/// `--online` probes every remote; one that only answers over the other
/// protocol gets a `--protocol` hint.
#[test]
fn doctor_online_probes_remotes_and_suggests_the_other_protocol() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v0.1.0", "--name", "greet"]);
    let (ok, doc) = doctor(&sb, &["--online"]);
    assert!(ok, "{doc:#}");
    assert_eq!(doc["online"], true);
    check(&doc, "remotes", "ok");

    let good = sb.skill_url();
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"skills":{{"greet":{{"git":"{HTTPS_URL}","branch":"main"}}}}}}"#
        ),
    );
    let remotes = [(HTTPS_URL, unreachable_url()), (SSH_URL, good.as_str())];
    let (ok, doc) = doctor_json(sb.remote_command(&["doctor", "--json", "--online"], &remotes));
    assert!(!ok);
    let c = check(&doc, "remotes", "error");
    assert!(
        c["details"][0].as_str().unwrap().starts_with(HTTPS_URL),
        "{c}"
    );
    assert!(c["fix"].as_str().unwrap().contains("--protocol ssh"), "{c}");

    // With the fallback the remote is reachable, as `install` would find it.
    let (_, doc) = doctor_json(sb.remote_command(
        &["doctor", "--json", "--online", "--protocol-fallback"],
        &remotes,
    ));
    check(&doc, "remotes", "ok");
}
