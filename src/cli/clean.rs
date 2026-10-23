use super::common::{confirm, human_size};
use crate::links::Links;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::owned::Owned;
use crate::scope::Scope;
use crate::vendor;
use crate::{paths, store};
use anyhow::Result;

pub(super) fn clean(scope: &Scope) -> Result<()> {
    let dir = scope.manifest_dir()?;
    // Held from reading `ai.json` and link state until the deletions below are
    // done (see `StateLock`).
    let _state_lock = crate::links::StateLock::acquire(scope, false)?;
    let manifest = Manifest::load(&dir, scope)?;
    let lock = Lockfile::load_or_default(&dir)?;
    let (managed, managed_plugins) = managed_names(scope, &lock, &Links::load(scope)?)?;
    let vendors = manifest
        .targets
        .iter()
        .map(|t| vendor::for_target(t))
        .collect::<Result<Vec<_>>>()?;
    super::sync::guard_working_copies(
        scope,
        &dir,
        &vendors,
        &Default::default(),
        &managed,
        &managed_plugins,
        None,
    )?;
    for vendor in &vendors {
        vendor.clean(scope, &managed)?;
        vendor.clean_plugins(scope, &managed_plugins)?;
    }
    Owned::remove(scope)?;
    println!(
        "cleaned generated {} config for target(s): {}",
        scope.label(),
        manifest.targets.join(", ")
    );
    Ok(())
}

/// The skill and plugin names spm currently owns: everything `ai.lock`
/// installs, whatever the local ownership record still lists (installed before
/// a lock-only write or a pulled `ai.lock` dropped it), and the skills last
/// materialized from linked plugins (see [`Links::managed`]). What a vendor's
/// `clean` / `clean_plugins` must remove.
pub(super) fn managed_names(
    scope: &Scope,
    lock: &Lockfile,
    links: &Links,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut owned = Owned::from_lock(lock);
    owned.extend(Owned::load(scope)?);
    owned.skills.extend(links.managed.iter().cloned());
    Ok((
        owned.skills.into_iter().collect(),
        owned.plugins.into_iter().collect(),
    ))
}

/// `spm prune`: wipe the whole global store (`$SPM_HOME/store`, default
/// `~/.spm/store`). Unlike `clean` (project-local vendor config), this is global
/// and shared across every project, so it always confirms first unless `--yes`
/// is passed. The store is a pure cache — anything removed re-fetches on the
/// next `install`.
pub(super) fn prune(yes: bool) -> Result<()> {
    let dir = paths::store_dir()?;
    // Gate on total emptiness, not the checkout count: a store holding only
    // stray files still has something to remove, and prune removes everything.
    if store::is_empty()? {
        println!("store is empty — nothing to prune ({})", dir.display());
        return Ok(());
    }
    if !yes {
        // Show the checkout count (a cheap shallow read) in the prompt, but
        // defer the recursive size walk until after confirmation — answering
        // `n` on a large store should be instant, not pay for a full du.
        println!(
            "store holds {} cached checkout(s) in {}",
            store::checkout_count()?,
            dir.display()
        );
        if !confirm("Remove everything from the store?")? {
            println!("aborted — nothing removed");
            return Ok(());
        }
    }
    // Size only now — once we know we're deleting — so `freed ~X` stays
    // accurate without slowing down an abort.
    let stats = store::stats()?;
    store::remove_all()?;
    println!(
        "pruned {} cached checkout(s), freed ~{}",
        stats.entries,
        human_size(stats.bytes)
    );
    Ok(())
}
