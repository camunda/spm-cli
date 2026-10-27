//! Comparing each locked selector with its remote: one `git ls-remote` per
//! repository, then the branch tip, the pinned tag's current commit or the
//! newest tag in a `version` range against the locked commit.

use super::report::{Row, SelectorCheck, Status};
use super::upgrade::Upgrade;
use crate::git;
use crate::ids::LockReference;
use crate::lockfile::LockedSkill;
use crate::version::{semver_of_tag, Version};
use std::collections::BTreeMap;

/// Remote refs (`refs/heads/*` + `refs/tags/*`), fetched once per repository
/// URL so a monorepo of skills costs a single `ls-remote`. Failures are cached
/// too, so an unreachable remote is reported on each of its rows without being
/// retried.
#[derive(Default)]
pub(super) struct Remotes(BTreeMap<String, Result<BTreeMap<String, String>, String>>);

impl Remotes {
    fn get(&mut self, url: &str) -> Result<&BTreeMap<String, String>, String> {
        self.0
            .entry(url.to_string())
            .or_insert_with(|| {
                git::remote_refs(url, &["refs/heads/*", "refs/tags/*"])
                    .map_err(|e| format!("{e:#}"))
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

pub(super) fn check<'a>(
    name: &'a str,
    l: &'a LockedSkill,
    plugin: bool,
    transitive: bool,
    remotes: &mut Remotes,
) -> Row<'a> {
    let mut row = Row {
        plugin,
        name,
        git: &l.git,
        path: l.path.as_deref(),
        selector: None,
        resolved_tag: l.resolved_tag.as_deref(),
        locked: &l.commit,
        remote_commit: None,
        latest_in_range: None,
        latest_tag: None,
        latest_tag_commit: None,
        status: Status::Error,
        outdated: false,
        transitive,
        requested_by: &l.requested_by,
        upgrade: Upgrade::default(),
        also_satisfies: Vec::new(),
        drift: None,
        error: None,
    };
    let primary = evaluate(
        &l.reference,
        &l.git,
        &l.commit,
        l.resolved_tag.as_deref(),
        remotes,
    );
    row.remote_commit = primary.remote_commit;
    row.latest_in_range = primary.latest_in_range;
    row.latest_tag = primary.latest_tag;
    row.latest_tag_commit = primary.latest_tag_commit;
    row.status = primary.status;
    row.error = primary.error;
    row.also_satisfies = l
        .also_satisfies
        .iter()
        .map(|s| {
            evaluate(
                &s.reference,
                &l.git,
                &l.commit,
                s.resolved_tag.as_deref(),
                remotes,
            )
        })
        .collect();
    row.outdated =
        row.status.is_outdated() || row.also_satisfies.iter().any(|s| s.status.is_outdated());
    row.selector = primary.selector;
    row
}

/// Compare one selector (the primary `reference` or an `also_satisfies`
/// entry) with the locked commit. Primary and secondary selectors share this
/// so neither can miss a check the other gets. `resolved_tag` is the tag a
/// `version` range is locked to.
fn evaluate(
    reference: &LockReference,
    git: &str,
    locked: &str,
    resolved_tag: Option<&str>,
    remotes: &mut Remotes,
) -> SelectorCheck {
    let label = &reference.label();
    let selector = Some(reference.version().clone());
    let mut out = SelectorCheck {
        selector: None,
        resolved_tag: resolved_tag.map(str::to_string),
        remote_commit: None,
        latest_in_range: None,
        latest_tag: None,
        latest_tag_commit: None,
        status: Status::Error,
        error: None,
    };
    // The source policy is judged here, for every selector kind: a commit pin
    // never reaches the remote query (where the transport gate sits), so
    // without this a denied commit-pinned source would report `pinned`.
    let checked = crate::policy::gate(git)
        .map_err(|e| format!("{e:#}"))
        .and_then(|()| {
            compare(
                &mut out,
                selector.as_ref(),
                label,
                Locked {
                    git,
                    commit: locked,
                    resolved_tag,
                },
                remotes,
            )
        });
    if let Err(e) = checked {
        out.status = Status::Error;
        out.error = Some(e);
    }
    out.selector = selector;
    out
}

/// The commit a branch/tag/range selector resolves to on the remote now
/// (`None` for a commit, which is never queried).
fn remote_commit_of(
    selector: Option<&Version>,
    reference: &str,
    git: &str,
    remotes: &mut Remotes,
) -> Result<Option<String>, String> {
    match selector {
        None => Err(format!("unrecognized reference `{reference}` in ai.lock")),
        Some(Version::Commit(_)) => Ok(None),
        Some(Version::Branch(b)) => remotes
            .get(git)?
            .get(&format!("refs/heads/{b}"))
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("branch `{b}` not found on the remote")),
        Some(Version::Tag(t)) => remotes
            .get(git)?
            .get(&format!("refs/tags/{t}"))
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("tag `{t}` no longer exists on the remote")),
        Some(Version::Range(r)) => {
            in_range(remotes.get(git)?, r).map(|(_, sha)| Some(sha.to_string()))
        }
    }
}

/// The newest tag (and its commit) satisfying range `r`, as `spm update`
/// would resolve it — the very same rule, so the two never disagree.
fn in_range<'r>(refs: &'r BTreeMap<String, String>, r: &str) -> Result<(&'r str, &'r str), String> {
    crate::resolver::highest_matching_tag(refs, r).map_err(|e| format!("{e:#}"))
}

/// What `ai.lock` pins for the selector being compared.
struct Locked<'a> {
    git: &'a str,
    commit: &'a str,
    /// The tag a `version` range is locked to, when known.
    resolved_tag: Option<&'a str>,
}

fn compare(
    out: &mut SelectorCheck,
    selector: Option<&Version>,
    reference: &str,
    locked: Locked,
    remotes: &mut Remotes,
) -> Result<(), String> {
    let Locked {
        git,
        commit: locked,
        resolved_tag,
    } = locked;
    let now = remote_commit_of(selector, reference, git, remotes)?;
    // The tag newer versions are measured from: the pinned tag, or the newest
    // tag inside a range (so `latestTag` is what lies *outside* the range).
    let baseline = match selector {
        Some(Version::Tag(t)) => Some(t.clone()),
        Some(Version::Range(r)) => {
            let (tag, _) = in_range(remotes.get(git)?, r)?;
            out.latest_in_range = Some(tag.to_string());
            Some(tag.to_string())
        }
        _ => None,
    };
    if let Some(base) = baseline {
        if let Some((tag, sha)) = newest_tag(remotes.get(git)?, &base) {
            out.latest_tag = Some(tag.to_string());
            out.latest_tag_commit = Some(sha.to_string());
        }
    }
    // The tag a range is locked to was re-pointed since it was resolved. A
    // deleted one is an error (as for an exact tag): comparing tag names alone
    // would call an older surviving match "newer" and steer into a downgrade.
    let range_tag_moved = match (selector, resolved_tag) {
        (Some(Version::Range(r)), Some(t)) => {
            match remotes.get(git)?.get(&format!("refs/tags/{t}")) {
                Some(sha) => !sha.eq_ignore_ascii_case(locked),
                None => {
                    return Err(format!(
                        "tag `{t}` (locked for `version: {r}`) no longer exists on the remote"
                    ))
                }
            }
        }
        _ => false,
    };
    // A newer in-range tag can share the locked commit; `spm update` would
    // still advance `resolved_tag`, so comparing commits alone misses it.
    let range_tag_advanced = matches!(selector, Some(Version::Range(_)))
        && resolved_tag.is_some_and(|t| out.latest_in_range.as_deref() != Some(t));
    out.status = match (selector, &now) {
        (_, None) => Status::Pinned,
        (Some(Version::Tag(_)), Some(now)) if !now.eq_ignore_ascii_case(locked) => Status::TagMoved,
        (Some(Version::Range(_)), _) if range_tag_moved => Status::TagMoved,
        (Some(Version::Range(_)), _) if range_tag_advanced => Status::Behind,
        (_, Some(now)) if !now.eq_ignore_ascii_case(locked) => Status::Behind,
        _ if out.latest_tag.is_some() => Status::NewerTag,
        _ => Status::UpToDate,
    };
    out.remote_commit = now;
    Ok(())
}

/// The newest semver tag in `refs` (name and commit) that is strictly newer
/// than `pinned`, or `None` when `pinned` is already newest or is not semver.
/// Pre-releases are only considered when the pinned tag is itself one, so a
/// stable pin is never nudged towards an `-rc`.
fn newest_tag<'r>(refs: &'r BTreeMap<String, String>, pinned: &str) -> Option<(&'r str, &'r str)> {
    let current = semver_of_tag(pinned)?;
    let allow_pre = !current.pre.is_empty();
    refs.iter()
        .filter_map(|(r, sha)| {
            let tag = r.strip_prefix("refs/tags/")?;
            let v = semver_of_tag(tag)?;
            (allow_pre || v.pre.is_empty()).then_some((v, tag, sha.as_str()))
        })
        // SemVer precedence ignores build metadata (`1.2.3+b2` == `1.2.3`).
        .max_by(|a, b| a.0.cmp_precedence(&b.0))
        .filter(|(v, _, _)| v.cmp_precedence(&current).is_gt())
        .map(|(_, tag, sha)| (tag, sha))
}

#[cfg(test)]
mod tests {
    use super::newest_tag;
    use std::collections::BTreeMap;

    fn refs(tags: &[&str]) -> BTreeMap<String, String> {
        tags.iter()
            .map(|t| (format!("refs/tags/{t}"), format!("sha-{t}")))
            .collect()
    }

    #[test]
    fn newest_tag_uses_semver_order_and_ignores_non_semver() {
        let r = refs(&["v1.2.0", "v1.10.0", "v1.9.0", "v1", "latest", "v2.0.0-rc.1"]);
        assert_eq!(newest_tag(&r, "v1.2.0"), Some(("v1.10.0", "sha-v1.10.0")));
        assert_eq!(newest_tag(&r, "v1.10.0"), None, "already newest");
        assert_eq!(newest_tag(&r, "v1"), None, "non-semver pin is not compared");
    }

    #[test]
    fn newest_tag_considers_prereleases_only_for_prerelease_pins() {
        let r = refs(&["v2.0.0-rc.1", "v2.0.0-rc.2", "v1.0.0"]);
        assert_eq!(newest_tag(&r, "v1.0.0"), None);
        assert_eq!(
            newest_tag(&r, "v2.0.0-rc.1"),
            Some(("v2.0.0-rc.2", "sha-v2.0.0-rc.2"))
        );
        // Unprefixed tags are semver too.
        assert_eq!(
            newest_tag(&refs(&["1.0.0", "1.1.0"]), "1.0.0"),
            Some(("1.1.0", "sha-1.1.0"))
        );
    }

    #[test]
    fn newest_tag_ignores_build_metadata_only_differences() {
        let r = refs(&["v1.2.3", "v1.2.3+build.2"]);
        assert_eq!(newest_tag(&r, "v1.2.3"), None);
        assert_eq!(newest_tag(&r, "v1.2.3+build.1"), None);
    }
}
