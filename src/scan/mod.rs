//! Deterministic content scanner for materialized skills.
//!
//! Skills are markdown + scripts that Claude/Copilot auto-discover and act on,
//! so a malicious or compromised skill repo could smuggle in a payload that
//! hijacks the agent or exfiltrates secrets — and a human skimming a diff may
//! miss it (hidden via invisible Unicode, buried in a huge file, or split into
//! encoded chunks). Per AGENTS.md's "Automated Verification Over Human Review
//! Gates" policy this module encodes those checks as a deterministic gate rather
//! than relying on manual review.
//!
//! The engine is a flat rule table (pattern → category → severity). It is used
//! two ways: `enforce` runs it as a pre-materialize gate in the sync pipeline
//! (blocking on High/Critical findings), and `scan_path` backs the standalone
//! `spm scan` command. Both share the exact same rules, so the gate and the
//! command can never disagree about what counts as suspicious.
//!
//! Layout:
//! - `rules`: the rule table (id → category → severity), the pattern lists
//!   the rules match against, and one triggering + one benign fixture per rule.
//! - `engine`: walks the tree and applies the file-level and line rules.
//! - `decode`: spots base64/hex blobs that decode to a shell command.
//! - `finding`: severities, categories and a finding's rendering.
//! - `gate`: the pre-materialize gate and its `SPM_ALLOW_SUSPICIOUS` override.

mod decode;
mod engine;
mod finding;
mod gate;
mod rules;
mod testutil;

pub use engine::{scan_path, scan_within};
pub use finding::Finding;
pub use gate::{allow_suspicious, enforce, ALLOW_ENV};
