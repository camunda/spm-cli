use anyhow::{bail, Context, Result};
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// The wire protocol a remote URL is spoken over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Protocol {
    Ssh,
    Https,
}

impl Protocol {
    fn other(self) -> Self {
        match self {
            Protocol::Ssh => Protocol::Https,
            Protocol::Https => Protocol::Ssh,
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Protocol::Ssh => "ssh",
            Protocol::Https => "https",
        })
    }
}

/// How spm contacts a remote. The default is "use the URL exactly as given, no
/// retry"; both knobs are opt-in (`--protocol`, `--protocol-fallback`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transport {
    /// Rewrite the URL to this protocol before contacting the remote.
    pub force: Option<Protocol>,
    /// On a connection/auth failure, retry once over the other protocol.
    pub fallback: bool,
}

static TRANSPORT: OnceLock<Transport> = OnceLock::new();

/// Set the process-wide transport policy (once, from the CLI flags). Only the
/// first call takes effect; without one, [`Transport::default`] applies.
pub fn set_transport(transport: Transport) {
    let _ = TRANSPORT.set(transport);
}

fn transport() -> Transport {
    TRANSPORT.get().copied().unwrap_or_default()
}

/// A failed `git` invocation. Keeps stderr so callers can tell a connection or
/// authentication failure (worth retrying over another protocol) from a genuine
/// "not found" error. `remote` records whether the failing command actually
/// contacted the remote (`ls-remote`, `fetch`): only a *remote-stage* failure
/// is eligible for connectivity classification, so a local `init`,
/// `remote set-url`, or `checkout` error whose stderr happens to contain a
/// connectivity marker (e.g. "no such file or directory") is never mistaken for
/// a remote reachability failure and retried over the other protocol.
#[derive(Debug)]
struct GitFailure {
    args: String,
    stderr: String,
    remote: bool,
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
fn git_command(args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    // `core.longpaths=true` lets git on Windows write paths longer than the
    // legacy 260-char MAX_PATH (deep object/checkout paths under the store).
    // The setting is a no-op on other platforms.
    cmd.args(["-c", "core.longpaths=true"]);
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

fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    run_git(args, cwd, false)
}

/// Like [`git`], but tags any failure as a *remote-stage* failure so the
/// transport fallback classifier may consider it (see
/// [`is_connectivity_failure`]). Use only for commands that actually contact
/// the remote — `ls-remote` and `fetch` — never for local store/config steps.
fn git_remote(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    run_git(args, cwd, true)
}

fn run_git(args: &[&str], cwd: Option<&Path>, remote: bool) -> Result<String> {
    let mut cmd = git_command(args);
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

/// The pieces of a remote URL that both protocols share: `<host>` and `<path>`.
struct RemoteParts<'a> {
    protocol: Protocol,
    host: &'a str,
    path: &'a str,
}

/// Split a URL into protocol/host/path, but only when it has an equivalent form
/// in the other protocol: `https://host/path`, `ssh://git@host/path` and
/// scp-style `git@host:path`. Anything that does not map cleanly (a port,
/// userinfo, a non-`git` SSH user, an absolute scp path, `file://`, a local
/// path) yields `None` so it is never rewritten. So does any URL whose path
/// would change meaning between the two forms: a `#fragment` or `?query` (URL
/// metadata over HTTPS, literal path characters over scp), percent-encoding
/// (decoded over HTTPS, literal over scp), a backslash, an SSH home-relative
/// `~` path, or a `..` segment (HTTPS normalizes dot segments while scp-style
/// SSH resolves them relative to the account's home).
fn split_remote(url: &str) -> Option<RemoteParts<'_>> {
    if url.contains(['#', '?', '%', '\\']) {
        return None;
    }
    // URL schemes are case-insensitive (`SSH://…`, `HTTPS://…` name the same
    // transport), so match them case-insensitively — otherwise an uppercase
    // spelling would fall through, dodging the protocol equivalence and the
    // identity-based deduplication built on it. The authority/path are sliced
    // from the original (case preserved); only hosts are lowercased later.
    let strip_scheme_ci = |prefix: &str| {
        url.get(..prefix.len())
            .filter(|s| s.eq_ignore_ascii_case(prefix))
            .map(|s| &url[s.len()..])
    };
    let (protocol, host, path) = if let Some(rest) = strip_scheme_ci("https://") {
        let (host, path) = rest.split_once('/')?;
        (Protocol::Https, host, path)
    } else if let Some(rest) = strip_scheme_ci("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        (Protocol::Ssh, authority.strip_prefix("git@")?, path)
    } else if !url.contains("://") {
        let (authority, path) = url.split_once(':')?;
        (Protocol::Ssh, authority.strip_prefix("git@")?, path)
    } else {
        return None;
    };
    let plain_host = !host.is_empty() && !host.contains(['@', ':', '/', '\\']);
    let dotdot = path.split('/').any(|seg| seg == "..");
    let plain_path = !path.is_empty() && !path.starts_with(['/', '~']) && !dotdot;
    (plain_host && plain_path).then_some(RemoteParts {
        protocol,
        host,
        path,
    })
}

/// The protocol `url` is spoken over, if it is one [`rewrite`] can switch.
fn protocol_of(url: &str) -> Option<Protocol> {
    split_remote(url).map(|p| p.protocol)
}

/// A canonical, protocol-independent identity for a remote URL that has an
/// equivalent form in both protocols (see [`split_remote`]): the HTTPS spelling
/// with a lowercased host, path case preserved. `https://host/path`,
/// `ssh://git@host/path` and scp-style `git@host:path` therefore collapse to one
/// string, so callers that key on repository identity (e.g. transitive
/// deduplication and version-conflict detection) treat the interchangeable
/// spellings as the same repo. Returns `None` for any source with no
/// cross-protocol equivalent (a port, userinfo, a non-`git` SSH user, `file://`,
/// a local path, …), which the caller must keep distinct. Built from the same
/// equivalence `--protocol` rewriting relies on, so the two never drift.
pub fn remote_identity(url: &str) -> Option<String> {
    let parts = split_remote(url)?;
    Some(format!(
        "https://{}/{}",
        parts.host.to_ascii_lowercase(),
        parts.path
    ))
}

/// Express `url` in `target`. Returns the URL unchanged when it already uses
/// `target`, and `None` when it has no equivalent form (see [`split_remote`]).
fn rewrite(url: &str, target: Protocol) -> Option<String> {
    let parts = split_remote(url)?;
    Some(match (parts.protocol == target, target) {
        (true, _) => url.to_string(),
        (false, Protocol::Https) => format!("https://{}/{}", parts.host, parts.path),
        (false, Protocol::Ssh) => format!("git@{}:{}", parts.host, parts.path),
    })
}

/// Stderr fragments (lowercase) that mean git could not reach or authenticate to
/// the remote. Deliberately excludes ref/path errors (`couldn't find remote
/// ref`, `not our ref`, an empty ls-remote): those are the same over any
/// protocol, so retrying would only obscure the real problem.
const CONNECTIVITY_MARKERS: &[&str] = &[
    "permission denied",
    "authentication failed",
    "could not read username",
    "could not read password",
    "invalid username or password",
    "http basic: access denied",
    "the requested url returned error: 401",
    "the requested url returned error: 403",
    "the requested url returned error: 407",
    "http code 407",
    "terminal prompts disabled",
    "host key verification failed",
    "could not resolve host",
    "connection refused",
    "connection timed out",
    "connection reset",
    "connection closed",
    "network is unreachable",
    "no route to host",
    "operation timed out",
    "failed to connect",
    "could not read from remote repository",
    // A missing `ssh`/`GIT_SSH_COMMAND` helper: git cannot spawn the transport,
    // so the SSH protocol is not set up and HTTPS should be tried.
    "unable to fork",
    "cannot run ",
    "no such file or directory",
];

/// True if `err` is a `git` failure caused by connectivity or authentication.
///
/// Restricted to *remote-stage* failures (`ls-remote`, `fetch`): a local store
/// or configuration step (`init`, `remote set-url`, `checkout`) is never
/// retried over another protocol, even if its stderr contains a connectivity
/// marker, so a local error is never misreported as a remote reachability
/// failure.
fn is_connectivity_failure(err: &anyhow::Error) -> bool {
    err.downcast_ref::<GitFailure>().is_some_and(|f| {
        if !f.remote {
            return false;
        }
        let stderr = f.stderr.to_lowercase();
        CONNECTIVITY_MARKERS.iter().any(|m| stderr.contains(m))
    })
}

/// Run `op` against `url` under `transport`.
///
/// With the default transport this is exactly `op(url)`. `force` rewrites the URL
/// first; `fallback` retries once over the other protocol after a connectivity
/// or auth failure — never after a ref/path error — and always says so on
/// stderr. Only the URL handed to git changes: callers keep recording the URL
/// the user supplied.
fn with_transport<T>(
    transport: Transport,
    url: &str,
    mut op: impl FnMut(&str) -> Result<T>,
) -> Result<T> {
    let first = transport
        .force
        .and_then(|p| rewrite(url, p))
        .unwrap_or_else(|| url.to_string());
    let first_err = match op(&first) {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    if !transport.fallback || !is_connectivity_failure(&first_err) {
        return Err(first_err);
    }
    let Some(from) = protocol_of(&first) else {
        return Err(first_err);
    };
    let to = from.other();
    let Some(second) = rewrite(&first, to) else {
        return Err(first_err);
    };
    match op(&second) {
        Ok(v) => {
            eprintln!(
                "note: could not reach {first} over {from}; used {to} ({second}) instead. \
                 Falling back can mask a credential problem."
            );
            Ok(v)
        }
        Err(second_err) => bail!(
            "could not reach {url} over either protocol:\n  {from} ({first}): {first_err:#}\n  {to} ({second}): {second_err:#}"
        ),
    }
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
fn reject_option_like(kind: &str, value: &str) -> Result<()> {
    if value.starts_with('-') {
        bail!(
            "refusing to pass option-like {kind} `{value}` to git: values beginning with `-` \
             could be misinterpreted as git options"
        );
    }
    Ok(())
}

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
    let out = git_remote(&args, None)?;
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
    reject_option_like("repository URL", url)?;
    for refspec in refspecs {
        reject_option_like("ref", refspec)?;
    }
    with_transport(transport(), url, |u| remote_ref_names_once(u, refspecs))
}

fn remote_ref_names_once(url: &str, refspecs: &[&str]) -> Result<Vec<String>> {
    let mut args = vec!["ls-remote", url];
    args.extend_from_slice(refspecs);
    let out = git_remote(&args, None)?;
    Ok(out
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(_, name)| name.trim_end_matches("^{}").to_string())
        .collect())
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
    git_remote(&["ls-remote", "--symref", url, "HEAD"], None)
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
    reject_option_like("repository URL", url)?;
    with_transport(transport(), url, |u| fetch_commit_once(u, sha, dest))
}

fn fetch_commit_once(url: &str, sha: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    git(&["init", "-q"], Some(dest))?;
    // The remote already exists if a prior attempt (interrupted, or over another
    // protocol) got this far; point it at the URL now being tried.
    if git(&["remote", "add", "origin", url], Some(dest)).is_err() {
        git(&["remote", "set-url", "origin", url], Some(dest))?;
    }

    if git_remote(&["fetch", "--depth", "1", "origin", sha], Some(dest)).is_err() {
        git_remote(&["fetch", "origin"], Some(dest)).with_context(|| format!("fetching {url}"))?;
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

    fn failure(stderr: &str) -> anyhow::Error {
        GitFailure {
            args: "ls-remote".into(),
            stderr: stderr.into(),
            remote: true,
        }
        .into()
    }

    /// A failure from a *local* store/config step (`init`, `remote set-url`,
    /// `checkout`) — never eligible for connectivity classification.
    fn local_failure(stderr: &str) -> anyhow::Error {
        GitFailure {
            args: "checkout".into(),
            stderr: stderr.into(),
            remote: false,
        }
        .into()
    }

    const SSH: &str = "git@github.com:org/repo.git";
    const SSH_URL: &str = "ssh://git@github.com/org/repo.git";
    const HTTPS: &str = "https://github.com/org/repo.git";

    /// GitHub-style URLs map both ways; everything without an equivalent form is
    /// left alone (`None`) rather than guessed at.
    #[test]
    fn rewrite_maps_equivalent_forms_and_refuses_the_rest() {
        assert_eq!(rewrite(SSH, Protocol::Https).as_deref(), Some(HTTPS));
        assert_eq!(rewrite(SSH_URL, Protocol::Https).as_deref(), Some(HTTPS));
        assert_eq!(rewrite(HTTPS, Protocol::Ssh).as_deref(), Some(SSH));
        assert_eq!(
            rewrite("https://gitlab.com/org/repo", Protocol::Ssh).as_deref(),
            Some("git@gitlab.com:org/repo")
        );
        // Already in the requested protocol: unchanged, byte for byte.
        assert_eq!(rewrite(SSH, Protocol::Ssh).as_deref(), Some(SSH));
        assert_eq!(rewrite(SSH_URL, Protocol::Ssh).as_deref(), Some(SSH_URL));
        assert_eq!(rewrite(HTTPS, Protocol::Https).as_deref(), Some(HTTPS));

        for unmappable in [
            "file:///tmp/repo",
            "/tmp/repo",
            r"C:\tmp\repo",
            "skills/foo",
            // a port or userinfo has no counterpart in the other protocol
            "ssh://git@git.example.com:7999/p/repo.git",
            "https://user@github.com/org/repo.git",
            "https://github.com:8443/org/repo.git",
            // fragment, query, percent-encoding, backslash and `~` paths change
            // meaning between HTTPS and scp-style SSH
            "https://github.com/org/repo#x",
            "https://github.com/org/repo.git?ref=x",
            "https://github.com/org/my%20repo.git",
            r"https://github.com/org\repo.git",
            "git@github.com:org/repo#x",
            "git@github.com:org/repo.git?x=1",
            "git@github.com:~alice/repo.git",
            "ssh://git@github.com/~alice/repo.git",
            // `..` path segments normalize over HTTPS but resolve home-relative
            // over scp-style SSH, so they change meaning between the two forms
            "https://github.com/org/../repo.git",
            "git@github.com:org/../repo.git",
            "ssh://git@github.com/../repo.git",
            // non-`git` SSH user, absolute scp path, empty path/host
            "alice@github.com:org/repo.git",
            "git@host:/abs/repo.git",
            "git@github.com:",
            "https://github.com",
            "https:///org/repo",
        ] {
            assert_eq!(rewrite(unmappable, Protocol::Https), None, "{unmappable}");
            assert_eq!(rewrite(unmappable, Protocol::Ssh), None, "{unmappable}");
        }
    }

    #[test]
    fn protocol_of_identifies_the_scheme() {
        assert_eq!(protocol_of(SSH), Some(Protocol::Ssh));
        assert_eq!(protocol_of(SSH_URL), Some(Protocol::Ssh));
        assert_eq!(protocol_of(HTTPS), Some(Protocol::Https));
        assert_eq!(protocol_of("file:///tmp/repo"), None);
    }

    /// URL schemes are case-insensitive, so an uppercase or mixed-case spelling
    /// names the same remote and must yield the same protocol-independent
    /// identity — otherwise it dodges the HTTPS/SSH equivalence the transitive
    /// deduplication keys on.
    #[test]
    fn remote_identity_folds_case_insensitive_schemes() {
        let canonical = "https://github.com/org/repo";
        for spelling in [
            "https://github.com/org/repo",
            "HTTPS://GitHub.com/org/repo",
            "ssh://git@github.com/org/repo",
            "SSH://git@GitHub.com/org/repo",
            "Ssh://git@github.com/org/repo",
            "git@github.com:org/repo",
        ] {
            assert_eq!(
                remote_identity(spelling).as_deref(),
                Some(canonical),
                "{spelling}"
            );
        }
        // Still `None` for forms with no cross-protocol equivalent.
        assert_eq!(remote_identity("file:///tmp/repo"), None);
        assert_eq!(remote_identity("/tmp/repo"), None);
    }

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

    /// Only connection/auth failures are retryable; ref and path errors and
    /// non-git errors never are.
    #[test]
    fn only_connectivity_failures_are_retryable() {
        for stderr in [
            "git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.",
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
            "fatal: unable to access 'https://x/': Could not resolve host: x",
            "ssh: connect to host x port 22: Connection refused",
            "fatal: Authentication failed for 'https://x/'",
            "Host key verification failed.",
            "fatal: unable to access 'https://x/': The requested URL returned error: 401",
            "fatal: unable to access 'https://x/': The requested URL returned error: 403",
            "fatal: unable to access 'https://x/': The requested URL returned error: 407",
            "fatal: unable to access 'https://x/': Received HTTP code 407 from proxy after CONNECT",
            "remote: HTTP Basic: Access denied\nfatal: Authentication failed for 'https://x/'",
            "remote: Invalid username or password.",
            // A missing ssh/GIT_SSH_COMMAND helper: git cannot spawn the transport.
            "fatal: unable to fork",
            "error: cannot run ssh: No such file or directory\nfatal: unable to fork",
        ] {
            assert!(is_connectivity_failure(&failure(stderr)), "{stderr}");
        }
        for stderr in [
            "fatal: couldn't find remote ref refs/heads/nope",
            "fatal: remote error: upload-pack: not our ref abc",
            "fatal: reference is not a tree: abc",
            // a 404 is a missing repo/path, not an auth problem
            "fatal: unable to access 'https://x/': The requested URL returned error: 404",
        ] {
            assert!(!is_connectivity_failure(&failure(stderr)), "{stderr}");
        }
        assert!(!is_connectivity_failure(&anyhow::anyhow!(
            "ref `x` not found in y"
        )));
    }

    /// Connectivity classification is gated on the *stage*, not just the text:
    /// a local store/config step (`init`, `remote set-url`, `checkout`) that
    /// fails with stderr containing a connectivity marker is NOT a connectivity
    /// failure, so it is never retried over another protocol and never
    /// misreported as a remote reachability failure. The identical message from
    /// the remote operation still is.
    #[test]
    fn local_stage_failures_are_never_connectivity_failures() {
        for stderr in [
            "fatal: could not create work tree dir: Permission denied",
            "error: could not lock config file .git/config: No such file or directory",
            "fatal: unable to fork",
        ] {
            assert!(
                !is_connectivity_failure(&local_failure(stderr)),
                "local stage must not classify: {stderr}"
            );
            assert!(
                is_connectivity_failure(&failure(stderr)),
                "same text from the remote op must classify: {stderr}"
            );
        }
    }

    /// Run `with_transport`, recording each URL tried and answering from `script`
    /// (the error to return for that attempt, or `None` for success).
    fn run_transport(
        transport: Transport,
        url: &str,
        script: &[Option<&str>],
    ) -> (Result<String>, Vec<String>) {
        let mut tried = Vec::new();
        let result = with_transport(transport, url, |u| {
            let attempt = tried.len();
            tried.push(u.to_string());
            match script[attempt] {
                None => Ok(u.to_string()),
                Some(stderr) => Err(failure(stderr)),
            }
        });
        (result, tried)
    }

    const DENIED: &str = "Permission denied (publickey).";
    const NO_REF: &str = "fatal: couldn't find remote ref refs/heads/nope";

    /// Default transport: the URL as given, exactly one attempt, even when it
    /// fails in a retryable way.
    #[test]
    fn default_transport_uses_the_url_as_given_and_never_retries() {
        let (ok, tried) = run_transport(Transport::default(), SSH, &[None]);
        assert_eq!(ok.unwrap(), SSH);
        assert_eq!(tried, [SSH]);

        let (err, tried) = run_transport(Transport::default(), SSH, &[Some(DENIED), None]);
        assert!(format!("{:#}", err.unwrap_err()).contains(DENIED));
        assert_eq!(tried, [SSH], "no retry without --protocol-fallback");
    }

    /// `--protocol` rewrites before the first contact; an unmappable URL is used
    /// as given.
    #[test]
    fn forced_protocol_rewrites_before_contacting_the_remote() {
        let force = |p| Transport {
            force: Some(p),
            fallback: false,
        };
        let (ok, tried) = run_transport(force(Protocol::Https), SSH, &[None]);
        assert_eq!(ok.unwrap(), HTTPS);
        assert_eq!(tried, [HTTPS]);

        let (_, tried) = run_transport(force(Protocol::Ssh), HTTPS, &[None]);
        assert_eq!(tried, [SSH]);

        let (_, tried) = run_transport(force(Protocol::Ssh), "file:///tmp/r", &[None]);
        assert_eq!(tried, ["file:///tmp/r"]);

        // Forcing is not a fallback: a failure is reported, not retried.
        let (err, tried) = run_transport(force(Protocol::Https), SSH, &[Some(DENIED), None]);
        assert!(err.is_err());
        assert_eq!(tried, [HTTPS]);
    }

    const FALLBACK: Transport = Transport {
        force: None,
        fallback: true,
    };

    #[test]
    fn fallback_retries_the_other_protocol_after_a_connectivity_failure() {
        let (ok, tried) = run_transport(FALLBACK, SSH, &[Some(DENIED), None]);
        assert_eq!(ok.unwrap(), HTTPS);
        assert_eq!(tried, [SSH, HTTPS]);

        let (ok, tried) = run_transport(FALLBACK, HTTPS, &[Some(DENIED), None]);
        assert_eq!(ok.unwrap(), SSH);
        assert_eq!(tried, [HTTPS, SSH]);

        // Success on the first protocol never touches the second.
        let (_, tried) = run_transport(FALLBACK, SSH, &[None]);
        assert_eq!(tried, [SSH]);
    }

    /// The reverse direction the advisory flagged: an unavailable SSH transport
    /// (a missing `ssh` helper) must fall back to HTTPS, not strand the user on
    /// the SSH error. The end-to-end fallback above only exercises HTTPS → SSH.
    #[test]
    fn fallback_from_a_missing_ssh_helper_tries_https() {
        const NO_SSH: &str =
            "error: cannot run ssh: No such file or directory\nfatal: unable to fork";
        let (ok, tried) = run_transport(FALLBACK, SSH, &[Some(NO_SSH), None]);
        assert_eq!(ok.unwrap(), HTTPS);
        assert_eq!(tried, [SSH, HTTPS]);
    }

    /// With `--protocol` and the fallback together, the forced protocol is tried
    /// first and the *other* one is the fallback.
    #[test]
    fn fallback_after_a_forced_protocol_tries_the_opposite_one() {
        let t = Transport {
            force: Some(Protocol::Https),
            fallback: true,
        };
        let (ok, tried) = run_transport(t, SSH, &[Some(DENIED), None]);
        assert_eq!(ok.unwrap(), SSH);
        assert_eq!(tried, [HTTPS, SSH]);
    }

    /// A missing ref (or any non-connectivity error) must surface as-is and
    /// never trigger a protocol retry.
    #[test]
    fn fallback_never_retries_a_ref_error() {
        let (err, tried) = run_transport(FALLBACK, SSH, &[Some(NO_REF), None]);
        let msg = format!("{:#}", err.unwrap_err());
        assert!(msg.contains("couldn't find remote ref"), "{msg}");
        assert!(!msg.contains("either protocol"), "{msg}");
        assert_eq!(tried, [SSH]);

        let mut tried = 0;
        let err = with_transport(FALLBACK, SSH, |_| -> Result<()> {
            tried += 1;
            bail!("ref `x` not found in y")
        })
        .unwrap_err();
        assert_eq!(tried, 1);
        assert!(format!("{err}").contains("not found"));
    }

    /// A local-stage failure whose stderr matches a connectivity marker must not
    /// trigger a protocol fallback: only the remote operation is retryable, so a
    /// local `checkout`/`init` error surfaces as-is with a single attempt.
    #[test]
    fn fallback_never_retries_a_local_stage_failure() {
        let mut tried = 0;
        let err = with_transport(FALLBACK, SSH, |_| -> Result<()> {
            tried += 1;
            Err(local_failure(
                "fatal: unable to update the ref: No such file or directory",
            ))
        })
        .unwrap_err();
        assert_eq!(
            tried, 1,
            "a local-stage error is not retried over a protocol"
        );
        let msg = format!("{err:#}");
        assert!(msg.contains("No such file or directory"), "{msg}");
        assert!(!msg.contains("either protocol"), "{msg}");
    }

    /// A URL with no equivalent in the other protocol has nothing to fall back to.
    #[test]
    fn fallback_is_a_noop_for_unmappable_urls() {
        let (err, tried) = run_transport(FALLBACK, "file:///tmp/r", &[Some(DENIED), None]);
        assert!(format!("{:#}", err.unwrap_err()).contains(DENIED));
        assert_eq!(tried, ["file:///tmp/r"]);
    }

    /// When both protocols fail the error names both attempts and their causes.
    #[test]
    fn fallback_failure_reports_both_attempts() {
        let (err, tried) = run_transport(
            FALLBACK,
            SSH,
            &[Some(DENIED), Some("fatal: Authentication failed")],
        );
        let msg = format!("{:#}", err.unwrap_err());
        assert_eq!(tried, [SSH, HTTPS]);
        assert!(msg.contains("either protocol"), "{msg}");
        assert!(msg.contains(&format!("ssh ({SSH})")), "{msg}");
        assert!(msg.contains(DENIED), "{msg}");
        assert!(msg.contains(&format!("https ({HTTPS})")), "{msg}");
        assert!(msg.contains("Authentication failed"), "{msg}");
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

        let err = default_branch(evil).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");

        let dest = scratch("evil-fetch-dest");
        let err = fetch_commit(evil, &"a".repeat(40), &dest).unwrap_err();
        assert!(format!("{err}").contains("option-like"), "{err}");
        // The guard runs before any `create_dir_all`, so nothing was written.
        assert!(
            !dest.exists(),
            "fetch_commit must not touch the fs for a rejected URL"
        );
    }
}
