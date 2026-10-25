//! `spm outdated`.

use crate::common::*;

/// Lock a branch skill, a tag skill, a commit skill and a branch plugin, all
/// from the fixture repo. Returns the commit `v0.1.0` (and `pin`) points at.
fn install_outdated_fixture(sb: &Sandbox) -> String {
    let first = skill_head(&sb.skill_repo);
    sb.add_plugin();
    sb.ok(&["init", "--target", "claude"]);
    write_manifest(
        sb,
        &format!(
            r#"{{"targets":["claude"],"skills":{{
                "br":{{"git":"{u}","branch":"main"}},
                "tg":{{"git":"{u}","tag":"v0.1.0"}},
                "pin":{{"git":"{u}","commit":"{first}"}}}},
              "plugins":{{"ds":{{"git":"{u}","branch":"main","path":"pkg"}}}}}}"#,
            u = sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    first
}

/// With nothing locked, the text report says why: nothing declared, or
/// declared but never installed.
#[test]
fn outdated_text_explains_an_empty_report() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    assert_eq!(sb.ok(&["outdated"]), "no skills or plugins declared\n");
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"skills":{{"greet":{{"git":"{}","tag":"v0.1.0"}}}}}}"#,
            sb.skill_url()
        ),
    );
    assert_eq!(
        sb.ok(&["outdated"]),
        "nothing locked yet — run `spm install`\n"
    );
}

#[test]
fn outdated_reports_up_to_date_and_exit_code_passes_when_nothing_moved() {
    let sb = Sandbox::new();
    install_outdated_fixture(&sb);
    let out = sb.ok(&["outdated", "--exit-code"]);
    assert!(out.contains("everything is up to date"), "{out}");
    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    assert_eq!(outdated_row(&doc, "br")["status"], "up-to-date", "{doc}");
    assert_eq!(outdated_row(&doc, "tg")["status"], "up-to-date", "{doc}");
    assert_eq!(outdated_row(&doc, "pin")["status"], "pinned", "{doc}");
    assert_eq!(outdated_row(&doc, "ds")["status"], "up-to-date", "{doc}");
}

/// Moved branch, newer semver tag, pinned commit and a plugin, all reported
/// with full SHAs — and the command is strictly read-only.
#[test]
fn outdated_reports_behind_newer_tag_and_pinned_without_writing_anything() {
    let sb = Sandbox::new();
    let first = install_outdated_fixture(&sb);
    let locked_main = skill_head(&sb.skill_repo);
    let tip = advance_main(&sb);
    // Newer semver tags plus floating/non-semver ones that must be ignored.
    sb.git(&["tag", "v0.2.0"]);
    sb.git(&["tag", "-a", "v0.10.0", "-m", "v0.10.0"]);
    sb.git(&["tag", "v9"]);
    sb.git(&["tag", "latest"]);
    sb.git(&["tag", "v1.0.0-rc.1"]);

    let before = (tree_snapshot(&sb.project), tree_snapshot(&sb.spm_home));

    // Default: report, but exit 0.
    let out = sb.spm(&["outdated"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    let line = |name: &str| {
        text.lines()
            .find(|l| l.split_whitespace().next() == Some(name))
            .unwrap_or_else(|| panic!("no `{name}` line: {text}"))
            .to_string()
    };
    assert!(text.lines().next().unwrap().starts_with("name"), "{text}");
    let br = line("br");
    assert!(br.contains("branch:main"), "{br}");
    assert!(
        br.contains(&locked_main[..8]) && br.contains(&tip[..8]),
        "{br}"
    );
    assert!(br.contains("behind"), "{br}");
    let tg = line("tg");
    assert!(
        tg.contains("tag:v0.10.0") && tg.contains("newer tag"),
        "{tg}"
    );
    assert!(line("pin").contains("pinned"), "{text}");
    let ds = line("ds");
    assert!(ds.contains("behind") && ds.contains("(plugin)"), "{ds}");
    assert!(text.contains("3 dependencies outdated"), "{text}");
    // Moved refs are refreshed by `update`; a newer tag needs an ai.json edit.
    assert!(
        text.contains("br, ds: run `spm update` to re-pin"),
        "{text}"
    );
    assert!(
        text.contains("tg: newer tag — change the tag in ai.json, then run `spm install`"),
        "{text}"
    );

    // --exit-code turns "outdated" into a failure.
    let out = sb.spm(&["outdated", "--exit-code"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("3 dependencies outdated"),
        "{}",
        stderr_of(&out)
    );

    // JSON: versioned envelope, full SHAs, status per row; same exit code rules.
    let out = sb.spm(&["outdated", "--json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let doc = json_doc(&out);
    assert_eq!(doc["scope"], "project");
    assert_eq!(
        *outdated_row(&doc, "br"),
        serde_json::json!({
            "name": "br", "git": sb.skill_url(), "path": null,
            "selector": { "branch": "main" }, "locked": locked_main,
            "resolvedTag": null, "remoteCommit": tip, "latestInRange": null,
            "latestTag": null, "latestTagCommit": null,
            "status": "behind", "outdated": true, "transitive": false,
            "requestedBy": [], "alsoSatisfies": [], "drift": null, "error": null
        }),
        "{doc}"
    );
    let tg = outdated_row(&doc, "tg");
    assert_eq!(tg["status"], "newer-tag", "{doc}");
    assert_eq!(tg["locked"], first.as_str(), "{doc}");
    assert_eq!(tg["remoteCommit"], first.as_str(), "{doc}");
    assert_eq!(tg["latestTag"], "v0.10.0", "{doc}");
    // The annotated tag is peeled to its commit, not the tag object.
    assert_eq!(tg["latestTagCommit"], tip.as_str(), "{doc}");
    let pin = outdated_row(&doc, "pin");
    assert_eq!(pin["status"], "pinned", "{doc}");
    assert_eq!(pin["outdated"], false, "{doc}");
    assert!(pin["remoteCommit"].is_null(), "{doc}");
    assert_eq!(doc["plugins"][0]["name"], "ds", "{doc}");
    assert_eq!(doc["plugins"][0]["status"], "behind", "{doc}");
    assert!(!sb
        .spm(&["outdated", "--json", "--exit-code"])
        .status
        .success());

    // A name narrows the report; an unknown name is an error.
    let doc = json_doc(&sb.spm(&["outdated", "tg", "--json"]));
    assert_eq!(doc["skills"].as_array().unwrap().len(), 1, "{doc}");
    assert_eq!(doc["plugins"], serde_json::json!([]), "{doc}");
    let out = sb.spm(&["outdated", "nope"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("`nope` is not in ai.lock"),
        "{}",
        stderr_of(&out)
    );

    // Nothing written: ai.json, ai.lock, vendor dirs and the store are intact.
    assert_eq!(
        (tree_snapshot(&sb.project), tree_snapshot(&sb.spm_home)),
        before,
        "spm outdated must be read-only"
    );
}

/// A commit selector is never queried: its row is `pinned` even when the
/// remote is gone, and no remote error is reported for it.
#[test]
fn outdated_does_not_query_commit_selectors() {
    let sb = Sandbox::new();
    let first = skill_head(&sb.skill_repo);
    sb.ok(&["init", "--target", "copilot"]);
    sb.ok(&["add", &sb.skill_url(), "--commit", &first, "--name", "pin"]);
    std::fs::remove_dir_all(&sb.skill_repo).unwrap();
    let out = sb.spm(&["outdated", "--json", "--exit-code"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let doc = json_doc(&out);
    assert_eq!(outdated_row(&doc, "pin")["status"], "pinned", "{doc}");
}

#[test]
fn outdated_warns_when_the_pinned_tag_was_moved() {
    let sb = Sandbox::new();
    let first = install_outdated_fixture(&sb);
    let tip = advance_main(&sb);
    sb.git(&["tag", "-f", "-a", "v0.1.0", "-m", "re-pointed"]);

    let out = sb.spm(&["outdated"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    let tg = text.lines().find(|l| l.starts_with("tg ")).unwrap();
    assert!(tg.contains("tag moved") && tg.contains(&tip[..8]), "{tg}");
    let err = stderr_of(&out);
    assert!(err.contains("warning: tag `v0.1.0` of `tg`"), "{err}");
    assert!(err.contains("re-pointed"), "{err}");

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let tg = outdated_row(&doc, "tg");
    assert_eq!(tg["status"], "tag-moved", "{doc}");
    assert_eq!(tg["locked"], first.as_str(), "{doc}");
    assert_eq!(tg["remoteCommit"], tip.as_str(), "{doc}");
    assert_eq!(tg["outdated"], true, "{doc}");
    assert!(!sb.spm(&["outdated", "--exit-code"]).status.success());
}

/// `outdated` reports the pins in ai.lock; a direct entry whose ai.json
/// request changed since (selector, source or path; skills and plugins) is
/// flagged so nothing plans an update of the stale pin from that row.
#[test]
fn outdated_flags_direct_entries_whose_ai_json_request_changed() {
    let sb = Sandbox::new();
    let first = install_outdated_fixture(&sb);
    let manifest = |tg: &str, br_git: &str| {
        format!(
            r#"{{"targets":["claude"],"skills":{{
                "br":{{"git":"{br_git}","branch":"main"}},
                "tg":{{"git":"{u}",{tg}}},
                "pin":{{"git":"{u}","commit":"{first}"}}}},
              "plugins":{{"ds":{{"git":"{u}","branch":"main","path":"other"}}}}}}"#,
            u = sb.skill_url()
        )
    };
    let other_git = format!("{}-moved", sb.skill_url());
    write_manifest(&sb, &manifest(r#""version":"^0.1""#, &other_git));
    let lock_before = std::fs::read(sb.project.join("ai.lock")).unwrap();

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let tg = outdated_row(&doc, "tg");
    assert_eq!(tg["drift"], "`tag:v0.1.0` -> `version:^0.1`", "{doc}");
    assert_eq!(
        tg["selector"]["tag"], "v0.1.0",
        "the row still reports the pin: {doc}"
    );
    let br = outdated_row(&doc, "br")["drift"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(br.starts_with("git `") && br.contains("-moved`"), "{br}");
    assert_eq!(
        outdated_row(&doc, "ds")["drift"],
        "path `pkg` -> `other`",
        "plugins too: {doc}"
    );
    assert_eq!(outdated_row(&doc, "pin")["drift"], serde_json::Value::Null);

    let out = sb.spm(&["outdated"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    let row = |n: &str| {
        text.lines()
            .find(|l| l.starts_with(&format!("{n} ")))
            .unwrap()
            .to_string()
    };
    assert!(row("tg").contains("ai.json changed"), "{text}");
    assert!(!row("pin").contains("ai.json changed"), "{text}");
    let err = stderr_of(&out);
    assert!(
        err.contains(
            "warning: `tg` in ai.json no longer matches ai.lock (`tag:v0.1.0` -> `version:^0.1`)"
        ) && err.contains("run `spm install` to re-lock it first"),
        "{err}"
    );
    assert_eq!(
        std::fs::read(sb.project.join("ai.lock")).unwrap(),
        lock_before,
        "still read-only"
    );

    // Back in line with the lock: no drift.
    write_manifest(&sb, &manifest(r#""tag":"v0.1.0""#, &sb.skill_url()));
    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    for n in ["br", "tg", "pin"] {
        assert_eq!(
            outdated_row(&doc, n)["drift"],
            serde_json::Value::Null,
            "{n}: {doc}"
        );
    }
}

// Global scope needs a home dir, which the sandbox can only redirect on Unix
// (like the other global `outdated` tests).
#[cfg(unix)]
#[test]
fn outdated_drift_warning_carries_the_global_flag() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "copilot"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    let path = sb.spm_home.join("ai.json");
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace(r#""branch": "main""#, r#""branch": "dev""#);
    assert!(edited.contains(r#""branch": "dev""#), "{edited}");
    std::fs::write(&path, edited).unwrap();
    let out = sb.spm(&["outdated", "-g"]);
    let err = stderr_of(&out);
    assert!(
        err.contains("`branch:main` -> `branch:dev`")
            && err.contains("run `spm install -g` to re-lock it first"),
        "{err}"
    );
}

/// An unreachable remote fails only its own row; the rest are still checked,
/// the JSON document is still printed, and the command exits non-zero even
/// without `--exit-code`.
#[test]
fn outdated_reports_an_unreachable_remote_per_row_and_fails() {
    let sb = Sandbox::new();
    let gone_dir = sb.root.join("gone");
    let gone = make_repo(&gone_dir, &[("SKILL.md", "---\nname: gone\n---\nGone.\n")]);
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"skills":{{
                "ok":{{"git":"{u}","branch":"main"}},
                "gone":{{"git":"{gone}","branch":"main"}}}}}}"#,
            u = sb.skill_url()
        ),
    );
    sb.ok(&["install"]);
    std::fs::remove_dir_all(&gone_dir).unwrap();

    let out = sb.spm(&["outdated"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.lines()
            .any(|l| l.starts_with("ok ") && l.contains("up to date")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("gone ") && l.contains("error")),
        "{text}"
    );
    assert!(!text.contains("everything is up to date"), "{text}");
    assert!(
        text.contains("incomplete: 1 dependency could not be checked"),
        "{text}"
    );
    let err = stderr_of(&out);
    assert!(err.contains("error: `gone`:"), "{err}");
    assert!(err.contains("could not check 1 dependency"), "{err}");

    let out = sb.spm(&["outdated", "--json"]);
    assert!(!out.status.success());
    let doc = json_doc(&out);
    assert_eq!(outdated_row(&doc, "ok")["status"], "up-to-date", "{doc}");
    let gone = outdated_row(&doc, "gone");
    assert_eq!(gone["status"], "error", "{doc}");
    assert!(
        gone["error"].as_str().unwrap().contains("git ls-remote"),
        "{doc}"
    );
}

/// Transitive skills are listed but marked, since their fix lives in the
/// requesting skill's own `ai.json`.
#[test]
fn outdated_marks_transitive_skills() {
    let sb = Sandbox::new();
    let leaf_dir = sb.root.join("out-leaf");
    let leaf = make_repo(&leaf_dir, &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")]);
    let top = make_repo(
        &sb.root.join("out-top"),
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
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);
    std::fs::write(leaf_dir.join("NOTES.md"), "x\n").unwrap();
    git_in(&leaf_dir, &["add", "-A"]);
    git_in(&leaf_dir, &["commit", "-qm", "advance leaf"]);

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let skills = doc["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 2, "{doc}");
    let t = skills.iter().find(|r| r["transitive"] == true).unwrap();
    assert_eq!(t["requestedBy"], serde_json::json!(["top"]), "{doc}");
    assert_eq!(t["status"], "behind", "{doc}");
    assert_eq!(outdated_row(&doc, "top")["transitive"], false, "{doc}");

    let text = sb.ok(&["outdated"]);
    assert!(text.contains("(transitive; via top)"), "{text}");
    assert!(text.contains(": run `spm update` to re-pin"), "{text}");
}

#[cfg(unix)]
#[test]
fn outdated_works_in_global_scope() {
    let sb = Sandbox::new();
    sb.ok(&["init", "-g", "--target", "copilot"]);
    sb.ok(&[
        "add",
        "-g",
        &sb.skill_url(),
        "--branch",
        "main",
        "--name",
        "greet",
    ]);
    let tip = advance_main(&sb);
    let doc = json_doc(&sb.spm(&["outdated", "-g", "--json"]));
    assert_eq!(doc["scope"], "global", "{doc}");
    let greet = outdated_row(&doc, "greet");
    assert_eq!(greet["status"], "behind", "{doc}");
    assert_eq!(greet["remoteCommit"], tip.as_str(), "{doc}");
    let text = sb.ok(&["outdated", "-g"]);
    assert!(text.contains("run `spm update -g`"), "{text}");
    // The project scope has no ai.json, so it is unaffected.
    assert!(!sb.spm(&["outdated"]).status.success());
}

#[test]
fn outdated_honors_protocol_override_and_fallback() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    let good = sb.skill_url();
    // Only the HTTPS form of the recorded SSH URL is served.
    let remotes = [(HTTPS_URL, good.as_str())];
    let out = sb.spm_remotes(
        &[
            "add",
            SSH_URL,
            "--branch",
            "main",
            "--protocol",
            "https",
            "--name",
            "greet",
        ],
        &remotes,
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    advance_main(&sb);

    let out = sb.spm_remotes(&["outdated", "--json"], &remotes);
    assert!(!out.status.success(), "SSH is unreachable without a flag");
    assert_eq!(outdated_row(&json_doc(&out), "greet")["status"], "error");

    let out = sb.spm_remotes(&["outdated", "--json", "--protocol", "https"], &remotes);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(outdated_row(&json_doc(&out), "greet")["status"], "behind");

    let out = sb.spm_remotes(&["outdated", "--json", "--protocol-fallback"], &remotes);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains(&format!("used https ({HTTPS_URL})")));
    assert_eq!(outdated_row(&json_doc(&out), "greet")["status"], "behind");
}

#[test]
fn outdated_named_transitive_newer_tag_gets_the_same_guidance_as_unfiltered() {
    let sb = Sandbox::new();
    let leaf_dir = sb.root.join("tag-leaf");
    let leaf = make_repo(&leaf_dir, &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")]);
    git_in(&leaf_dir, &["tag", "v1.0.0"]);
    let top = make_repo(
        &sb.root.join("tag-top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","tag":"v1.0.0"}}}}}}"#),
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
    git_in(&leaf_dir, &["tag", "v1.1.0"]);

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let leaf_row = doc["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["transitive"] == true)
        .unwrap_or_else(|| panic!("no transitive row: {doc}"));
    assert_eq!(leaf_row["status"], "newer-tag", "{doc}");
    let leaf_name = leaf_row["name"].as_str().unwrap().to_string();

    let hint = "bump it in the upstream ai.json of top; then run `spm update`";
    let all = sb.ok(&["outdated"]);
    assert!(all.contains(hint), "{all}");
    let named = sb.ok(&["outdated", &leaf_name]);
    assert!(named.contains(hint), "{named}");
}

#[test]
fn outdated_checks_secondary_selectors_of_deduplicated_dependencies() {
    let sb = Sandbox::new();
    let leaf_dir = sb.root.join("dedup-leaf");
    let leaf = make_repo(&leaf_dir, &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")]);
    git_in(&leaf_dir, &["tag", "v1.0.0"]);
    let requester = |dir: &str, sel: &str| {
        make_repo(
            &sb.root.join(dir),
            &[
                ("SKILL.md", &format!("---\nname: {dir}\n---\nR.\n")),
                (
                    "ai.json",
                    &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}",{sel}}}}}}}"#),
                ),
            ],
        )
    };
    // Two roots request the same leaf commit by a branch and by a tag, so the
    // single deduplicated lock entry records the second in `also_satisfies`.
    let a = requester("a-top", r#""branch":"main""#);
    let b = requester("b-top", r#""tag":"v1.0.0""#);
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{
                "a-top":{{"git":"{a}","branch":"main"}},
                "b-top":{{"git":"{b}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let leaf_row = |doc: &serde_json::Value| {
        doc["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["transitive"] == true)
            .cloned()
            .unwrap_or_else(|| panic!("no transitive row: {doc}"))
    };
    let row = leaf_row(&doc);
    assert_eq!(row["status"], "up-to-date", "{doc}");
    let secondary = row["alsoSatisfies"].as_array().unwrap();
    assert_eq!(secondary.len(), 1, "{doc}");
    assert_eq!(secondary[0]["status"], "up-to-date", "{doc}");
    // Roots are walked in name order, so `a-top`'s branch is the primary.
    assert_eq!(
        row["selector"],
        serde_json::json!({ "branch": "main" }),
        "{doc}"
    );
    assert_eq!(
        secondary[0]["selector"],
        serde_json::json!({ "tag": "v1.0.0" }),
        "{doc}"
    );
    sb.ok(&["outdated", "--exit-code"]);

    // A newer semver tag at the same commit leaves both selectors' refs
    // unchanged, but the secondary tag selector is still outdated.
    git_in(&leaf_dir, &["tag", "v1.1.0"]);
    let out = sb.spm(&["outdated", "--json"]);
    let doc = json_doc(&out);
    let row = leaf_row(&doc);
    assert_eq!(row["status"], "up-to-date", "{doc}");
    assert_eq!(row["outdated"], true, "{doc}");
    let sec = &row["alsoSatisfies"][0];
    assert_eq!(sec["status"], "newer-tag", "{doc}");
    assert_eq!(sec["latestTag"], "v1.1.0", "{doc}");
    let out = sb.spm(&["outdated", "--exit-code"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("also tag:v1.0.0: newer tag v1.1.0"), "{text}");
    assert!(
        text.contains("newer tag v1.1.0 for tag:v1.0.0 — bump it in the upstream ai.json"),
        "{text}"
    );

    // Re-point the tag at a commit off `main`, leaving the branch untouched.
    git_in(&leaf_dir, &["checkout", "-q", "-b", "side"]);
    std::fs::write(leaf_dir.join("NOTES.md"), "x\n").unwrap();
    git_in(&leaf_dir, &["add", "-A"]);
    git_in(&leaf_dir, &["commit", "-qm", "side"]);
    git_in(&leaf_dir, &["tag", "-f", "v1.0.0"]);
    git_in(&leaf_dir, &["checkout", "-q", "main"]);

    let out = sb.spm(&["outdated", "--json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let doc = json_doc(&out);
    let row = leaf_row(&doc);
    assert_eq!(row["outdated"], true, "{doc}");
    // The primary branch did not move, yet the entry is outdated.
    assert_eq!(row["status"], "up-to-date", "{doc}");
    assert_eq!(row["alsoSatisfies"][0]["status"], "tag-moved", "{doc}");

    let out = sb.spm(&["outdated", "--exit-code"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let err = stderr_of(&out);
    assert!(err.contains("tag `v1.0.0`"), "{err}");
    assert!(text.contains("also tag:v1.0.0: tag moved"), "{text}");
    assert!(text.contains("run `spm update`"), "{text}");
}

#[test]
fn outdated_guides_a_newer_tag_through_a_pinned_transitive_requester() {
    // root (branch) -> middle (tag) -> leaf (tag): a newer leaf tag needs a
    // new middle release bumped in root's upstream ai.json, and since root
    // tracks a branch, `spm update` (not `install`) picks that up.
    let sb = Sandbox::new();
    let leaf_dir = sb.root.join("chain-leaf");
    let leaf = make_repo(&leaf_dir, &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")]);
    git_in(&leaf_dir, &["tag", "v1.0.0"]);
    let middle_dir = sb.root.join("chain-middle");
    let middle = make_repo(
        &middle_dir,
        &[
            ("SKILL.md", "---\nname: middle\n---\nMiddle.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","tag":"v1.0.0"}}}}}}"#),
            ),
        ],
    );
    git_in(&middle_dir, &["tag", "v1.0.0"]);
    let root = make_repo(
        &sb.root.join("chain-root"),
        &[
            ("SKILL.md", "---\nname: root\n---\nRoot.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"middle":{{"git":"{middle}","tag":"v1.0.0"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    let manifest = |sel: &str| {
        format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"root":{{"git":"{root}",{sel}}}}}}}"#
        )
    };
    write_manifest(&sb, &manifest(r#""branch":"main""#));
    sb.ok(&["install"]);
    git_in(&leaf_dir, &["tag", "v1.1.0"]);

    let doc = json_doc(&sb.spm(&["outdated", "--json"]));
    let names: Vec<String> = doc["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    let leaf_row = doc["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["status"] == "newer-tag")
        .unwrap_or_else(|| panic!("no newer-tag row: {doc}"));
    let middle_name = leaf_row["requestedBy"][0].as_str().unwrap().to_string();
    assert!(names.contains(&middle_name), "{doc}");

    let text = sb.ok(&["outdated"]);
    let expected = format!(
        "bump it in the upstream ai.json of {middle_name}; publish a new {middle_name} version \
         and bump {middle_name} in the upstream ai.json of root; then run `spm update`"
    );
    assert!(text.contains(&expected), "{text}");

    // With root pinned to a tag too, root's selector changes locally.
    git_in(&sb.root.join("chain-root"), &["tag", "v1.0.0"]);
    write_manifest(&sb, &manifest(r#""tag":"v1.0.0""#));
    sb.ok(&["install"]);
    let text = sb.ok(&["outdated"]);
    let expected = format!(
        "publish a new {middle_name} version and bump {middle_name} in the upstream ai.json of \
         root; then point root at a version that includes it in ai.json and run `spm install`"
    );
    assert!(text.contains(&expected), "{text}");
}
