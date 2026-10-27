use crate::git;
use crate::ids::{CommitSha, LockReference, StoreKey};
use crate::lockfile::LockedSkill;
use crate::manifest::SkillSpec;
use crate::version::{parse_version_req, semver_of_tag, Version};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

/// Resolve a single skill spec to a locked entry (ref -> commit SHA + store key).
pub fn resolve(spec: &SkillSpec) -> Result<LockedSkill> {
    let version = spec.version()?;
    let mut resolved_tag = None;
    let commit = match &version {
        Version::Commit(c) => c.clone(),
        Version::Tag(t) => {
            // Query the tag and its peel; ls_remote prefers the dereferenced commit.
            git::ls_remote(
                &spec.git,
                &[&format!("refs/tags/{t}"), &format!("refs/tags/{t}^{{}}")],
            )?
        }
        Version::Branch(b) => git::ls_remote(&spec.git, &[&format!("refs/heads/{b}")])?,
        Version::Range(r) => {
            // One `ls-remote` lists every tag, already peeled to its commit.
            let refs = git::remote_refs(&spec.git, &["refs/tags/*"])?;
            let (tag, sha) = highest_matching_tag(&refs, r)
                .map_err(|e| e.context(format!("resolving `version: {r}` in {}", spec.git)))?;
            resolved_tag = Some(tag.to_string());
            sha.to_string()
        }
    };
    // Normalize to lowercase so the pin matches git's lowercase `rev-parse HEAD`
    // (otherwise an uppercase SHA is treated as stale and refetched every run).
    let commit = CommitSha::try_from(commit.to_lowercase())?;

    Ok(LockedSkill {
        git: spec.git.clone(),
        store: StoreKey::derive(&spec.git, &commit),
        path: spec.path.clone(),
        commit,
        resolved_tag,
        reference: LockReference::from(version),
        bundled_skills: Vec::new(),
        requested_by: Vec::new(),
        also_satisfies: Vec::new(),
    })
}

/// How many semver tags a "no tag matches" error lists before eliding.
const LISTED_TAGS: usize = 10;

/// Pick the highest semver tag in `refs` (`refs/tags/<name> -> commit`) that
/// satisfies the semver requirement `req`. One leading `v` is stripped,
/// non-semver tags are ignored, and prereleases only match when the
/// requirement opts in (the `semver` crate's rules). When `v1.2.0` and
/// `1.2.0` both exist, the `v`-prefixed one wins so the choice is stable.
///
/// No match is an error listing the newest available semver tags, so a typo
/// or a too-narrow range is easy to fix.
pub fn highest_matching_tag<'r>(
    refs: &'r BTreeMap<String, String>,
    req: &str,
) -> Result<(&'r str, &'r str)> {
    let parsed = parse_version_req(req)?;
    let mut tags: Vec<(semver::Version, &str, &str)> = refs
        .iter()
        .filter_map(|(r, sha)| {
            let tag = r.strip_prefix("refs/tags/")?;
            Some((semver_of_tag(tag)?, tag, sha.as_str()))
        })
        .collect();
    // Newest first; ties (same version, different spelling) broken by name.
    tags.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(a.1)));
    if let Some((_, tag, sha)) = tags.iter().find(|(v, _, _)| parsed.matches(v)) {
        return Ok((tag, sha));
    }
    if tags.is_empty() {
        bail!("no tag matches `{req}`: the repository has no semver tags (`v1.2.3` or `1.2.3`)");
    }
    let shown: Vec<&str> = tags.iter().take(LISTED_TAGS).map(|t| t.1).collect();
    let more = match tags.len().saturating_sub(LISTED_TAGS) {
        0 => String::new(),
        n => format!(" (and {n} older)"),
    };
    bail!(
        "no tag matches `{req}`; available semver tags: {}{more}",
        shown.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::highest_matching_tag;
    use std::collections::BTreeMap;

    fn refs(tags: &[&str]) -> BTreeMap<String, String> {
        tags.iter()
            .map(|t| (format!("refs/tags/{t}"), format!("sha-{t}")))
            .collect()
    }

    fn pick<'r>(r: &'r BTreeMap<String, String>, req: &str) -> &'r str {
        highest_matching_tag(r, req).unwrap().0
    }

    #[test]
    fn picks_the_highest_tag_in_range_and_never_crosses_a_major() {
        let r = refs(&["v1.1.0", "v1.2.0", "v1.4.1", "v1.10.0", "v2.0.0"]);
        assert_eq!(pick(&r, "^1.2"), "v1.10.0", "numeric, not lexical, order");
        assert_eq!(pick(&r, "~1.4"), "v1.4.1");
        assert_eq!(pick(&r, ">=1.2, <1.5"), "v1.4.1");
        assert_eq!(pick(&r, "1.2"), "v1.10.0", "a bare version means ^");
        assert_eq!(pick(&r, "=1.2.0"), "v1.2.0");
        assert_eq!(pick(&r, "*"), "v2.0.0");
        assert_eq!(
            highest_matching_tag(&r, "^1.2").unwrap().1,
            "sha-v1.10.0",
            "returns the tag's commit"
        );
    }

    #[test]
    fn matches_v_prefixed_and_bare_tags_and_ignores_non_semver() {
        let r = refs(&[
            "1.3.0",
            "v1.2.0",
            "v1",
            "v1.9",
            "latest",
            "pdf-v1.9.0",
            "vv1.8.0",
        ]);
        assert_eq!(pick(&r, "^1.2"), "1.3.0");
        // Same version spelled both ways: deterministic `v`-prefixed pick.
        let r = refs(&["1.2.0", "v1.2.0"]);
        assert_eq!(pick(&r, "^1"), "v1.2.0");
    }

    #[test]
    fn excludes_prereleases_unless_the_requirement_opts_in() {
        let r = refs(&["v1.2.0", "v1.3.0-rc.1", "v2.0.0-beta.1"]);
        assert_eq!(pick(&r, "^1.2"), "v1.2.0");
        assert_eq!(pick(&r, ">=1.3.0-rc.1, <2"), "v1.3.0-rc.1");
        let only_pre = refs(&["v1.3.0-rc.1"]);
        assert!(highest_matching_tag(&only_pre, "^1").is_err());
    }

    #[test]
    fn no_match_lists_the_newest_available_semver_tags() {
        let r = refs(&["v0.9.0", "v2.0.0", "v2.1.0", "latest"]);
        let err = format!("{:#}", highest_matching_tag(&r, "^1.2").unwrap_err());
        assert!(err.contains("no tag matches `^1.2`"), "{err}");
        assert!(err.contains("v2.1.0, v2.0.0, v0.9.0"), "{err}");
        assert!(!err.contains("latest"), "{err}");

        let many: Vec<String> = (0..15).map(|i| format!("v3.{i}.0")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let err = format!(
            "{:#}",
            highest_matching_tag(&refs(&many), "^1").unwrap_err()
        );
        assert!(err.contains("v3.14.0") && !err.contains("v3.4.0,"), "{err}");
        assert!(err.contains("(and 5 older)"), "{err}");

        let err = format!(
            "{:#}",
            highest_matching_tag(&refs(&["latest", "v1"]), "^1").unwrap_err()
        );
        assert!(err.contains("has no semver tags"), "{err}");
    }

    #[test]
    fn rejects_an_invalid_requirement() {
        let err = highest_matching_tag(&refs(&["v1.0.0"]), "not semver").unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid version requirement"),
            "{err:#}"
        );
    }
}
