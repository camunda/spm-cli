use super::common::render_report;
use super::index::DependencyIndex;
use super::session::{Access, ScopeSession};
use crate::ids::DepName;
use crate::links::Links;
use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::{DependencyKind, Manifest};
use crate::scope::Scope;
use crate::version::Version;
use anyhow::Result;
use serde::Serialize;

/// Everything `spm list` reports, collected once and rendered either as human
/// text or as the `--json` document.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListReport<'a> {
    scope: &'static str,
    /// Direct skills (manifest order) followed by transitive ones.
    skills: Vec<SkillRow<'a>>,
    plugins: Vec<PluginRow<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillRow<'a> {
    name: &'a str,
    git: &'a str,
    path: Option<&'a str>,
    selector: Option<Version>,
    /// For a `version` range: the semver tag it is locked to; `null` otherwise
    /// or when `ai.lock` has no pin.
    resolved_tag: Option<&'a str>,
    /// Full locked commit SHA; `null` when `ai.lock` has no pin.
    commit: Option<&'a str>,
    installed: bool,
    transitive: bool,
    requested_by: &'a [DepName],
    /// The local directory a `spm link` override materializes this skill
    /// from instead of its locked commit; `null` when not linked.
    linked: Option<&'a str>,
    #[serde(skip)]
    lock: Option<&'a LockedSkill>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginRow<'a> {
    name: &'a str,
    git: &'a str,
    path: Option<&'a str>,
    selector: Option<Version>,
    resolved_tag: Option<&'a str>,
    commit: Option<&'a str>,
    installed: bool,
    bundled_skills: &'a [DepName],
    /// The local directory a `spm link` override materializes this plugin
    /// from instead of its locked commit; `null` when not linked.
    linked: Option<&'a str>,
    #[serde(skip)]
    lock: Option<&'a LockedSkill>,
}

pub(super) fn list(scope: &Scope, json: bool) -> Result<()> {
    let session = ScopeSession::open(scope, Access::Read)?;
    let manifest = session.manifest()?;
    let lock = session.lockfile()?;
    let links = session.links()?;
    let report = collect(scope, &manifest, &lock, &links)?;
    render_report(&report, json, render_text)
}

fn collect<'a>(
    scope: &Scope,
    manifest: &'a Manifest,
    lock: &'a Lockfile,
    links: &'a Links,
) -> Result<ListReport<'a>> {
    let linked = |name: &str| links.links.get(name).map(String::as_str);
    let mut report = ListReport {
        scope: scope.label(),
        skills: Vec::new(),
        plugins: Vec::new(),
    };
    if !manifest.declares_any() {
        return Ok(report);
    }
    let index = DependencyIndex::new(manifest, lock);
    for d in index.direct(DependencyKind::Skill) {
        report.skills.push(SkillRow {
            name: d.name,
            git: &d.spec.git,
            path: d.spec.path.as_deref(),
            selector: Some(d.spec.version()?),
            resolved_tag: d.locked.and_then(|l| l.resolved_tag.as_deref()),
            commit: d.locked.map(|l| l.commit.as_str()),
            installed: d.locked.is_some(),
            transitive: false,
            requested_by: &[],
            linked: linked(d.name),
            lock: d.locked,
        });
    }
    // Transitively-resolved skills live in `ai.lock` but not the manifest; list
    // them under their direct requester(s) so the dependency graph is visible.
    for t in index.transitive() {
        let l = t.locked;
        report.skills.push(SkillRow {
            name: t.name,
            git: &l.git,
            path: l.path.as_deref(),
            selector: Some(l.reference.version().clone()),
            resolved_tag: l.resolved_tag.as_deref(),
            commit: Some(&l.commit),
            installed: true,
            transitive: true,
            requested_by: &l.requested_by,
            linked: None,
            lock: Some(l),
        });
    }
    for d in index.direct(DependencyKind::Plugin) {
        report.plugins.push(PluginRow {
            name: d.name,
            git: &d.spec.git,
            path: d.spec.path.as_deref(),
            selector: Some(d.spec.version()?),
            resolved_tag: d.locked.and_then(|l| l.resolved_tag.as_deref()),
            commit: d.locked.map(|l| l.commit.as_str()),
            installed: d.locked.is_some(),
            bundled_skills: d.locked.map(|l| l.bundled_skills.as_slice()).unwrap_or(&[]),
            linked: linked(d.name),
            lock: d.locked,
        });
    }
    Ok(report)
}

/// `; linked → <dir>` for an entry with an active `spm link` override.
fn link_note(linked: Option<&str>) -> String {
    linked
        .map(|d| format!("; linked → {d}"))
        .unwrap_or_default()
}

fn render_text(report: &ListReport) {
    if report.skills.is_empty() && report.plugins.is_empty() {
        println!("no skills or plugins declared");
        return;
    }
    for row in report.skills.iter().filter(|r| !r.transitive) {
        let pinned = row
            .lock
            .map(|l| format!("{} @ {}", l.display_reference(), l.commit.short()))
            .unwrap_or_else(|| "not installed".into());
        println!(
            "{:<24} {}  ({pinned}{})",
            row.name,
            row.git,
            link_note(row.linked)
        );
    }
    let mut transitive = report
        .skills
        .iter()
        .filter_map(|r| r.lock.filter(|_| r.transitive).map(|l| (r, l)))
        .peekable();
    if transitive.peek().is_some() {
        println!("\ntransitive skills:");
        for (row, l) in transitive {
            println!(
                "{:<24} {}  ({} @ {}; via {})",
                row.name,
                row.git,
                l.display_reference(),
                l.commit.short(),
                row.requested_by.join(", ")
            );
        }
    }
    for row in &report.plugins {
        let pinned = row
            .lock
            .map(|l| {
                if l.bundled_skills.is_empty() {
                    format!("{} @ {}", l.display_reference(), l.commit.short())
                } else {
                    format!(
                        "{} @ {}, skills: {}",
                        l.display_reference(),
                        l.commit.short(),
                        l.bundled_skills.join(", ")
                    )
                }
            })
            .unwrap_or_else(|| "not installed".into());
        println!(
            "{:<24} {}  (plugin; {pinned}{})",
            row.name,
            row.git,
            link_note(row.linked)
        );
    }
}
