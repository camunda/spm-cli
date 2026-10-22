use super::clean::managed_names;
use super::sync::sync;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::scope::Scope;
use crate::vendor;
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

/// `spm target add [vendors...]`: append target vendors to an existing ai.json
/// and materialize skills for them. Explicit `vendors` are validated and added
/// verbatim; with none given, the vendors *not yet configured* are offered as an
/// interactive numbered picker. Adding an already-configured target is a skip,
/// not an error, so the command is safe to re-run.
pub(super) fn target_add(scope: &Scope, vendors: Vec<String>) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir, scope)?;

    let requested = if vendors.is_empty() {
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

    manifest.save(&dir)?;
    // Materialize existing skills into the newly-added vendor(s).
    sync(scope, false, None, false)?;
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
    let (managed, managed_plugins) = managed_names(&lock);
    // Paths a remaining target still loads skills from, resolved through
    // symlinks. A removed vendor must not delete any of them or an ancestor —
    // e.g. `codex` and `amp` share `.agents/skills/<name>/`, and a vendor dir may
    // be symlinked onto (or into) another target's output.
    let mut needed = Vec::new();
    for t in &remaining {
        for p in vendor::for_target(t)?.skill_paths(scope, &managed)? {
            needed.push((t.as_str(), resolved_path(&p)?));
        }
    }
    for t in &removed {
        let v = vendor::for_target(t)?;
        let mut deletable = Vec::new();
        for p in v.clean_paths(scope, &managed)? {
            let resolved = resolved_path(&p)?;
            match needed.iter().find(|(_, n)| n.starts_with(&resolved)) {
                Some((owner, _)) => {
                    println!("keeping {}: still used by target `{owner}`", p.display())
                }
                None => deletable.push(p),
            }
        }
        vendor::remove_paths(&deletable)?;
        v.deregister(scope)?;
        v.clean_plugins(scope, &lock.id, &managed_plugins)?;
    }

    manifest.targets = remaining;
    manifest.save(&dir)?;
    println!("removed target(s): {}", removed.join(", "));
    Ok(())
}

/// The path `p` actually resolves to, so two spellings of one location (e.g.
/// via a symlink) compare equal. A missing path holds nothing to delete or
/// protect and is kept as spelled; any other resolution error is propagated
/// rather than treated as "not shared".
fn resolved_path(p: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(p) {
        Ok(r) => Ok(r),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(p.to_path_buf()),
        Err(e) => Err(anyhow!(e).context(format!("resolving {}", p.display()))),
    }
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
