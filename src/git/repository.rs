//! Operations on a repository: resolving and listing remote refs, probing a
//! remote, fetching one commit into a checkout, and local work-tree queries.

use super::command::{git, git_command, git_remote, reject_option_like, run_git};
use super::transport::{is_connectivity_failure, transport, with_transport};
use crate::ids::CommitSha;
use crate::remote::{protocol_of, rewrite, Protocol};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// Resolve remote refs to a commit SHA without cloning. Pass multiple refspecs
/// (e.g. a tag and its `^{}` peel) — the peeled/dereferenced commit wins, so
/// annotated tags resolve to the underlying commit rather than the tag object.
pub fn ls_remote(url: &str, refspecs: &[&str]) -> Result<String> {
    reject_option_like("repository URL", url)?;
    for refspec in refspecs {
        reject_option_like("ref", refspec)?;
    }
    with_transport(transport(), url, |u| ls_remote_once(u, refspecs))
}

fn ls_remote_once(url: &str, refspecs: &[&str]) -> Result<String> {
    let mut args = vec!["ls-remote", url];
    args.extend_from_slice(refspecs);
    let out = git_remote(url, &args, None)?;
    if out.is_empty() {
        bail!("ref `{}` not found in {url}", refspecs.join(" "));
    }
    // Lines: "<sha>\t<ref>". Prefer a "<ref>^{}" (annotated-tag deref) line if present.
    let mut fallback: Option<String> = None;
    for line in out.lines() {
        let (sha, name) = line.split_once('\t').unwrap_or((line, ""));
        if name.ends_with("^{}") {
            return Ok(sha.to_string());
        }
        fallback.get_or_insert_with(|| sha.to_string());
    }
    fallback.context("could not parse ls-remote output")
}

/// List the remote ref names (e.g. `refs/tags/v1`) that match any of `refspecs`,
/// without cloning. Unlike [`ls_remote`], no match is an empty list rather than
/// an error, so callers can tell "ref absent" from "remote unreachable".
pub fn remote_ref_names(url: &str, refspecs: &[&str]) -> Result<Vec<String>> {
    Ok(remote_refs(url, refspecs)?.into_keys().collect())
}

/// Map every remote ref matching any of `refspecs` (globs such as
/// `refs/tags/*` allowed) to the commit it points at, in one `ls-remote` and
/// without cloning. An annotated tag maps to its peeled commit, not the tag
/// object, matching [`ls_remote`]. No match is an empty map, not an error.
pub fn remote_refs(url: &str, refspecs: &[&str]) -> Result<BTreeMap<String, String>> {
    reject_option_like("repository URL", url)?;
    for refspec in refspecs {
        reject_option_like("ref", refspec)?;
    }
    with_transport(transport(), url, |u| {
        let mut args = vec!["ls-remote", u];
        args.extend_from_slice(refspecs);
        Ok(parse_ref_shas(&git_remote(u, &args, None)?))
    })
}

/// Parse `ls-remote` output (`<sha>\t<ref>` lines) into `ref -> sha`, letting a
/// `<ref>^{}` peel line override the tag-object SHA whichever order they come in.
fn parse_ref_shas(out: &str) -> BTreeMap<String, String> {
    let mut refs = BTreeMap::new();
    let mut peeled = Vec::new();
    for (sha, name) in out.lines().filter_map(|line| line.split_once('\t')) {
        let sha = sha.to_lowercase();
        match name.strip_suffix("^{}") {
            Some(base) => peeled.push((base.to_string(), sha)),
            None => {
                refs.insert(name.to_string(), sha);
            }
        }
    }
    refs.extend(peeled);
    refs
}

/// Resolve the remote's default branch (what `HEAD` points at) without cloning,
/// via `git ls-remote --symref <url> HEAD`. Errors, rather than guessing, when
/// the remote reports no symbolic `HEAD` (empty repo, detached `HEAD`).
pub fn default_branch(url: &str) -> Result<String> {
    reject_option_like("repository URL", url)?;
    let out = with_transport(transport(), url, default_branch_once)?;
    parse_default_branch(&out).with_context(|| {
        format!(
            "could not determine the default branch of {url} (empty repository or detached \
             HEAD?); pass --branch, --tag or --commit explicitly"
        )
    })
}

fn default_branch_once(url: &str) -> Result<String> {
    git_remote(url, &["ls-remote", "--symref", url, "HEAD"], None)
}

/// Whether an ignore rule of the work tree containing `dir` matches `path`
/// (relative to `dir`), tracked or not — see [`is_tracked`] for whether the
/// rule applies. `None` when `dir` is not in a git work tree or git can't tell.
pub fn is_ignored(dir: &Path, path: &str) -> Option<bool> {
    let out = git_command(&["check-ignore", "-q", "--no-index", "--", path])
        .current_dir(dir)
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Whether `path` (relative to `dir`) is tracked by git (in the index), so
/// ignore rules don't apply to it. `None` when `dir` is not in a git work tree.
pub fn is_tracked(dir: &Path, path: &str) -> Option<bool> {
    // Literal: `path` is a file name, not a glob (`g[1]` must not match `g1`).
    let out = git_command(&[
        "--literal-pathspecs",
        "ls-files",
        "--error-unmatch",
        "--",
        path,
    ])
    .current_dir(dir)
    .output()
    .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        // Not tracked, or not in a work tree: only the former is an answer.
        Some(1) => is_ignored(dir, path).map(|_| false),
        _ => None,
    }
}

/// Outcome of [`probe`].
pub enum Reach {
    Reachable,
    Unreachable {
        error: anyhow::Error,
        /// The other protocol, when `url` could not be reached for a
        /// connectivity or auth reason but answers over it.
        other: Option<Protocol>,
    },
}

/// Check that `url` answers `git ls-remote` under the process transport and
/// source policy, without fetching anything. After a connectivity or auth
/// failure, also try the other protocol once (through the same policy gate) so
/// the caller can suggest `--protocol`/`--protocol-fallback`.
pub fn probe(url: &str) -> Reach {
    let head = |u: &str| git_remote(u, &["ls-remote", u, "HEAD"], None).map(drop);
    let attempt = reject_option_like("repository URL", url)
        .and_then(|()| with_transport(transport(), url, head));
    let Err(error) = attempt else {
        return Reach::Reachable;
    };
    let other = is_connectivity_failure(&error)
        .then(|| {
            let first = transport()
                .force
                .and_then(|p| rewrite(url, p))
                .unwrap_or_else(|| url.to_string());
            let to = protocol_of(&first)?.other();
            let second = rewrite(&first, to)?;
            let works = crate::policy::gate(&second).is_ok() && head(&second).is_ok();
            works.then_some(to)
        })
        .flatten();
    Reach::Unreachable { error, other }
}

/// Extract the branch name from the `ref: refs/heads/<name>\tHEAD` line that
/// `ls-remote --symref` prints ahead of the `<sha>\tHEAD` line.
fn parse_default_branch(out: &str) -> Option<String> {
    out.lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// True if `dir` is a git checkout already sitting at `sha`.
pub fn is_at_commit(dir: &Path, sha: &CommitSha) -> bool {
    if !dir.join(".git").exists() {
        return false;
    }
    matches!(git(&["rev-parse", "HEAD"], Some(dir)), Ok(head) if *sha == head)
}

/// Fetch just `sha` from `url` into a fresh checkout at `dest`.
///
/// Tries a shallow single-commit fetch first (cheapest); if the server refuses
/// fetch-by-SHA (not all enable `uploadpack.allowAnySHA1InWant`), falls back to
/// a full fetch. `dest` is created if missing.
pub fn fetch_commit(url: &str, sha: &CommitSha, dest: &Path) -> Result<()> {
    reject_option_like("repository URL", url)?;
    with_transport(transport(), url, |u| fetch_commit_once(u, sha, dest))
}

fn fetch_commit_once(url: &str, sha: &CommitSha, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    git(&["init", "-q"], Some(dest))?;
    // The remote already exists if a prior attempt (interrupted, or over another
    // protocol) got this far; point it at the URL now being tried.
    if git(&["remote", "add", "origin", url], Some(dest)).is_err() {
        git(&["remote", "set-url", "origin", url], Some(dest))?;
    }

    if git_remote(
        url,
        &["fetch", "--depth", "1", "origin", sha.as_str()],
        Some(dest),
    )
    .is_err()
    {
        git_remote(url, &["fetch", "origin"], Some(dest))
            .with_context(|| format!("fetching {url}"))?;
    }
    let lockdown = filter_lockdown(dest)?;
    let pre: Vec<&str> = lockdown.iter().map(String::as_str).collect();
    run_git(
        &pre,
        &["checkout", "--detach", sha.as_str()],
        Some(dest),
        false,
    )
    .with_context(|| {
        let mut msg = format!("checking out {sha} in {}", dest.display());
        if !lockdown.is_empty() {
            msg.push_str(
                " (a source policy is in force, so checkout filters such as Git LFS are \
                 disabled: what they download can't be checked against the policy)",
            );
        }
        msg
    })?;
    Ok(())
}

/// Under a source policy, disable every checkout filter driver configured for
/// `dest` (system, global, env and repository config alike).
///
/// The repository picks a filter through its own `.gitattributes`, and a smudge
/// filter can fetch content from wherever it likes — Git LFS reads its endpoint
/// from the repository's `.lfsconfig`, and its batch API hands out arbitrary
/// download URLs — none of which spm can check. So each driver's `smudge` and
/// `process` commands are replaced with one that fails, and the driver is
/// marked `required`: a checkout that would run a filter fails instead of
/// downloading, or of silently leaving unfiltered (e.g. LFS pointer) files.
/// Returns the `-c` arguments to pass to `git checkout`.
fn filter_lockdown(dest: &Path) -> Result<Vec<String>> {
    if !crate::policy::restricts() {
        return Ok(Vec::new());
    }
    let out = git_command(&[
        "config",
        "-z",
        "--name-only",
        "--get-regexp",
        r"^filter\..*\.(smudge|process)$",
    ])
    .current_dir(dest)
    .output()
    .context("failed to spawn `git config`")?;
    // Exit 1 means no key matched: no filter driver can run.
    if !out.status.success() && out.status.code() != Some(1) {
        bail!(
            "listing checkout filters in {}: {}",
            dest.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let names = String::from_utf8_lossy(&out.stdout).into_owned();
    let drivers: std::collections::BTreeSet<&str> = names
        .split('\0')
        .filter_map(|key| key.strip_prefix("filter.")?.rsplit_once('.'))
        .map(|(driver, _)| driver)
        .collect();
    let mut pin = Vec::new();
    for driver in drivers {
        // `=` would end the `-c` key early and leave the driver untouched.
        if driver.contains('=') {
            bail!(
                "refusing to check out in {}: a source policy is in force, but checkout \
                 filter `{driver}` can't be disabled",
                dest.display()
            );
        }
        for setting in ["smudge=false", "process=false", "required=true"] {
            pin.push("-c".to_string());
            pin.push(format!("filter.{driver}.{setting}"));
        }
    }
    Ok(pin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command as StdCommand;

    /// Build a throwaway git repo with one commit, returning (repo dir, HEAD sha).
    fn make_repo(root: &Path) -> String {
        std::fs::create_dir_all(root).unwrap();
        let run = |args: &[&str]| {
            assert!(StdCommand::new("git")
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("f.txt"), "hi").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "initial"]);
        let out = StdCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "spm-git-test-{name}-{}-{nanos}",
            std::process::id(),
        ))
    }

    /// `ls_remote` against a URL that isn't a git repo at all must surface the
    /// underlying git failure (exercises `git()`'s bail branch), not panic.
    #[test]
    fn ls_remote_surfaces_git_failure_for_bad_url() {
        let dir = scratch("bad-url");
        std::fs::create_dir_all(&dir).unwrap();
        let bogus = format!("file://{}/does-not-exist", dir.display());
        let err = ls_remote(&bogus, &["refs/heads/main"]).unwrap_err();
        assert!(
            format!("{err:#}").contains("git ls-remote") || format!("{err:#}").contains("failed"),
            "{err:#}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `ls_remote` for a ref that doesn't exist in an otherwise valid repo
    /// must report "not found", not silently return an empty sha.
    #[test]
    fn ls_remote_reports_missing_ref() {
        let dir = scratch("missing-ref");
        make_repo(&dir);
        let url = format!("file://{}", dir.display());
        let err = ls_remote(&url, &["refs/heads/does-not-exist"]).unwrap_err();
        assert!(format!("{err}").contains("not found"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `default_branch` follows the remote's `HEAD`, so a repo whose default
    /// branch is not `main` (here `trunk`) resolves to that name.
    #[test]
    fn default_branch_follows_remote_head() {
        let dir = scratch("default-branch");
        make_repo(&dir);
        assert!(StdCommand::new("git")
            .args(["branch", "-m", "trunk"])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());
        let url = format!("file://{}", dir.display());
        assert_eq!(default_branch(&url).unwrap(), "trunk");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A detached `HEAD` has no symbolic ref, so there is no default branch to
    /// report: fail with a clear error, never an empty name.
    #[test]
    fn default_branch_errors_on_detached_head() {
        let dir = scratch("default-branch-detached");
        make_repo(&dir);
        assert!(StdCommand::new("git")
            .args(["checkout", "-q", "--detach"])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());
        let url = format!("file://{}", dir.display());
        let err = default_branch(&url).unwrap_err();
        assert!(
            format!("{err:#}").contains("could not determine the default branch"),
            "{err:#}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An annotated tag's `^{}` peel line must win over the tag-object SHA, in
    /// either order, so a tag compares against the commit `ai.lock` records.
    #[test]
    fn parse_ref_shas_prefers_peeled_commit() {
        let out = "AAAA\trefs/tags/v1\nbbbb\trefs/tags/v1^{}\ncccc\trefs/heads/main\n\
                   eeee\trefs/tags/v2^{}\ndddd\trefs/tags/v2";
        let refs = parse_ref_shas(out);
        assert_eq!(refs.len(), 3);
        assert_eq!(refs["refs/tags/v1"], "bbbb");
        assert_eq!(refs["refs/tags/v2"], "eeee");
        assert_eq!(refs["refs/heads/main"], "cccc");
    }

    #[test]
    fn parse_default_branch_handles_output_shapes() {
        assert_eq!(
            parse_default_branch("ref: refs/heads/trunk\tHEAD\nabc123\tHEAD"),
            Some("trunk".to_string())
        );
        // Branch names may contain slashes.
        assert_eq!(
            parse_default_branch("ref: refs/heads/release/1.x\tHEAD\nabc123\tHEAD"),
            Some("release/1.x".to_string())
        );
        // No symref line (detached HEAD), empty output, or an empty name.
        assert_eq!(parse_default_branch("abc123\tHEAD"), None);
        assert_eq!(parse_default_branch(""), None);
        assert_eq!(parse_default_branch("ref: refs/heads/\tHEAD"), None);
    }

    /// `is_at_commit` is false both when there's no `.git` at all and when the
    /// checkout is at a different commit than requested.
    #[test]
    fn is_at_commit_false_cases() {
        let dir = scratch("is-at-commit");
        std::fs::create_dir_all(&dir).unwrap();
        let zero = CommitSha::try_from("0".repeat(40)).unwrap();
        assert!(!is_at_commit(&dir, &zero), "no .git at all");

        let sha = CommitSha::try_from(make_repo(&dir)).unwrap();
        assert!(is_at_commit(&dir, &sha), "checkout is at HEAD");
        assert!(!is_at_commit(&dir, &zero), "different sha must not match");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// When a shallow fetch-by-sha is refused by the server (unknown ref), the
    /// fallback full `fetch origin` must run — and if the sha still can't be
    /// found afterwards (here: it never existed), the overall error should
    /// come from the checkout step, proving the fallback path executed.
    #[test]
    fn fetch_commit_falls_back_to_full_fetch_then_reports_checkout_failure() {
        let src = scratch("fetch-fallback-src");
        make_repo(&src);
        let url = format!("file://{}", src.display());
        let dest = scratch("fetch-fallback-dest");

        let fake_sha = CommitSha::try_from("a".repeat(40)).unwrap();
        let err = fetch_commit(&url, &fake_sha, &dest).unwrap_err();
        assert!(
            format!("{err:#}").contains("checking out"),
            "expected the failure to surface from the checkout step (proving the \
             depth-1 fetch failed and the full-fetch fallback ran first): {err:#}"
        );

        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&dest).unwrap();
    }

    /// A hostile manifest could set the repository URL (or a ref) to an
    /// option-like string such as `--upload-pack=…`; git would parse it as an
    /// option and run an attacker-chosen command. Both remote entry points must
    /// refuse a value beginning with `-` *before* spawning git — no network or
    /// filesystem side effect may occur.
    #[test]
    fn remote_ops_reject_option_like_values() {
        let evil = "--upload-pack=touch /tmp/spm-pwned";
        let err = ls_remote(evil, &["refs/heads/main"]).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");

        let err = ls_remote("file:///tmp/repo", &["--upload-pack=evil"]).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");

        // Every remote entry point is guarded, not just `ls_remote`/`fetch_commit`.
        let err = remote_ref_names(evil, &["refs/tags/*"]).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");

        let err = remote_ref_names("file:///tmp/repo", &["--upload-pack=evil"]).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");

        let err = remote_refs(evil, &["refs/tags/*"]).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");

        let err = remote_refs("file:///tmp/repo", &["--upload-pack=evil"]).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");

        let err = default_branch(evil).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");

        let dest = scratch("evil-fetch-dest");
        let err =
            fetch_commit(evil, &CommitSha::try_from("a".repeat(40)).unwrap(), &dest).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");
        // The guard runs before any `create_dir_all`, so nothing was written.
        assert!(
            !dest.exists(),
            "fetch_commit must not touch the fs for a rejected URL"
        );
    }
}
