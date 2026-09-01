use super::sync::sync;
use crate::manifest::Manifest;
use crate::scope::Scope;
use anyhow::{bail, Result};

pub(super) fn remove(scope: &Scope, name: &str, plugin: bool) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir)?;
    let (removed, kind) = if plugin {
        (manifest.plugins.remove(name).is_some(), "plugin")
    } else {
        (manifest.skills.remove(name).is_some(), "skill")
    };
    if !removed {
        bail!(
            "no {kind} named `{name}` in {}",
            Manifest::path_in(&dir).display()
        );
    }
    manifest.save(&dir)?;
    sync(scope, false, None)?;
    println!("removed {kind} {name}");
    Ok(())
}
