use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::paths;
use crate::scope::Scope;
use crate::vendor;
use anyhow::{bail, Result};

/// Show, per target, which declared skills are materialized for this scope.
/// Because spm materializes into gitignored dirs, a fresh clone or a new git
/// worktree sees nothing until `spm install` runs inside it — this command
/// surfaces exactly that, and exits non-zero when anything is missing so it
/// doubles as an automated gate.
pub(super) fn status(scope: &Scope) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir)?;
    let lock = Lockfile::load_or_default(&dir)?;
    // Plugin-bundled skills are flattened into every vendor's skills dir just
    // like standalone skills, so they belong in `expected` — otherwise `status`
    // would flag them as stale (present on disk, not declared).
    let mut expected: Vec<String> = lock.skills.keys().cloned().collect();
    for l in lock.plugins.values() {
        expected.extend(l.bundled_skills.iter().cloned());
    }
    expected.sort();
    expected.dedup();

    match scope {
        Scope::Project { root } => println!("project: {}", root.display()),
        Scope::Global => println!("global: {}", dir.display()),
    }
    println!("targets: {}", manifest.targets.join(", "));

    // Warn when a skill name is materialized in *both* scopes: the vendor
    // discovery layer keys by name (`/spm:foo`, `.agents/skills/foo`), so a
    // project skill and a global skill of the same name collide/shadow at
    // runtime. Surface it here rather than letting the tool silently pick one.
    for shadow in shadowed_names(scope, &expected)? {
        let other = if scope.is_global() {
            "project"
        } else {
            "global"
        };
        println!("! `{shadow}` is also installed in the {other} scope — they will collide by name");
    }

    // Nothing locked and nothing declared: a clean, correct empty state.
    if expected.is_empty() && manifest.plugins.is_empty() {
        if manifest.skills.is_empty() {
            println!("\nno skills or plugins declared — add one with `spm add <git-url>`");
            return Ok(());
        }
        bail!(
            "ai.json declares {} skill(s) but ai.lock has none — run `spm install`",
            manifest.skills.len()
        );
    }

    // Declared plugins come from the *manifest* (the source of truth for what
    // should be installed). Comparing against `ai.lock` catches a plugin that
    // was declared but never resolved/installed; comparing against the on-disk
    // marketplace (via each vendor's `status_plugins`) catches a deleted
    // `.spm/claude-plugins` dir or a fresh worktree that was never installed.
    let declared_plugins: Vec<String> = manifest.plugins.keys().cloned().collect();
    let width = expected
        .iter()
        .chain(declared_plugins.iter())
        .map(String::len)
        .max()
        .unwrap_or(0);
    let mut incomplete = false;

    // A plugin declared in ai.json but absent from ai.lock was never resolved —
    // installs would be silently incomplete. Flag it explicitly.
    for name in &declared_plugins {
        if !lock.plugins.contains_key(name) {
            incomplete = true;
            println!(
                "! plugin `{name}` is declared in ai.json but not in ai.lock — run `spm install`"
            );
        }
    }

    for target in &manifest.targets {
        let vendor = vendor::for_target(target)?;
        if !expected.is_empty() {
            let st = vendor.status(scope, &expected)?;
            println!(
                "\n[{target}]  {}/{} installed  {}",
                st.present.len(),
                expected.len(),
                st.location.display()
            );
            for name in &expected {
                let missing = st.missing.iter().any(|m| m == name);
                if missing {
                    incomplete = true;
                }
                println!(
                    "  {name:<width$}  {}",
                    if missing { "MISSING" } else { "ok" }
                );
            }
            for s in &st.stale {
                println!("  {s:<width$}  stale (not in ai.lock)");
            }
            for note in &st.notes {
                incomplete = true;
                println!("  ! {note}");
            }
        }

        // Full-plugin materialization is only meaningful for vendors that
        // register plugins beyond their bundled skills (currently Claude);
        // `status_plugins` returns `None` for the rest.
        if !declared_plugins.is_empty() {
            if let Some(st) = vendor.status_plugins(scope, &declared_plugins)? {
                println!(
                    "\n[{target}] plugins  {}/{} installed  {}",
                    st.present.len(),
                    declared_plugins.len(),
                    st.location.display()
                );
                for name in &declared_plugins {
                    let missing = st.missing.iter().any(|m| m == name);
                    if missing {
                        incomplete = true;
                    }
                    println!(
                        "  {name:<width$}  {}",
                        if missing { "MISSING" } else { "ok" }
                    );
                }
                for note in &st.notes {
                    incomplete = true;
                    println!("  ! {note}");
                }
            }
        }
    }

    if incomplete {
        bail!("some declared skills or plugins are not materialized — run `spm install`");
    }
    println!("\nall declared skills and plugins are materialized");
    Ok(())
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
