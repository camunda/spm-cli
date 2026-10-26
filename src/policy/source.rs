//! What a dependency's `git` value points at: the protocol-independent
//! [`Source`] identity patterns are matched against, classified strictly so
//! anything ambiguous fails closed.

use super::pattern::ANY_LOCAL;

/// What a dependency's `git` value points at, as far as the policy is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Source {
    /// A network remote: lowercased host (with its port, when one is spelled)
    /// and the repository path split into segments, `.git` suffix dropped.
    Remote { host: String, segments: Vec<String> },
    /// A `file://` URL or a local filesystem path.
    Local,
    /// Something spm cannot pin to a host and path safely (a transport helper
    /// such as `ext::`, an unknown scheme, a `..` segment, percent-encoding, …).
    /// No pattern ever allows it, so it is denied whenever a policy is active.
    Opaque,
}

impl Source {
    /// Classify a dependency's `git` value.
    ///
    /// Remotes with an HTTPS/SSH equivalent are first folded through
    /// [`crate::remote::remote_identity`] — the same equivalence `--protocol`
    /// rewriting relies on — so the interchangeable spellings of one repository
    /// can never be judged differently. Everything else is parsed strictly and
    /// fails closed to [`Source::Opaque`].
    pub(super) fn classify(git: &str) -> Source {
        // Whitespace around the value is passed to git verbatim (for SSH a
        // trailing space is part of the repository path), so it can't be
        // trimmed for matching: judge exactly what git will see, or nothing.
        let s = git;
        if s.trim() != s {
            return Source::Opaque;
        }
        if let Some(identity) = crate::remote::remote_identity(s) {
            return match identity
                .strip_prefix("https://")
                .and_then(|r| r.split_once('/'))
            {
                Some((host, path)) => remote(host, path),
                None => Source::Opaque,
            };
        }
        if let Some((scheme, rest)) = split_scheme(s) {
            return match scheme.to_ascii_lowercase().as_str() {
                "file" => Source::Local,
                "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git" => {
                    // The authority ends at the first `/`, `?` or `#` (and a `\`
                    // is one on some stacks). Only a `/` gives a host and path
                    // we can judge; otherwise an `@` *after* the real end of the
                    // authority (`https://evil?@allowed/o/r`) would be mistaken
                    // for a userinfo separator.
                    match rest.find(['/', '?', '#', '\\']) {
                        Some(i) if rest.as_bytes()[i] == b'/' => {
                            remote_at(&rest[..i], &rest[i + 1..])
                        }
                        _ => Source::Opaque,
                    }
                }
                _ => Source::Opaque,
            };
        }
        // `<transport>::<address>` runs a git remote helper (e.g. `ext::` runs an
        // arbitrary command); it names no host a pattern could vouch for.
        if s.contains("::") {
            return Source::Opaque;
        }
        // Git's own rule (`url_is_local_not_ssh`): with no colon, or a `/`
        // before the first colon, it is a local path; otherwise it is
        // scp-style `[user@]host:path`. Only `/` counts — a backslash does not,
        // so `domain\user@host:o/r` is SSH on every platform. A drive letter
        // (`C:\…`, `C:/…`, `C:repo`) is a local path only on Windows; elsewhere
        // git reads it as the one-letter host `C`, and so do we.
        match s.find(':') {
            None => local_path(s),
            Some(i) if s[..i].contains('/') => local_path(s),
            Some(1) if cfg!(windows) && s.as_bytes()[0].is_ascii_alphabetic() => Source::Local,
            Some(i) => remote_at(&s[..i], &s[i + 1..]),
        }
    }

    /// A human-readable form of the identity patterns are matched against.
    pub(super) fn describe(&self) -> String {
        match self {
            Source::Remote { host, segments } => format!("{host}/{}", segments.join("/")),
            Source::Local => format!("a local source; only `{ANY_LOCAL}` allows it"),
            Source::Opaque => {
                "not a plain host/path source, so no pattern can allow it".to_string()
            }
        }
    }
}

/// Split `scheme://rest`, accepting only a syntactically valid URL scheme so a
/// local path that merely contains `://` is not mistaken for a URL.
fn split_scheme(s: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = s.split_once("://")?;
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some((scheme, rest))
}

/// A local path git can only read as a path. A *relative* name such as
/// `origin` or `team/skills` is ambiguous: git resolves a configured remote of
/// that name first (`git ls-remote origin` contacts `remote.origin.url`), so it
/// is opaque. Only forms that can't be remote names are local: absolute paths
/// and `./`/`../` relative ones (`.`/`..` components are invalid in remote
/// names), plus, on Windows, `\\`-prefixed (UNC) and `.\`/`..\` paths (a
/// remote name can't hold a backslash).
fn local_path(s: &str) -> Source {
    let unambiguous = s.starts_with('/')
        || s == "."
        || s == ".."
        || s.starts_with("./")
        || s.starts_with("../")
        || (cfg!(windows)
            && (s.starts_with('\\') || s.starts_with(".\\") || s.starts_with("..\\")));
    if unambiguous {
        Source::Local
    } else {
        Source::Opaque
    }
}

/// A remote from its raw authority (`[user[:password]@]host[:port]`) and path.
/// The userinfo is dropped — the policy is about hosts — but only after the
/// whole authority is checked: a `%`-escape or a stray delimiter could make
/// git (which decodes SSH URLs before splitting them) see a different host
/// than the text after the last `@`, as in `ssh://evil%2Fgit@allowed/o/r`.
fn remote_at(authority: &str, path: &str) -> Source {
    let clean = !authority
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '%' | '/' | '\\' | '?' | '#'));
    if !clean {
        return Source::Opaque;
    }
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    remote(host, path)
}

pub(super) fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']'))
}

/// A path segment that means the same thing on every transport. Anything that
/// could be reinterpreted on the way to the server — `.`/`..` (normalized by
/// HTTPS, resolved by SSH), percent-encoding, a query or fragment, a backslash,
/// a home-relative `~` — is refused, so a URL like
/// `https://host/allowed/../evil/repo` cannot ride on an `host/allowed/*` rule.
pub(super) fn valid_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg != "."
        && seg != ".."
        && !seg.starts_with('~')
        && !seg.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '%' | '\\' | '?' | '#' | '*' | ':')
        })
}

fn remote(host: &str, path: &str) -> Source {
    let host = host.to_ascii_lowercase();
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<String> = path.split('/').map(str::to_string).collect();
    if !valid_host(&host) || !segments.iter().all(|s| valid_segment(s)) {
        return Source::Opaque;
    }
    Source::Remote { host, segments }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::testutil::policy;

    fn remote_src(host: &str, path: &str) -> Source {
        Source::Remote {
            host: host.to_string(),
            segments: path.split('/').map(str::to_string).collect(),
        }
    }

    #[test]
    fn https_ssh_url_and_scp_spellings_classify_to_one_identity() {
        let want = remote_src("github.com", "camunda/skills");
        for url in [
            "https://github.com/camunda/skills",
            "https://github.com/camunda/skills.git",
            "https://GitHub.com/camunda/skills/",
            "HTTPS://github.com/camunda/skills",
            "http://github.com/camunda/skills",
            "ssh://git@github.com/camunda/skills.git",
            "SSH://git@GitHub.COM/camunda/skills",
            "git@github.com:camunda/skills.git",
            "git@github.com:camunda/skills",
            "deploy@github.com:camunda/skills",
            "ssh://user:secret@github.com/camunda/skills",
            "git://github.com/camunda/skills",
        ] {
            assert_eq!(Source::classify(url), want, "{url}");
        }
    }

    /// Surrounding whitespace is part of what git is handed (for SSH a trailing
    /// space names a different repository), so it is never trimmed away.
    #[test]
    fn padded_sources_are_opaque() {
        let exact = policy(&["git.example/team/approved", "file://*"]);
        for url in [
            "  https://github.com/camunda/skills  ",
            "git@git.example:team/approved.git ",
            " git@git.example:team/approved.git",
            "https://git.example/team/approved\t",
            "/tmp/repo\n",
        ] {
            assert_eq!(Source::classify(url), Source::Opaque, "{url:?}");
            assert!(exact.check(url).is_err(), "{url:?}");
        }
    }

    #[test]
    fn ports_are_part_of_the_host_identity() {
        assert_eq!(
            Source::classify("https://git.corp:8443/team/repo"),
            remote_src("git.corp:8443", "team/repo")
        );
        assert_eq!(
            Source::classify("ssh://git@git.corp:2222/team/repo"),
            remote_src("git.corp:2222", "team/repo")
        );
    }

    #[test]
    fn local_sources_classify_as_local_on_every_platform() {
        for url in [
            "file:///tmp/repo",
            "FILE:///tmp/repo",
            "file://C:/Users/me/repo",
            "file:///C:/Users/me/repo",
            "/tmp/repo",
            ".",
            "./repo",
            "../repo",
            "//server/share/repo",
            "./dir/with:colon",
        ] {
            assert_eq!(Source::classify(url), Source::Local, "{url}");
        }
    }

    /// A relative name git could resolve as a configured remote (`git
    /// ls-remote origin` contacts `remote.origin.url`) is not a local source.
    #[test]
    fn possible_remote_aliases_are_opaque() {
        for url in [
            "origin",
            "repo",
            "team/skills",
            "dir/with:colon",
            r"dir\repo",
        ] {
            assert_eq!(Source::classify(url), Source::Opaque, "{url}");
        }
        assert!(policy(&[ANY_LOCAL]).check("origin").is_err());
    }

    /// A drive letter is a local path only where git treats it as one.
    #[test]
    #[cfg(windows)]
    fn drive_paths_are_local_on_windows() {
        for url in [
            r"C:\Users\me\repo",
            "C:/Users/me/repo",
            "C:repo",
            r"\\server\share\repo",
            r".\repo",
            r"..\repo",
        ] {
            assert_eq!(Source::classify(url), Source::Local, "{url}");
        }
    }

    /// Off Windows, git reads `x:path` as scp-style SSH to the one-letter host
    /// `x`, so it must be judged as that remote — never as an `file://*` path.
    #[test]
    #[cfg(not(windows))]
    fn drive_like_paths_are_remotes_off_windows() {
        assert_eq!(Source::classify("x:org/repo"), remote_src("x", "org/repo"));
        // An absolute or backslashed scp path can't be judged: fail closed.
        assert_eq!(Source::classify("C:/Users/me/repo"), Source::Opaque);
        assert_eq!(Source::classify(r"C:\Users\me\repo"), Source::Opaque);
        let local_only = policy(&[ANY_LOCAL]);
        assert!(local_only.check("x:org/repo").is_err());
    }

    /// A backslash before the colon doesn't make a path local (git only looks
    /// at `/`): `domain\user@host:o/r` is an SSH source on every platform, so
    /// it is never `file://*` — and with a backslash in its authority, opaque.
    #[test]
    fn backslash_in_scp_userinfo_is_never_local() {
        assert_eq!(
            Source::classify(r"domain\user@evil.test:org/repo"),
            Source::Opaque
        );
        let local_only = policy(&[ANY_LOCAL]);
        assert!(local_only.check(r"domain\user@evil.test:org/repo").is_err());
    }

    /// `?`/`#` end a URL authority, so an `@` after them is not a userinfo
    /// separator: the real host is `evil.test`, never the allowed one.
    #[test]
    fn authority_delimiters_before_userinfo_fail_closed() {
        let p = policy(&["github.com/camunda/*"]);
        for url in [
            "https://evil.test?@github.com/camunda/repo.git",
            "https://evil.test#@github.com/camunda/repo.git",
            r"https://evil.test\@github.com/camunda/repo.git",
            "ssh://git@evil.test?@github.com/camunda/repo.git",
            "git://evil.test#@github.com/camunda/repo",
            // git decodes SSH URLs before splitting host from path.
            "ssh://evil.test%2Fgit@github.com/camunda/repo.git",
            "https://evil.test%40@github.com/camunda/repo.git",
            "evil.test%2F@github.com:camunda/repo.git",
            "ev il@github.com:camunda/repo.git",
        ] {
            assert_eq!(Source::classify(url), Source::Opaque, "{url}");
            assert!(p.check(url).is_err(), "{url}");
        }
        // A genuine userinfo is still stripped.
        assert_eq!(
            Source::classify("https://user:pw@github.com/camunda/repo.git"),
            remote_src("github.com", "camunda/repo")
        );
    }

    /// Anything that could be reinterpreted on the way to the server must fail
    /// closed rather than match a pattern by its surface spelling.
    #[test]
    fn ambiguous_or_exotic_sources_are_opaque() {
        for url in [
            "https://github.com/camunda/../evil/repo",
            "https://github.com/camunda/./repo",
            "https://github.com/camunda/%2e%2e/evil/repo",
            "https://github.com/camunda//repo",
            "https://github.com/camunda/repo?x=1",
            "https://github.com/camunda/repo#frag",
            r"https://github.com/camunda\..\evil",
            "ssh://git@host/~user/repo",
            "git@host:~/repo",
            "git@host:/abs/repo",
            "ext::sh -c touch% /tmp/pwned",
            "fd::17",
            "foo://github.com/camunda/repo",
            "https://github.com",
            "https:///camunda/repo",
        ] {
            assert_eq!(Source::classify(url), Source::Opaque, "{url}");
        }
    }
}
