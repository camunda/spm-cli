//! Spawning `git`: the hardened base command (C locale, no prompts, no
//! redirects under a source policy), failure capture, and the option-injection
//! guard every remote entry point applies to its arguments.

use anyhow::{bail, Context, Result};
use std::fmt;
use std::path::Path;
use std::process::Command;

/// A failed `git` invocation. Keeps stderr so callers can tell a connection or
/// authentication failure (worth retrying over another protocol) from a genuine
/// "not found" error. `remote` records whether the failing command actually
/// contacted the remote (`ls-remote`, `fetch`): only a *remote-stage* failure
/// is eligible for connectivity classification, so a local `init`,
/// `remote set-url`, or `checkout` error whose stderr happens to contain a
/// connectivity marker (e.g. "no such file or directory") is never mistaken for
/// a remote reachability failure and retried over the other protocol.
#[derive(Debug)]
pub(super) struct GitFailure {
    pub(super) args: String,
    pub(super) stderr: String,
    pub(super) remote: bool,
}

impl fmt::Display for GitFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "git {} failed: {}", self.args, self.stderr)
    }
}

impl std::error::Error for GitFailure {}

/// Run `git` with the given args, returning trimmed stdout. Errors on non-zero exit.
///
/// SSH remotes (`git@host:org/repo.git`, `ssh://…`) work transparently through
/// the user's ssh-agent/keys. `GIT_TERMINAL_PROMPT=0` stops git from blocking on
/// an interactive username/password prompt for private repos — credential
/// helpers and ssh-agent still supply auth non-interactively; only the hanging
/// TTY fallback is disabled, so auth failures surface as errors instead of hangs.
pub(super) fn git_command(args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    // `core.longpaths=true` lets git on Windows write paths longer than the
    // legacy 260-char MAX_PATH (deep object/checkout paths under the store).
    // The setting is a no-op on other platforms.
    cmd.args(["-c", "core.longpaths=true"]);
    // Under a source policy, git must not follow HTTP redirects: the policy
    // judged the URL spm handed over, and a redirect (a transferred repo, a
    // hostile server) would land on a host or namespace it never checked.
    if crate::policy::restricts() {
        cmd.args(["-c", "http.followRedirects=false"]);
    }
    cmd.args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    // Pin git (and the ssh/curl helpers it spawns) to the C locale so failure
    // diagnostics are stable English text. The connectivity/auth classifier
    // (`CONNECTIVITY_MARKERS`) matches those messages verbatim; without this a
    // localized host would emit translated stderr, the markers would miss, and
    // `--protocol-fallback` would give up instead of retrying over the other
    // protocol. `LC_ALL` overrides every other locale category and `LANGUAGE`;
    // clearing `LANGUAGE` defends against gettext ignoring `LC_ALL` when it is
    // set.
    cmd.env("LC_ALL", "C");
    cmd.env_remove("LANGUAGE");
    cmd
}

pub(super) fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    run_git(&[], args, cwd, false)
}

/// Like [`git`], but tags any failure as a *remote-stage* failure so the
/// transport fallback classifier may consider it (see
/// [`is_connectivity_failure`](super::transport::is_connectivity_failure)).
/// Use only for commands that actually contact the remote — `ls-remote` and
/// `fetch` — never for local store/config steps.
/// `url` is the remote being contacted (for `fetch`, what `origin` points at).
pub(super) fn git_remote(url: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let pin = redirect_pin(url, cwd)?;
    let pin: Vec<&str> = pin.iter().map(String::as_str).collect();
    run_git(&pin, args, cwd, true)
}

/// Under a source policy, make sure git won't follow HTTP redirects (or
/// dumb-HTTP `http-alternates`, which git gates on the same setting) for `url`.
///
/// The generic `-c http.followRedirects=false` from [`git_command`] loses to a
/// URL-scoped `http.<url>.followRedirects` in the user's git config, because
/// git applies the most specific URL match. So pin the key for this exact URL
/// (no config entry can match it more specifically, and command-line config
/// wins ties), then ask git for the value it will actually use and refuse
/// unless it is `false` — that also covers a URL `-c` can't carry (one with
/// `=`, which ends a `-c` key). Returns the extra `-c` arguments to pass.
fn redirect_pin(url: &str, cwd: Option<&Path>) -> Result<Vec<String>> {
    let http = url.split_once("://").is_some_and(|(scheme, _)| {
        matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
    });
    if !http || !crate::policy::restricts() {
        return Ok(Vec::new());
    }
    let mut pin = Vec::new();
    if !url.contains('=') {
        pin.push("-c".to_string());
        pin.push(format!("http.{url}.followRedirects=false"));
    }
    let mut args: Vec<&str> = pin.iter().map(String::as_str).collect();
    args.extend([
        "config",
        "--bool",
        "--get-urlmatch",
        "http.followRedirects",
        url,
    ]);
    let mut cmd = git_command(&args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().context("failed to spawn `git config`")?;
    if !out.status.success() || String::from_utf8_lossy(&out.stdout).trim() != "false" {
        bail!(
            "refusing to contact {url}: a source policy is in force, but git config enables \
             `http.followRedirects` for this URL and spm can't override it; remove that setting \
             (see `git config --get-urlmatch http.followRedirects {url}`)"
        );
    }
    Ok(pin)
}

pub(super) fn run_git(
    pre: &[&str],
    args: &[&str],
    cwd: Option<&Path>,
    remote: bool,
) -> Result<String> {
    let full: Vec<&str> = pre.iter().chain(args).copied().collect();
    let mut cmd = git_command(&full);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn `git {}`", args.join(" ")))?;
    if !out.status.success() {
        return Err(GitFailure {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            remote,
        }
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Reject a value that git could misinterpret as a command-line option.
///
/// Repository URLs and refspecs come straight from (possibly hostile) manifests
/// and are passed to `git` as positional arguments. git parses with getopt, which
/// treats **any** argument beginning with `-` as an option no matter where it sits
/// on the line — so a value like `--upload-pack=touch /tmp/pwned` would run an
/// attacker-chosen command instead of being read as a repository or ref. No
/// legitimate URL or git ref name begins with `-`, so refusing that shape at
/// every remote entry point (`ls_remote`, `remote_ref_names`, `default_branch`,
/// `fetch_commit`) closes option injection for every caller (root,
/// nested/transitive, and plugin specs alike), independent of the git version.
pub(super) fn reject_option_like(kind: &str, value: &str) -> Result<()> {
    if value.starts_with('-') {
        bail!(
            "refusing to pass option-like {kind} `{value}` to git: values beginning with `-` \
             could be misinterpreted as git options"
        );
    }
    Ok(())
}

/// The installed git's version line (e.g. `git version 2.45.1`), or why git
/// could not be run.
pub fn version() -> Result<String> {
    git(&["--version"], None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The connectivity/auth classifier matches English stderr verbatim, so git
    /// (and the ssh/curl helpers it spawns) must run under a fixed message
    /// locale — otherwise a localized host emits translated diagnostics, the
    /// markers miss, and `--protocol-fallback` never retries.
    #[test]
    fn git_command_pins_the_c_locale_for_stable_diagnostics() {
        let cmd = git_command(&["ls-remote"]);
        let lc_all = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("LC_ALL"))
            .and_then(|(_, v)| v);
        assert_eq!(lc_all, Some(std::ffi::OsStr::new("C")));
        // LANGUAGE must be cleared so gettext cannot override LC_ALL=C.
        let language = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("LANGUAGE"));
        assert_eq!(language, Some((std::ffi::OsStr::new("LANGUAGE"), None)));
    }
}
