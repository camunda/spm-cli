use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::scope::Scope;
use anyhow::Result;

pub(super) fn list(scope: &Scope) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir)?;
    let lock = Lockfile::load_or_default(&dir)?;
    if manifest.skills.is_empty() && manifest.plugins.is_empty() {
        println!("no skills or plugins declared");
        return Ok(());
    }
    for (name, spec) in &manifest.skills {
        let pinned = lock
            .skills
            .get(name)
            .map(|l| format!("{} @ {}", l.reference, &l.commit[..l.commit.len().min(8)]))
            .unwrap_or_else(|| "not installed".into());
        println!("{name:<24} {}  ({pinned})", spec.git);
    }
    for (name, spec) in &manifest.plugins {
        let pinned = lock
            .plugins
            .get(name)
            .map(|l| {
                let short = &l.commit[..l.commit.len().min(8)];
                if l.bundled_skills.is_empty() {
                    format!("{} @ {short}", l.reference)
                } else {
                    format!(
                        "{} @ {short}, skills: {}",
                        l.reference,
                        l.bundled_skills.join(", ")
                    )
                }
            })
            .unwrap_or_else(|| "not installed".into());
        println!("{name:<24} {}  (plugin; {pinned})", spec.git);
    }
    Ok(())
}
