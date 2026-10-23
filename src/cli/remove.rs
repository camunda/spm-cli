use super::sync::save_and_sync;
use crate::manifest::Manifest;
use crate::scope::Scope;
use anyhow::{bail, Result};

pub(super) fn remove(scope: &Scope, name: &str, plugin: bool) -> Result<()> {
    let dir = scope.manifest_dir()?;
    // Held from reading `ai.json` to the last write (see `StateLock`).
    let state_lock = crate::links::StateLock::acquire(scope, false)?;
    let mut manifest = Manifest::load(&dir, scope)?;
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
    save_and_sync(scope, state_lock, &manifest)?;
    println!("removed {kind} {name}");
    Ok(())
}
