use super::common::print_json;
use crate::git;
use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::{semver_of_tag, Manifest, Version};
use crate::scope::Scope;
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;

/// How a locked dependency compares to its remote.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Status {
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
    /// For a `version` range: the tag it is locked to; `null` otherwise.
    resolved_tag: Option<&'a str>,
    /// Full locked commit SHA.
    locked: &'a str,
    /// Full SHA the selector resolves to on the remote now (branch tip, the
    /// pinned tag's current commit, or the newest tag in a `version` range);
    /// `null` for commits and errors.
    remote_commit: Option<String>,
    /// For a `version` range: the newest tag that satisfies it now.
    latest_in_range: Option<String>,
    /// Newest semver tag on the remote, only when newer than the pinned tag
    /// (for a `version` range: newer than `latestInRange`, i.e. outside it).
    latest_tag: Option<String>,
    /// Full SHA `latestTag` points at.
    latest_tag_commit: Option<String>,
    status: Status,
    outdated: bool,
    /// Resolved through another skill's `ai.json`: update it there, not here.
    transitive: bool,
    requested_by: &'a [String],
    /// How an upstream bump of this transitive skill reaches the project
    /// (read from the whole lockfile, so a name filter cannot change it).
    #[serde(skip)]
    upgrade: Upgrade,
    /// The other selectors this deduplicated entry satisfies, each checked.
    also_satisfies: Vec<SelectorCheck>,
    error: Option<String>,
}

/// A secondary selector a deduplicated dependency was verified to satisfy
/// (`also_satisfies` in ai.lock). Each is checked against the locked commit,
/// so a moved tag or branch on a secondary edge cannot hide behind an
/// up-to-date primary selector.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectorCheck {
    /// `null` if `ai.lock` holds an unrecognized reference.
    selector: Option<Version>,
    /// For a `version` range: the tag it is locked to; `null` otherwise.
    resolved_tag: Option<String>,
    /// Full SHA the selector resolves to on the remote now; `null` for
    /// commits and errors.
    remote_commit: Option<String>,
    /// For a `version` range: the newest tag that satisfies it now.
    latest_in_range: Option<String>,
    /// Newest semver tag on the remote, only when newer than a tag selector
    /// (or than `latestInRange`).
    latest_tag: Option<String>,
    /// Full SHA `latestTag` points at.
    latest_tag_commit: Option<String>,
    /// Same values as a row's `status`.
    status: Status,
    error: Option<String>,
}

/// How an upstream selector change propagates up the requester chain to a
/// locally declared dependency. A requester pinned to a tag or commit only
/// sees the change once a new version of it is published *and* its own
/// selector is bumped by its requester, recursively; a branch-tracking
/// requester picks it up through `spm update`, and a pinned local dependency
/// through an `ai.json` edit plus `spm install`.
#[derive(Default, Debug, PartialEq)]
struct Upgrade {
    /// Pinned transitive requesters to republish, each with the requesters
    /// whose upstream `ai.json` must then bump it, in order up the chain.
    republish: Vec<(String, Vec<String>)>,
    /// Pinned, locally declared dependencies whose selector must change in
    /// this project's `ai.json`.
    local_edits: Vec<String>,
    /// Some chain ends at a branch-tracking requester (`spm update`).
    via_branch: bool,
}

impl Upgrade {
    fn plan(requested_by: &[String], lock: &Lockfile, manifest: &Manifest) -> Self {
        let mut up = Upgrade::default();
        let mut seen = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<&String> = requested_by.iter().collect();
        while let Some(req) = queue.pop_front() {
            if !seen.insert(req.clone()) {
                continue;
            }
            let locked = lock.skills.get(req).or_else(|| lock.plugins.get(req));
            let branch = locked.is_some_and(|q| {
                matches!(Version::from_label(&q.reference), Some(Version::Branch(_)))
            });
            let local = manifest.skills.contains_key(req) || manifest.plugins.contains_key(req);
            match locked {
                _ if branch => up.via_branch = true,
                Some(q) if !local && !q.requested_by.is_empty() => {
                    up.republish.push((req.clone(), q.requested_by.clone()));
                    queue.extend(&q.requested_by);
                }
                _ => up.local_edits.push(req.clone()),
            }
        }
        up
    }
}

impl Row<'_> {
    fn has_error(&self) -> bool {
        self.error.is_some() || self.also_satisfies.iter().any(|s| s.error.is_some())
    }
}

impl OutdatedReport<'_> {
    fn rows(&self) -> impl Iterator<Item = &Row<'_>> {
        self.skills.iter().chain(&self.plugins)
    }

    /// The command's exit status, shared by the text and JSON renderings: a
    /// remote that could not be checked always fails; being outdated fails only
    /// with `--exit-code`.
    fn outcome(&self, exit_code: bool) -> Result<()> {
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
    // Every remote this reports on is queried through the policy gate; a denied
    // source shows up as an error row instead of being contacted.
    crate::policy::activate(&manifest, &dir)?;
    let lock = Lockfile::load_or_default(&dir)?;
    if let Some(name) = name {
        if !lock.skills.contains_key(name) && !lock.plugins.contains_key(name) {
            bail!("`{name}` is not in ai.lock — run `spm install` first, or check the name");
        }
    }
    let wanted = |n: &str| name.is_none_or(|want| want == n);
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
                if transitive {
                    row.upgrade = Upgrade::plan(&l.requested_by, &lock, &manifest);
                }
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
    label: &str,
    git: &str,
    locked: &str,
    resolved_tag: Option<&str>,
    remotes: &mut Remotes,
) -> SelectorCheck {
    let selector = Version::from_label(label);
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

fn label_of(selector: &Option<Version>) -> String {
    selector.as_ref().map_or_else(|| "?".into(), Version::label)
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
            let range = matches!(r.selector, Some(Version::Range(_)));
            let selector = match (&r.selector, r.resolved_tag) {
                (Some(Version::Commit(c)), _) => format!("commit:{}", short(c)),
                (Some(v @ Version::Range(_)), Some(tag)) => format!("{} → {tag}", v.label()),
                (Some(v), _) => v.label(),
                (None, _) => "?".into(),
            };
            let latest = match (r.status, &r.latest_tag, &r.remote_commit) {
                (Status::NewerTag, Some(tag), _) => format!("tag:{tag}"),
                (Status::UpToDate, _, _) if matches!(r.selector, Some(Version::Tag(_))) => {
                    selector.clone()
                }
                (Status::UpToDate | Status::Behind, _, _) if range => r
                    .latest_in_range
                    .as_ref()
                    .map_or_else(|| "-".into(), |t| format!("tag:{t}")),
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
            match (r.status, &r.latest_tag) {
                (Status::TagMoved, Some(tag)) => notes.push(format!("newer tag: {tag}")),
                (Status::Behind, Some(tag)) if range => {
                    notes.push(format!("newer tag outside the range: {tag}"))
                }
                _ => {}
            }
            for sec in r.also_satisfies.iter().filter(|s| s.status.is_outdated()) {
                let newer = match (sec.status, &sec.latest_tag) {
                    (Status::NewerTag, Some(t)) => format!(" {t}"),
                    _ => String::new(),
                };
                notes.push(format!(
                    "also {}: {}{newer}",
                    label_of(&sec.selector),
                    sec.status.text()
                ));
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
            match (&r.selector, &r.remote_commit, r.resolved_tag) {
                (Some(Version::Tag(t)), Some(now), _) => eprintln!(
                    "warning: tag `{t}` of `{}` now points at {} but ai.lock pins {} — \
                     someone re-pointed the tag; review it before `spm update`",
                    r.name,
                    short(now),
                    short(r.locked)
                ),
                (Some(Version::Range(req)), _, Some(t)) => eprintln!(
                    "warning: tag `{t}` (locked for `version: {req}`) of `{}` no longer \
                     points at {} — someone re-pointed the tag; review it before `spm update`",
                    r.name,
                    short(r.locked)
                ),
                _ => {}
            }
        }
        for sec in &r.also_satisfies {
            let sel = label_of(&sec.selector);
            match (&sec.selector, sec.status, &sec.remote_commit) {
                (Some(Version::Tag(t)), Status::TagMoved, Some(now)) => eprintln!(
                    "warning: tag `{t}` (also satisfied by `{}`) now points at {} but ai.lock \
                     pins {} — someone re-pointed the tag; review it before `spm update`",
                    r.name,
                    short(now),
                    short(r.locked)
                ),
                (Some(Version::Range(req)), Status::TagMoved, _) => {
                    if let Some(t) = &sec.resolved_tag {
                        eprintln!(
                            "warning: tag `{t}` (locked for `version: {req}`, also satisfied by \
                             `{}`) no longer points at {} — someone re-pointed the tag; review \
                             it before `spm update`",
                            r.name,
                            short(r.locked)
                        )
                    }
                }
                (Some(Version::Range(_)), Status::Behind, _) => {
                    if let Some(t) = &sec.latest_in_range {
                        eprintln!(
                            "warning: `{sel}` (also satisfied by `{}`) has a newer tag {t} in \
                             range; ai.lock pins {}",
                            r.name,
                            short(r.locked)
                        )
                    }
                }
                (_, Status::Behind, Some(now)) => eprintln!(
                    "warning: `{sel}` (also satisfied by `{}`) moved to {} but ai.lock pins {}",
                    r.name,
                    short(now),
                    short(r.locked)
                ),
                _ => {}
            }
            if let Some(e) = &sec.error {
                eprintln!("error: `{}` ({sel}): {e}", r.name);
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
    let errors = report.rows().filter(|r| r.has_error()).count();
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
        let is_range = |sel: &Option<Version>| matches!(sel, Some(Version::Range(_)));
        // A range that is merely behind is the expected case, not a moved ref.
        let in_range = names(&|r| r.status == Status::Behind && is_range(&r.selector));
        let moved_status = |s: Status| matches!(s, Status::Behind | Status::TagMoved);
        let moved = names(&|r| {
            (moved_status(r.status) && !in_range.contains(&r.name))
                || r.also_satisfies.iter().any(|s| moved_status(s.status))
        });
        if !moved.is_empty() {
            lines.push(format!(
                "  {}: run `spm update{g}` to re-pin to the moved branch/tag",
                moved.join(", ")
            ));
        }
        if !in_range.is_empty() {
            lines.push(format!(
                "  {}: newer tag within the version range — run `spm update{g}` to move to it",
                in_range.join(", ")
            ));
        }
        let tagged =
            names(&|r| r.status == Status::NewerTag && !r.transitive && !is_range(&r.selector));
        if !tagged.is_empty() {
            lines.push(format!(
                "  {}: newer tag — change the tag in ai.json, then run `spm install{g}`",
                tagged.join(", ")
            ));
        }
        let widen =
            names(&|r| r.status == Status::NewerTag && !r.transitive && is_range(&r.selector));
        if !widen.is_empty() {
            lines.push(format!(
                "  {}: newer tag outside the version range — widen `version` in ai.json, \
                 then run `spm install{g}`",
                widen.join(", ")
            ));
        }
        for r in outdated
            .iter()
            .filter(|r| r.status == Status::NewerTag && r.transitive)
        {
            // `install` reuses every locked commit (and its old manifest), so
            // only `update` chases a branch-tracking requester; pinned ones
            // must be republished and re-pinned all the way up.
            let up = &r.upgrade;
            let mut steps = vec![format!(
                "bump it in the upstream ai.json of {}",
                r.requested_by.join(", ")
            )];
            for (pinned, parents) in &up.republish {
                steps.push(format!(
                    "publish a new {pinned} version and bump {pinned} in the upstream ai.json of {}",
                    parents.join(", ")
                ));
            }
            let run = if up.via_branch { "update" } else { "install" };
            steps.push(if up.local_edits.is_empty() {
                format!("then run `spm {run}{g}`")
            } else {
                format!(
                    "then point {} at a version that includes it in ai.json and run `spm {run}{g}`",
                    up.local_edits.join(", ")
                )
            });
            lines.push(format!("  {}: newer tag — {}", r.name, steps.join("; ")));
        }
        // A secondary selector comes from some requester's upstream ai.json
        // edge, never from this project's own entry for the name.
        for r in &outdated {
            for sec in r
                .also_satisfies
                .iter()
                .filter(|s| s.status == Status::NewerTag)
            {
                lines.push(format!(
                    "  {}: newer tag {} for {} — bump it in the upstream ai.json that requests \
                     it, then move that requester onto the change (`spm update{g}` if it \
                     tracks a branch)",
                    r.name,
                    sec.latest_tag.as_deref().unwrap_or("?"),
                    label_of(&sec.selector)
                ));
            }
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
    use super::{newest_tag, summary, OutdatedReport, Row, Status, Upgrade};
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
            resolved_tag: None,
            locked: "0",
            remote_commit: None,
            latest_in_range: None,
            latest_tag: None,
            latest_tag_commit: None,
            status,
            outdated: matches!(status, Status::Behind | Status::NewerTag | Status::TagMoved),
            transitive: !requested_by.is_empty(),
            requested_by,
            upgrade: Default::default(),
            also_satisfies: Vec::new(),
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
                    upgrade: Upgrade {
                        via_branch: true,
                        ..Default::default()
                    },
                    ..row("leaf", Status::NewerTag, &via)
                },
                Row {
                    upgrade: Upgrade {
                        local_edits: vec!["fixed".into()],
                        ..Default::default()
                    },
                    ..row("deep", Status::NewerTag, &pinned_via)
                },
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
                "  leaf: newer tag — bump it in the upstream ai.json of top; then run `spm update -g`",
                "  deep: newer tag — bump it in the upstream ai.json of fixed; then point fixed at a \
                 version that includes it in ai.json and run `spm install -g`",
                "incomplete: 1 dependency could not be checked (see the errors above)",
            ]
        );
    }

    #[test]
    fn summary_routes_range_rows_to_update_or_a_wider_range() {
        use crate::manifest::Version;
        let range = || Some(Version::Range("^1.2".into()));
        let report = OutdatedReport {
            scope: "project",
            skills: vec![
                Row {
                    selector: range(),
                    ..row("inr", Status::Behind, &[])
                },
                Row {
                    selector: range(),
                    ..row("out", Status::NewerTag, &[])
                },
                row("br", Status::Behind, &[]),
            ],
            plugins: vec![],
        };
        assert_eq!(
            summary(&report),
            [
                "3 dependencies outdated:",
                "  br: run `spm update` to re-pin to the moved branch/tag",
                "  inr: newer tag within the version range — run `spm update` to move to it",
                "  out: newer tag outside the version range — widen `version` in ai.json, \
                 then run `spm install`",
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
