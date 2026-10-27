//! How spm contacts a remote: the `--protocol` rewrite between equivalent
//! SSH/HTTPS spellings (see [`crate::remote`]) and the `--protocol-fallback`
//! retry after a connectivity or auth failure.

use super::command::GitFailure;
use crate::remote::{protocol_of, rewrite, Protocol};
use anyhow::{bail, Result};
use std::sync::OnceLock;

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

pub(super) fn transport() -> Transport {
    TRANSPORT.get().copied().unwrap_or_default()
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
pub(super) fn is_connectivity_failure(err: &anyhow::Error) -> bool {
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
///
/// Every URL actually handed to git — the original and any protocol rewrite —
/// first passes the source policy ([`crate::policy::gate`]), so no remote
/// operation can reach a denied source, whatever the caller.
pub(super) fn with_transport<T>(
    transport: Transport,
    url: &str,
    mut op: impl FnMut(&str) -> Result<T>,
) -> Result<T> {
    let mut op = |u: &str| {
        crate::policy::gate(u)?;
        op(u)
    };
    crate::policy::gate(url)?;
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

/// Whether contacting `url` under the process transport goes over SSH
/// (`git@host:path`, `ssh://…`, or a URL `--protocol ssh` rewrites).
pub fn contacts_over_ssh(url: &str) -> bool {
    let url = transport()
        .force
        .and_then(|p| rewrite(url, p))
        .unwrap_or_else(|| url.to_string());
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("ssh://") || lower.starts_with("git+ssh://") {
        return true;
    }
    // scp-like `[user@]host:path` — no scheme, and a `:` before any `/`
    // (a single letter before it is a Windows drive, not a host).
    !url.contains("://")
        && url
            .split_once(':')
            .is_some_and(|(host, _)| host.len() > 1 && !host.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    const HTTPS: &str = "https://github.com/org/repo.git";

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
}
