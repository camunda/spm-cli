use anyhow::{bail, Result};
use std::path::Path;

/// `spm scan [path]` — run the content scanner over a path (a single file or a
/// directory) and print every finding. Exits non-zero when any blocking
/// (high/critical) finding is present so it can serve as a CI gate on skill
/// sources.
pub(super) fn scan_cmd(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("path does not exist: {}", path.display());
    }
    let findings = crate::scan::scan_path(path)?;
    if findings.is_empty() {
        println!("no suspicious patterns found in {}", path.display());
        return Ok(());
    }
    let blocking = findings.iter().filter(|f| f.severity.blocks()).count();
    for f in &findings {
        println!("{}", f.render());
    }
    let total = findings.len();
    println!("\n{total} finding(s), {blocking} blocking (high/critical)");
    if blocking > 0 {
        bail!("content scan found {blocking} blocking finding(s)");
    }
    Ok(())
}
