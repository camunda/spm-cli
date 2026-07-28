use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// Run `git` with the given args, returning trimmed stdout. Errors on non-zero exit.
///
/// SSH remotes (`git@host:org/repo.git`, `ssh://…`) work transparently through
/// the user's ssh-agent/keys. `GIT_TERMINAL_PROMPT=0` stops git from blocking on
/// an interactive username/password prompt for private repos — credential
/// helpers and ssh-agent still supply auth non-interactively; only the hanging
/// TTY fallback is disabled, so auth failures surface as errors instead of hangs.
fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Resolve remote refs to a commit SHA without cloning. Pass multiple refspecs
/// (e.g. a tag and its `^{}` peel) — the peeled/dereferenced commit wins, so
/// annotated tags resolve to the underlying commit rather than the tag object.
pub fn ls_remote(url: &str, refspecs: &[&str]) -> Result<String> {
    let mut args = vec!["ls-remote", url];
    args.extend_from_slice(refspecs);
    let out = git(&args, None)?;
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

/// True if `dir` is a git checkout already sitting at `sha`.
pub fn is_at_commit(dir: &Path, sha: &str) -> bool {
    if !dir.join(".git").exists() {
        return false;
    }
    matches!(git(&["rev-parse", "HEAD"], Some(dir)), Ok(head) if head == sha)
}

/// Fetch just `sha` from `url` into a fresh checkout at `dest`.
///
/// Tries a shallow single-commit fetch first (cheapest); if the server refuses
/// fetch-by-SHA (not all enable `uploadpack.allowAnySHA1InWant`), falls back to
/// a full fetch. `dest` is created if missing.
pub fn fetch_commit(url: &str, sha: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    git(&["init", "-q"], Some(dest))?;
    // Remote may already exist if a prior attempt was interrupted.
    let _ = git(&["remote", "add", "origin", url], Some(dest));

    if git(&["fetch", "--depth", "1", "origin", sha], Some(dest)).is_err() {
        git(&["fetch", "origin"], Some(dest)).with_context(|| format!("fetching {url}"))?;
    }
    git(&["checkout", "--detach", sha], Some(dest))
        .with_context(|| format!("checking out {sha} in {}", dest.display()))?;
    Ok(())
}
