//! The `spm outdated` report model: one [`Row`] per locked dependency (and a
//! [`SelectorCheck`] per secondary selector), their [`Status`], and the exit
//! status both renderings share.

use super::upgrade::Upgrade;
use crate::ids::DepName;
use crate::version::Version;
use anyhow::{bail, Result};
use serde::Serialize;

/// How a locked dependency compares to its remote.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Status {
    UpToDate,
    /// The branch tip moved past the locked commit, or a newer tag inside a
    /// `version` range exists (`spm update` moves to it).
    Behind,
    /// A newer semver tag than the pinned one (or, for a `version` range,
    /// than the newest tag in range) exists on the remote.
    NewerTag,
    /// The pinned tag (or the tag a range is locked to) now resolves to a
    /// different commit.
    TagMoved,
    /// A commit selector: immutable, never queried.
    Pinned,
    /// The remote could not be checked; see `error`.
    Error,
}

impl Status {
    pub(super) fn is_outdated(self) -> bool {
        matches!(self, Status::Behind | Status::NewerTag | Status::TagMoved)
    }

    pub(super) fn text(self) -> &'static str {
        match self {
            Status::UpToDate => "up to date",
            Status::Behind => "behind",
            Status::NewerTag => "newer tag",
            Status::TagMoved => "tag moved",
            Status::Pinned => "pinned",
            Status::Error => "error",
        }
    }
}

/// Everything `spm outdated` reports, collected once and rendered either as
/// human text or as the `--json` document. Both renderings share [`outcome`]
/// so the exit status never depends on the output format.
///
/// [`outcome`]: OutdatedReport::outcome
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OutdatedReport<'a> {
    pub(super) scope: &'static str,
    /// Locked skills, direct and transitive.
    pub(super) skills: Vec<Row<'a>>,
    pub(super) plugins: Vec<Row<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Row<'a> {
    /// Plugins are reported in their own array, so this only drives the text.
    #[serde(skip)]
    pub(super) plugin: bool,
    pub(super) name: &'a str,
    pub(super) git: &'a str,
    pub(super) path: Option<&'a str>,
    /// The locked selector; `null` if `ai.lock` holds an unrecognized reference.
    pub(super) selector: Option<Version>,
    /// For a `version` range: the tag it is locked to; `null` otherwise.
    pub(super) resolved_tag: Option<&'a str>,
    /// Full locked commit SHA.
    pub(super) locked: &'a str,
    /// Full SHA the selector resolves to on the remote now (branch tip, the
    /// pinned tag's current commit, or the newest tag in a `version` range);
    /// `null` for commits and errors.
    pub(super) remote_commit: Option<String>,
    /// For a `version` range: the newest tag that satisfies it now.
    pub(super) latest_in_range: Option<String>,
    /// Newest semver tag on the remote, only when newer than the pinned tag
    /// (for a `version` range: newer than `latestInRange`, i.e. outside it).
    pub(super) latest_tag: Option<String>,
    /// Full SHA `latestTag` points at.
    pub(super) latest_tag_commit: Option<String>,
    pub(super) status: Status,
    pub(super) outdated: bool,
    /// Resolved through another skill's `ai.json`: update it there, not here.
    pub(super) transitive: bool,
    pub(super) requested_by: &'a [DepName],
    /// How an upstream bump of this transitive skill reaches the project
    /// (read from the whole lockfile, so a name filter cannot change it).
    #[serde(skip)]
    pub(super) upgrade: Upgrade,
    /// The other selectors this deduplicated entry satisfies, each checked.
    pub(super) also_satisfies: Vec<SelectorCheck>,
    /// For a direct dependency whose `ai.json` request (git, selector or
    /// path) no longer matches its pin: what changed. The row still describes
    /// the pin, which the next `spm install` re-resolves, so nothing here says
    /// how to update the request as declared now.
    pub(super) drift: Option<String>,
    pub(super) error: Option<String>,
}

/// A secondary selector a deduplicated dependency was verified to satisfy
/// (`also_satisfies` in ai.lock). Each is checked against the locked commit,
/// so a moved tag or branch on a secondary edge cannot hide behind an
/// up-to-date primary selector.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SelectorCheck {
    /// `null` if `ai.lock` holds an unrecognized reference.
    pub(super) selector: Option<Version>,
    /// For a `version` range: the tag it is locked to; `null` otherwise.
    pub(super) resolved_tag: Option<String>,
    /// Full SHA the selector resolves to on the remote now; `null` for
    /// commits and errors.
    pub(super) remote_commit: Option<String>,
    /// For a `version` range: the newest tag that satisfies it now.
    pub(super) latest_in_range: Option<String>,
    /// Newest semver tag on the remote, only when newer than a tag selector
    /// (or than `latestInRange`).
    pub(super) latest_tag: Option<String>,
    /// Full SHA `latestTag` points at.
    pub(super) latest_tag_commit: Option<String>,
    /// Same values as a row's `status`.
    pub(super) status: Status,
    pub(super) error: Option<String>,
}

impl Row<'_> {
    pub(super) fn has_error(&self) -> bool {
        self.error.is_some() || self.also_satisfies.iter().any(|s| s.error.is_some())
    }
}

impl OutdatedReport<'_> {
    pub(super) fn rows(&self) -> impl Iterator<Item = &Row<'_>> {
        self.skills.iter().chain(&self.plugins)
    }

    /// The command's exit status, shared by the text and JSON renderings: a
    /// remote that could not be checked always fails; being outdated fails only
    /// with `--exit-code`.
    pub(super) fn outcome(&self, exit_code: bool) -> Result<()> {
        let errors = self.rows().filter(|r| r.has_error()).count();
        if errors > 0 {
            bail!("could not check {errors} {}", noun(errors));
        }
        let outdated = self.rows().filter(|r| r.outdated).count();
        if exit_code && outdated > 0 {
            bail!("{outdated} {} outdated", noun(outdated));
        }
        Ok(())
    }
}

pub(super) fn noun(n: usize) -> &'static str {
    if n == 1 {
        "dependency"
    } else {
        "dependencies"
    }
}
