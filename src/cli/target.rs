use super::sync::{save_and_sync, Apply};
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::owned::Ownership;
use crate::pathid::{deletion_target, existing_identity, identity, resolve, NodeId};
use crate::scope::Scope;
use crate::vendor;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

/// `spm target add [vendors...]`: append target vendors to an existing ai.json
/// and materialize skills for them. Explicit `vendors` are validated and added
/// verbatim; with none given, the vendors *not yet configured* are offered as an
/// interactive numbered picker. Adding an already-configured target is a skip,
/// not an error, so the command is safe to re-run.
pub(super) fn target_add(scope: &Scope, vendors: Vec<String>) -> Result<()> {
    let dir = scope.manifest_dir()?;

    let requested = if vendors.is_empty() {
        // Not locked yet: no command should wait on an interactive prompt. The
        // choice is re-checked against `ai.json` as read under the lock below.
        let manifest = Manifest::load(&dir, scope)?;
        let available: Vec<&str> = vendor::ALL_TARGETS
            .iter()
            .copied()
            .filter(|t| !manifest.targets.iter().any(|cur| cur == t))
            .collect();
        if available.is_empty() {
            println!(
                "all supported targets already configured: {}",
                manifest.targets.join(", ")
            );
            return Ok(());
        }
        prompt_targets(&available)?
    } else {
        // Validate every explicitly-requested vendor up front so a typo aborts
        // before the manifest is touched.
        for v in &vendors {
            vendor::for_target(v)?;
        }
        vendors
    };

    // Held from reading `ai.json` to the last write (see `StateLock`).
    let state_lock = crate::links::StateLock::acquire(scope, false)?;
    let mut manifest = Manifest::load(&dir, scope)?;
    let mut added = Vec::new();
    for v in requested {
        if manifest.targets.iter().any(|cur| cur == &v) {
            println!("target `{v}` already configured — skipping");
            continue;
        }
        manifest.targets.push(v.clone());
        added.push(v);
    }

    if added.is_empty() {
        println!("no new targets added");
        return Ok(());
    }

    // Materialize existing skills into the newly-added vendor(s).
    save_and_sync(scope, state_lock, &manifest, Apply::Install)?;
    println!("added target(s): {}", added.join(", "));
    Ok(())
}

/// `spm target remove <vendors...>`: drop target vendors from ai.json and clean
/// what spm materialized for them. Every vendor is validated up front, so a typo
/// aborts before anything is touched; a vendor that isn't configured is a skip,
/// so the command is safe to re-run. Removing every remaining target is refused
/// (`sync` needs at least one) — `spm clean` is the tool for that.
pub(super) fn target_remove(scope: &Scope, vendors: Vec<String>) -> Result<()> {
    for v in &vendors {
        vendor::for_target(v)?;
    }
    let dir = scope.manifest_dir()?;
    // Held from reading `ai.json` and link state until the deletions below are
    // done (see `StateLock`).
    let _state_lock = crate::links::StateLock::acquire(scope, false)?;
    let mut manifest = Manifest::load(&dir, scope)?;

    let mut removed: Vec<String> = Vec::new();
    for v in vendors {
        if removed.contains(&v) {
            continue;
        }
        if manifest.targets.contains(&v) {
            removed.push(v);
        } else {
            println!("target `{v}` not configured — skipping");
        }
    }
    if removed.is_empty() {
        println!("no targets removed");
        return Ok(());
    }

    let remaining: Vec<String> = manifest
        .targets
        .iter()
        .filter(|t| !removed.contains(t))
        .cloned()
        .collect();
    if remaining.is_empty() {
        let clean = if scope.is_global() {
            "spm clean --global"
        } else {
            "spm clean"
        };
        bail!(
            "cannot remove the last target ({}): ai.json needs at least one; \
             use `{clean}` to remove all generated vendor config instead",
            removed.join(", ")
        );
    }

    let lock = Lockfile::load_or_default(&dir)?;
    // Each target's own spm output (see `crate::owned`).
    let mut owned = Ownership::load(
        scope,
        &lock,
        &crate::links::Links::load(scope)?.managed,
        &manifest.targets,
    )?;
    // Physical locations a remaining target still loads skills, plugins or
    // their metadata from (`content`), plus every location reaching them
    // depends on (`traversed`: symlinks, and dirs passed through before a
    // `..`), including symlinks nested anywhere inside the content. A removed
    // vendor must not delete a traversed location or its ancestors, nor content,
    // its ancestors or anything inside it — e.g. `codex` and `amp` share
    // `.agents/skills/<name>/`, and a vendor dir may be symlinked onto (or into)
    // another target's output. Locations are compared by filesystem identity so
    // spelling aliases (e.g. case on a case-insensitive filesystem) still match.
    //
    // A remaining target's record can lag behind what it loads (e.g. a `target
    // add codex cursor` that failed at `codex` never reached `cursor`, or an
    // install that failed after `codex` wrote the `.agents/skills/` it shares
    // with `amp`), so each remaining target also protects where it would load
    // the removed targets' names from. Protecting more only keeps more.
    let mut inferred = crate::owned::Owned::default();
    for t in &removed {
        inferred.extend(&owned.for_target(t));
    }
    let mut needed: Vec<(&str, Vec<NodeId>, bool)> = Vec::new();
    for t in &remaining {
        let v = vendor::for_target(t)?;
        let mut names = owned.for_target(t);
        names.extend(&inferred);
        let mut paths = v.skill_paths(scope, &names.skill_names())?;
        paths.extend(v.plugin_paths(scope, &names.plugin_names())?);
        let (mut traversed, mut content) = (Vec::new(), Vec::new());
        let mut visited = std::collections::HashSet::new();
        for p in paths {
            reach(&p, &mut traversed, &mut content, &mut visited)?;
        }
        for (paths, is_content) in [(traversed, false), (content, true)] {
            for p in paths {
                needed.push((t.as_str(), identity(&p)?, is_content));
            }
        }
    }
    // Neither the deletions nor the settings/`ai.json` writes below may touch a
    // linked working copy.
    let removed_vendors = removed
        .iter()
        .map(|t| vendor::for_target(t))
        .collect::<Result<Vec<_>>>()?;
    let removed_owned: Vec<_> = removed.iter().map(|t| owned.for_target(t)).collect();
    super::sync::guard_working_copies(
        scope,
        &dir,
        &removed_vendors,
        &Default::default(),
        &removed_owned,
        None,
        false,
    )?;
    for ((t, v), names) in removed.iter().zip(&removed_vendors).zip(&removed_owned) {
        let mut candidates = v.clean_paths(scope, &names.skill_names())?;
        candidates.extend(v.plugin_clean_paths(scope, &names.plugin_names())?);
        let mut spans = Vec::new();
        for p in &candidates {
            spans.push(identity(&deletion_target(p)?)?);
        }
        // What a remaining target loads from inside `t`'s output — the same
        // shared entry, or one nested in `t`'s container — or the very entry
        // `t` loads a skill from (e.g. both reach it through symlinks) is kept
        // (protected above) and is now that target's: hand it over so a later
        // `clean` or install still removes it. A location a symlink nested
        // inside `t`'s entry points at is not `t`'s output, so is never handed.
        for x in &remaining {
            let xv = vendor::for_target(x)?;
            let mut handed = crate::owned::Owned::default();
            for (name, is_plugin) in names
                .skill_names()
                .into_iter()
                .map(|n| (n, false))
                .chain(names.plugin_names().into_iter().map(|n| (n, true)))
            {
                let (paths, ours) = if is_plugin {
                    (xv.plugin_paths(scope, std::slice::from_ref(&name))?, None)
                } else {
                    let ours = existing_identity(&v.skills_dir(scope)?.join(&name))?;
                    (vec![xv.skills_dir(scope)?.join(&name)], ours)
                };
                for q in paths {
                    let Some(id) = existing_identity(&q)? else {
                        continue;
                    };
                    if Some(&id) == ours.as_ref() || spans.iter().any(|s| id.starts_with(s)) {
                        let set = if is_plugin {
                            &mut handed.plugins
                        } else {
                            &mut handed.skills
                        };
                        set.insert(name.clone());
                    }
                }
            }
            owned.grant(x, &handed);
        }
        let mut deletable = Vec::new();
        for p in candidates {
            let target = identity(&deletion_target(&p)?)?;
            let owner = needed.iter().find(|(_, n, content)| {
                n.starts_with(&target) || (*content && target.starts_with(n))
            });
            match owner {
                Some((owner, ..)) => {
                    println!("keeping {}: still used by target `{owner}`", p.display())
                }
                None => deletable.push(p),
            }
        }
        vendor::remove_paths(&deletable)?;
        v.deregister(scope)?;
        v.deregister_plugins(scope)?;
        owned = owned.without(t);
    }
    owned.save(scope)?;

    manifest.targets = remaining;
    manifest.save(&dir)?;
    println!("removed target(s): {}", removed.join(", "));
    Ok(())
}

/// Record what loading from `p` depends on: resolve it (pushing traversed
/// locations), push the physical location as content, then walk it for nested
/// symlinks — each of which is a further dependency to resolve and reach.
fn reach(
    p: &Path,
    traversed: &mut Vec<PathBuf>,
    content: &mut Vec<PathBuf>,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Result<()> {
    let fin = resolve(p, traversed)?;
    if !visited.insert(fin.clone()) {
        return Ok(());
    }
    content.push(fin.clone());
    let mut dirs = vec![fin];
    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue
            }
            Err(e) => return Err(anyhow!(e).context(format!("reading {}", dir.display()))),
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                traversed.push(entry.path());
                reach(&entry.path(), traversed, content, visited)?;
            } else if ft.is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    Ok(())
}

/// Interactive numbered picker over the vendors not yet configured. Reads a
/// comma-separated list of 1-based indices (or `all`) from stdin — a plain line
/// read, not a raw-terminal TUI, so it stays portable across Windows and Unix
/// shells and is drivable from tests with piped input.
fn prompt_targets(available: &[&str]) -> Result<Vec<String>> {
    use std::io::{self, BufRead, Write};

    println!("Select target(s) to add:");
    for (i, t) in available.iter().enumerate() {
        println!("  {}) {t}", i + 1);
    }
    print!("Enter numbers (comma-separated), or `all`: ");
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        bail!("no target selected");
    }
    if line.eq_ignore_ascii_case("all") {
        return Ok(available.iter().map(|s| s.to_string()).collect());
    }

    // Preserve the user's typed order while dropping duplicate picks.
    let mut chosen = Vec::new();
    for tok in line.split(',') {
        let tok = tok.trim();
        let n: usize = tok
            .parse()
            .map_err(|_| anyhow!("invalid selection `{tok}`: enter numbers from the list"))?;
        let vendor = available
            .get(
                n.checked_sub(1)
                    .ok_or_else(|| anyhow!("invalid selection `{tok}`: numbers start at 1"))?,
            )
            .ok_or_else(|| anyhow!("invalid selection `{tok}`: no such option"))?
            .to_string();
        if !chosen.contains(&vendor) {
            chosen.push(vendor);
        }
    }
    Ok(chosen)
}
