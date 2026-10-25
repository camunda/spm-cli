//! Version ranges (`version` requirements).

use crate::common::*;
use std::path::PathBuf;

// ---- `version` semver range selector -------------------------------------

/// Commit a new revision in the fixture skill repo and tag it. Annotated tags
/// (`annotated`) exercise the `^{}` peel; lightweight ones the plain ref.
/// Returns the commit SHA the tag points at.
fn release(sb: &Sandbox, tag: &str, annotated: bool) -> String {
    std::fs::write(sb.skill_repo.join("VERSION"), format!("{tag}\n")).unwrap();
    sb.git(&["add", "-A"]);
    sb.git(&["commit", "-qm", tag]);
    if annotated {
        sb.git(&["tag", "-a", tag, "-m", tag]);
    } else {
        sb.git(&["tag", tag]);
    }
    skill_head(&sb.skill_repo)
}

/// Tag a ladder of releases: `v`-prefixed and bare, annotated and lightweight,
/// a prerelease, a non-semver tag and a next major. Returns
/// `(v1.2.0, 1.3.0, v1.4.1, v1.5.0-rc.1)` commits.
fn range_fixture(sb: &Sandbox) -> (String, String, String, String) {
    release(sb, "v1.0.0", true);
    let v120 = release(sb, "v1.2.0", false);
    let b130 = release(sb, "1.3.0", false);
    let v141 = release(sb, "v1.4.1", true);
    let rc = release(sb, "v1.5.0-rc.1", true);
    release(sb, "latest", false);
    release(sb, "v2.0.0", true);
    (v120, b130, v141, rc)
}

#[test]
fn add_version_range_locks_the_highest_matching_tag() {
    let sb = Sandbox::new();
    let (_, _, v141, _) = range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    let out = sb.ok(&[
        "add",
        &sb.skill_url(),
        "--version",
        "^1.2",
        "--name",
        "greet",
    ]);
    assert!(out.contains("version:^1.2 → v1.4.1"), "{out}");

    let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    let spec = &manifest["skills"]["greet"];
    assert_eq!(spec["version"], "^1.2", "{manifest}");
    for other in ["tag", "branch", "commit"] {
        assert!(spec.get(other).is_none(), "{manifest}");
    }

    // Ignores the non-semver `latest`, the prerelease and the next major;
    // the annotated tag is peeled to its commit.
    let lock = lock_doc(&sb);
    let locked = &lock["skills"]["greet"];
    assert_eq!(locked["reference"], "version:^1.2", "{lock}");
    assert_eq!(locked["resolved_tag"], "v1.4.1", "{lock}");
    assert_eq!(locked["commit"], v141.as_str(), "{lock}");

    let list = sb.ok(&["list"]);
    assert!(list.contains("version:^1.2 → v1.4.1"), "{list}");
    let doc = json_doc(&sb.spm(&["list", "--json"]));
    let row = doc["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "greet")
        .unwrap();
    assert_eq!(row["resolvedTag"], "v1.4.1", "{doc}");
}

#[test]
fn version_range_matches_bare_tags_and_opt_in_prereleases() {
    let sb = Sandbox::new();
    let (_, b130, _, rc) = range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude"],"skills":{{
                "bare":{{"git":"{u}","version":"~1.3"}},
                "pre":{{"git":"{u}","version":"~1.5.0-rc.1"}},
                "nopre":{{"git":"{u}","version":">=1.4.1, <2"}}}}}}"#,
            u = sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    let lock = lock_doc(&sb);
    assert_eq!(lock["skills"]["bare"]["resolved_tag"], "1.3.0", "{lock}");
    assert_eq!(lock["skills"]["bare"]["commit"], b130.as_str(), "{lock}");
    assert_eq!(
        lock["skills"]["pre"]["resolved_tag"], "v1.5.0-rc.1",
        "{lock}"
    );
    assert_eq!(lock["skills"]["pre"]["commit"], rc.as_str(), "{lock}");
    // A prerelease never satisfies a requirement that did not opt in.
    assert_eq!(lock["skills"]["nopre"]["resolved_tag"], "v1.4.1", "{lock}");
}

#[test]
fn exact_tag_stays_exact_even_when_newer_versions_exist() {
    let sb = Sandbox::new();
    let (v120, _, _, _) = range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&["add", &sb.skill_url(), "--tag", "v1.2.0", "--name", "greet"]);
    let lock = lock_doc(&sb);
    assert_eq!(lock["skills"]["greet"]["reference"], "tag:v1.2.0", "{lock}");
    assert_eq!(lock["skills"]["greet"]["commit"], v120.as_str(), "{lock}");
    assert!(
        lock["skills"]["greet"].get("resolved_tag").is_none(),
        "{lock}"
    );
}

#[test]
fn install_keeps_the_range_pin_and_update_moves_within_it() {
    let sb = Sandbox::new();
    let (_, _, v141, _) = range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--version",
        "^1.2",
        "--name",
        "greet",
    ]);
    let v160 = release(&sb, "v1.6.0", true);
    let v210 = release(&sb, "v2.1.0", true);

    // A new in-range tag does not move an existing pin on install.
    sb.ok(&["install"]);
    sb.ok(&["install", "--locked"]);
    assert_eq!(lock_doc(&sb)["skills"]["greet"]["commit"], v141.as_str());

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let row = outdated_row(&doc, "greet");
    assert_eq!(row["status"], "behind", "{doc}");
    assert_eq!(row["selector"], serde_json::json!({"version": "^1.2"}));
    assert_eq!(row["resolvedTag"], "v1.4.1", "{doc}");
    assert_eq!(row["latestInRange"], "v1.6.0", "{doc}");
    assert_eq!(row["remoteCommit"], v160.as_str(), "{doc}");
    assert_eq!(row["latestTag"], "v2.1.0", "{doc}");
    assert_eq!(row["latestTagCommit"], v210.as_str(), "{doc}");
    let text = sb.ok(&["outdated"]);
    assert!(text.contains("version:^1.2 → v1.4.1"), "{text}");
    assert!(text.contains("tag:v1.6.0"), "{text}");
    assert!(
        text.contains("newer tag outside the range: v2.1.0"),
        "{text}"
    );
    assert!(
        text.contains("greet: newer tag within the version range — run `spm update`"),
        "{text}"
    );

    // `update` re-resolves to the newest in-range tag, never the next major.
    sb.ok(&["update", "greet"]);
    let lock = lock_doc(&sb);
    assert_eq!(lock["skills"]["greet"]["resolved_tag"], "v1.6.0", "{lock}");
    assert_eq!(lock["skills"]["greet"]["commit"], v160.as_str(), "{lock}");

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let row = outdated_row(&doc, "greet");
    assert_eq!(row["status"], "newer-tag", "{doc}");
    assert_eq!(row["latestInRange"], "v1.6.0", "{doc}");
    assert_eq!(row["latestTag"], "v2.1.0", "{doc}");
    let text = sb.ok(&["outdated"]);
    assert!(
        text.contains("greet: newer tag outside the version range — widen `version`"),
        "{text}"
    );

    // Widening the requirement is drift: the next install re-resolves.
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude"],"skills":{{"greet":{{"git":"{}","version":">=1.2"}}}}}}"#,
            sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    let lock = lock_doc(&sb);
    assert_eq!(
        lock["skills"]["greet"]["reference"], "version:>=1.2",
        "{lock}"
    );
    assert_eq!(lock["skills"]["greet"]["resolved_tag"], "v2.1.0", "{lock}");
    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    assert_eq!(outdated_row(&doc, "greet")["status"], "up-to-date", "{doc}");
}

#[test]
fn outdated_warns_when_the_resolved_range_tag_was_repointed() {
    let sb = Sandbox::new();
    range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--version",
        "~1.4",
        "--name",
        "greet",
    ]);
    advance_main(&sb);
    sb.git(&["tag", "-f", "-a", "v1.4.1", "-m", "moved"]);

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    assert_eq!(outdated_row(&doc, "greet")["status"], "tag-moved", "{doc}");
    let err = stderr_of(&sb.spm(&["outdated"]));
    assert!(
        err.contains("tag `v1.4.1` (locked for `version: ~1.4`) of `greet`"),
        "{err}"
    );
}

#[test]
fn version_range_errors_are_actionable() {
    let sb = Sandbox::new();
    range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    let url = sb.skill_url();

    let out = sb.spm(&["add", &url, "--version", "^9", "--name", "greet"]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no tag") && err.contains("^9"), "{err}");
    assert!(err.contains("v2.0.0") && err.contains("v1.4.1"), "{err}");
    assert!(
        !err.contains("latest"),
        "non-semver tags are not listed: {err}"
    );

    let out = sb.spm(&["add", &url, "--version", "^v1.2", "--name", "greet"]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("invalid version requirement"), "{err}");
    assert!(err.contains("`v`"), "{err}");

    let out = sb.spm(&["add", &url, "--version", "^1.2", "--tag", "v1.2.0"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("cannot be used with"),
        "{}",
        stderr_of(&out)
    );

    assert!(!sb.project.join("ai.lock").exists() || !sb.read("ai.lock").contains("greet"));
}

#[test]
fn manifest_rejects_version_with_another_selector_or_a_bad_requirement() {
    let sb = Sandbox::new();
    for (spec, needle) in [
        (r#"{"git":"u","version":"^1","tag":"v1.0.0"}"#, "oneOf"),
        (r#"{"git":"u","version":""}"#, "does not match schema"),
        (
            r#"{"git":"u","version":"banana"}"#,
            "invalid version requirement",
        ),
    ] {
        write_manifest(
            &sb,
            &format!(r#"{{"targets":["claude"],"skills":{{"x":{spec}}}}}"#),
        );
        let out = sb.spm(&["install"]);
        assert!(!out.status.success(), "{spec} must be rejected");
        let err = stderr_of(&out);
        assert!(err.contains(needle), "{spec}: {err}");
    }
}

#[test]
fn plugin_accepts_a_version_range() {
    let sb = Sandbox::new();
    sb.add_plugin();
    let v1 = release(&sb, "v1.1.0", true);
    release(&sb, "v2.0.0", true);
    sb.ok(&["init", "--target", "claude"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude"],"plugins":{{"ds":{{"git":"{}","version":"1","path":"pkg"}}}}}}"#,
            sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    let lock = lock_doc(&sb);
    assert_eq!(lock["plugins"]["ds"]["reference"], "version:1", "{lock}");
    assert_eq!(lock["plugins"]["ds"]["resolved_tag"], "v1.1.0", "{lock}");
    assert_eq!(lock["plugins"]["ds"]["commit"], v1.as_str(), "{lock}");
    let list = sb.ok(&["list"]);
    assert!(list.contains("version:1 → v1.1.0"), "{list}");
}

#[test]
fn transitive_dependency_can_request_a_version_range() {
    let sb = Sandbox::new();
    let leaf_dir = sb.root.join("range-leaf");
    make_repo(&leaf_dir, &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")]);
    git_in(&leaf_dir, &["tag", "v1.0.0"]);
    std::fs::write(leaf_dir.join("NOTES.md"), "x\n").unwrap();
    git_in(&leaf_dir, &["add", "-A"]);
    git_in(&leaf_dir, &["commit", "-qm", "1.1"]);
    git_in(&leaf_dir, &["tag", "-a", "v1.1.0", "-m", "v1.1.0"]);
    let v110 = skill_head(&leaf_dir);
    std::fs::write(leaf_dir.join("NOTES.md"), "y\n").unwrap();
    git_in(&leaf_dir, &["add", "-A"]);
    git_in(&leaf_dir, &["commit", "-qm", "2"]);
    git_in(&leaf_dir, &["tag", "v2.0.0"]);
    let leaf = format!(
        "file://{}",
        leaf_dir.display().to_string().replace('\\', "/")
    );
    let top = make_repo(
        &sb.root.join("range-top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","version":"^1"}}}}}}"#),
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
    let lock = lock_doc(&sb);
    let leaf_row = lock["skills"]
        .as_object()
        .unwrap()
        .values()
        .find(|v| v["reference"] == "version:^1")
        .unwrap_or_else(|| panic!("no transitive range entry: {lock}"));
    assert_eq!(leaf_row["resolved_tag"], "v1.1.0", "{lock}");
    assert_eq!(leaf_row["commit"], v110.as_str(), "{lock}");
}

#[test]
fn lock_rejects_a_resolved_tag_that_does_not_satisfy_the_range() {
    let sb = Sandbox::new();
    range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--version",
        "^1.2",
        "--name",
        "greet",
    ]);
    for (from, to) in [
        (r#""resolved_tag": "v1.4.1""#, r#""resolved_tag": "v2.0.0""#),
        (r#""resolved_tag": "v1.4.1""#, r#""resolved_tag": "latest""#),
    ] {
        let lock = sb.read("ai.lock");
        assert!(lock.contains(from), "{lock}");
        std::fs::write(sb.project.join("ai.lock"), lock.replace(from, to)).unwrap();
        let out = sb.spm(&["install", "--locked"]);
        assert!(!out.status.success(), "{to} must be rejected");
        assert!(
            stderr_of(&out).contains("resolved_tag"),
            "{}",
            stderr_of(&out)
        );
        std::fs::write(sb.project.join("ai.lock"), lock).unwrap();
    }
}

/// A newer in-range tag on the *same* commit as the locked one still means
/// `spm update` would advance `resolved_tag`, so the range is `behind` — even
/// when a release outside the range also exists.
#[test]
fn outdated_detects_a_newer_in_range_tag_on_the_locked_commit() {
    let sb = Sandbox::new();
    let (_, _, v141, _) = range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--version",
        "^1.2",
        "--name",
        "greet",
    ]);
    sb.git(&["tag", "v1.9.0", &v141]);

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let row = outdated_row(&doc, "greet");
    assert_eq!(row["status"], "behind", "{doc}");
    assert_eq!(row["latestInRange"], "v1.9.0", "{doc}");
    assert_eq!(row["remoteCommit"], v141.as_str(), "{doc}");
    assert_eq!(row["latestTag"], "v2.0.0", "{doc}");
    assert!(!sb.spm(&["outdated", "--exit-code"]).status.success());
    let text = sb.ok(&["outdated"]);
    assert!(
        text.contains("greet: newer tag within the version range"),
        "{text}"
    );

    sb.ok(&["update", "greet"]);
    let lock = lock_doc(&sb);
    assert_eq!(lock["skills"]["greet"]["resolved_tag"], "v1.9.0", "{lock}");
    assert_eq!(lock["skills"]["greet"]["commit"], v141.as_str(), "{lock}");
}

/// A direct `leaf` tracked by `branch: main`, which a nested `ai.json` also
/// requests by `version: ^1`: the graph dedups both onto one entry, recording
/// the range (and the tag it resolved to) in `also_satisfies`. Returns the
/// leaf repo dir and the commit `main` and `v1.0.0` share.
fn dedup_range_fixture(sb: &Sandbox) -> (PathBuf, String) {
    let leaf_dir = sb.root.join("dedup-leaf");
    let leaf = make_repo(&leaf_dir, &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")]);
    git_in(&leaf_dir, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);
    let top = make_repo(
        &sb.root.join("dedup-top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","version":"^1"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{
                "leaf":{{"git":"{leaf}","branch":"main"}},
                "top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    let lock = lock_doc(sb);
    assert_eq!(
        lock["skills"]["leaf"]["also_satisfies"],
        serde_json::json!([{ "reference": "version:^1", "resolved_tag": "v1.0.0" }]),
        "{lock}"
    );
    sb.ok(&["install", "--locked"]);
    let head = skill_head(&leaf_dir);
    (leaf_dir, head)
}

#[test]
fn outdated_checks_a_deduplicated_range_against_its_resolved_tag() {
    let sb = Sandbox::new();
    let (leaf_dir, head) = dedup_range_fixture(&sb);
    // A newer in-range release on the very commit `main` (and the lock) holds.
    git_in(&leaf_dir, &["tag", "v1.1.0", &head]);

    let out = sb.spm(&["outdated", "--json", "--exit-code"]);
    assert!(!out.status.success(), "a range update is pending");
    let doc = json_doc(&out);
    let row = outdated_row(&doc, "leaf");
    assert_eq!(row["status"], "up-to-date", "{doc}");
    let sec = &row["alsoSatisfies"][0];
    assert_eq!(
        sec["selector"],
        serde_json::json!({"version": "^1"}),
        "{doc}"
    );
    assert_eq!(sec["resolvedTag"], "v1.0.0", "{doc}");
    assert_eq!(sec["latestInRange"], "v1.1.0", "{doc}");
    assert_eq!(sec["status"], "behind", "{doc}");
    assert_eq!(row["outdated"], true, "{doc}");

    let err = stderr_of(&sb.spm(&["outdated"]));
    assert!(
        err.contains("`version:^1` (also satisfied by `leaf`) has a newer tag v1.1.0"),
        "{err}"
    );
}

#[test]
fn outdated_warns_when_a_deduplicated_range_tag_was_repointed() {
    let sb = Sandbox::new();
    let (leaf_dir, _) = dedup_range_fixture(&sb);
    // Re-point v1.0.0 to a commit off `main`, so the branch stays put.
    git_in(&leaf_dir, &["checkout", "-qb", "side"]);
    std::fs::write(leaf_dir.join("NOTES.md"), "x\n").unwrap();
    git_in(&leaf_dir, &["add", "-A"]);
    git_in(&leaf_dir, &["commit", "-qm", "side"]);
    git_in(&leaf_dir, &["tag", "-f", "-a", "v1.0.0", "-m", "moved"]);
    git_in(&leaf_dir, &["checkout", "-q", "main"]);

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let row = outdated_row(&doc, "leaf");
    assert_eq!(row["status"], "up-to-date", "{doc}");
    assert_eq!(row["alsoSatisfies"][0]["status"], "tag-moved", "{doc}");
    let err = stderr_of(&sb.spm(&["outdated"]));
    assert!(
        err.contains("tag `v1.0.0` (locked for `version: ^1`, also satisfied by `leaf`)"),
        "{err}"
    );
}

/// A range's locked tag that disappeared is an error, like a deleted exact
/// tag — never `behind`, which would steer `spm update` into a downgrade.
#[test]
fn outdated_errors_when_the_resolved_range_tag_was_deleted() {
    let sb = Sandbox::new();
    range_fixture(&sb);
    sb.ok(&["init", "--target", "claude"]);
    sb.ok(&[
        "add",
        &sb.skill_url(),
        "--version",
        "^1.2",
        "--name",
        "greet",
    ]);
    sb.git(&["tag", "-d", "v1.4.1"]);

    let out = sb.spm(&["outdated", "--json"]);
    assert!(!out.status.success(), "an error row fails the command");
    let doc = json_doc(&out);
    let row = outdated_row(&doc, "greet");
    assert_eq!(row["status"], "error", "{doc}");
    let err = row["error"].as_str().unwrap();
    assert!(
        err.contains("tag `v1.4.1` (locked for `version: ^1.2`) no longer exists"),
        "{doc}"
    );
}

/// A requester tracking a `version` range ends the upgrade chain: publishing a
/// release inside its range reaches the project through `spm update`, with no
/// selector edit above it — whether the range is the local root's or an
/// intermediate transitive edge's.
#[test]
fn outdated_guides_a_newer_tag_through_range_tracking_requesters() {
    let sb = Sandbox::new();
    let leaf_dir = sb.root.join("rchain-leaf");
    let leaf = make_repo(&leaf_dir, &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")]);
    git_in(&leaf_dir, &["tag", "v1.0.0"]);
    let middle_dir = sb.root.join("rchain-middle");
    let middle_manifest =
        |tag: &str| format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","tag":"{tag}"}}}}}}"#);
    let middle = make_repo(
        &middle_dir,
        &[
            ("SKILL.md", "---\nname: middle\n---\nMiddle.\n"),
            ("ai.json", &middle_manifest("v1.0.0")),
        ],
    );
    git_in(&middle_dir, &["tag", "v1.0.0"]);
    let root_dir = sb.root.join("rchain-root");
    let root = make_repo(
        &root_dir,
        &[
            ("SKILL.md", "---\nname: root\n---\nRoot.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"middle":{{"git":"{middle}","version":"^1"}}}}}}"#),
            ),
        ],
    );
    git_in(&root_dir, &["tag", "v1.0.0"]);
    sb.ok(&["init", "--target", "copilot"]);
    let manifest = |sel: &str| {
        format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"root":{{"git":"{root}",{sel}}}}}}}"#
        )
    };
    let newer_leaf = |sb: &Sandbox| {
        let doc = json_doc(&sb.spm(&["outdated", "--json"]));
        let row = doc["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "newer-tag")
            .cloned();
        (doc, row)
    };

    // Intermediate range: root is a local exact tag, middle tracks `^1`.
    write_manifest(&sb, &manifest(r#""tag":"v1.0.0""#));
    sb.ok(&["install"]);
    git_in(&leaf_dir, &["tag", "v1.1.0"]);
    let (doc, row) = newer_leaf(&sb);
    let row = row.unwrap_or_else(|| panic!("no newer-tag row: {doc}"));
    let middle_name = row["requestedBy"][0].as_str().unwrap().to_string();
    let text = sb.ok(&["outdated"]);
    let expected = format!(
        "bump it in the upstream ai.json of {middle_name}; publish a new {middle_name} release \
         within its `version: ^1` range (one outside it needs that range widened); then run \
         `spm update`"
    );
    assert!(text.contains(&expected), "{text}");
    assert!(!text.contains("point root at"), "{text}");

    // Following that guidance really applies the change: no edit to root.
    std::fs::write(middle_dir.join("ai.json"), middle_manifest("v1.1.0")).unwrap();
    git_in(&middle_dir, &["commit", "-qam", "bump leaf"]);
    git_in(&middle_dir, &["tag", "v1.1.0"]);
    sb.ok(&["update"]);
    let (doc, row) = newer_leaf(&sb);
    assert!(row.is_none(), "leaf picked up v1.1.0: {doc}");

    // Direct range: root tracks `^1` locally and now pins middle exactly, so
    // middle is republished and bumped in root, whose in-range release ends it.
    std::fs::write(
        root_dir.join("ai.json"),
        format!(r#"{{"skills":{{"middle":{{"git":"{middle}","tag":"v1.1.0"}}}}}}"#),
    )
    .unwrap();
    git_in(&root_dir, &["commit", "-qam", "pin middle"]);
    git_in(&root_dir, &["tag", "v1.1.0"]);
    write_manifest(&sb, &manifest(r#""version":"^1""#));
    sb.ok(&["install"]);
    git_in(&leaf_dir, &["tag", "v1.2.0"]);
    let (doc, row) = newer_leaf(&sb);
    let row = row.unwrap_or_else(|| panic!("no newer-tag row: {doc}"));
    let middle_name = row["requestedBy"][0].as_str().unwrap().to_string();
    let text = sb.ok(&["outdated"]);
    let expected = format!(
        "publish a new {middle_name} version and bump {middle_name} in the upstream ai.json of \
         root; publish a new root release within its `version: ^1` range (one outside it needs \
         that range widened); then run `spm update`"
    );
    assert!(text.contains(&expected), "{text}");
    assert!(!text.contains("point root at"), "{text}");
}
