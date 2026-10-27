//! `allowedSources` patterns: their syntax and how they match a [`Source`].

use super::source::{valid_host, valid_segment, Source};
use anyhow::{bail, Result};

/// The pattern that allows `file://` URLs and local filesystem paths.
pub(super) const ANY_LOCAL: &str = "file://*";

/// One `allowedSources` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pattern {
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
    pub(super) fn parse(raw: &str) -> Result<Pattern> {
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

    pub(super) fn matches(&self, source: &Source) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::testutil::policy;

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
        assert!(err.contains("a local source"), "{err}");
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
}
