//! End-to-end tests that drive the real `spm` binary against throwaway git
//! repos.
//! Each test gets an isolated scratch dir and its own `SPM_HOME`, so the global
//! store/vendor areas never leak between tests or into the developer's home.

mod add;
mod commands;
mod common;
mod concurrency;
mod docs_drift;
mod doctor;
// Every global-scope test needs HOME to be honored, which only holds on Unix.
#[cfg(unix)]
mod global;
mod json;
mod layout;
mod links;
mod lock_only;
mod locked;
mod outdated;
mod ownership;
mod plugins;
mod policy;
mod protocol;
mod ranges;
mod scan;
mod targets;
mod transitive;
