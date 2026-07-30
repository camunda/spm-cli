use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const LOCK_FILE: &str = "ai.lock";

/// Generated, VCS-committed pin file (`ai.lock`). Makes installs reproducible:
/// every version selector is resolved down to an immutable commit SHA + store key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lockfile {
    /// Stable, path-independent project id (`spm-xxxxxxxx`), generated once and
    /// then committed. Used to name per-project vendor registrations (e.g. the
    /// Copilot marketplace/plugin) so they stay identical across clones, moves,
    /// and machines — preventing orphaned duplicate registrations.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default)]
    pub skills: BTreeMap<String, LockedSkill>,
}

/// Generate a fresh, kebab-safe project id. Called once when a lockfile has none;
/// the value is then persisted, so uniqueness (not determinism) is what matters.
pub fn generate_id(root: &Path) -> String {
    let seed = format!(
        "{:?}|{:?}|{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH),
        root,
        std::process::id()
    );
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("spm-{:08x}", h & 0xffff_ffff)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedSkill {
    pub git: String,
    /// Human-readable ref that was requested (e.g. "tag:v1.2.0", "branch:main").
    pub reference: String,
    /// Resolved immutable commit.
    pub commit: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    /// Store directory key: `<repo-name>-<url-hash>@<sha>`.
    pub store: String,
}

impl Lockfile {
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(LOCK_FILE)
    }

    pub fn load_or_default(dir: &Path) -> Result<Self> {
        let p = Self::path_in(dir);
        if !p.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&p)?;
        let lock: Self =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        lock.validate()
            .with_context(|| format!("in {}", p.display()))?;
        Ok(lock)
    }

    /// `ai.lock` is committed to the repo and therefore untrusted input. Its
    /// `id`, per-skill `store` keys, and `commit`s all feed directly into
    /// filesystem paths that get recursively deleted and rewritten. Reject any
    /// value that could escape the intended directories before we act on it.
    fn validate(&self) -> Result<()> {
        if !self.id.is_empty() {
            validate_project_id(&self.id)?;
        }
        for (name, l) in &self.skills {
            validate_commit(&l.commit).with_context(|| format!("skill `{name}`"))?;
            validate_store_key(&l.store).with_context(|| format!("skill `{name}`"))?;
            // The store key must be exactly what we would derive; a mismatch means
            // the lockfile was hand-edited to point somewhere else.
            let expected = store_key(&l.git, &l.commit);
            if l.store != expected {
                bail!(
                    "skill `{name}`: store key `{}` does not match `{expected}` derived from git+commit",
                    l.store
                );
            }
        }
        Ok(())
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let p = Self::path_in(dir);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text + "\n").with_context(|| format!("writing {}", p.display()))
    }
}

/// A project id may be used verbatim as a directory name, so restrict it to a
/// safe, separator-free token to keep untrusted `ai.lock` input from escaping
/// the intended paths.
pub fn validate_project_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!("invalid project id `{id}`: expected `[A-Za-z0-9_-]+`");
    }
    Ok(())
}

/// A commit must be a full 40-character lowercase hex SHA. Full SHAs keep the
/// store key unambiguous and let `is_at_commit` compare against the lowercase
/// `rev-parse HEAD` without spurious refetches; they also prevent path-injection
/// through the SHA, which is appended verbatim into the store key.
pub fn validate_commit(commit: &str) -> Result<()> {
    let ok = commit.len() == 40
        && commit
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if !ok {
        bail!("invalid commit `{commit}`: expected a full 40-character lowercase hex SHA");
    }
    Ok(())
}

/// A store key names a single directory beneath the global store root. It must
/// not contain path separators, `..`, or anything that could escape that root.
pub fn validate_store_key(key: &str) -> Result<()> {
    let bad = key.is_empty()
        || key.contains('/')
        || key.contains('\\')
        || key.contains('\0')
        || key.split('@').any(|seg| seg == "." || seg == "..");
    if bad {
        bail!("invalid store key `{key}`: must be a single path component");
    }
    Ok(())
}

/// Derive a filesystem-safe, **bounded-length** store key from a repo URL and
/// commit SHA. The previous scheme sanitized the entire URL into the directory
/// name, which for long URLs (e.g. `file://` paths on CI) pushed the full
/// `.../store/<key>/.git/objects/...` path past Windows' 260-char limit. We now
/// use a short human-readable repo-name hint plus a hash of the full URL, so the
/// key length is bounded regardless of URL length while staying collision-safe.
pub fn store_key(url: &str, sha: &str) -> String {
    // FNV-1a 64-bit hash of the full URL — stable across platforms.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Short readable hint: the trailing repo name, alphanumerics only, capped.
    let hint: String = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(32)
        .collect();
    format!("{hint}-{h:016x}@{sha}")
}

#[cfg(test)]
mod tests {
    use super::{store_key, validate_store_key};

    #[test]
    fn store_key_is_bounded_safe_and_stable() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        // A pathologically long URL (e.g. a `file://` temp path on CI) must still
        // yield a short, filesystem-safe key so paths stay under Windows' limit.
        let long = format!(
            "file://{}/skill",
            "C_Users_runner_AppData_Local_Temp".repeat(20)
        );
        let key = store_key(&long, sha);
        validate_store_key(&key).expect("generated key must pass validation");
        // hint(<=32) + '-' + 16 hex + '@' + 40 sha == 90 max.
        assert!(key.len() <= 90, "key too long: {} ({})", key.len(), key);
        assert!(key.ends_with(&format!("@{sha}")));
        // Deterministic.
        assert_eq!(key, store_key(&long, sha));
        // Distinct URLs do not collide on the same sha.
        assert_ne!(
            store_key("https://github.com/a/repo", sha),
            store_key("https://github.com/b/repo", sha)
        );
    }
}
