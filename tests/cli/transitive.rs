//! Transitive skill dependencies.

use crate::common::*;

/// With the opt-in flag *off* (the default), a resolved skill's own nested
/// `ai.json` is ignored entirely — behavior is exactly as before.
#[test]
fn transitive_disabled_by_default_ignores_nested_ai_json() {
    let sb = Sandbox::new();
    let leaf = make_repo(
        &sb.root.join("leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf skill.\n")],
    );
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop skill.\n"),
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
            r#"{{"targets":["copilot"],"skills":{{"top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);

    assert_eq!(
        copilot_skill_dirs(&sb),
        vec!["top".to_string()],
        "nested ai.json must be ignored when resolveTransitive is off"
    );
    let lock = sb.read("ai.lock");
    assert!(
        !lock.contains("requested_by"),
        "no provenance without opt-in"
    );
}

/// With `resolveTransitive: true`, a skill's nested `ai.json` dependency is
/// fetched, scanned, and materialized for *every* configured target, and pinned
/// into `ai.lock` with provenance.
#[test]
fn transitive_resolves_and_materializes_for_all_targets() {
    let sb = Sandbox::new();
    let leaf = make_repo(
        &sb.root.join("leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf skill.\n")],
    );
    let top = make_repo(
        &sb.root.join("top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop skill.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "claude", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude","copilot"],"resolveTransitive":true,"skills":{{"top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.ok(&["install"]);
    assert!(out.contains("transitive (via top)"), "{out}");

    // Copilot: both the direct `top` and the synthesized transitive leaf dir.
    let dirs = copilot_skill_dirs(&sb);
    assert!(dirs.contains(&"top".to_string()), "{dirs:?}");
    let leaf_dir = dirs
        .iter()
        .find(|n| n.starts_with("top__leaf-"))
        .unwrap_or_else(|| panic!("no transitive leaf dir in {dirs:?}"));
    assert!(sb
        .project
        .join(".agents/skills/spm-managed-skills")
        .join(leaf_dir)
        .join("SKILL.md")
        .exists());

    // Claude: the same two skills land in its project-local marketplace.
    let cdir = sb.claude_market_dir().join("plugin/skills");
    assert!(cdir.join("top/SKILL.md").exists(), "claude top");
    assert!(cdir.join(leaf_dir).join("SKILL.md").exists(), "claude leaf");

    // ai.lock records the transitive entry with one-hop provenance.
    let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    let entry = &lock["skills"][leaf_dir];
    assert_eq!(entry["requested_by"], serde_json::json!(["top"]), "{lock}");
}

/// A diamond (two requesters reaching the same repo+ref) resolves the shared
/// skill exactly once, with both requesters recorded in `requested_by`.
#[test]
fn transitive_diamond_resolves_shared_skill_once() {
    let sb = Sandbox::new();
    let shared = make_repo(
        &sb.root.join("shared"),
        &[("SKILL.md", "---\nname: shared\n---\nShared.\n")],
    );
    let dep_json = format!(r#"{{"skills":{{"shared":{{"git":"{shared}","branch":"main"}}}}}}"#);
    let left = make_repo(
        &sb.root.join("left"),
        &[
            ("SKILL.md", "---\nname: left\n---\nLeft.\n"),
            ("ai.json", &dep_json),
        ],
    );
    let right = make_repo(
        &sb.root.join("right"),
        &[
            ("SKILL.md", "---\nname: right\n---\nRight.\n"),
            ("ai.json", &dep_json),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"left":{{"git":"{left}","branch":"main"}},"right":{{"git":"{right}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);

    let dirs = copilot_skill_dirs(&sb);
    let shared_dirs: Vec<&String> = dirs.iter().filter(|n| n.contains("__shared-")).collect();
    assert_eq!(
        shared_dirs.len(),
        1,
        "shared skill must be materialized exactly once: {dirs:?}"
    );
    let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    let entry = &lock["skills"][shared_dirs[0]];
    assert_eq!(
        entry["requested_by"],
        serde_json::json!(["left", "right"]),
        "both requesters recorded, sorted+deduped: {lock}"
    );
}

/// `spm update <name>` is deliberately surgical: it advances the named direct
/// root and re-reads its nested manifest, but does NOT cascade a moving-ref
/// refresh into that root's unchanged transitive children — a child requested
/// by the same branch stays pinned until a bare `spm update` refreshes the whole
/// frontier. Guards the `resolve_child` non-cascade contract.
#[test]
fn update_named_root_does_not_cascade_refresh_to_transitive_children() {
    let sb = Sandbox::new();
    let leaf_dir = sb.root.join("leaf");
    let leaf = make_repo(
        &leaf_dir,
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf v1.\n")],
    );
    let leaf_c1 = skill_head(&leaf_dir);
    let top_dir = sb.root.join("top");
    make_repo(
        &top_dir,
        &[
            ("SKILL.md", "---\nname: top\n---\nTop v1.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    let top = format!(
        "file://{}",
        top_dir.display().to_string().replace('\\', "/")
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"top":{{"git":"{top}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);

    // Helper: the synthesized transitive-leaf key + its pinned commit in ai.lock.
    let leaf_pin = |sb: &Sandbox| -> String {
        let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
        let skills = lock["skills"].as_object().unwrap();
        let (_, entry) = skills
            .iter()
            .find(|(k, _)| k.starts_with("top__leaf-"))
            .unwrap_or_else(|| panic!("no transitive leaf entry in {lock}"));
        entry["commit"].as_str().unwrap().to_string()
    };
    let top_pin = |sb: &Sandbox| -> String {
        let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
        lock["skills"]["top"]["commit"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(leaf_pin(&sb), leaf_c1, "leaf pinned at v1 after install");

    // Advance BOTH branches upstream: top -> T2 (root itself moves) and the
    // still-`branch:main` leaf child -> L2 (moving-ref tip advances). top's
    // nested ai.json is unchanged (still `leaf@main`).
    std::fs::write(top_dir.join("SKILL.md"), "---\nname: top\n---\nTop v2.\n").unwrap();
    git_in(&top_dir, &["commit", "-aqm", "top v2"]);
    let top_t2 = skill_head(&top_dir);
    std::fs::write(
        leaf_dir.join("SKILL.md"),
        "---\nname: leaf\n---\nLeaf v2.\n",
    )
    .unwrap();
    git_in(&leaf_dir, &["commit", "-aqm", "leaf v2"]);
    let leaf_l2 = skill_head(&leaf_dir);
    assert_ne!(leaf_c1, leaf_l2, "leaf branch must have actually advanced");

    // `spm update top`: the named root advances to T2, but its unchanged
    // same-branch leaf child stays pinned at v1 (surgical, no cascade).
    sb.ok(&["update", "top"]);
    assert_eq!(
        top_pin(&sb),
        top_t2,
        "named root advanced to its latest commit"
    );
    assert_eq!(
        leaf_pin(&sb),
        leaf_c1,
        "update <name> must NOT cascade a moving-ref refresh into the child"
    );

    // A bare `spm update` refreshes the whole frontier: the leaf child advances.
    sb.ok(&["update"]);
    assert_eq!(
        leaf_pin(&sb),
        leaf_l2,
        "bare update must advance the transitive child to the branch tip"
    );
}

/// A dependency cycle wholly among *transitive* nodes (root -> a -> b -> a) is
/// detected and reported, not looped into a stack overflow. The cycle must not
/// route through a direct root (those are a satisfied back-edge, see
/// `transitive_back_edge_onto_direct_root_is_satisfied`), so the reproducer uses
/// a distinct root `r` whose transitive subtree `a <-> b` closes on itself.
#[test]
fn transitive_cycle_is_detected() {
    let sb = Sandbox::new();
    let r_dir = sb.root.join("cyc-r");
    let a_dir = sb.root.join("cyc-a");
    let b_dir = sb.root.join("cyc-b");
    let r_url = format!("file://{}", r_dir.display().to_string().replace('\\', "/"));
    let a_url = format!("file://{}", a_dir.display().to_string().replace('\\', "/"));
    let b_url = format!("file://{}", b_dir.display().to_string().replace('\\', "/"));
    make_repo(
        &r_dir,
        &[
            ("SKILL.md", "---\nname: r\n---\nR.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"a":{{"git":"{a_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    make_repo(
        &a_dir,
        &[
            ("SKILL.md", "---\nname: a\n---\nA.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"b":{{"git":"{b_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    make_repo(
        &b_dir,
        &[
            ("SKILL.md", "---\nname: b\n---\nB.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"a":{{"git":"{a_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"r":{{"git":"{r_url}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "a cycle must fail the install");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("dependency cycle detected"), "{err}");
}

/// A *differing-reference* back-edge onto an active transitive ancestor
/// (`r -> a@main -> b -> a@other`) is still a self-dependency cycle — not an
/// independent version conflict. `b` lives inside `a`'s own subtree, so the two
/// `a` requirements are not independent requesters; you cannot even discover
/// `b`'s requirement without first resolving `a`. Guards the classification
/// against regressing into a (misleading) version-conflict report, or a panic,
/// merely because the second edge names a different ref than the ancestor.
#[test]
fn transitive_differing_ref_back_edge_onto_ancestor_is_a_cycle() {
    let sb = Sandbox::new();
    let a_dir = sb.root.join("drc-a");
    let b_dir = sb.root.join("drc-b");
    let b_url = format!("file://{}", b_dir.display().to_string().replace('\\', "/"));
    // `a` carries two branches at different commits so the back-edge (`a@other`)
    // names a genuinely different reference than the one `a` is resolved at
    // (`a@main`). `a@other` is never fetched — the cycle fires on identity,
    // before the differing ref is ever resolved.
    let a_url = make_repo(
        &a_dir,
        &[
            ("SKILL.md", "---\nname: a\n---\nA v1.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"b":{{"git":"{b_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    git_in(&a_dir, &["checkout", "-q", "-b", "other"]);
    std::fs::write(a_dir.join("SKILL.md"), "---\nname: a\n---\nA v2.\n").unwrap();
    git_in(&a_dir, &["add", "-A"]);
    git_in(&a_dir, &["commit", "-qm", "a v2"]);
    git_in(&a_dir, &["checkout", "-q", "main"]);
    // `b` closes the loop back onto `a`, but by the *other* branch.
    make_repo(
        &b_dir,
        &[
            ("SKILL.md", "---\nname: b\n---\nB.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"a":{{"git":"{a_url}","branch":"other"}}}}}}"#),
            ),
        ],
    );
    let r_url = make_repo(
        &sb.root.join("drc-r"),
        &[
            ("SKILL.md", "---\nname: r\n---\nR.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"a":{{"git":"{a_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"r":{{"git":"{r_url}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(
        !out.status.success(),
        "a self-dependency cycle must fail the install"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("dependency cycle detected"), "{err}");
    assert!(
        !err.contains("version conflict"),
        "an on-branch self-dependency is a cycle, not a version conflict: {err}"
    );
}

/// A transitive back-edge whose identity lands on a directly declared root —
/// even while that root is still on the active DFS branch (`a -> c -> a`) — is a
/// satisfied diamond against the root's fixed install point, NOT a cycle. This
/// is the on-branch case the stack guard previously mis-flagged: `a` is pushed
/// for the whole walk of its own subtree, so `c -> a` sees `a` on the stack.
#[test]
fn transitive_back_edge_onto_direct_root_is_satisfied() {
    let sb = Sandbox::new();
    let a_dir = sb.root.join("be-a");
    let c_dir = sb.root.join("be-c");
    let a_url = format!("file://{}", a_dir.display().to_string().replace('\\', "/"));
    let c_url = format!("file://{}", c_dir.display().to_string().replace('\\', "/"));
    // a (direct root) -> c (transitive) -> a (back to the root).
    make_repo(
        &a_dir,
        &[
            ("SKILL.md", "---\nname: a\n---\nA.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"c":{{"git":"{c_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    make_repo(
        &c_dir,
        &[
            ("SKILL.md", "---\nname: c\n---\nC.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"a":{{"git":"{a_url}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"a":{{"git":"{a_url}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(
        out.status.success(),
        "a back-edge onto a direct root must resolve as a diamond, not a cycle: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Both the root `a` and the transitive `c` are materialized; the back-edge
    // dedups against the root instead of synthesizing a second copy of `a`.
    let dirs = copilot_skill_dirs(&sb);
    assert!(dirs.iter().any(|n| n == "a"), "root a present: {dirs:?}");
    assert!(
        dirs.iter().any(|n| n.contains("__c-")),
        "transitive c present: {dirs:?}"
    );
    assert!(
        !dirs.iter().any(|n| n.contains("__a-")),
        "root a is not re-synthesized as a transitive copy: {dirs:?}"
    );
}

/// Two skills whose names differ only by case materialize into the same vendor
/// directory on case-insensitive filesystems (Windows, the default macOS APFS),
/// so one silently overwrites the other. `install` rejects the case-folding
/// collision before any vendor writes a byte.
#[test]
fn case_folding_skill_name_collision_is_rejected() {
    let sb = Sandbox::new();
    let foo = make_repo(
        &sb.root.join("foo"),
        &[("SKILL.md", "---\nname: foo\n---\nFoo.\n")],
    );
    let bar = make_repo(
        &sb.root.join("bar"),
        &[("SKILL.md", "---\nname: bar\n---\nBar.\n")],
    );
    sb.ok(&["init", "--target", "copilot"]);
    // `Foo` and `foo` are distinct manifest keys but fold to the same directory.
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"skills":{{"Foo":{{"git":"{foo}","branch":"main"}},"foo":{{"git":"{bar}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(
        !out.status.success(),
        "a case-folding name collision must fail the install"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("differ only by case"), "{err}");
}

/// Plugins materialize into their own vendor directory, so the same
/// case-insensitive-filesystem overwrite hazard applies independently of
/// skills. Two plugin keys differing only by case must be rejected before any
/// vendor writes a byte — closing the Windows/macOS collision gap for the
/// plugin set, not just the skill set.
#[test]
fn case_folding_plugin_name_collision_is_rejected() {
    let sb = Sandbox::new();
    let plugin_json = |name: &str| {
        format!(
            r#"{{"name":"{name}","version":"1.0.0","mcpServers":{{"demo":{{"command":"node","args":["x"]}}}}}}"#
        )
    };
    let one = make_repo(
        &sb.root.join("plugin-one"),
        &[(".claude-plugin/plugin.json", &plugin_json("one"))],
    );
    let two = make_repo(
        &sb.root.join("plugin-two"),
        &[(".claude-plugin/plugin.json", &plugin_json("two"))],
    );
    sb.ok(&["init", "--target", "claude"]);
    // `Ds` and `ds` are distinct manifest keys that fold to the same plugin dir.
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["claude"],"plugins":{{"Ds":{{"git":"{one}","branch":"main"}},"ds":{{"git":"{two}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(
        !out.status.success(),
        "a case-folding plugin name collision must fail the install"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("plugin name collision"), "{err}");
    assert!(err.contains("differ only by case"), "{err}");
}

/// `resolve_child` scopes lockfile reuse to the exact edge (its synthesized
/// name), so a newly-declared transitive edge resolves fresh at the current tip
/// instead of inheriting a stale pin left behind by a removed root. Guards
/// against a brand-new child being silently pinned to an unrelated old commit.
#[test]
fn newly_discovered_transitive_edge_does_not_reuse_stale_pin() {
    let sb = Sandbox::new();
    let shared_dir = sb.root.join("shared");
    let shared = make_repo(
        &shared_dir,
        &[("SKILL.md", "---\nname: shared\n---\nShared v1.\n")],
    );
    let shared_v1 = skill_head(&shared_dir);

    let dep_json = format!(r#"{{"skills":{{"shared":{{"git":"{shared}","branch":"main"}}}}}}"#);
    let old = make_repo(
        &sb.root.join("old-root"),
        &[
            ("SKILL.md", "---\nname: old\n---\nOld.\n"),
            ("ai.json", &dep_json),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"old":{{"git":"{old}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);

    let shared_pin = |sb: &Sandbox| -> String {
        let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
        let skills = lock["skills"].as_object().unwrap();
        let (_, e) = skills
            .iter()
            .find(|(k, _)| k.contains("__shared-"))
            .unwrap_or_else(|| panic!("no transitive shared entry in {lock}"));
        e["commit"].as_str().unwrap().to_string()
    };
    assert_eq!(
        shared_pin(&sb),
        shared_v1,
        "shared pinned at v1 after install"
    );

    // Advance shared upstream to v2.
    std::fs::write(
        shared_dir.join("SKILL.md"),
        "---\nname: shared\n---\nShared v2.\n",
    )
    .unwrap();
    git_in(&shared_dir, &["commit", "-aqm", "shared v2"]);
    let shared_v2 = skill_head(&shared_dir);
    assert_ne!(shared_v1, shared_v2, "shared branch must have advanced");

    // Swap `old` out for a brand-new root that also declares shared@main. The
    // removed root's `old__shared-*` entry lingers in the previous lockfile.
    let new = make_repo(
        &sb.root.join("new-root"),
        &[
            ("SKILL.md", "---\nname: new\n---\nNew.\n"),
            ("ai.json", &dep_json),
        ],
    );
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"new":{{"git":"{new}","branch":"main"}}}}}}"#
        ),
    );
    // A plain install (no refresh): the new edge has no pin of its own, so it
    // resolves fresh at the tip rather than reusing the stale v1 pin.
    sb.ok(&["install"]);
    assert_eq!(
        shared_pin(&sb),
        shared_v2,
        "a newly-discovered edge must resolve fresh, not reuse the removed root's stale pin"
    );
}

/// A nested dependency manifest that declares a skill whose name is invalid as a
/// filesystem path component (here a Windows-invalid `*`) is rejected when the
/// parent's `ai.json` is read — before the named child is synthesized or
/// fetched. Guards the transitive surface against unportable/hostile names
/// reaching the materializer on a case- or device-sensitive platform.
#[test]
fn transitive_nested_invalid_skill_name_is_rejected_before_fetch() {
    let sb = Sandbox::new();
    // The child URL points nowhere resolvable: if the guard fired *after* trying
    // to fetch, we'd see a git/resolve error instead of the name error.
    let child_url = "file:///nonexistent/never-fetched";
    let dep_json =
        format!(r#"{{"skills":{{"bad*name":{{"git":"{child_url}","branch":"main"}}}}}}"#);
    let parent = make_repo(
        &sb.root.join("parent"),
        &[
            ("SKILL.md", "---\nname: parent\n---\nParent.\n"),
            ("ai.json", &dep_json),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"parent":{{"git":"{parent}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(
        !out.status.success(),
        "an invalid nested skill name must fail the install"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("invalid skill name"), "{err}");
    assert!(err.contains("not allowed"), "{err}");
}

/// A transitive dependency chain deeper than the hard depth cap fails fast with
/// a clear error instead of resolving unbounded nesting. Guards the depth-cap
/// boundary that the cycle/diamond tests do not exercise.
#[test]
fn transitive_depth_cap_is_enforced() {
    let sb = Sandbox::new();
    // Build a linear chain d0 -> d1 -> ... -> d11, each declaring the next.
    // With the cap at 8, resolution must bail before reaching the deepest node.
    const LEVELS: usize = 12;
    let dirs: Vec<_> = (0..LEVELS)
        .map(|i| sb.root.join(format!("depth-{i}")))
        .collect();
    let urls: Vec<String> = dirs
        .iter()
        .map(|d| format!("file://{}", d.display().to_string().replace('\\', "/")))
        .collect();
    for i in 0..LEVELS {
        let mut files = vec![(
            "SKILL.md".to_string(),
            format!("---\nname: d{i}\n---\nLevel {i}.\n"),
        )];
        if i + 1 < LEVELS {
            files.push((
                "ai.json".to_string(),
                format!(
                    r#"{{"skills":{{"d{next}":{{"git":"{url}","branch":"main"}}}}}}"#,
                    next = i + 1,
                    url = urls[i + 1]
                ),
            ));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        make_repo(&dirs[i], &refs);
    }
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"d0":{{"git":"{}","branch":"main"}}}}}}"#,
            urls[0]
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(
        !out.status.success(),
        "an over-deep chain must fail the install"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("depth cap"), "reports the depth cap: {err}");
    // The deepest node must never be materialized — resolution stops at the cap.
    assert!(
        !copilot_skill_dirs(&sb).iter().any(|n| n.contains("d11")),
        "the beyond-cap node must not be materialized"
    );
}

/// A single nested `ai.json` with a very large fan-out is refused by the global
/// edge budget, independently of chain depth. The depth cap bounds how *deep* a
/// graph goes; it does nothing about one manifest declaring thousands of edges,
/// so an abusive (or buggy) manifest could still force unbounded per-edge work.
/// Here the edges all re-touch one identity, so the budget trips cheaply (one
/// real fetch) — proving the counter aborts the walk before the fan-out runs
/// away, not that the children happen to be expensive.
#[test]
fn transitive_fan_out_edge_budget_is_enforced() {
    let sb = Sandbox::new();
    let child = make_repo(
        &sb.root.join("fan-child"),
        &[("SKILL.md", "---\nname: child\n---\nChild.\n")],
    );
    // One parent manifest declaring well over MAX_EDGES (1024) distinct edges,
    // all onto the same child identity. Distinct keys are legal and dedup to a
    // single node, so only the edge counter — not fetching — trips the cap.
    let mut entries = String::new();
    for i in 0..1100 {
        if i > 0 {
            entries.push(',');
        }
        entries.push_str(&format!(
            r#""dep{i:04}":{{"git":"{child}","branch":"main"}}"#
        ));
    }
    let parent = make_repo(
        &sb.root.join("fan-parent"),
        &[
            ("SKILL.md", "---\nname: parent\n---\nParent.\n"),
            ("ai.json", &format!(r#"{{"skills":{{{entries}}}}}"#)),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"parent":{{"git":"{parent}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(
        !out.status.success(),
        "a huge single-manifest fan-out must fail the install"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("edge cap"), "reports the edge cap: {err}");
}

/// Two requesters that pull the same repo at different commits is a version
/// conflict, reported with both requesters rather than silently picking one.
#[test]
fn transitive_version_conflict_is_reported() {
    let sb = Sandbox::new();
    // A shared repo with two branches at different commits.
    let shared_dir = sb.root.join("conflict-shared");
    let shared = make_repo(
        &shared_dir,
        &[("SKILL.md", "---\nname: shared\n---\nv1.\n")],
    );
    git_in(&shared_dir, &["checkout", "-q", "-b", "other"]);
    std::fs::write(shared_dir.join("SKILL.md"), "---\nname: shared\n---\nv2.\n").unwrap();
    git_in(&shared_dir, &["add", "-A"]);
    git_in(&shared_dir, &["commit", "-qm", "second"]);

    let left = make_repo(
        &sb.root.join("cf-left"),
        &[
            ("SKILL.md", "---\nname: left\n---\nLeft.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"shared":{{"git":"{shared}","branch":"main"}}}}}}"#),
            ),
        ],
    );
    let right = make_repo(
        &sb.root.join("cf-right"),
        &[
            ("SKILL.md", "---\nname: right\n---\nRight.\n"),
            (
                "ai.json",
                &format!(r#"{{"skills":{{"shared":{{"git":"{shared}","branch":"other"}}}}}}"#),
            ),
        ],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"resolveTransitive":true,"skills":{{"left":{{"git":"{left}","branch":"main"}},"right":{{"git":"{right}","branch":"main"}}}}}}"#
        ),
    );
    let out = sb.spm(&["install"]);
    assert!(!out.status.success(), "a version conflict must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("version conflict"), "{err}");
    assert!(
        err.contains("left") && err.contains("right"),
        "names both requesters: {err}"
    );
}

/// A dependency's own `ai.json` may not carry `resolveTransitive` — recursion is
/// governed solely by the root project, so a nested flag is a hard error.
#[test]
fn nested_resolve_transitive_flag_is_rejected() {
    let sb = Sandbox::new();
    let leaf = make_repo(
        &sb.root.join("nt-leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")],
    );
    let top = make_repo(
        &sb.root.join("nt-top"),
        &[
            ("SKILL.md", "---\nname: top\n---\nTop.\n"),
            (
                "ai.json",
                &format!(
                    r#"{{"resolveTransitive":true,"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#
                ),
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
    let out = sb.spm(&["install"]);
    assert!(
        !out.status.success(),
        "nested resolveTransitive must be rejected"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("resolveTransitive"), "{err}");
}

/// `spm list` and `spm status` surface transitive provenance.
#[test]
fn list_and_status_show_transitive_provenance() {
    let sb = Sandbox::new();
    let leaf = make_repo(
        &sb.root.join("prov-leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")],
    );
    let top = make_repo(
        &sb.root.join("prov-top"),
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

    let list = sb.ok(&["list"]);
    assert!(list.contains("transitive skills:"), "{list}");
    assert!(list.contains("via top"), "{list}");

    let status = sb.ok(&["status"]);
    assert!(status.contains("transitive; via top"), "{status}");
}

/// A direct skill whose reused lock entry carries stale `requested_by`
/// provenance — e.g. it was a synthesized transitive entry before being promoted
/// to a direct dependency — must be re-normalized to a provenance-free direct
/// entry on the next sync. `spm status`/`list` key "transitive" purely on a
/// non-empty `requested_by`, so a stale value would misreport a declared skill
/// as transitive.
#[test]
fn reused_direct_entry_drops_stale_transitive_provenance() {
    let sb = Sandbox::new();
    let leaf = make_repo(
        &sb.root.join("stale-prov-leaf"),
        &[("SKILL.md", "---\nname: leaf\n---\nLeaf.\n")],
    );
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        &format!(
            r#"{{"targets":["copilot"],"skills":{{"leaf":{{"git":"{leaf}","branch":"main"}}}}}}"#
        ),
    );
    sb.ok(&["install"]);

    // Simulate a lock whose direct `leaf` entry carries stale provenance (as if
    // it had been a synthesized transitive entry promoted to a direct dep).
    let mut lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    lock["skills"]["leaf"]["requested_by"] = serde_json::json!(["ghost"]);
    std::fs::write(
        sb.project.join("ai.lock"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // Re-sync: the entry is reused (spec unchanged) and must be re-normalized.
    sb.ok(&["install"]);

    let lock: serde_json::Value = serde_json::from_str(&sb.read("ai.lock")).unwrap();
    let prov = &lock["skills"]["leaf"]["requested_by"];
    assert!(
        prov.is_null() || *prov == serde_json::json!([]),
        "direct entry must not keep stale provenance: {lock}"
    );

    let status = sb.ok(&["status"]);
    assert!(
        !status.contains("transitive; via"),
        "direct skill must not be reported as transitive: {status}"
    );
}

/// The schema accepts `resolveTransitive` on the root manifest.
#[test]
fn schema_accepts_resolve_transitive_flag() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "copilot"]);
    write_manifest(
        &sb,
        r#"{"targets":["copilot"],"resolveTransitive":false,"skills":{}}"#,
    );
    // A manifest-reading command must accept the flag without a schema error.
    let out = sb.ok(&["list"]);
    assert!(out.contains("no skills"), "{out}");
}
