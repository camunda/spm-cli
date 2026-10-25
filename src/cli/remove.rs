use super::session::{Access, ScopeSession};
use super::sync::{save_and_sync, Apply};
use crate::manifest::{DependencyKind, Manifest};
use crate::scope::Scope;
use anyhow::{bail, Result};

pub(super) fn remove(scope: &Scope, name: &str, plugin: bool) -> Result<()> {
    // Held from reading `ai.json` to the last write.
    let session = ScopeSession::open(scope, Access::Write)?;
    let mut manifest = session.manifest()?;
    let kind = DependencyKind::from_plugin_flag(plugin);
    if manifest.deps_mut(kind).remove(name).is_none() {
        bail!(
            "no {kind} named `{name}` in {}",
            Manifest::path_in(session.dir()).display()
        );
    }
    save_and_sync(session, &manifest, Apply::Install)?;
    println!("removed {kind} {name}");
    Ok(())
}
