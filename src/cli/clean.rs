use super::common::{confirm, human_size};
use crate::links::Links;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::scope::Scope;
use crate::vendor;
use crate::{paths, store};
use anyhow::Result;

pub(super) fn clean(scope: &Scope) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir, scope)?;
    let lock = Lockfile::load_or_default(&dir)?;
    let (managed, managed_plugins) = managed_names(&lock, &Links::load(scope)?);
    let vendors = manifest
        .targets
        .iter()
        .map(|t| vendor::for_target(t))
        .collect::<Result<Vec<_>>>()?;
    // Held until the deletions below are done (see `StateLock`).
    let _state_lock = crate::links::StateLock::acquire(false)?;
    super::sync::guard_working_copies(
        scope,
        &dir,
        &vendors,
        &Default::default(),
        &managed,
        &managed_plugins,
    )?;
    for vendor in &vendors {
        vendor.clean(scope, &managed)?;
        vendor.clean_plugins(scope, &managed_plugins)?;
    }
    println!(
        "cleaned generated {} config for target(s): {}",
        scope.label(),
        manifest.targets.join(", ")
    );
    Ok(())
}

/// The skill and plugin names spm currently owns, per `ai.lock` plus the skills
/// last materialized from linked plugins (see [`Links::managed`]): what a
/// vendor's `clean` / `clean_plugins` must remove.
pub(super) fn managed_names(lock: &Lockfile, links: &Links) -> (Vec<String>, Vec<String>) {
    // Plugin-bundled skills are flattened into the same shared skills dirs as
    // standalone skills (see `sync`'s `previously_managed`), so they must be
    // included here too — otherwise a clean would leave their materialized
    // dirs behind in shared-dir/global vendors.
    let mut managed: Vec<String> = lock.skills.keys().cloned().collect();
    managed.extend(
        lock.plugins
            .values()
            .flat_map(|l| l.bundled_skills.iter().cloned()),
    );
    managed.extend(links.managed.iter().cloned());
    managed.sort();
    managed.dedup();
    let managed_plugins: Vec<String> = lock.plugins.keys().cloned().collect();
    (managed, managed_plugins)
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
