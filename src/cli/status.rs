use super::common::print_json;
use crate::links::Links;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::paths;
use crate::scope::Scope;
use crate::vendor::{self, VendorStatus};
use anyhow::{bail, Result};
use serde::Serialize;
use std::path::PathBuf;

/// Everything `spm status` reports, collected once and rendered either as human
/// text or as the `--json` document. Both renderings share [`outcome`] so the
/// exit status can never depend on the output format.
///
/// [`outcome`]: StatusReport::outcome
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusReport {
    scope: &'static str,
    /// Directory holding this scope's `ai.json`/`ai.lock`.
    #[serde(serialize_with = "crate::jsonutil::serialize_path")]
    root: PathBuf,
    /// Skill names expected on disk: every `ai.lock` skill plus every
    /// plugin-bundled skill.
    expected: Vec<ExpectedSkill>,
    /// Plugins declared in `ai.json`.
    plugins: Vec<String>,
    /// Declared plugins with no `ai.lock` pin (never resolved here).
    unlocked_plugins: Vec<String>,
    collisions: Vec<Collision>,
    /// Active `spm link` overrides (materialized from a local directory
    /// instead of the locked commit).
    links: Vec<LinkStatus>,
    targets: Vec<TargetStatus>,
    complete: bool,
    #[serde(skip)]
    declared_skills: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpectedSkill {
    name: String,
    /// Direct requester(s) of a transitively-resolved skill; empty otherwise.
    requested_by: Vec<String>,
}

/// One `spm link` override.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkStatus {
    name: String,
    /// The linked directory, as recorded by `spm link`.
    path: String,
    /// Whether `name` is still declared in `ai.json` (an undeclared link is
    /// ignored by syncs until `spm unlink` drops it).
    declared: bool,
}

/// A skill name materialized in both scopes (they collide by name at runtime).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Collision {
    name: String,
    other_scope: &'static str,
}

#[derive(Serialize)]
struct TargetStatus {
    target: String,
    /// `null` when no skills are expected.
    skills: Option<VendorStatus>,
    /// `null` when no plugins are declared or the target only receives the
    /// plugins' bundled skills (already covered by `skills`).
    plugins: Option<VendorStatus>,
}

impl StatusReport {
    /// The active (declared) link for `name`, if any.
    fn link(&self, name: &str) -> Option<&LinkStatus> {
        self.links.iter().find(|l| l.declared && l.name == name)
    }

    fn nothing_expected(&self) -> bool {
        self.expected.is_empty() && self.plugins.is_empty()
    }

    /// The command's exit status, shared by the text and JSON renderings.
    fn outcome(&self) -> Result<()> {
        if self.complete {
            return Ok(());
        }
        if self.nothing_expected() {
            bail!(
                "ai.json declares {} skill(s) but ai.lock has none — run `spm install`",
                self.declared_skills
            );
        }
        bail!("some declared skills or plugins are not materialized — run `spm install`");
    }
}

/// Show, per target, which declared skills are materialized for this scope.
/// Because spm materializes into gitignored dirs, a fresh clone or a new git
/// worktree sees nothing until `spm install` runs inside it — this command
/// surfaces exactly that, and exits non-zero when anything is missing so it
/// doubles as an automated gate. With `json`, the report is printed *before*
/// the non-zero exit, so a consumer gets both the data and the status.
pub(super) fn status(scope: &Scope, json: bool) -> Result<()> {
    let report = collect(scope)?;
    if json {
        print_json(&report)?;
    } else {
        render_text(&report);
    }
    report.outcome()
}

fn collect(scope: &Scope) -> Result<StatusReport> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir, scope)?;
    let lock = Lockfile::load_or_default(&dir)?;
    let links = Links::load(scope)?;
    let managed_by_links = links.managed.clone();
    let links = links
        .links
        .into_iter()
        .map(|(name, path)| LinkStatus {
            declared: manifest.skills.contains_key(&name) || manifest.plugins.contains_key(&name),
            name,
            path,
        })
        .collect();
    // Plugin-bundled skills are flattened into every vendor's skills dir just
    // like standalone skills, so they belong in `expected` — otherwise `status`
    // would flag them as stale (present on disk, not declared).
    let mut expected: Vec<String> = lock.skills.keys().cloned().collect();
    for l in lock.plugins.values() {
        expected.extend(l.bundled_skills.iter().cloned());
    }
    // Skills a linked plugin's working tree bundles beyond its locked commit.
    expected.extend(managed_by_links);
    expected.sort();
    expected.dedup();

    // Warn when a skill name is materialized in *both* scopes: the vendor
    // discovery layer keys by name (`/spm:foo`, `.agents/skills/foo`), so a
    // project skill and a global skill of the same name collide/shadow at
    // runtime. Surface it rather than letting the tool silently pick one.
    let other_scope = if scope.is_global() {
        "project"
    } else {
        "global"
    };
    let collisions = shadowed_names(scope, &expected)?
        .into_iter()
        .map(|name| Collision { name, other_scope })
        .collect();

    // Declared plugins come from the *manifest* (the source of truth for what
    // should be installed). Comparing against `ai.lock` catches a plugin that
    // was declared but never resolved/installed; comparing against the on-disk
    // marketplace (via each vendor's `status_plugins`) catches a deleted
    // `.spm/claude-plugins` dir or a fresh worktree that was never installed.
    let declared_plugins: Vec<String> = manifest.plugins.keys().cloned().collect();
    let unlocked_plugins: Vec<String> = declared_plugins
        .iter()
        .filter(|n| !lock.plugins.contains_key(*n))
        .cloned()
        .collect();

    let mut targets = Vec::new();
    for target in &manifest.targets {
        let vendor = vendor::for_target(target)?;
        let skills = if expected.is_empty() {
            None
        } else {
            Some(vendor.status(scope, &expected)?)
        };
        // Full-plugin materialization is only meaningful for vendors that
        // register plugins beyond their bundled skills (currently Claude);
        // `status_plugins` returns `None` for the rest.
        let plugins = if declared_plugins.is_empty() {
            None
        } else {
            vendor.status_plugins(scope, &declared_plugins)?
        };
        targets.push(TargetStatus {
            target: target.clone(),
            skills,
            plugins,
        });
    }

    // Annotate a transitively-resolved skill with its requester(s) so `status`
    // surfaces the dependency graph, not just names. A name declared directly
    // in `ai.json` is a direct skill even if a stale or hand-edited `ai.lock`
    // left a `requested_by` on it, so exclude manifest skills — matching
    // `spm list`.
    let expected: Vec<ExpectedSkill> = expected
        .into_iter()
        .map(|name| {
            let requested_by = lock
                .skills
                .get(&name)
                .filter(|_| !manifest.skills.contains_key(&name))
                .map(|l| l.requested_by.clone())
                .unwrap_or_default();
            ExpectedSkill { name, requested_by }
        })
        .collect();

    let incomplete_here = |st: &Option<VendorStatus>| {
        st.as_ref()
            .is_some_and(|st| !st.missing.is_empty() || !st.notes.is_empty())
    };
    let mut report = StatusReport {
        scope: scope.label(),
        root: dir,
        expected,
        plugins: declared_plugins,
        unlocked_plugins,
        collisions,
        links,
        complete: false,
        declared_skills: manifest.skills.len(),
        targets: Vec::new(),
    };
    report.complete = if report.nothing_expected() {
        // Nothing locked and nothing declared is a clean, correct empty state;
        // declared-but-unlocked skills are not.
        report.declared_skills == 0
    } else {
        report.unlocked_plugins.is_empty()
            && !targets
                .iter()
                .any(|t| incomplete_here(&t.skills) || incomplete_here(&t.plugins))
    };
    report.targets = targets;
    Ok(report)
}

fn render_text(report: &StatusReport) {
    println!("{}: {}", report.scope, report.root.display());
    let names: Vec<&str> = report.targets.iter().map(|t| t.target.as_str()).collect();
    println!("targets: {}", names.join(", "));
    for c in &report.collisions {
        println!(
            "! `{}` is also installed in the {} scope — they will collide by name",
            c.name, c.other_scope
        );
    }
    for l in &report.links {
        if l.declared {
            println!(
                "linked: `{}` → {} (local override of the locked version)",
                l.name, l.path
            );
        } else {
            println!(
                "! linked: `{}` → {} is not declared in ai.json and is ignored — run `spm unlink {}{}`",
                l.name,
                l.path,
                l.name,
                if report.scope == "global" { " -g" } else { "" }
            );
        }
    }

    if report.nothing_expected() {
        if report.complete {
            println!("\nno skills or plugins declared — add one with `spm add <git-url>`");
        }
        return;
    }

    let width = report
        .expected
        .iter()
        .map(|e| e.name.len())
        .chain(report.plugins.iter().map(String::len))
        .max()
        .unwrap_or(0);

    for name in &report.unlocked_plugins {
        println!("! plugin `{name}` is declared in ai.json but not in ai.lock — run `spm install`");
    }

    for t in &report.targets {
        let target = &t.target;
        if let Some(st) = &t.skills {
            println!(
                "\n[{target}]  {}/{} installed  {}",
                st.present.len(),
                report.expected.len(),
                st.location.display()
            );
            for e in &report.expected {
                let missing = st.missing.contains(&e.name);
                let via = if let Some(l) = report.link(&e.name) {
                    format!("  (linked → {})", l.path)
                } else if e.requested_by.is_empty() {
                    String::new()
                } else {
                    format!("  (transitive; via {})", e.requested_by.join(", "))
                };
                println!(
                    "  {:<width$}  {}{via}",
                    e.name,
                    if missing { "MISSING" } else { "ok" }
                );
            }
            for s in &st.stale {
                println!("  {s:<width$}  stale (not in ai.lock)");
            }
            for note in &st.notes {
                println!("  ! {note}");
            }
        }
        if let Some(st) = &t.plugins {
            println!(
                "\n[{target}] plugins  {}/{} installed  {}",
                st.present.len(),
                report.plugins.len(),
                st.location.display()
            );
            for name in &report.plugins {
                let missing = st.missing.contains(name);
                let linked = report
                    .link(name)
                    .map(|l| format!("  (linked → {})", l.path))
                    .unwrap_or_default();
                println!(
                    "  {name:<width$}  {}{linked}",
                    if missing { "MISSING" } else { "ok" }
                );
            }
            for note in &st.notes {
                println!("  ! {note}");
            }
        }
    }

    if report.complete {
        println!("\nall declared skills and plugins are materialized");
    }
}

/// Skill names in `expected` that are *also* locked in the opposite scope. Used
/// by `status` to warn about global/project name collisions. Returns an empty
/// list when the opposite scope has no lockfile.
fn shadowed_names(scope: &Scope, expected: &[String]) -> Result<Vec<String>> {
    let other_dir = match scope {
        Scope::Project { .. } => paths::spm_home()?,
        Scope::Global => std::env::current_dir()?,
    };
    let other = Lockfile::load_or_default(&other_dir)?;
    // A name provided by a plugin's bundled skills in the *other* scope
    // collides on disk just as much as a standalone skill of the same name
    // (both are flattened into the same vendor skills dir), so it must be
    // checked here too — not just `other.skills`.
    let other_names: std::collections::HashSet<&str> = other
        .skills
        .keys()
        .map(String::as_str)
        .chain(
            other
                .plugins
                .values()
                .flat_map(|l| l.bundled_skills.iter().map(String::as_str)),
        )
        .collect();
    Ok(expected
        .iter()
        .filter(|n| other_names.contains(n.as_str()))
        .cloned()
        .collect())
}
