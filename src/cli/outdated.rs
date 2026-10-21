use super::common::print_json;
use crate::git;
use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::{Manifest, Version};
use crate::scope::Scope;
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;

/// How a locked dependency compares to its remote.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Status {
    UpToDate,
    /// The branch tip moved past the locked commit.
    Behind,
    /// A newer semver tag than the pinned one exists on the remote.
    NewerTag,
    /// The pinned tag itself now resolves to a different commit.
    TagMoved,
    /// A commit selector: immutable, never queried.
    Pinned,
    /// The remote could not be checked; see `error`.
    Error,
}

impl Status {
    fn is_outdated(self) -> bool {
        matches!(self, Status::Behind | Status::NewerTag | Status::TagMoved)
    }

    fn text(self) -> &'static str {
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
struct OutdatedReport<'a> {
    scope: &'static str,
    /// Locked skills, direct and transitive.
    skills: Vec<Row<'a>>,
    plugins: Vec<Row<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Row<'a> {
    /// Plugins are reported in their own array, so this only drives the text.
    #[serde(skip)]
    plugin: bool,
    name: &'a str,
    git: &'a str,
    path: Option<&'a str>,
    /// The locked selector; `null` if `ai.lock` holds an unrecognized reference.
    selector: Option<Version>,
    /// Full locked commit SHA.
    locked: &'a str,
    /// Full SHA the selector resolves to on the remote now (branch tip, or the
    /// pinned tag's current commit); `null` for commits and errors.
    remote_commit: Option<String>,
    /// Newest semver tag on the remote, only when newer than the pinned tag.
    latest_tag: Option<String>,
    /// Full SHA `latestTag` points at.
    latest_tag_commit: Option<String>,
    status: Status,
    outdated: bool,
    /// Resolved through another skill's `ai.json`: update it there, not here.
    transitive: bool,
    requested_by: &'a [String],
    /// Every requester tracks a branch (read from the whole lockfile, so a
    /// name filter cannot change it): an upstream bump then arrives through
    /// `spm update`, whereas a pinned requester must be re-pinned.
    #[serde(skip)]
    requesters_track_branch: bool,
    error: Option<String>,
}

impl OutdatedReport<'_> {
    fn rows(&self) -> impl Iterator<Item = &Row<'_>> {
        self.skills.iter().chain(&self.plugins)
    }

    /// The command's exit status, shared by the text and JSON renderings: a
    /// remote that could not be checked always fails; being outdated fails only
    /// with `--exit-code`.
    fn outcome(&self, exit_code: bool) -> Result<()> {
        let errors = self.rows().filter(|r| r.status == Status::Error).count();
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

fn noun(n: usize) -> &'static str {
    if n == 1 {
        "dependency"
    } else {
        "dependencies"
    }
}

/// Report which locked dependencies have newer upstream versions. Strictly
/// read-only: it only runs `git ls-remote` (once per repository) and never
/// writes `ai.json`/`ai.lock`, fetches into the store or touches vendor dirs.
pub(super) fn outdated(
    scope: &Scope,
    name: Option<&str>,
    json: bool,
    exit_code: bool,
) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir, scope)?;
    let lock = Lockfile::load_or_default(&dir)?;
    if let Some(name) = name {
        if !lock.skills.contains_key(name) && !lock.plugins.contains_key(name) {
            bail!("`{name}` is not in ai.lock — run `spm install` first, or check the name");
        }
    }
    let wanted = |n: &str| name.is_none_or(|want| want == n);
    let tracks_branch = |l: &LockedSkill| {
        l.requested_by.iter().all(|req| {
            lock.skills
                .get(req)
                .or_else(|| lock.plugins.get(req))
                .is_some_and(|q| {
                    matches!(Version::from_label(&q.reference), Some(Version::Branch(_)))
                })
        })
    };
    let mut remotes = Remotes::default();
    let report = OutdatedReport {
        scope: scope.label(),
        skills: lock
            .skills
            .iter()
            .filter(|(n, _)| wanted(n))
            .map(|(n, l)| {
                let transitive = !l.requested_by.is_empty() && !manifest.skills.contains_key(n);
                let mut row = check(n, l, false, transitive, &mut remotes);
                row.requesters_track_branch = tracks_branch(l);
                row
            })
            .collect(),
        plugins: lock
            .plugins
            .iter()
            .filter(|(n, _)| wanted(n))
            .map(|(n, l)| check(n, l, true, false, &mut remotes))
            .collect(),
    };
    if json {
        print_json(&report)?;
    } else {
        render_text(&report, manifest.skills.len() + manifest.plugins.len());
    }
    report.outcome(exit_code)
}

/// Remote refs (`refs/heads/*` + `refs/tags/*`), fetched once per repository
/// URL so a monorepo of skills costs a single `ls-remote`. Failures are cached
/// too, so an unreachable remote is reported on each of its rows without being
/// retried.
#[derive(Default)]
struct Remotes(BTreeMap<String, Result<BTreeMap<String, String>, String>>);

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

fn check<'a>(
    name: &'a str,
    l: &'a LockedSkill,
    plugin: bool,
    transitive: bool,
    remotes: &mut Remotes,
) -> Row<'a> {
    let selector = Version::from_label(&l.reference);
    let mut row = Row {
        plugin,
        name,
        git: &l.git,
        path: l.path.as_deref(),
        selector: None,
        locked: &l.commit,
        remote_commit: None,
        latest_tag: None,
        latest_tag_commit: None,
        status: Status::Error,
        outdated: false,
        transitive,
        requested_by: &l.requested_by,
        requesters_track_branch: false,
        error: None,
    };
    if let Err(e) = compare(&mut row, selector.as_ref(), &l.reference, remotes) {
        row.status = Status::Error;
        row.error = Some(e);
    }
    row.outdated = row.status.is_outdated();
    row.selector = selector;
    row
}

fn compare(
    row: &mut Row,
    selector: Option<&Version>,
    reference: &str,
    remotes: &mut Remotes,
) -> Result<(), String> {
    let locked = row.locked;
    match selector {
        None => Err(format!("unrecognized reference `{reference}` in ai.lock")),
        Some(Version::Commit(_)) => {
            row.status = Status::Pinned;
            Ok(())
        }
        Some(Version::Branch(b)) => {
            let refs = remotes.get(row.git)?;
            let tip = refs
                .get(&format!("refs/heads/{b}"))
                .ok_or_else(|| format!("branch `{b}` not found on the remote"))?;
            row.status = if tip.eq_ignore_ascii_case(locked) {
                Status::UpToDate
            } else {
                Status::Behind
            };
            row.remote_commit = Some(tip.clone());
            Ok(())
        }
        Some(Version::Tag(t)) => {
            let refs = remotes.get(row.git)?;
            let now = refs
                .get(&format!("refs/tags/{t}"))
                .ok_or_else(|| format!("tag `{t}` no longer exists on the remote"))?;
            if let Some((tag, sha)) = newest_tag(refs, t) {
                row.latest_tag = Some(tag.to_string());
                row.latest_tag_commit = Some(sha.to_string());
            }
            row.status = if !now.eq_ignore_ascii_case(locked) {
                Status::TagMoved
            } else if row.latest_tag.is_some() {
                Status::NewerTag
            } else {
                Status::UpToDate
            };
            row.remote_commit = Some(now.clone());
            Ok(())
        }
    }
}

/// Parse a tag as strict semver, allowing a leading `v` (`v1.2.0`). Floating
/// or non-semver tags (`v1`, `latest`, `stable`) yield `None`.
fn semver_of(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// The newest semver tag in `refs` (name and commit) that is strictly newer
/// than `pinned`, or `None` when `pinned` is already newest or is not semver.
/// Pre-releases are only considered when the pinned tag is itself one, so a
/// stable pin is never nudged towards an `-rc`.
fn newest_tag<'r>(refs: &'r BTreeMap<String, String>, pinned: &str) -> Option<(&'r str, &'r str)> {
    let current = semver_of(pinned)?;
    let allow_pre = !current.pre.is_empty();
    refs.iter()
        .filter_map(|(r, sha)| {
            let tag = r.strip_prefix("refs/tags/")?;
            let v = semver_of(tag)?;
            (allow_pre || v.pre.is_empty()).then_some((v, tag, sha.as_str()))
        })
        // SemVer precedence ignores build metadata (`1.2.3+b2` == `1.2.3`).
        .max_by(|a, b| a.0.cmp_precedence(&b.0))
        .filter(|(v, _, _)| v.cmp_precedence(&current).is_gt())
        .map(|(_, tag, sha)| (tag, sha))
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn render_text(report: &OutdatedReport, declared: usize) {
    if report.skills.is_empty() && report.plugins.is_empty() {
        if declared == 0 {
            println!("no skills or plugins declared");
        } else {
            println!("nothing locked yet — run `spm install`");
        }
        return;
    }
    let rows: Vec<[String; 6]> = report
        .rows()
        .map(|r| {
            let selector = match &r.selector {
                Some(Version::Commit(c)) => format!("commit:{}", short(c)),
                Some(v) => v.label(),
                None => "?".into(),
            };
            let latest = match (r.status, &r.latest_tag, &r.remote_commit) {
                (Status::NewerTag, Some(tag), _) => format!("tag:{tag}"),
                (Status::UpToDate, _, _) if matches!(r.selector, Some(Version::Tag(_))) => {
                    selector.clone()
                }
                (_, _, Some(sha)) => short(sha).to_string(),
                _ => "-".into(),
            };
            let mut notes = Vec::new();
            if r.plugin {
                notes.push("plugin".to_string());
            }
            if r.transitive {
                notes.push(format!("transitive; via {}", r.requested_by.join(", ")));
            }
            if let (Status::TagMoved, Some(tag)) = (r.status, &r.latest_tag) {
                notes.push(format!("newer tag: {tag}"));
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!("({})", notes.join("; "))
            };
            [
                r.name.to_string(),
                selector,
                short(r.locked).to_string(),
                latest,
                r.status.text().to_string(),
                notes,
            ]
        })
        .collect();
    let header = ["name", "selector", "locked", "latest", "status", ""].map(String::from);
    let mut widths = [0usize; 6];
    for line in std::iter::once(&header).chain(&rows) {
        for (w, cell) in widths.iter_mut().zip(line) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for line in std::iter::once(&header).chain(&rows) {
        let cells: Vec<String> = line
            .iter()
            .zip(widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        println!("{}", cells.join("  ").trim_end());
    }

    for r in report.rows() {
        if r.status == Status::TagMoved {
            if let (Some(Version::Tag(t)), Some(now)) = (&r.selector, &r.remote_commit) {
                eprintln!(
                    "warning: tag `{t}` of `{}` now points at {} but ai.lock pins {} — \
                     someone re-pointed the tag; review it before `spm update`",
                    r.name,
                    short(now),
                    short(r.locked)
                );
            }
        }
        if let Some(e) = &r.error {
            eprintln!("error: `{}`: {e}", r.name);
        }
    }
    println!();
    for line in summary(report) {
        println!("{line}");
    }
}

/// The closing summary: what is outdated and how to apply each kind of
/// change. Moved refs are refreshed by `spm update`, but a newer tag is a
/// different selector, which only an edit to the declaring `ai.json` adopts.
fn summary(report: &OutdatedReport) -> Vec<String> {
    let g = if report.scope == Scope::Global.label() {
        " -g"
    } else {
        ""
    };
    let errors = report.rows().filter(|r| r.error.is_some()).count();
    let outdated: Vec<&Row> = report.rows().filter(|r| r.outdated).collect();
    let mut lines = Vec::new();
    if outdated.is_empty() {
        lines.push(if errors == 0 {
            "everything is up to date".to_string()
        } else {
            "everything that could be checked is up to date".to_string()
        });
    } else {
        lines.push(format!(
            "{} {} outdated:",
            outdated.len(),
            noun(outdated.len())
        ));
        let names = |pick: &dyn Fn(&Row) -> bool| -> Vec<&str> {
            outdated
                .iter()
                .filter(|r| pick(r))
                .map(|r| r.name)
                .collect()
        };
        let moved = names(&|r| r.status != Status::NewerTag);
        if !moved.is_empty() {
            lines.push(format!(
                "  {}: run `spm update{g}` to re-pin to the moved branch/tag",
                moved.join(", ")
            ));
        }
        let tagged = names(&|r| r.status == Status::NewerTag && !r.transitive);
        if !tagged.is_empty() {
            lines.push(format!(
                "  {}: newer tag — change the tag in ai.json, then run `spm install{g}`",
                tagged.join(", ")
            ));
        }
        for r in outdated
            .iter()
            .filter(|r| r.status == Status::NewerTag && r.transitive)
        {
            // `install` reuses a requester's locked commit (and its old
            // manifest), so only a branch-tracking requester picks up an
            // upstream bump via `update`; a pinned one must be re-pinned.
            let apply = if r.requesters_track_branch {
                format!("then run `spm update{g}`")
            } else {
                format!(
                    "then point {} at a version that includes it and run `spm install{g}`",
                    r.requested_by.join(", ")
                )
            };
            lines.push(format!(
                "  {}: newer tag — bump it in the upstream ai.json of {}, {apply}",
                r.name,
                r.requested_by.join(", ")
            ));
        }
    }
    if errors > 0 {
        lines.push(format!(
            "incomplete: {errors} {} could not be checked (see the errors above)",
            noun(errors)
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{newest_tag, summary, OutdatedReport, Row, Status};
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

    fn row<'a>(name: &'a str, status: Status, requested_by: &'a [String]) -> Row<'a> {
        Row {
            plugin: false,
            name,
            git: "g",
            path: None,
            selector: None,
            locked: "0",
            remote_commit: None,
            latest_tag: None,
            latest_tag_commit: None,
            status,
            outdated: matches!(status, Status::Behind | Status::NewerTag | Status::TagMoved),
            transitive: !requested_by.is_empty(),
            requested_by,
            requesters_track_branch: false,
            error: (status == Status::Error).then(|| "unreachable".to_string()),
        }
    }

    #[test]
    fn summary_routes_each_kind_of_change_to_the_command_that_applies_it() {
        let via = ["top".to_string()];
        let pinned_via = ["fixed".to_string()];
        let report = OutdatedReport {
            scope: "global",
            skills: vec![
                row("br", Status::Behind, &[]),
                row("moved", Status::TagMoved, &[]),
                row("tg", Status::NewerTag, &[]),
                Row {
                    requesters_track_branch: true,
                    ..row("leaf", Status::NewerTag, &via)
                },
                row("deep", Status::NewerTag, &pinned_via),
                row("gone", Status::Error, &[]),
            ],
            plugins: vec![],
        };
        assert_eq!(
            summary(&report),
            [
                "5 dependencies outdated:",
                "  br, moved: run `spm update -g` to re-pin to the moved branch/tag",
                "  tg: newer tag — change the tag in ai.json, then run `spm install -g`",
                "  leaf: newer tag — bump it in the upstream ai.json of top, then run `spm update -g`",
                "  deep: newer tag — bump it in the upstream ai.json of fixed, then point fixed at a \
                 version that includes it and run `spm install -g`",
                "incomplete: 1 dependency could not be checked (see the errors above)",
            ]
        );
    }

    #[test]
    fn summary_never_claims_success_when_a_row_errored() {
        let report = OutdatedReport {
            scope: "project",
            skills: vec![
                row("ok", Status::UpToDate, &[]),
                row("gone", Status::Error, &[]),
            ],
            plugins: vec![],
        };
        let lines = summary(&report);
        assert!(
            !lines.contains(&"everything is up to date".to_string()),
            "{lines:?}"
        );
        assert_eq!(
            lines.last().unwrap(),
            "incomplete: 1 dependency could not be checked (see the errors above)"
        );
    }
}
