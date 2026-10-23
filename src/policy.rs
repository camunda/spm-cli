//! Source allowlist policy: restricts which git sources spm may fetch skills and
//! plugins from.
//!
//! A policy is made of up to two **layers**, and a source must be allowed by
//! *every* layer that exists (the effective policy is their intersection):
//!
//! 1. the **machine** layer — `$SPM_HOME/policy.json`, or the file `SPM_POLICY`
//!    points at — which CI and managed laptops set and a project cannot loosen;
//! 2. the **project** layer — `allowedSources` in the root `ai.json` (never in a
//!    dependency's nested `ai.json`, which is rejected at load).
//!
//! With no layer configured nothing is restricted. Patterns are matched against
//! a protocol-independent identity of the source (see [`Source::classify`]), so
//! the HTTPS, SSH-URL and scp-style spellings of one repository — and anything
//! `--protocol` rewrites between them — are judged identically.
//!
//! Enforcement happens before any network access: callers check each source up
//! front for a precise error, and two choke points back that up categorically —
//! every URL handed to git for a remote operation (`git::with_transport`) and
//! every store checkout, even a cached one (`store::ensure`), passes [`gate`].

use crate::manifest::Manifest;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The pattern that allows `file://` URLs and local filesystem paths.
pub const ANY_LOCAL: &str = "file://*";

/// What a dependency's `git` value points at, as far as the policy is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
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
    /// [`crate::git::remote_identity`] — the same equivalence `--protocol`
    /// rewriting relies on — so the interchangeable spellings of one repository
    /// can never be judged differently. Everything else is parsed strictly and
    /// fails closed to [`Source::Opaque`].
    fn classify(git: &str) -> Source {
        let s = git.trim();
        if let Some(identity) = crate::git::remote_identity(s) {
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
    fn describe(&self) -> String {
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

fn valid_host(host: &str) -> bool {
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
fn valid_segment(seg: &str) -> bool {
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

/// One `allowedSources` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    /// `file://*`: any `file://` URL or local path.
    AnyLocal,
    /// `host/seg/…/seg` (exact repository) or `host/seg/…/*` (`*` matches one
    /// or more further segments, so `host/org/*` also covers subgroups).
    Remote {
        host: String,
        prefix: Vec<String>,
        wildcard: bool,
    },
}

impl Pattern {
    fn parse(raw: &str) -> Result<Pattern> {
        let usage = "expected `host/org/repo`, `host/org/*`, `host/*` or `file://*`";
        if raw == ANY_LOCAL {
            return Ok(Pattern::AnyLocal);
        }
        if raw.contains("://") {
            bail!(
                "invalid source pattern `{raw}`: patterns have no scheme (they match HTTPS, \
                 SSH and scp-style URLs alike); {usage}"
            );
        }
        let mut parts = raw.split('/');
        let host = parts.next().unwrap_or_default().to_ascii_lowercase();
        let rest: Vec<&str> = parts.collect();
        if host.contains('*') {
            bail!(
                "invalid source pattern `{raw}`: `*` is only allowed as the whole last path \
                 segment; {usage}"
            );
        }
        if !valid_host(&host) {
            bail!("invalid source pattern `{raw}`: bad host; {usage}");
        }
        if rest.is_empty() {
            bail!(
                "invalid source pattern `{raw}`: it names only a host; use `{raw}/*` to allow \
                 every repository on it"
            );
        }
        let wildcard = rest.last() == Some(&"*");
        let mut prefix: Vec<String> = Vec::new();
        for (i, seg) in rest.iter().enumerate() {
            if wildcard && i == rest.len() - 1 {
                break;
            }
            if seg.contains('*') {
                bail!(
                    "invalid source pattern `{raw}`: `*` is only allowed as the whole last \
                     path segment; {usage}"
                );
            }
            let seg = if !wildcard && i == rest.len() - 1 {
                seg.strip_suffix(".git").unwrap_or(seg)
            } else {
                seg
            };
            if !valid_segment(seg) {
                bail!("invalid source pattern `{raw}`: bad path segment `{seg}`; {usage}");
            }
            prefix.push(seg.to_string());
        }
        Ok(Pattern::Remote {
            host,
            prefix,
            wildcard,
        })
    }

    fn matches(&self, source: &Source) -> bool {
        match (self, source) {
            (Pattern::AnyLocal, Source::Local) => true,
            (
                Pattern::Remote {
                    host,
                    prefix,
                    wildcard,
                },
                Source::Remote { host: h, segments },
            ) => {
                host == h
                    && if *wildcard {
                        segments.len() > prefix.len() && segments.starts_with(prefix)
                    } else {
                        segments == prefix
                    }
            }
            _ => false,
        }
    }
}

/// Validate a list of `allowedSources` patterns (e.g. while loading `ai.json`).
pub fn validate_patterns(raw: &[String]) -> Result<()> {
    raw.iter().try_for_each(|p| Pattern::parse(p).map(drop))
}

/// One policy layer: where it came from, plus its patterns.
#[derive(Debug, Clone)]
struct Layer {
    /// e.g. `machine policy /etc/spm/policy.json`.
    origin: String,
    raw: Vec<String>,
    patterns: Vec<Pattern>,
}

impl Layer {
    fn new(origin: String, raw: &[String]) -> Result<Layer> {
        let patterns = raw
            .iter()
            .map(|p| Pattern::parse(p))
            .collect::<Result<_>>()
            .with_context(|| format!("in the {origin}"))?;
        Ok(Layer {
            origin,
            raw: raw.to_vec(),
            patterns,
        })
    }

    fn allows(&self, source: &Source) -> bool {
        self.patterns.iter().any(|p| p.matches(source))
    }

    fn summary(&self) -> String {
        let allows = if self.raw.is_empty() {
            "allows nothing".to_string()
        } else {
            let list: Vec<String> = self.raw.iter().map(|p| format!("`{p}`")).collect();
            format!("allows {}", list.join(", "))
        };
        format!("the {} ({allows})", self.origin)
    }
}

/// The machine policy file: `{ "allowedSources": [...] }`. Unknown keys are an
/// error so a typo (say `allowedSource`) cannot silently disable the policy.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    #[serde(rename = "allowedSources")]
    allowed_sources: Vec<String>,
}

/// The effective source policy: the intersection of every configured layer.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    layers: Vec<Layer>,
}

impl Policy {
    /// The policy for a command operating on `manifest` (read from `manifest_path`):
    /// the machine layer, if configured, plus the manifest's `allowedSources`.
    pub fn load(manifest: &Manifest, manifest_path: &Path) -> Result<Policy> {
        let mut policy = Policy::machine()?;
        if let Some(raw) = &manifest.allowed_sources {
            policy.layers.push(Layer::new(
                format!(
                    "project policy `allowedSources` in {}",
                    manifest_path.display()
                ),
                raw,
            )?);
        }
        Ok(policy)
    }

    /// Only the machine layer (`SPM_POLICY`, else `$SPM_HOME/policy.json`). A
    /// missing default file means no machine policy; a missing file that
    /// `SPM_POLICY` explicitly names is an error, as is any unreadable or
    /// malformed policy — a broken policy must never fail open.
    fn machine() -> Result<Policy> {
        let loc = crate::paths::policy_file()?;
        let origin = if loc.explicit {
            format!("machine policy {} (from SPM_POLICY)", loc.path.display())
        } else {
            format!("machine policy {}", loc.path.display())
        };
        // Only a default location with *no directory entry at all* means "no
        // machine policy". `symlink_metadata` doesn't follow links, so a
        // dangling `policy.json` symlink (say, to a managed file that went
        // missing) still counts as configured and its read error surfaces.
        if !loc.explicit {
            match std::fs::symlink_metadata(&loc.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Policy::default()),
                Err(e) => return Err(anyhow!(e).context(format!("reading the {origin}"))),
                Ok(_) => {}
            }
        }
        let text =
            std::fs::read_to_string(&loc.path).with_context(|| format!("reading the {origin}"))?;
        let file: PolicyFile =
            serde_json::from_str(&text).with_context(|| format!("parsing the {origin}"))?;
        Ok(Policy {
            layers: vec![Layer::new(origin, &file.allowed_sources)?],
        })
    }

    /// Fail unless every layer allows `git`. The error names the source, the
    /// identity it was matched as, and each layer that denied it.
    pub fn check(&self, git: &str) -> Result<()> {
        if self.layers.is_empty() {
            return Ok(());
        }
        let source = Source::classify(git);
        let denied: Vec<String> = self
            .layers
            .iter()
            .filter(|l| !l.allows(&source))
            .map(Layer::summary)
            .collect();
        if denied.is_empty() {
            return Ok(());
        }
        bail!(
            "source `{git}` ({}) is not allowed: denied by {}",
            source.describe(),
            denied.join(" and by ")
        )
    }
}

/// The policy the current command activated (see [`activate`]).
static ACTIVE: Mutex<Option<Arc<Policy>>> = Mutex::new(None);

/// Load the policy for `manifest` (in `dir`) and make it the one [`gate`]
/// enforces for the rest of the process. Returns it for up-front checks.
pub fn activate(manifest: &Manifest, dir: &Path) -> Result<Arc<Policy>> {
    let policy = Arc::new(Policy::load(manifest, &Manifest::path_in(dir))?);
    *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(policy.clone());
    Ok(policy)
}

/// The backstop every source access goes through. Uses the activated policy;
/// if no command activated one, the machine layer still applies, so a code path
/// that forgets to activate can never bypass the machine policy.
pub fn gate(git: &str) -> Result<()> {
    let active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match active {
        Some(policy) => policy.check(git),
        None => fallback()?.check(git),
    }
}

/// Whether any policy layer is in force for this process — the active one, or
/// else the machine layer (a machine policy that fails to load counts as in
/// force: fail closed). Lets the git layer tighten transport behavior (no HTTP
/// redirects) only when a policy exists, keeping no-policy behavior unchanged.
pub fn restricts() -> bool {
    let active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match active {
        Some(policy) => !policy.layers.is_empty(),
        None => fallback().map_or(true, |p| !p.layers.is_empty()),
    }
}

/// The policy [`gate`] applies when no command activated one: the machine
/// layer. In-process unit tests (which drive `git`/`store` helpers directly and
/// never activate a policy) get no layer instead, so a policy configured on the
/// developer's or CI machine can't change their outcome. The real binary — and
/// so every end-to-end test of the machine layer — always takes the first arm.
#[cfg(not(test))]
fn fallback() -> Result<Policy> {
    Policy::machine()
}

#[cfg(test)]
fn fallback() -> Result<Policy> {
    Ok(Policy::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_src(host: &str, path: &str) -> Source {
        Source::Remote {
            host: host.to_string(),
            segments: path.split('/').map(str::to_string).collect(),
        }
    }

    fn policy(raw: &[&str]) -> Policy {
        let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        Policy {
            layers: vec![Layer::new("test policy".to_string(), &raw).unwrap()],
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
            "  https://github.com/camunda/skills  ",
        ] {
            assert_eq!(Source::classify(url), want, "{url}");
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

    #[test]
    fn patterns_parse_and_reject_bad_syntax() {
        assert_eq!(Pattern::parse("file://*").unwrap(), Pattern::AnyLocal);
        assert_eq!(
            Pattern::parse("GitHub.com/camunda/*").unwrap(),
            Pattern::Remote {
                host: "github.com".into(),
                prefix: vec!["camunda".into()],
                wildcard: true
            }
        );
        assert_eq!(
            Pattern::parse("github.com/camunda/skills.git").unwrap(),
            Pattern::parse("github.com/camunda/skills").unwrap()
        );
        for (bad, why) in [
            ("https://github.com/camunda/*", "no scheme"),
            ("file:///tmp/*", "no scheme"),
            ("github.com", "names only a host"),
            ("*", "only allowed as the whole last"),
            ("*.github.com/*", "only allowed as the whole last"),
            ("github.com/*/repo", "only allowed as the whole last"),
            (
                "github.com/camunda/skill-*",
                "only allowed as the whole last",
            ),
            ("github.com/camunda/", "bad path segment"),
            ("github.com//repo", "bad path segment"),
            ("github.com/camunda/../evil", "bad path segment"),
            ("github.com/camunda repo/*", "bad path segment"),
            ("/camunda/*", "bad host"),
            ("", "bad host"),
        ] {
            let err = format!("{:#}", Pattern::parse(bad).unwrap_err());
            assert!(err.contains(why), "{bad}: {err}");
        }
    }

    #[test]
    fn wildcard_matches_one_or_more_segments_under_the_prefix() {
        let p = policy(&["github.com/camunda/*"]);
        assert!(p.check("https://github.com/camunda/skills").is_ok());
        assert!(p.check("git@github.com:camunda/skills.git").is_ok());
        assert!(p
            .check("https://gitlab.com/camunda/sub/group/repo")
            .is_err());
        assert!(policy(&["gitlab.com/camunda/*"])
            .check("https://gitlab.com/camunda/sub/group/repo")
            .is_ok());
        // The prefix itself is not a repository under it.
        assert!(p.check("https://github.com/camunda").is_err());
        // Segment-wise, not string-prefix, matching.
        assert!(p.check("https://github.com/camunda-evil/skills").is_err());
        // Host-wide wildcard.
        assert!(policy(&["github.com/*"])
            .check("ssh://git@github.com/anyone/anything")
            .is_ok());
    }

    #[test]
    fn exact_patterns_match_only_that_repository() {
        let p = policy(&["github.com/camunda/skills"]);
        assert!(p.check("https://github.com/camunda/skills.git").is_ok());
        assert!(p.check("git@github.com:camunda/skills").is_ok());
        assert!(p.check("https://github.com/camunda/skills/extra").is_err());
        assert!(p.check("https://github.com/camunda/other").is_err());
    }

    /// Hosts are case-insensitive; repository paths are not (some forges treat
    /// `Org` and `org` as different namespaces), so a case change fails closed.
    #[test]
    fn host_is_case_insensitive_but_path_is_not() {
        let p = policy(&["GITHUB.com/camunda/*"]);
        assert!(p.check("https://github.COM/camunda/x").is_ok());
        assert!(p.check("https://github.com/Camunda/x").is_err());
    }

    #[test]
    fn ports_must_be_spelled_in_the_pattern() {
        assert!(policy(&["git.corp/team/*"])
            .check("https://git.corp:8443/team/repo")
            .is_err());
        assert!(policy(&["git.corp:8443/team/*"])
            .check("https://git.corp:8443/team/repo")
            .is_ok());
    }

    #[test]
    fn local_sources_need_the_explicit_file_pattern() {
        let remote_only = policy(&["github.com/*"]);
        let err = format!("{:#}", remote_only.check("file:///tmp/repo").unwrap_err());
        assert!(err.contains("file://*"), "{err}");
        assert!(remote_only.check("../work/repo").is_err());
        let local = policy(&["file://*"]);
        assert!(local.check("file:///tmp/repo").is_ok());
        assert!(local.check("../work/repo").is_ok());
        assert!(local.check("https://github.com/o/r").is_err());
    }

    #[test]
    fn opaque_sources_are_denied_even_by_a_host_wildcard() {
        let p = policy(&["github.com/*", "file://*"]);
        let err = format!(
            "{:#}",
            p.check("https://github.com/ok/../evil/x").unwrap_err()
        );
        assert!(err.contains("no pattern can allow it"), "{err}");
        assert!(p.check("ext::sh -c id").is_err());
    }

    #[test]
    fn no_layers_means_no_restriction_and_empty_list_denies_all() {
        assert!(Policy::default().check("ext::anything").is_ok());
        let err = format!(
            "{:#}",
            policy(&[]).check("https://github.com/o/r").unwrap_err()
        );
        assert!(err.contains("allows nothing"), "{err}");
    }

    /// The effective policy is the intersection: a source must pass every
    /// layer, and the error names each layer that denied it.
    #[test]
    fn layers_intersect_and_errors_name_every_denying_layer() {
        let machine = Layer::new(
            "machine policy M".to_string(),
            &["github.com/camunda/*".to_string()],
        )
        .unwrap();
        let project = Layer::new(
            "project policy P".to_string(),
            &["github.com/*".to_string()],
        )
        .unwrap();
        let p = Policy {
            layers: vec![machine, project],
        };
        assert!(p.check("https://github.com/camunda/x").is_ok());
        let err = format!("{:#}", p.check("https://github.com/other/x").unwrap_err());
        assert!(err.contains("machine policy M"), "{err}");
        assert!(!err.contains("project policy P"), "{err}");
        assert!(err.contains("github.com/other/x"), "{err}");
        let err = format!("{:#}", p.check("https://gitlab.com/o/x").unwrap_err());
        assert!(
            err.contains("machine policy M") && err.contains("project policy P"),
            "{err}"
        );
    }
}
