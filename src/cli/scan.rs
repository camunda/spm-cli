use super::common::print_json;
use crate::scan::Finding;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct ScanReport<'a> {
    #[serde(serialize_with = "crate::jsonutil::serialize_path")]
    path: &'a Path,
    findings: Vec<FindingRow<'a>>,
    summary: Summary,
}

#[derive(Serialize)]
struct FindingRow<'a> {
    #[serde(flatten)]
    finding: &'a Finding,
    blocking: bool,
}

#[derive(Serialize)]
struct Summary {
    total: usize,
    blocking: usize,
}

/// `spm scan [path]` — run the content scanner over a path (a single file or a
/// directory) and print every finding. Exits non-zero when any blocking
/// (high/critical) finding is present so it can serve as a CI gate on skill
/// sources. With `json`, the report is printed as a JSON document *before* the
/// non-zero exit, so a consumer gets both the data and the status.
///
/// With `within`, symlinks are bounded by that directory instead of by `path`
/// itself — the same [`crate::scan::scan_within`] call the install gate makes
/// with a dependency's checkout root, so a scan of a plugin subdir sees exactly
/// what installing it would materialize.
pub(super) fn scan_cmd(path: &Path, within: Option<&Path>, json: bool) -> Result<()> {
    if !path.exists() {
        bail!("path does not exist: {}", path.display());
    }
    let findings = match within {
        None => crate::scan::scan_path(path)?,
        Some(boundary) => {
            let canon = |p: &Path| {
                std::fs::canonicalize(p).with_context(|| format!("resolving {}", p.display()))
            };
            if !crate::fsutil::contains(&canon(boundary)?, &canon(path)?) {
                bail!(
                    "{} is not inside --within {}",
                    path.display(),
                    boundary.display()
                );
            }
            crate::scan::scan_within(path, boundary)?
        }
    };
    let blocking = findings.iter().filter(|f| f.severity.blocks()).count();
    if json {
        print_json(&ScanReport {
            path,
            findings: findings
                .iter()
                .map(|f| FindingRow {
                    finding: f,
                    blocking: f.severity.blocks(),
                })
                .collect(),
            summary: Summary {
                total: findings.len(),
                blocking,
            },
        })?;
    } else if findings.is_empty() {
        println!("no suspicious patterns found in {}", path.display());
    } else {
        for f in &findings {
            println!("{}", f.render());
        }
        let total = findings.len();
        println!("\n{total} finding(s), {blocking} blocking (high/critical)");
    }
    if blocking > 0 {
        bail!("content scan found {blocking} blocking finding(s)");
    }
    Ok(())
}
