//! `github.com/<owner>/<repo>[@<ref>]` shorthand for the positional argument of
//! `spm add`. The shorthand is expanded to a normal HTTPS clone URL up front, so
//! everything downstream (the manifest, the lock file, the store) only ever sees
//! a regular git URL and never the shorthand itself.

use super::common::VersionArg;
use crate::git;
use anyhow::{bail, Result};

const PREFIX: &str = "github.com/";

/// A parsed shorthand: the expanded clone URL plus the optional `@<ref>`.
#[derive(Debug, PartialEq, Eq)]
struct Shorthand {
    url: String,
    reference: Option<String>,
}

/// Parse `github.com/<owner>/<repo>[@<ref>]`.
///
/// Returns `Ok(None)` for anything that does not start with `github.com/`, so
/// full URLs (`https://`, `ssh://`, `file://`), scp-style remotes and local
/// paths are passed through untouched. A malformed shorthand is an error.
fn parse(input: &str) -> Result<Option<Shorthand>> {
    let Some(rest) = input.strip_prefix(PREFIX) else {
        return Ok(None);
    };
    let (repo_part, reference) = match rest.split_once('@') {
        Some((repo, r)) => (repo, Some(r)),
        None => (rest, None),
    };
    let usage = "expected `github.com/<owner>/<repo>[@<ref>]`";
    let segments: Vec<&str> = repo_part.trim_end_matches('/').split('/').collect();
    let [owner, repo] = segments[..] else {
        bail!("invalid GitHub shorthand `{input}`: {usage} (use `--path` for a subdirectory)");
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    for (label, part) in [("owner", owner), ("repo", repo)] {
        let valid = !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !valid {
            bail!("invalid GitHub shorthand `{input}`: bad or missing {label}; {usage}");
        }
    }
    if let Some(r) = reference {
        if r.is_empty() {
            bail!("invalid GitHub shorthand `{input}`: empty ref after `@`; {usage}");
        }
        // A leading `-` is not an option-injection risk here: `classify` always
        // prefixes the value with `refs/heads/` or `refs/tags/` before passing
        // it to Git, so the resulting argument never starts with `-`. Valid
        // refs such as `refs/heads/-topic` must stay usable through shorthand,
        // the same as through the explicit --tag/--branch selectors.
        if r.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("invalid GitHub shorthand `{input}`: `{r}` is not a valid ref");
        }
    }
    Ok(Some(Shorthand {
        url: format!("https://github.com/{owner}/{repo}.git"),
        reference: reference.map(str::to_string),
    }))
}

/// A full-length SHA-1 commit ID (the only form `ai.json` and `ai.lock` accept),
/// which is taken as a commit without probing for a tag or branch.
fn is_full_sha(r: &str) -> bool {
    r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit())
}

/// Decide whether `@<ref>` names a tag, a branch or a commit.
fn classify(url: &str, reference: &str) -> Result<VersionArg> {
    let mut version = VersionArg {
        tag: None,
        branch: None,
        commit: None,
    };
    if is_full_sha(reference) {
        version.commit = Some(reference.to_string());
        return Ok(version);
    }
    let tag = format!("refs/tags/{reference}");
    let head = format!("refs/heads/{reference}");
    let found = git::remote_ref_names(url, &[&tag, &head]).map_err(|e| {
        e.context(format!(
            "looking up `@{reference}` in {url}; pass --tag, --branch or --commit instead to skip the lookup"
        ))
    })?;
    match (found.contains(&tag), found.contains(&head)) {
        (true, false) => version.tag = Some(reference.to_string()),
        (false, true) => version.branch = Some(reference.to_string()),
        (true, true) => bail!(
            "`@{reference}` is both a tag and a branch in {url}; remove `@{reference}` \
             from the shorthand, then pass --tag {reference} or --branch {reference} \
             (adding --tag/--branch alongside `@{reference}` still conflicts)"
        ),
        (false, false) => bail!(
            "`@{reference}` is neither a tag nor a branch in {url}; for a commit, \
             give the full 40-character SHA or use --commit"
        ),
    }
    Ok(version)
}

/// Expand the positional `git` argument of `spm add`.
///
/// Non-shorthand input is returned unchanged together with the flags the user
/// gave. For a shorthand the URL is expanded, and an `@<ref>` is resolved to a
/// tag, branch or commit selector. Combining `@<ref>` with `--tag`, `--branch`
/// or `--commit` is an error rather than a silent precedence rule.
pub(super) fn expand(input: String, version: VersionArg) -> Result<(String, VersionArg)> {
    let Some(short) = parse(&input)? else {
        return Ok((input, version));
    };
    let Some(reference) = short.reference else {
        return Ok((short.url, version));
    };
    if version.tag.is_some() || version.branch.is_some() || version.commit.is_some() {
        bail!(
            "`{input}` already names a ref with `@{reference}`; drop `@{reference}` or \
             drop --tag/--branch/--commit"
        );
    }
    let version = classify(&short.url, &reference)?;
    Ok((short.url, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short(url: &str, reference: Option<&str>) -> Option<Shorthand> {
        Some(Shorthand {
            url: url.to_string(),
            reference: reference.map(str::to_string),
        })
    }

    #[test]
    fn expands_to_https_url_with_and_without_ref() {
        assert_eq!(
            parse("github.com/o/r").unwrap(),
            short("https://github.com/o/r.git", None)
        );
        assert_eq!(
            parse("github.com/o/r@v1.2.0").unwrap(),
            short("https://github.com/o/r.git", Some("v1.2.0"))
        );
        // Branch names may contain `/`; a `.git` suffix and trailing `/` are tolerated.
        assert_eq!(
            parse("github.com/o/r.git@feature/x").unwrap(),
            short("https://github.com/o/r.git", Some("feature/x"))
        );
        assert_eq!(
            parse("github.com/o/r/").unwrap(),
            short("https://github.com/o/r.git", None)
        );
    }

    #[test]
    fn everything_else_passes_through() {
        for input in [
            "https://github.com/o/r",
            "https://github.com/o/r.git",
            "http://github.com/o/r",
            "ssh://git@github.com/o/r.git",
            "git@github.com:o/r.git",
            "file:///tmp/repo",
            "/tmp/repo",
            "./github.com/o/r",
            "gitlab.com/o/r",
            "github.community/o/r",
        ] {
            assert_eq!(parse(input).unwrap(), None, "{input}");
        }
    }

    #[test]
    fn malformed_shorthand_is_rejected() {
        for input in [
            "github.com/",
            "github.com/o",
            "github.com/o/",
            "github.com//r",
            "github.com/o/r/extra",
            "github.com/o/r/tree/main",
            "github.com/o/..",
            "github.com/o/.git",
            "github.com/o b/r",
            "github.com/o/r@",
            "github.com/o/r@ v1",
            "github.com/@v1",
        ] {
            assert!(parse(input).is_err(), "{input} should be rejected");
        }
    }

    #[test]
    fn leading_dash_refs_are_allowed() {
        // `classify` always prefixes the value with `refs/heads/` or
        // `refs/tags/` before passing it to Git, so a leading `-` here never
        // reaches Git as a bare, injectable argument. Valid refs such as
        // `refs/heads/-topic` or `refs/tags/--upload-pack=x` must stay usable
        // through shorthand, the same as through explicit --tag/--branch.
        assert_eq!(
            parse("github.com/o/r@-topic").unwrap(),
            short("https://github.com/o/r.git", Some("-topic"))
        );
        assert_eq!(
            parse("github.com/o/r@--upload-pack=x").unwrap(),
            short("https://github.com/o/r.git", Some("--upload-pack=x"))
        );
    }

    #[test]
    fn full_sha_is_a_commit_without_probing() {
        let sha = "a".repeat(40);
        // The URL is unreachable: a full SHA must be classified without any lookup.
        let v = classify("file:///nonexistent", &sha).unwrap();
        assert_eq!(v.commit.as_deref(), Some(sha.as_str()));
        assert!(v.tag.is_none() && v.branch.is_none());
        // A 64-character (SHA-256) ID is not a valid `ai.json` commit, so it must
        // not be accepted here only to be rejected by the reload in `sync`.
        assert!(!is_full_sha(&"0".repeat(64)));
        assert!(!is_full_sha("abc1234"));
        assert!(!is_full_sha(&"g".repeat(40)));
    }

    #[test]
    fn passthrough_keeps_the_users_version_flags() {
        let v = VersionArg {
            tag: Some("v1".into()),
            branch: None,
            commit: None,
        };
        let (url, v) = expand("https://example.com/o/r".into(), v).unwrap();
        assert_eq!(url, "https://example.com/o/r");
        assert_eq!(v.tag.as_deref(), Some("v1"));
    }

    #[test]
    fn ref_conflicts_with_explicit_version_flags() {
        let v = VersionArg {
            tag: None,
            branch: Some("main".into()),
            commit: None,
        };
        let err = expand("github.com/o/r@v1".into(), v).err().unwrap();
        assert!(
            format!("{err:#}").contains("already names a ref"),
            "{err:#}"
        );
    }

    #[test]
    fn shorthand_without_ref_keeps_flags() {
        let v = VersionArg {
            tag: Some("v1".into()),
            branch: None,
            commit: None,
        };
        let (url, v) = expand("github.com/o/r".into(), v).unwrap();
        assert_eq!(url, "https://github.com/o/r.git");
        assert_eq!(v.tag.as_deref(), Some("v1"));
    }
}
