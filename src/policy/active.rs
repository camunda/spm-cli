//! The process-wide active policy and the [`gate`] every source access
//! passes through.

use super::Policy;
use anyhow::Result;
use std::sync::{Arc, Mutex};

/// The policy the current command activated (see [`activate`]).
static ACTIVE: Mutex<Option<Arc<Policy>>> = Mutex::new(None);

/// Make `policy` (see `Manifest::policy`) the one [`gate`] enforces for the
/// rest of the process. Returns it for up-front checks.
pub fn activate(policy: Policy) -> Arc<Policy> {
    let policy = Arc::new(policy);
    *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(policy.clone());
    policy
}

/// The backstop every source access goes through. Uses the activated policy;
/// if no command activated one, the machine layer still applies, so a code path
/// that forgets to activate can never bypass the machine policy.
pub fn gate(git: &str) -> Result<()> {
    let active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match active {
        Some(policy) => policy.check(git),
        None => fallback()?.check(git),
    }
}

/// Whether any policy layer is in force for this process — the active one, or
/// else the machine layer (a machine policy that fails to load counts as in
/// force: fail closed). Lets the git layer tighten transport behavior (no HTTP
/// redirects) only when a policy exists, keeping no-policy behavior unchanged.
pub fn restricts() -> bool {
    let active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match active {
        Some(policy) => !policy.layers.is_empty(),
        None => fallback().map_or(true, |p| !p.layers.is_empty()),
    }
}

/// The policy [`gate`] applies when no command activated one: the machine
/// layer. In-process unit tests (which drive `git`/`store` helpers directly and
/// never activate a policy) get no layer instead, so a policy configured on the
/// developer's or CI machine can't change their outcome. The real binary — and
/// so every end-to-end test of the machine layer — always takes the first arm.
#[cfg(not(test))]
fn fallback() -> Result<Policy> {
    Policy::machine()
}

#[cfg(test)]
fn fallback() -> Result<Policy> {
    Ok(Policy::default())
}
