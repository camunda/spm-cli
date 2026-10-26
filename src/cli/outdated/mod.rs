//! `spm outdated`: which locked dependencies have newer upstream versions.
//! `check.rs` queries the remotes and compares each selector with its pin,
//! `upgrade.rs` plans how a transitive bump reaches the project, `report.rs`
//! is the model (and `--json` document) both renderings share, and
//! `render.rs` is the human table and summary.

mod check;
mod render;
mod report;
mod upgrade;

use super::common::render_report;
use super::index::DependencyIndex;
use super::session::{Access, ScopeSession};
use crate::lockfile::LockedSkill;
use crate::manifest::DependencyKind;
use crate::scope::Scope;
use anyhow::{bail, Result};
use check::{check, Remotes};
use render::render_text;
use report::OutdatedReport;
use upgrade::Upgrade;

/// Report which locked dependencies have newer upstream versions. Strictly
/// read-only: it only runs `git ls-remote` (once per repository) and never
/// writes `ai.json`/`ai.lock`, fetches into the store or touches vendor dirs.
pub(super) fn outdated(
    scope: &Scope,
    name: Option<&str>,
    json: bool,
    exit_code: bool,
) -> Result<()> {
    let session = ScopeSession::open(scope, Access::Read)?;
    let manifest = session.manifest()?;
    // Every remote this reports on is queried through the policy gate; a denied
    // source shows up as an error row instead of being contacted.
    crate::policy::activate(manifest.policy(session.dir())?);
    let lock = session.lockfile()?;
    if let Some(name) = name {
        if !lock.contains_dependency(name) {
            bail!("`{name}` is not in ai.lock — run `spm install` first, or check the name");
        }
    }
    let wanted = |n: &str| name.is_none_or(|want| want == n);
    // The same comparison `spm install` uses to decide whether a pin can be
    // reused for its `ai.json` request.
    let drift = |l: &LockedSkill, spec: Option<&crate::manifest::SkillSpec>| {
        spec.map(|s| l.drift_from(s))
            .transpose()
            .map(Option::flatten)
    };
    let mut remotes = Remotes::default();
    let mut skills = Vec::new();
    let mut plugins = Vec::new();
    let index = DependencyIndex::new(&manifest, &lock);
    for e in index.locked().filter(|e| wanted(e.name)) {
        let (kind, l) = (e.kind, e.locked);
        let mut row = check(
            e.name,
            l,
            kind == DependencyKind::Plugin,
            e.transitive,
            &mut remotes,
        );
        if e.transitive {
            row.upgrade = Upgrade::plan(&l.requested_by, &lock, &index);
        }
        row.drift = drift(l, e.spec)?;
        match kind {
            DependencyKind::Skill => skills.push(row),
            DependencyKind::Plugin => plugins.push(row),
        }
    }
    let report = OutdatedReport {
        scope: scope.label(),
        skills,
        plugins,
    };
    render_report(&report, json, |r| {
        render_text(r, manifest.dependencies().count())
    })?;
    report.outcome(exit_code)
}
