//! Remote URL equivalence: which SSH and HTTPS spellings name the same
//! repository. The one rule behind the `--protocol` rewrite (`git`) and the
//! protocol-independent repository identity that source-policy matching
//! (`policy`) and transitive deduplication key on, so they never drift.

use std::fmt;

/// The wire protocol a remote URL is spoken over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Protocol {
    Ssh,
    Https,
}

impl Protocol {
    pub fn other(self) -> Self {
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
pub fn protocol_of(url: &str) -> Option<Protocol> {
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
pub fn rewrite(url: &str, target: Protocol) -> Option<String> {
    let parts = split_remote(url)?;
    Some(match (parts.protocol == target, target) {
        (true, _) => url.to_string(),
        (false, Protocol::Https) => format!("https://{}/{}", parts.host, parts.path),
        (false, Protocol::Ssh) => format!("git@{}:{}", parts.host, parts.path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
