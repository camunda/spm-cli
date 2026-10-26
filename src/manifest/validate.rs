//! Semantic validation of a dependency spec: exactly one version selector, a
//! contained `path` and a full-SHA `commit`, shared by root and nested
//! manifests.

use super::SkillSpec;
use crate::version::{parse_version_req, Version};
use anyhow::{bail, Context, Result};
use std::path::{Component, Path};

impl SkillSpec {
    /// Validate that exactly one version selector is set (and, for `version`,
    /// that it is a valid semver requirement) and return it.
    pub fn version(&self) -> Result<Version> {
        match (&self.tag, &self.branch, &self.commit, &self.range) {
            (Some(t), None, None, None) => Ok(Version::Tag(t.clone())),
            (None, Some(b), None, None) => Ok(Version::Branch(b.clone())),
            (None, None, Some(c), None) => Ok(Version::Commit(c.clone())),
            (None, None, None, Some(r)) => {
                parse_version_req(r).with_context(|| format!("skill `{}`", self.git))?;
                Ok(Version::Range(r.clone()))
            }
            (None, None, None, None) => {
                bail!("skill `{}`: set one of tag/branch/commit/version", self.git)
            }
            _ => bail!(
                "skill `{}`: set only one of tag/branch/commit/version",
                self.git
            ),
        }
    }

    /// Check every field the way the schema's `skill` definition does (plus
    /// the semantic `path`/`version` rules) and return the version selector.
    /// Shared by root and nested manifests: a nested one skips the schema, so
    /// these checks are all that stand between it and git and the store path.
    pub fn validate(&self) -> Result<Version> {
        for (field, value) in [
            ("git", Some(&self.git)),
            ("tag", self.tag.as_ref()),
            ("branch", self.branch.as_ref()),
            ("path", self.path.as_ref()),
        ] {
            if value.is_some_and(|v| v.is_empty()) {
                bail!("`{field}` must be non-empty");
            }
        }
        if let Some(sub) = &self.path {
            validate_subpath(sub)?;
        }
        // The commit is appended to a store path (see `validate_commit_selector`).
        if let Some(commit) = &self.commit {
            validate_commit_selector(commit)?;
        }
        self.version()
    }
}

/// Reject a `commit` selector that is not a full 40-character hex SHA.
///
/// The root `ai.json` is guarded by the JSON schema, whose `commit` pattern
/// (`^[0-9a-fA-F]{40}$`) is the only thing keeping an untrusted commit string
/// out of the store path: `resolver::resolve` feeds a `commit` selector verbatim
/// into `StoreKey::derive`, which appends it into a directory name that `store::ensure`
/// joins onto the store root and recursively removes/rewrites. A *nested*
/// (`DependencyManifest`) spec bypasses that schema, so a hostile dependency
/// could otherwise smuggle slashes or `..` into the store path via the commit
/// field. Enforce the same shape here (case-insensitively, before the resolver
/// lowercases it) so nested specs get identical protection.
fn validate_commit_selector(commit: &str) -> Result<()> {
    if commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid commit `{commit}`: expected a full 40-character hexadecimal SHA");
    }
    Ok(())
}

/// Reject a skill `path` that could escape the fetched repo root. The subdir is
/// joined onto the store checkout (`repo_dir.join(path)`); without this a hostile
/// manifest could use an absolute path or `..` to read arbitrary files off the
/// host (e.g. `/etc`, or a sibling repo's checkout in the shared store).
pub fn validate_subpath(path: &str) -> Result<()> {
    let p = Path::new(path);
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                bail!("invalid path `{path}`: must be relative to the repo root")
            }
            Component::ParentDir => {
                bail!("invalid path `{path}`: `..` components are not allowed")
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    if path.contains('\0') {
        bail!("invalid path `{path}`: must not contain NUL");
    }
    Ok(())
}

/// Reject a repeated entry in the `field` list of the manifest at `p`.
pub(super) fn ensure_unique(items: &[String], field: &str, p: &Path) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    if let Some(dup) = items.iter().find(|i| !seen.insert(i.as_str())) {
        bail!("`{field}` in {} lists `{dup}` more than once", p.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(git: &str, tag: Option<&str>, branch: Option<&str>, commit: Option<&str>) -> SkillSpec {
        SkillSpec {
            git: git.to_string(),
            tag: tag.map(str::to_string),
            branch: branch.map(str::to_string),
            commit: commit.map(str::to_string),
            range: None,
            path: None,
        }
    }

    /// The schema requires exactly one of tag/branch/commit on any manifest
    /// loaded from disk, but `SkillSpec::version` is also called on
    /// programmatically-constructed specs (e.g. `add --all`'s container spec)
    /// where that invariant isn't schema-enforced — so both error arms must be
    /// checked directly.
    #[test]
    fn version_rejects_no_selector() {
        let s = spec("u", None, None, None);
        let Err(err) = s.version() else {
            panic!("expected an error");
        };
        assert!(format!("{err}").contains("set one of tag/branch/commit/version"));
    }

    #[test]
    fn version_rejects_multiple_selectors() {
        let s = spec("u", Some("v1"), Some("main"), None);
        let Err(err) = s.version() else {
            panic!("expected an error");
        };
        assert!(format!("{err}").contains("set only one of tag/branch/commit/version"));

        let sha = "a".repeat(40);
        let all_three = spec("u", Some("v1"), Some("main"), Some(sha.as_str()));
        assert!(all_three.version().is_err());

        // `version` conflicts with each of the other three selectors.
        for mut s in [
            spec("u", Some("v1"), None, None),
            spec("u", None, Some("main"), None),
            spec("u", None, None, Some(sha.as_str())),
        ] {
            s.range = Some("^1.2".into());
            let err = s.version().unwrap_err();
            assert!(format!("{err}").contains("set only one"), "{err}");
        }
    }

    #[test]
    fn version_accepts_a_semver_range_and_rejects_an_invalid_one() {
        let mut s = spec("u", None, None, None);
        s.range = Some(">=1.2, <2".into());
        assert_eq!(s.version().unwrap(), Version::Range(">=1.2, <2".into()));
        for bad in ["", "not-a-version", "^v1.2", "1.2.3.4"] {
            s.range = Some(bad.into());
            let err = s.version().unwrap_err();
            assert!(
                format!("{err:#}").contains("invalid version requirement"),
                "`{bad}`: {err:#}"
            );
        }
        s.range = Some("^v1.2".into());
        assert!(
            format!("{:#}", s.version().unwrap_err()).contains("drop the `v`"),
            "a `v`-prefixed requirement gets a targeted hint"
        );
    }

    /// `1.2` is `^1.2` to the semver crate, which is exactly why ranges get
    /// their own `version` field: an existing exact `"tag": "1.2"` must keep
    /// meaning the literal tag `1.2`.
    #[test]
    fn tag_stays_exact_while_version_is_a_range() {
        assert_eq!(
            spec("u", Some("1.2"), None, None).version().unwrap(),
            Version::Tag("1.2".into())
        );
        let req = parse_version_req("1.2").unwrap();
        assert!(req.matches(&semver::Version::new(1, 9, 0)));
    }

    #[test]
    fn version_accepts_each_single_selector() {
        assert!(matches!(
            spec("u", Some("v1"), None, None).version().unwrap(),
            Version::Tag(t) if t == "v1"
        ));
        assert!(matches!(
            spec("u", None, Some("main"), None).version().unwrap(),
            Version::Branch(b) if b == "main"
        ));
        let sha = "a".repeat(40);
        assert!(matches!(
            spec("u", None, None, Some(&sha)).version().unwrap(),
            Version::Commit(c) if c == sha
        ));
    }

    #[test]
    fn validate_subpath_rejects_nul_byte() {
        let err = validate_subpath("foo\0bar").unwrap_err();
        assert!(format!("{err}").contains("NUL"), "{err}");
    }

    #[test]
    fn validate_subpath_accepts_plain_relative_path() {
        assert!(validate_subpath("skills/greet").is_ok());
        assert!(validate_subpath(".").is_ok());
    }

    #[test]
    fn validate_commit_selector_accepts_both_cases_and_rejects_bad() {
        assert!(validate_commit_selector(&"a".repeat(40)).is_ok());
        assert!(
            validate_commit_selector(&"A".repeat(40)).is_ok(),
            "case-insensitive"
        );
        assert!(validate_commit_selector("abc123").is_err(), "too short");
        assert!(
            validate_commit_selector(&"g".repeat(40)).is_err(),
            "non-hex"
        );
        assert!(
            validate_commit_selector(&format!("{}/x", "a".repeat(38))).is_err(),
            "path separator"
        );
    }
}
