//! End-to-end tests that drive the real `spm` binary against throwaway git
//! repos.
//! Each test gets an isolated scratch dir and its own `SPM_HOME`, so the global
//! store/vendor areas never leak between tests or into the developer's home.

// Marks the whole harness as test code so clippy's `allow-*-in-tests`
// (clippy.toml) also covers the shared helpers, not only `#[test]` fns.
#![cfg(test)]

mod add;
mod architecture;
mod commands;
mod common;
mod concurrency;
mod docs_drift;
mod doctor;
mod gateways;
mod hygiene;
// Every global-scope test needs HOME to be honored, which only holds on Unix.
#[cfg(unix)]
mod global;
mod json;
mod layout;
mod links;
mod lint_policy;
mod lock_only;
mod locked;
mod outdated;
mod ownership;
mod plugins;
mod policy;
mod protocol;
mod ranges;
mod scan;
mod source;
mod targets;
mod transitive;
