use super::session::{Access, ScopeSession};
use super::sync::{save_and_sync, Apply};
use crate::manifest::Manifest;
use crate::scope::Scope;
use anyhow::{bail, Result};

pub(super) fn remove(scope: &Scope, name: &str, plugin: bool) -> Result<()> {
    // Held from reading `ai.json` to the last write.
    let session = ScopeSession::open(scope, Access::Write)?;
    let mut manifest = session.manifest()?;
    let (removed, kind) = if plugin {
        (manifest.plugins.remove(name).is_some(), "plugin")
    } else {
        (manifest.skills.remove(name).is_some(), "skill")
    };
    if !removed {
        bail!(
            "no {kind} named `{name}` in {}",
            Manifest::path_in(session.dir()).display()
        );
    }
    save_and_sync(session, &manifest, Apply::Install)?;
    println!("removed {kind} {name}");
    Ok(())
}
