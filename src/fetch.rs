//! The one fetch step of a sync, shared by plugin and skill resolution
//! (`cli`'s sync and the `transitive` walk): how a pinned dependency's
//! checkout is obtained ([`Fetch`]) and the per-entry progress every phase
//! reports through ([`Reporter`]), silent in a read-only check.

use crate::ids::DepName;
use crate::lockfile::Pin;
use crate::manifest::DependencyKind;
use crate::store::{self, Ensured};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fmt;

/// How a sync obtains the store checkout of a pinned dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetch<'a> {
    /// Fetch every checkout missing from the store.
    Online,
    /// A read-only check: only look in the store, never fetch.
    Offline,
    /// A lock-only sync: only the pins this run (re)resolved are fetched;
    /// every pin reused from `ai.lock` is carried over without touching its
    /// remote or the store (see [`Fetch::skips`]).
    Fresh(&'a BTreeSet<DepName>),
}

/// The store checkout [`Fetch::checkout`] found for a dependency.
pub enum Checkout {
    /// Fetched or found by a sync: its content must pass the pre-materialize
    /// gate (`scan::enforce`) before any of it is used.
    Gate(Ensured),
    /// Found by a read-only check, which gates and materializes nothing.
    Inspect(Ensured),
    /// Missing from the store in a read-only check: what it pins cannot be
    /// verified offline.
    Unverified,
}

impl Checkout {
    /// The checkout, unless it is missing.
    pub fn ensured(&self) -> Option<&Ensured> {
        match self {
            Checkout::Gate(e) | Checkout::Inspect(e) => Some(e),
            Checkout::Unverified => None,
        }
    }
}

impl Fetch<'_> {
    /// Whether `name`'s pin is reused by a lock-only sync, so it is not
    /// fetched at all (the caller reports it via [`Reporter::unchanged`]).
    pub fn skips(self, name: &DepName) -> bool {
        matches!(self, Fetch::Fresh(fresh) if !fresh.contains(name))
    }

    /// The store checkout of the `kind` dependency `name` pinned at `locked`:
    /// fetched if missing (reported through `report`), or — offline — looked
    /// up only. Callers decide [`Fetch::skips`] first.
    pub fn checkout(
        self,
        kind: DependencyKind,
        name: &DepName,
        locked: &Pin,
        report: Reporter,
    ) -> Result<Checkout> {
        match self {
            Fetch::Offline => Ok(
                match store::cached(locked)
                    .with_context(|| format!("inspecting {kind} `{name}`"))?
                {
                    Some(ensured) => Checkout::Inspect(ensured),
                    None => Checkout::Unverified,
                },
            ),
            Fetch::Online | Fetch::Fresh(_) => {
                let ensured =
                    store::ensure(locked).with_context(|| format!("fetching {kind} `{name}`"))?;
                let how = if ensured.fetched { "fetched" } else { "cached" };
                report.entry(name, format_args!("{how}"));
                Ok(Checkout::Gate(ensured))
            }
        }
    }
}

/// Where a sync reports its progress (stdout): every line goes through here,
/// so a silent reporter — a read-only check's — prints nothing at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reporter {
    /// Column width per-entry names are padded to; `None` when silent.
    width: Option<usize>,
}

impl Reporter {
    /// Report to stdout, padding per-entry names to `width`.
    pub fn new(width: usize) -> Self {
        Self { width: Some(width) }
    }

    /// Report nothing.
    pub fn silent() -> Self {
        Self { width: None }
    }

    /// One line of progress.
    pub fn say(self, line: fmt::Arguments) {
        emit(self.line(line));
    }

    /// The progress of one entry: its aligned `name`, then `status`.
    pub fn entry(self, name: &str, status: fmt::Arguments) {
        emit(self.entry_line(name, status));
    }

    /// `name` was reused by a lock-only sync, so it was not fetched.
    pub fn unchanged(self, name: &str) {
        self.entry(name, format_args!("unchanged (lock only, not fetched)"));
    }

    fn line(self, line: fmt::Arguments) -> Option<String> {
        self.width.map(|_| line.to_string())
    }

    fn entry_line(self, name: &str, status: fmt::Arguments) -> Option<String> {
        self.width.map(|w| format!("  {name:<w$}  {status}"))
    }
}

fn emit(line: Option<String>) {
    if let Some(line) = line {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::dep;

    /// A silent reporter renders no line of any kind; a loud one pads entries.
    #[test]
    fn a_silent_reporter_renders_nothing() {
        let silent = Reporter::silent();
        assert_eq!(silent.line(format_args!("resolving")), None);
        assert_eq!(silent.entry_line("a", format_args!("cached")), None);
        let loud = Reporter::new(3);
        assert_eq!(
            loud.line(format_args!("resolving")).as_deref(),
            Some("resolving")
        );
        assert_eq!(
            loud.entry_line("a", format_args!("cached")).as_deref(),
            Some("  a    cached")
        );
    }

    /// Only a lock-only sync skips, and only the pins it did not resolve.
    #[test]
    fn only_a_lock_only_fetch_skips_reused_pins() {
        let fresh = BTreeSet::from([dep("new")]);
        for name in [dep("new"), dep("old")] {
            assert!(!Fetch::Online.skips(&name));
            assert!(!Fetch::Offline.skips(&name));
        }
        assert!(!Fetch::Fresh(&fresh).skips(&dep("new")));
        assert!(Fetch::Fresh(&fresh).skips(&dep("old")));
    }
}
