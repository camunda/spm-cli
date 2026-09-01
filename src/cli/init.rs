use crate::manifest::Manifest;
use crate::scope::Scope;
use crate::vendor;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

pub(super) fn init(scope: &Scope, targets: Vec<String>) -> Result<()> {
    // Validate --target before anything else so typos are caught regardless of
    // whether the project is already initialized — otherwise the idempotent
    // early-return below would silently accept a bogus target in scripts.
    if targets.is_empty() {
        bail!("at least one --target is required");
    }
    for target in &targets {
        vendor::for_target(target)?; // validate targets early
    }
    let dir = scope.manifest_dir()?;
    // The global manifest dir ($SPM_HOME) may not exist yet on a first run.
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Re-running `spm init` in an already-initialized scope is a harmless
    // no-op, not an error: leave the existing manifest untouched and tell the
    // user rather than failing the command.
    if Manifest::exists(&dir) {
        println!(
            "{} already exists — leaving it untouched",
            Manifest::path_in(&dir).display()
        );
        return Ok(());
    }
    let manifest = Manifest {
        targets,
        skills: BTreeMap::new(),
        plugins: BTreeMap::new(),
    };
    manifest.save(&dir)?;
    println!("created {}", Manifest::path_in(&dir).display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::init;
    use crate::scope::Scope;

    /// `clap`'s `--target` always yields at least one element (it defaults to
    /// `["claude"]` and a comma split never produces zero tokens), so the
    /// empty-`targets` guard in `init` can't be reached through the CLI parser
    /// — but the function itself must still reject it defensively when called
    /// directly (e.g. if a future caller stops going through clap).
    #[test]
    fn init_rejects_empty_targets_vec() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "spm-cli-test-init-empty-targets-{}-{nanos}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let err = init(&Scope::Project { root: dir.clone() }, Vec::new()).unwrap_err();
        assert!(
            format!("{err}").contains("at least one --target is required"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
