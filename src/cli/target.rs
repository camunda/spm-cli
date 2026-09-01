use super::sync::sync;
use crate::manifest::Manifest;
use crate::scope::Scope;
use crate::vendor;
use anyhow::{anyhow, bail, Result};

/// `spm target add [vendors...]`: append target vendors to an existing ai.json
/// and materialize skills for them. Explicit `vendors` are validated and added
/// verbatim; with none given, the vendors *not yet configured* are offered as an
/// interactive numbered picker. Adding an already-configured target is a skip,
/// not an error, so the command is safe to re-run.
pub(super) fn target_add(scope: &Scope, vendors: Vec<String>) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir)?;

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
    sync(scope, false, None)?;
    println!("added target(s): {}", added.join(", "));
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
