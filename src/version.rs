//! The version selector of a dependency (`tag`, `branch`, `commit` or a
//! semver `version` range) and the semver rules behind it: the one parser for
//! requirements and the one tag-to-version rule, shared by manifest
//! validation, resolution, lock validation and `spm outdated`.

use anyhow::{anyhow, Result};
use serde::Serialize;

/// The version selector, normalized. Serializes externally tagged
/// (`{"tag": "v1.2.0"}`), mirroring how the selector is written in `ai.json`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Version {
    Tag(String),
    Branch(String),
    Commit(String),
    /// A semver requirement, resolved to the highest matching semver tag.
    #[serde(rename = "version")]
    Range(String),
}

impl Version {
    /// Stable human label stored in ai.lock (see [`LockReference`](crate::ids::LockReference)) and used
    /// to detect request changes.
    pub fn label(&self) -> String {
        match self {
            Version::Tag(t) => format!("tag:{t}"),
            Version::Branch(b) => format!("branch:{b}"),
            Version::Commit(c) => format!("commit:{c}"),
            Version::Range(r) => format!("version:{r}"),
        }
    }
}

/// Parse a `version` selector as a semver requirement (`^1.2`, `~1.4`,
/// `>=1.2, <2`, `1.2` = `^1.2`). The single parser behind manifest validation,
/// resolution, lock validation and `spm outdated`, so they all agree on what a
/// requirement means.
pub fn parse_version_req(req: &str) -> Result<semver::VersionReq> {
    semver::VersionReq::parse(req).map_err(|e| {
        let hint = if req
            .trim_start_matches(['^', '~', '=', '>', '<', ' '])
            .starts_with(['v', 'V'])
        {
            " (drop the `v`: tags are matched with or without it)"
        } else {
            ""
        };
        anyhow!(
            "invalid version requirement `{req}`: {e}{hint} — expected a semver requirement \
             such as `^1.2`, `~1.4.0` or `>=1.2, <2`"
        )
    })
}

/// Parse a tag name as a semver version, allowing one leading `v` (`v1.2.0`).
/// Floating or non-semver tags (`v1`, `latest`, `stable`) yield `None`. The
/// single tag-to-version rule shared by range resolution and `spm outdated`.
pub fn semver_of_tag(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_of_tag_strips_one_v_and_ignores_non_semver() {
        assert_eq!(semver_of_tag("v1.2.3"), Some(semver::Version::new(1, 2, 3)));
        assert_eq!(semver_of_tag("1.2.3"), Some(semver::Version::new(1, 2, 3)));
        for t in ["vv1.2.3", "v1", "v1.2", "latest", "V1.2.3", "pdf-v1.2.0"] {
            assert_eq!(semver_of_tag(t), None, "`{t}`");
        }
    }
}
