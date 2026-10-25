use super::session::{Access, ScopeSession};
use super::sync::{install_command, sync_with_links, unlink_command};
use crate::links::record_path;
use crate::manifest::Manifest;
use crate::scope::Scope;
use anyhow::{bail, Context, Result};
use std::path::Path;

/// `spm link <name> <dir>`: materialize the declared skill or plugin `name`
/// from the local directory `dir` (working tree, uncommitted edits included)
/// instead of its locked commit. `ai.json` and `ai.lock` are left untouched.
pub(super) fn link(scope: &Scope, cwd: &Path, name: &str, dir: &str) -> Result<()> {
    // Before reading anything: the declaration checked below must still hold
    // when the sync runs (e.g. not be removed by a `spm remove` in between).
    let session = ScopeSession::open(scope, Access::Links)?;
    let manifest = session.manifest()?;
    let kind = if manifest.skills.contains_key(name) {
        "skill"
    } else if manifest.plugins.contains_key(name) {
        "plugin"
    } else {
        bail!(
            "no skill or plugin named `{name}` in {} — `spm link` overrides an \
             already-declared dependency; add it first with `spm add`",
            Manifest::path_in(session.dir()).display()
        );
    };
    let target = cwd.join(dir);
    let meta = std::fs::metadata(&target)
        .with_context(|| format!("linked directory `{dir}` not found"))?;
    if !meta.is_dir() {
        bail!("`{dir}` is not a directory");
    }
    let mut links = session.links()?;
    let recorded = record_path(scope, cwd, dir)?;
    links.links.insert(name.to_string(), recorded.clone());
    sync_with_links(session, links)?;
    println!(
        "linked {kind} {name} → {recorded} (ai.json and ai.lock unchanged; re-run `{}` after \
         edits, `{}` to restore the locked version)",
        install_command(scope),
        unlink_command(scope, name)
    );
    Ok(())
}

/// `spm unlink <name>` / `spm unlink --all`: drop local overrides and
/// re-materialize the locked versions.
pub(super) fn unlink(scope: &Scope, name: Option<&str>, all: bool) -> Result<()> {
    let session = ScopeSession::open(scope, Access::Links)?;
    let mut links = session.links()?;
    let removed: Vec<String> = if all {
        std::mem::take(&mut links.links).into_keys().collect()
    } else {
        let name = name.context("name a link to remove, or pass --all")?;
        if links.links.remove(name).is_none() {
            bail!("`{name}` is not linked");
        }
        vec![name.to_string()]
    };
    if removed.is_empty() {
        println!("no links");
        return Ok(());
    }
    sync_with_links(session, links)?;
    println!("unlinked {}", removed.join(", "));
    Ok(())
}
