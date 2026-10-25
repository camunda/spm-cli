use super::common::print_json;
use super::session::{Access, ScopeSession, ScopeSnapshot};
use crate::ids::DepName;
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
pub(super) struct StatusReport {
    pub(super) scope: &'static str,
    /// Directory holding this scope's `ai.json`/`ai.lock`.
    #[serde(serialize_with = "crate::jsonutil::serialize_path")]
    pub(super) root: PathBuf,
    /// Skill names expected on disk: every `ai.lock` skill plus every
    /// plugin-bundled skill.
    pub(super) expected: Vec<ExpectedSkill>,
    /// Plugins declared in `ai.json`.
    pub(super) plugins: Vec<DepName>,
    /// Declared plugins with no `ai.lock` pin (never resolved here).
    pub(super) unlocked_plugins: Vec<DepName>,
    pub(super) collisions: Vec<Collision>,
    /// Active `spm link` overrides (materialized from a local directory
    /// instead of the locked commit).
    pub(super) links: Vec<LinkStatus>,
    pub(super) targets: Vec<TargetStatus>,
    pub(super) complete: bool,
    #[serde(skip)]
    pub(super) declared_skills: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExpectedSkill {
    pub(super) name: DepName,
    /// Direct requester(s) of a transitively-resolved skill; empty otherwise.
    pub(super) requested_by: Vec<DepName>,
}

/// One `spm link` override.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LinkStatus {
    pub(super) name: DepName,
    /// The linked directory, as recorded by `spm link`.
    pub(super) path: String,
    /// Whether `name` is still declared in `ai.json` (an undeclared link is
    /// ignored by syncs until `spm unlink` drops it).
    pub(super) declared: bool,
}

/// A skill name materialized in both scopes (they collide by name at runtime).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Collision {
    pub(super) name: DepName,
    pub(super) other_scope: &'static str,
}

/// A target's skills or plugins dir, always inspected so leftovers stay
/// visible (e.g. to `doctor`), but serialized as `null` when nothing is
/// expected there — the version-1 `--json` contract.
pub(super) struct Inspected {
    pub(super) status: VendorStatus,
    pub(super) expected: bool,
}

impl Serialize for Inspected {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.expected {
            self.status.serialize(s)
        } else {
            s.serialize_none()
        }
    }
}

#[derive(Serialize)]
pub(super) struct TargetStatus {
    pub(super) target: String,
    /// `null` when no skills are expected.
    pub(super) skills: Inspected,
    /// `null` when no plugins are declared or the target only receives the
    /// plugins' bundled skills (already covered by `skills`).
    pub(super) plugins: Option<Inspected>,
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
    let report = collect(&ScopeSession::open(scope, Access::Read)?)?;
    if json {
        print_json(&report)?;
    } else {
        render_text(&report);
    }
    report.outcome()
}

pub(super) fn collect(session: &ScopeSession) -> Result<StatusReport> {
    let scope = session.scope();
    let ScopeSnapshot {
        manifest,
        lock,
        links,
        ownership,
    } = session.snapshot()?;
    // Plugin-bundled skills are flattened into every vendor's skills dir just
    // like standalone skills, so they belong in `expected` — otherwise `status`
    // would flag them as stale (present on disk, not declared). A linked
    // plugin contributes its working tree's bundled skills instead.
    let expected = links.effective_skill_names(&lock);
    let links = links
        .links
        .into_iter()
        .map(|(name, path)| LinkStatus {
            declared: manifest.skills.contains_key(&name) || manifest.plugins.contains_key(&name),
            name,
            path,
        })
        .collect();
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
    let declared_plugins: Vec<DepName> = manifest.plugins.keys().cloned().collect();
    let unlocked_plugins: Vec<DepName> = declared_plugins
        .iter()
        .filter(|n| !lock.plugins.contains_key(*n))
        .cloned()
        .collect();

    let mut targets = Vec::new();
    for target in &manifest.targets {
        let vendor = vendor::for_target(target)?;
        // Inspect even when nothing is expected: skills left behind in an
        // spm-owned dir (e.g. after switching to a branch that declares none)
        // are still loaded by the tool and must surface as stale.
        let mut status = vendor.status(scope, &expected)?;
        // A shared dir can't flag stale entries by listing it (the user's own
        // skills live there too), but what spm recorded installing there and
        // is no longer expected is its leftover all the same.
        let dir = vendor.skills_dir(scope)?;
        for name in ownership.for_target(target).skill_names() {
            if !expected.contains(&name)
                && !status.stale.iter().any(|s| *s == name)
                && dir.join(&name).is_dir()
            {
                status.stale.push(name.to_string());
            }
        }
        status.stale.sort();
        let skills = Inspected {
            status,
            expected: !expected.is_empty(),
        };
        // Full-plugin materialization is only meaningful for vendors that
        // register plugins beyond their bundled skills (currently Claude);
        // `status_plugins` returns `None` for the rest. Inspected even with
        // none declared, so a leftover registered plugin surfaces as stale.
        let plugins = vendor
            .status_plugins(scope, &declared_plugins)?
            .map(|status| Inspected {
                status,
                expected: !declared_plugins.is_empty(),
            });
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

    let incomplete_here = |st: Option<&VendorStatus>| {
        st.is_some_and(|st| !st.missing.is_empty() || !st.notes.is_empty())
    };
    let mut report = StatusReport {
        scope: scope.label(),
        root: session.dir().to_path_buf(),
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
            && !targets.iter().any(|t| {
                incomplete_here(Some(&t.skills.status))
                    || incomplete_here(t.plugins.as_ref().map(|p| &p.status))
            })
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
            let g = if report.scope == "global" { " -g" } else { "" };
            println!(
                "! linked: `{}` → {} is not declared in ai.json and is ignored — run `spm unlink{g} {}`",
                l.name,
                l.path,
                crate::shell::operands([l.name.as_str()])
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
        .chain(report.plugins.iter().map(|n| n.len()))
        .max()
        .unwrap_or(0);

    for name in &report.unlocked_plugins {
        println!("! plugin `{name}` is declared in ai.json but not in ai.lock — run `spm install`");
    }

    for t in &report.targets {
        let target = &t.target;
        if t.skills.expected {
            let st = &t.skills.status;
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
        if let Some(st) = t.plugins.as_ref().filter(|p| p.expected).map(|p| &p.status) {
            println!(
                "\n[{target}] plugins  {}/{} installed  {}",
                st.present.len(),
                report.plugins.len(),
                st.location.display()
            );
            for name in &report.plugins {
                let missing = st.missing.iter().any(|m| m == name);
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
fn shadowed_names(scope: &Scope, expected: &[DepName]) -> Result<Vec<DepName>> {
    let other_scope = match scope {
        Scope::Project { .. } => Scope::Global,
        Scope::Global => Scope::Project {
            root: std::env::current_dir()?,
        },
    };
    // Only peeked at, never written: no lock.
    let other = ScopeSession::open(&other_scope, Access::Read)?;
    // A name provided by a plugin's bundled skills in the *other* scope
    // collides on disk just as much as a standalone skill of the same name
    // (both are flattened into the same vendor skills dir), so it must be
    // checked here too — computed exactly as for this scope, so a linked
    // plugin's working-tree bundle replaces its locked one.
    let other_lock = other.lockfile()?;
    let other_names: std::collections::HashSet<DepName> = other
        .links()?
        .effective_skill_names(&other_lock)
        .into_iter()
        .collect();
    Ok(expected
        .iter()
        .filter(|n| other_names.contains(n.as_str()))
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::Links;
    use crate::lockfile::Lockfile;

    /// The other scope's `ai.lock` is read before its links, as in every
    /// [`ScopeSession::snapshot`], so with both broken the lock is reported.
    #[test]
    fn shadowed_names_reads_the_other_lock_before_its_links() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("spm-status-order-{nanos}"));
        let home = root.join("home");
        std::fs::create_dir_all(root.join("project")).unwrap();
        let _home = crate::paths::SpmHomeGuard::set(&home);
        let scope = Scope::Project {
            root: root.join("project"),
        };
        let links = Links::path(&Scope::Global).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(Lockfile::path_in(&home), "not json").unwrap();
        std::fs::write(&links, "not json").unwrap();
        let err = format!("{:#}", shadowed_names(&scope, &[]).unwrap_err());
        assert!(err.contains("ai.lock"), "{err}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
