//! The human rendering of `spm outdated`: the table, the warnings about
//! drifted requests and moved tags, and the closing summary of how to apply
//! each kind of change.

use super::report::{noun, OutdatedReport, Row, Status};
use crate::scope::Scope;
use crate::version::Version;

fn label_of(selector: &Option<Version>) -> String {
    selector.as_ref().map_or_else(|| "?".into(), Version::label)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

pub(super) fn render_text(report: &OutdatedReport, declared: usize) {
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
            if r.drift.is_some() {
                notes.push("ai.json changed".to_string());
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
        if let Some(changes) = &r.drift {
            eprintln!(
                "warning: `{}` in ai.json no longer matches ai.lock ({changes}); this reports the \
                 locked pin — run `spm install{}` to re-lock it first",
                r.name,
                scope_flag(report.scope)
            );
        }
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

/// The scope flag suggested commands carry: ` -g` in global scope.
fn scope_flag(scope: &str) -> &'static str {
    if scope == Scope::Global.label() {
        " -g"
    } else {
        ""
    }
}

/// The closing summary: what is outdated and how to apply each kind of
/// change. Moved refs are refreshed by `spm update`, but a newer tag is a
/// different selector, which only an edit to the declaring `ai.json` adopts.
fn summary(report: &OutdatedReport) -> Vec<String> {
    let g = scope_flag(report.scope);
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
            for (name, req) in &up.in_range {
                steps.push(format!(
                    "publish a new {name} release within its `version: {req}` range \
                     (one outside it needs that range widened)"
                ));
            }
            let run = if up.via_branch || !up.in_range.is_empty() {
                "update"
            } else {
                "install"
            };
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
                     tracks a branch or a `version` range)",
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
    use super::super::report::{OutdatedReport, Row, Status};
    use super::super::upgrade::Upgrade;
    use super::summary;
    use crate::ids::{dep, DepName};

    fn row<'a>(name: &'a str, status: Status, requested_by: &'a [DepName]) -> Row<'a> {
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
            drift: None,
            error: (status == Status::Error).then(|| "unreachable".to_string()),
        }
    }

    #[test]
    fn summary_routes_each_kind_of_change_to_the_command_that_applies_it() {
        let via = [dep("top")];
        let pinned_via = [dep("fixed")];
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
        use crate::version::Version;
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
    fn summary_ends_a_transitive_chain_at_a_range_tracking_requester() {
        let via = [dep("mid")];
        let report = OutdatedReport {
            scope: "project",
            skills: vec![Row {
                upgrade: Upgrade {
                    republish: vec![("mid".into(), vec!["top".into()])],
                    in_range: vec![("top".into(), "^1".into())],
                    ..Default::default()
                },
                ..row("leaf", Status::NewerTag, &via)
            }],
            plugins: vec![],
        };
        assert_eq!(
            summary(&report),
            [
                "1 dependency outdated:",
                "  leaf: newer tag — bump it in the upstream ai.json of mid; publish a new mid \
                 version and bump mid in the upstream ai.json of top; publish a new top release \
                 within its `version: ^1` range (one outside it needs that range widened); then \
                 run `spm update`",
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
