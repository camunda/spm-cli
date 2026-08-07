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
    // `core.longpaths=true` lets git on Windows write paths longer than the
    // legacy 260-char MAX_PATH (deep object/checkout paths under the store).
    // The setting is a no-op on other platforms.
    cmd.args(["-c", "core.longpaths=true"]);
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

    /// `is_at_commit` is false both when there's no `.git` at all and when the
    /// checkout is at a different commit than requested.
    #[test]
    fn is_at_commit_false_cases() {
        let dir = scratch("is-at-commit");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_at_commit(&dir, "deadbeef"), "no .git at all");

        let sha = make_repo(&dir);
        assert!(is_at_commit(&dir, &sha), "checkout is at HEAD");
        assert!(
            !is_at_commit(&dir, "0000000000000000000000000000000000000000"),
            "different sha must not match"
        );
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

        let fake_sha = "a".repeat(40);
        let err = fetch_commit(&url, &fake_sha, &dest).unwrap_err();
        assert!(
            format!("{err:#}").contains("checking out"),
            "expected the failure to surface from the checkout step (proving the \
             depth-1 fetch failed and the full-fetch fallback ran first): {err:#}"
        );

        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&dest).unwrap();
    }
}
