//! `--protocol` / `--protocol-fallback` transport flags.

use crate::common::*;

#[test]
fn protocol_flag_rewrites_the_url_but_manifest_keeps_the_supplied_one() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    // Only the HTTPS form is served; the SSH URL the user typed is unreachable.
    let out = sb.spm_remotes(
        &["add", SSH_URL, "--branch", "main", "--protocol", "https"],
        &[(HTTPS_URL, &good)],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        !stderr_of(&out).contains("note:"),
        "an override is not a fallback"
    );

    // ai.json and ai.lock still record the URL the user supplied.
    let manifest = sb.read("ai.json");
    assert!(manifest.contains(SSH_URL), "{manifest}");
    assert!(!manifest.contains("https://"), "{manifest}");
    let lock = sb.read("ai.lock");
    assert!(lock.contains(SSH_URL), "{lock}");
    assert!(!lock.contains("https://"), "{lock}");
}

#[test]
fn protocol_flag_uses_non_rewritable_urls_as_given() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let url = sb.skill_url();
    let out = sb.spm(&["add", &url, "--branch", "main", "--protocol", "ssh"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(sb.read("ai.json").contains(&url));
}

/// Regression test: without a `--tag`/`--branch`/`--commit` selector, `add`
/// first looks up the remote's default branch before resolving it. That
/// lookup must go through the same `--protocol` override as the rest of the
/// sync, not bypass it and contact the URL exactly as given.
#[test]
fn protocol_flag_applies_to_the_default_branch_lookup() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    // Only the HTTPS form is served; the SSH URL the user typed is unreachable.
    let out = sb.spm_remotes(
        &["add", SSH_URL, "--protocol", "https", "--name", "greet"],
        &[(HTTPS_URL, &good)],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("default branch"), "{stdout}");

    let manifest = sb.read("ai.json");
    assert!(manifest.contains(SSH_URL), "{manifest}");
    assert!(!manifest.contains("https://"), "{manifest}");
}

/// Regression test: the no-selector default-branch lookup is a genuine
/// remote-stage operation, so a connection/auth failure there must be
/// eligible for `--protocol-fallback`'s retry over the other protocol, just
/// like the ref/commit resolution that follows it.
#[test]
fn protocol_fallback_applies_to_the_default_branch_lookup() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    // HTTPS is unreachable; SSH works. No --branch/--tag/--commit, so `add`
    // must look up the default branch before anything else.
    let out = sb.spm_remotes(
        &["add", HTTPS_URL, "--protocol-fallback", "--name", "greet"],
        &[(HTTPS_URL, unreachable_url()), (SSH_URL, good.as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let err = stderr_of(&out);
    assert!(
        err.contains(&format!("used ssh ({SSH_URL})")),
        "the message must name the protocol that worked: {err}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("default branch"), "{stdout}");

    // The manifest keeps the HTTPS URL the user supplied, never the fallback.
    let manifest = sb.read("ai.json");
    assert!(manifest.contains(HTTPS_URL), "{manifest}");
    assert!(!manifest.contains(SSH_URL), "{manifest}");
}

/// `@<ref>` to a tag or branch via its own `git ls-remote` (see
/// `cli::shorthand::classify`), before the usual add/sync flow runs. That
/// lookup must go through the same `--protocol` override too, not bypass it
/// and contact the expanded `https://github.com/...` URL as given.
#[test]
fn protocol_flag_applies_to_the_github_shorthand_ref_lookup() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    // The shorthand always expands to the HTTPS form; only the SSH form (what
    // --protocol ssh rewrites it to) is served, so the lookup only succeeds if
    // it is actually forced over SSH.
    let out = sb.spm_remotes(
        &[
            "add",
            "github.com/org/skill@v0.1.0",
            "--protocol",
            "ssh",
            "--name",
            "greet",
        ],
        &[("git@github.com:org/skill.git", &good)],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let manifest: serde_json::Value = serde_json::from_str(&sb.read("ai.json")).unwrap();
    assert_eq!(manifest["skills"]["greet"]["tag"], "v0.1.0", "{manifest}");
    assert_eq!(
        manifest["skills"]["greet"]["git"], "https://github.com/org/skill.git",
        "{manifest}"
    );
}

#[test]
fn without_the_flags_the_url_is_used_as_given_with_no_retry() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    // The HTTPS form would work, but the user asked for SSH and passed no flag.
    let out = sb.spm_remotes(&["add", SSH_URL, "--branch", "main"], &[(HTTPS_URL, &good)]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        !err.contains("https://"),
        "must not have tried https: {err}"
    );
    assert!(!err.contains("note:"), "{err}");
    assert!(!sb.project.join(".spm").exists(), "nothing installed");
}

#[test]
fn protocol_fallback_retries_the_other_protocol_and_names_it() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    // HTTPS is unreachable; SSH works.
    let remotes = [(HTTPS_URL, unreachable_url()), (SSH_URL, good.as_str())];
    let out = sb.spm_remotes(
        &["add", HTTPS_URL, "--branch", "main", "--protocol-fallback"],
        &remotes,
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let err = stderr_of(&out);
    assert!(
        err.contains(&format!("used ssh ({SSH_URL})")),
        "the message must name the protocol that worked: {err}"
    );
    assert!(err.contains("mask a credential problem"), "{err}");

    // The manifest keeps the HTTPS URL the user supplied, never the fallback.
    let manifest = sb.read("ai.json");
    assert!(manifest.contains(HTTPS_URL), "{manifest}");
    assert!(!manifest.contains(SSH_URL), "{manifest}");
    assert!(!sb.read("ai.lock").contains(SSH_URL));

    // `install` re-fetches into the store through the same fallback.
    sb.ok(&["prune", "--yes"]);
    let out = sb.spm_remotes(&["install", "--protocol-fallback"], &remotes);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains(&format!("used ssh ({SSH_URL})")));
    assert!(sb.claude_market_dir().join("plugin/skills/skill").exists());

    // Without the opt-in, the same install fails rather than switching silently.
    sb.ok(&["prune", "--yes"]);
    let out = sb.spm_remotes(&["install"], &remotes);
    assert!(!out.status.success());
    assert!(!stderr_of(&out).contains("note:"));
}

#[test]
fn protocol_fallback_error_mentions_both_attempts_when_both_fail() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let remotes = [
        (HTTPS_URL, unreachable_url()),
        (SSH_URL, "file:///spm-test-also-unreachable/skill.git"),
    ];
    let out = sb.spm_remotes(
        &["add", HTTPS_URL, "--branch", "main", "--protocol-fallback"],
        &remotes,
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("either protocol"), "{err}");
    assert!(err.contains(&format!("https ({HTTPS_URL})")), "{err}");
    assert!(err.contains(&format!("ssh ({SSH_URL})")), "{err}");
    assert!(err.contains("spm-test-unreachable"), "first failure: {err}");
    assert!(
        err.contains("spm-test-also-unreachable"),
        "second failure: {err}"
    );
}

#[test]
fn protocol_fallback_does_not_retry_when_the_ref_is_missing() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    // HTTPS reaches the repo; SSH would fail. A missing ref/path is not a
    // connectivity problem, so SSH must never be tried (its failure would show).
    let remotes = [(HTTPS_URL, good.as_str()), (SSH_URL, unreachable_url())];

    let out = sb.spm_remotes(
        &[
            "add",
            HTTPS_URL,
            "--branch",
            "no-such-branch",
            "--protocol-fallback",
        ],
        &remotes,
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("not found"), "{err}");
    assert!(
        !err.contains("either protocol") && !err.contains("note:"),
        "{err}"
    );
    assert!(
        !err.contains("spm-test-unreachable"),
        "ssh was tried: {err}"
    );
}

#[test]
fn protocol_fallback_does_not_retry_when_the_path_is_missing() {
    let sb = Sandbox::new();
    sb.ok(&["init", "--target", "claude"]);
    let good = sb.skill_url();
    let remotes = [(HTTPS_URL, good.as_str()), (SSH_URL, unreachable_url())];
    let out = sb.spm_remotes(
        &[
            "add",
            HTTPS_URL,
            "--branch",
            "main",
            "--path",
            "no/such/dir",
            "--protocol-fallback",
        ],
        &remotes,
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("path `no/such/dir` not found"), "{err}");
    assert!(
        !err.contains("either protocol") && !err.contains("note:"),
        "{err}"
    );
    assert!(
        !err.contains("spm-test-unreachable"),
        "ssh was tried: {err}"
    );
}

/// The protocol flags exist only on the commands that can contact a remote (the
/// ones that run a sync); everywhere else clap rejects them rather than
/// silently ignoring them.
#[test]
fn protocol_flags_are_scoped_to_commands_that_contact_remotes() {
    let sb = Sandbox::new();
    for cmd in [
        &["add"][..],
        &["install"],
        &["update"],
        &["remove"],
        &["target", "add"],
        &["outdated"],
    ] {
        let mut args = cmd.to_vec();
        args.push("--help");
        let help = String::from_utf8_lossy(&sb.spm(&args).stdout).into_owned();
        assert!(help.contains("--protocol "), "{cmd:?}: {help}");
        assert!(help.contains("--protocol-fallback"), "{cmd:?}: {help}");
    }
    for cmd in [
        &["init"][..],
        &["list"],
        &["status"],
        &["clean"],
        &["prune"],
        &["scan"],
    ] {
        for flag in [&["--protocol", "https"][..], &["--protocol-fallback"]] {
            let mut args = cmd.to_vec();
            args.extend_from_slice(flag);
            let out = sb.spm(&args);
            assert!(!out.status.success(), "{args:?} should be rejected");
            assert!(
                stderr_of(&out).contains("unexpected argument"),
                "{args:?}: {}",
                stderr_of(&out)
            );
        }
    }
}
