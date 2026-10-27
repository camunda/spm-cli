//! Rendering of walk failures: requester chains and version conflicts.

use super::identity::Key;
use crate::ids::{CommitSha, LockReference};

/// One side of a version conflict — the reference/commit an identity was (or
/// would be) resolved at, plus the requester chain that led there. Grouping the
/// two symmetric sides keeps [`conflict_error`] to a readable argument list.
pub(super) struct ConflictSide<'a> {
    pub(super) reference: &'a LockReference,
    pub(super) commit: &'a CommitSha,
    pub(super) chain: &'a [String],
}

pub(super) fn chain_str(chain: &[String]) -> String {
    chain.join(" -> ")
}

pub(super) fn conflict_error(key: &Key, a: ConflictSide, b: ConflictSide) -> anyhow::Error {
    let path_note = key
        .1
        .as_deref()
        .map(|p| format!(" (path `{p}`)"))
        .unwrap_or_default();
    anyhow::anyhow!(
        "version conflict: `{}`{} is required at two different commits:\n  \
         {} @ {} (via {})\n  {} @ {} (via {})\n\
         resolve it by pinning both requesters to the same ref, or removing one dependency",
        key.0,
        path_note,
        a.reference,
        a.commit.short(),
        chain_str(a.chain),
        b.reference,
        b.commit.short(),
        chain_str(b.chain),
    )
}
