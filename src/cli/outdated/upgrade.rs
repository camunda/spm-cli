//! How an upstream bump of a transitive dependency reaches the project: the
//! walk up its requester chain to a locally declared dependency.

use crate::cli::index::DependencyIndex;
use crate::ids::DepName;
use crate::lockfile::Lockfile;
use crate::version::Version;

/// How an upstream selector change propagates up the requester chain to a
/// locally declared dependency. A requester pinned to a tag or commit only
/// sees the change once a new version of it is published *and* its own
/// selector is bumped by its requester, recursively; a branch-tracking
/// requester picks it up through `spm update`, a range-tracking one once it
/// publishes a release inside its range (then `spm update`, no selector edit),
/// and a pinned local dependency through an `ai.json` edit plus `spm install`.
#[derive(Default, Debug, PartialEq)]
pub(super) struct Upgrade {
    /// Pinned transitive requesters to republish, each with the requesters
    /// whose upstream `ai.json` must then bump it, in order up the chain.
    pub(super) republish: Vec<(String, Vec<String>)>,
    /// Pinned, locally declared dependencies whose selector must change in
    /// this project's `ai.json`.
    pub(super) local_edits: Vec<String>,
    /// Some chain ends at a branch-tracking requester (`spm update`).
    pub(super) via_branch: bool,
    /// Requesters tracking a `version` range (name, requirement): a release
    /// inside the range reaches the project through `spm update`.
    pub(super) in_range: Vec<(String, String)>,
}

impl Upgrade {
    pub(super) fn plan(requested_by: &[DepName], lock: &Lockfile, index: &DependencyIndex) -> Self {
        let mut up = Upgrade::default();
        let mut seen = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<&DepName> = requested_by.iter().collect();
        while let Some(req) = queue.pop_front() {
            if !seen.insert(req) {
                continue;
            }
            let locked = lock.get_dependency(req);
            let selector = locked.map(|q| q.reference.version());
            let requesters = index.requesters(req);
            match selector {
                Some(Version::Branch(_)) => up.via_branch = true,
                Some(Version::Range(r)) => up.in_range.push((req.to_string(), r.clone())),
                _ if !requesters.is_empty() => {
                    up.republish.push((
                        req.to_string(),
                        requesters.iter().map(DepName::to_string).collect(),
                    ));
                    queue.extend(requesters);
                }
                _ => up.local_edits.push(req.to_string()),
            }
        }
        up
    }
}

#[cfg(test)]
mod tests {
    use super::Upgrade;
    use crate::ids::dep;
    use std::collections::BTreeMap;

    /// A stale lock may leave `requested_by` on a name `ai.json` now declares
    /// as another kind: the walk up stops at that declaration, which is what
    /// to edit, instead of following its stale parents.
    #[test]
    fn upgrade_walk_stops_at_a_name_declared_as_any_kind() {
        use crate::cli::index::DependencyIndex;
        use crate::lockfile::{tests::entry, Lockfile};
        use crate::manifest::Manifest;
        let manifest = Manifest::parse(
            r#"{"targets":["claude"],"plugins":{"mid":{"git":"u","tag":"v1"}}}"#,
            std::path::Path::new("ai.json"),
        )
        .unwrap();
        let mut mid = entry("https://h/mid.git", 'a', "tag:v1");
        mid.requested_by = vec![dep("top")];
        let lock = Lockfile {
            skills: BTreeMap::from([(dep("mid"), mid)]),
            ..Default::default()
        };
        let up = Upgrade::plan(
            &[dep("mid")],
            &lock,
            &DependencyIndex::new(&manifest, &lock),
        );
        assert!(up.republish.is_empty(), "{:?}", up.republish);
        assert_eq!(up.local_edits, ["mid"]);
    }
}
