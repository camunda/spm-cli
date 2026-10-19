use super::common::print_json;
use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::{Manifest, Version};
use crate::scope::Scope;
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
    /// Full locked commit SHA; `null` when `ai.lock` has no pin.
    commit: Option<&'a str>,
    installed: bool,
    transitive: bool,
    requested_by: &'a [String],
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
    commit: Option<&'a str>,
    installed: bool,
    bundled_skills: &'a [String],
    #[serde(skip)]
    lock: Option<&'a LockedSkill>,
}

pub(super) fn list(scope: &Scope, json: bool) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir, scope)?;
    let lock = Lockfile::load_or_default(&dir)?;
    let report = collect(scope, &manifest, &lock)?;
    if json {
        print_json(&report)
    } else {
        render_text(&report);
        Ok(())
    }
}

fn collect<'a>(
    scope: &Scope,
    manifest: &'a Manifest,
    lock: &'a Lockfile,
) -> Result<ListReport<'a>> {
    let mut report = ListReport {
        scope: scope.label(),
        skills: Vec::new(),
        plugins: Vec::new(),
    };
    if manifest.skills.is_empty() && manifest.plugins.is_empty() {
        return Ok(report);
    }
    for (name, spec) in &manifest.skills {
        let l = lock.skills.get(name);
        report.skills.push(SkillRow {
            name,
            git: &spec.git,
            path: spec.path.as_deref(),
            selector: Some(spec.version()?),
            commit: l.map(|l| l.commit.as_str()),
            installed: l.is_some(),
            transitive: false,
            requested_by: &[],
            lock: l,
        });
    }
    // Transitively-resolved skills live in `ai.lock` but not the manifest; list
    // them under their direct requester(s) so the dependency graph is visible.
    for (name, l) in &lock.skills {
        if l.requested_by.is_empty() || manifest.skills.contains_key(name) {
            continue;
        }
        report.skills.push(SkillRow {
            name,
            git: &l.git,
            path: l.path.as_deref(),
            selector: Version::from_label(&l.reference),
            commit: Some(&l.commit),
            installed: true,
            transitive: true,
            requested_by: &l.requested_by,
            lock: Some(l),
        });
    }
    for (name, spec) in &manifest.plugins {
        let l = lock.plugins.get(name);
        report.plugins.push(PluginRow {
            name,
            git: &spec.git,
            path: spec.path.as_deref(),
            selector: Some(spec.version()?),
            commit: l.map(|l| l.commit.as_str()),
            installed: l.is_some(),
            bundled_skills: l.map(|l| l.bundled_skills.as_slice()).unwrap_or(&[]),
            lock: l,
        });
    }
    Ok(report)
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

fn render_text(report: &ListReport) {
    if report.skills.is_empty() && report.plugins.is_empty() {
        println!("no skills or plugins declared");
        return;
    }
    for row in report.skills.iter().filter(|r| !r.transitive) {
        let pinned = row
            .lock
            .map(|l| format!("{} @ {}", l.reference, short(&l.commit)))
            .unwrap_or_else(|| "not installed".into());
        println!("{:<24} {}  ({pinned})", row.name, row.git);
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
                l.reference,
                short(&l.commit),
                row.requested_by.join(", ")
            );
        }
    }
    for row in &report.plugins {
        let pinned = row
            .lock
            .map(|l| {
                if l.bundled_skills.is_empty() {
                    format!("{} @ {}", l.reference, short(&l.commit))
                } else {
                    format!(
                        "{} @ {}, skills: {}",
                        l.reference,
                        short(&l.commit),
                        l.bundled_skills.join(", ")
                    )
                }
            })
            .unwrap_or_else(|| "not installed".into());
        println!("{:<24} {}  (plugin; {pinned})", row.name, row.git);
    }
}
