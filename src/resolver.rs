use crate::git;
use crate::lockfile::{store_key, LockedSkill};
use crate::manifest::{SkillSpec, Version};
use anyhow::Result;

/// Resolve a single skill spec to a locked entry (ref -> commit SHA + store key).
pub fn resolve(spec: &SkillSpec) -> Result<LockedSkill> {
    let version = spec.version()?;
    let reference = version.label();
    let commit = match &version {
        Version::Commit(c) => c.clone(),
        Version::Tag(t) => {
            // Query the tag and its peel; ls_remote prefers the dereferenced commit.
            git::ls_remote(
                &spec.git,
                &[&format!("refs/tags/{t}"), &format!("refs/tags/{t}^{{}}")],
            )?
        }
        Version::Branch(b) => git::ls_remote(&spec.git, &[&format!("refs/heads/{b}")])?,
    };
    // Normalize to lowercase so the pin matches git's lowercase `rev-parse HEAD`
    // (otherwise an uppercase SHA is treated as stale and refetched every run).
    let commit = commit.to_lowercase();

    Ok(LockedSkill {
        git: spec.git.clone(),
        reference,
        store: store_key(&spec.git, &commit),
        path: spec.path.clone(),
        commit,
    })
}
