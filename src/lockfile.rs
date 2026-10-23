use crate::manifest::SkillSpec;
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
    /// Pinned full-plugin dependencies, parallel to `skills`. Same locked shape
    /// (a plugin resolves to a single commit + store checkout); the `path` just
    /// points at a plugin root rather than a single skill folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, LockedSkill>,
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
    /// For a `version:<requirement>` reference: the concrete semver tag the
    /// requirement resolved to (e.g. `v1.4.1`), so `list`/`outdated` can show
    /// `^1.2 → v1.4.1` without a remote lookup. Absent for every other selector.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resolved_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    /// Store directory key: `<repo-name>-<url-hash>@<sha>`.
    pub store: String,
    /// For a full-plugin lock entry: the names of the skills the plugin bundles,
    /// recorded at install time so `spm status` (which never touches the store)
    /// still knows the plugin's materialized component set. Always empty for an
    /// ordinary `skills` entry, so it is omitted from those.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bundled_skills: Vec<String>,
    /// Provenance for a transitively-resolved skill: the local name(s) of the
    /// direct dependency(ies) that pulled it in (a top-level manifest key, or
    /// another transitive skill's synthesized name). Empty for a normal,
    /// directly-declared skill, so it is omitted from those. Always kept sorted
    /// and deduplicated so the committed `ai.lock` is stable across runs
    /// regardless of traversal order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_by: Vec<String>,
    /// With `resolveTransitive`: other version selectors (`reference` labels)
    /// that nested `ai.json` edges request for this same skill identity and that
    /// resolved to this same `commit`. The deduplicated graph keeps one entry
    /// per identity, so this is the pin evidence that lets those edges be
    /// re-checked without a remote lookup (and lets `install --locked` accept
    /// them). Rebuilt on every sync; sorted and deduplicated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_satisfies: Vec<Satisfied>,
}

/// One `also_satisfies` entry: a selector (`reference` label) verified to
/// resolve to the entry's commit, plus — for a `version` range — the tag it
/// resolved to, so `outdated` can tell when the range would move or that tag
/// was re-pointed. Written as the bare label when no tag is recorded, keeping
/// the historical shape of non-range evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "SatisfiedRepr", into = "SatisfiedRepr")]
pub struct Satisfied {
    pub reference: String,
    pub resolved_tag: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SatisfiedRepr {
    Label(String),
    Tagged {
        reference: String,
        resolved_tag: String,
    },
}

impl From<SatisfiedRepr> for Satisfied {
    fn from(r: SatisfiedRepr) -> Self {
        match r {
            SatisfiedRepr::Label(reference) => Self {
                reference,
                resolved_tag: None,
            },
            SatisfiedRepr::Tagged {
                reference,
                resolved_tag,
            } => Self {
                reference,
                resolved_tag: Some(resolved_tag),
            },
        }
    }
}

impl From<Satisfied> for SatisfiedRepr {
    fn from(s: Satisfied) -> Self {
        match s.resolved_tag {
            None => Self::Label(s.reference),
            Some(resolved_tag) => Self::Tagged {
                reference: s.reference,
                resolved_tag,
            },
        }
    }
}

impl LockedSkill {
    /// The requested selector as shown to humans: the `reference` label, plus
    /// the concrete tag a `version` range resolved to (`version:^1.2 → v1.4.1`).
    pub fn display_reference(&self) -> String {
        match &self.resolved_tag {
            Some(tag) => format!("{} → {tag}", self.reference),
            None => self.reference.clone(),
        }
    }

    /// Compare this pin against the request it would satisfy. `None` when the
    /// pin was resolved from exactly `spec`'s git + selector + path, so it can
    /// be reused without re-resolving; otherwise a human-readable description of
    /// what changed. The single canonical reuse rule for direct skills, plugins
    /// and transitive children.
    pub fn drift_from(&self, spec: &SkillSpec) -> Result<Option<String>> {
        let reference = spec.version()?.label();
        let mut changes = Vec::new();
        if self.git != spec.git {
            changes.push(format!("git `{}` -> `{}`", self.git, spec.git));
        }
        if self.reference != reference {
            changes.push(format!("`{}` -> `{reference}`", self.reference));
        }
        if self.path != spec.path {
            let show = |p: &Option<String>| p.as_deref().unwrap_or("<repo root>").to_string();
            changes.push(format!(
                "path `{}` -> `{}`",
                show(&self.path),
                show(&spec.path)
            ));
        }
        Ok((!changes.is_empty()).then(|| changes.join(", ")))
    }
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
        validate_locked_map(&self.skills, "skill")?;
        validate_locked_map(&self.plugins, "plugin")?;
        Ok(())
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let p = Self::path_in(dir);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text + "\n").with_context(|| format!("writing {}", p.display()))
    }
}

/// Validate one locked-dependency map (`skills` or `plugins`). `kind` only
/// shapes the error text. Every entry must carry a full commit SHA and a store
/// key that is exactly what we would derive from its git+commit — a mismatch
/// means the committed (untrusted) `ai.lock` was hand-edited to point elsewhere.
fn validate_locked_map(map: &BTreeMap<String, LockedSkill>, kind: &str) -> Result<()> {
    for (name, l) in map {
        // The map key names a directory that `spm status`/vendor path checks
        // join onto their skills dir (`skills_dir.join(name)`), so a hand-edited
        // `ai.lock` key with `/`, `..`, etc. must be rejected up front — same
        // guarantee manifest parsing gives.
        crate::manifest::validate_skill_name(name)
            .with_context(|| format!("{kind} name in ai.lock"))?;
        validate_commit(&l.commit).with_context(|| format!("{kind} `{name}`"))?;
        validate_store_key(&l.store).with_context(|| format!("{kind} `{name}`"))?;
        let expected = store_key(&l.git, &l.commit);
        if l.store != expected {
            bail!(
                "{kind} `{name}`: store key `{}` does not match `{expected}` derived from git+commit",
                l.store
            );
        }
        // Bundled-skill names flow into the same vendor path joins via `status`,
        // so validate each one just like a top-level skill name.
        for s in &l.bundled_skills {
            crate::manifest::validate_skill_name(s)
                .with_context(|| format!("{kind} `{name}`: bundled skill name"))?;
        }
        // `requested_by` provenance names are rendered by `spm list`/`status`
        // and identify other lock entries; a hand-edited `ai.lock` could inject
        // a path-separator name here too, so hold it to the same rule.
        for r in &l.requested_by {
            crate::manifest::validate_skill_name(r)
                .with_context(|| format!("{kind} `{name}`: requested_by name"))?;
        }
        validate_resolved_tag(&l.reference, l.resolved_tag.as_deref())
            .with_context(|| format!("{kind} `{name}`"))?;
        for s in &l.also_satisfies {
            let r = &s.reference;
            if crate::manifest::Version::from_label(r).is_none() {
                bail!("{kind} `{name}`: also_satisfies entry `{r}` is not a version selector");
            }
            validate_resolved_tag(r, s.resolved_tag.as_deref())
                .with_context(|| format!("{kind} `{name}`: also_satisfies entry `{r}`"))?;
        }
    }
    Ok(())
}

/// A `version:<requirement>` pin must record the semver tag it resolved to,
/// and that tag must satisfy the requirement; any other selector must not carry
/// one. A hand-edited `ai.lock` therefore cannot make `list`/`outdated` claim a
/// tag the range could never have picked.
fn validate_resolved_tag(reference: &str, resolved_tag: Option<&str>) -> Result<()> {
    use crate::manifest::{parse_version_req, semver_of_tag, Version};
    match (Version::from_label(reference), resolved_tag) {
        (Some(Version::Range(req)), Some(tag)) => {
            let req = parse_version_req(&req)?;
            match semver_of_tag(tag) {
                Some(v) if req.matches(&v) => Ok(()),
                Some(_) => bail!("resolved_tag `{tag}` does not satisfy `{reference}`"),
                None => bail!("resolved_tag `{tag}` is not a semver tag"),
            }
        }
        (Some(Version::Range(_)), None) => bail!("`{reference}` has no resolved_tag"),
        (_, Some(tag)) => bail!(
            "resolved_tag `{tag}` is only valid for a `version:` reference, not `{reference}`"
        ),
        (_, None) => Ok(()),
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
    use super::*;

    /// A lockfile with no `id` (the serde default, empty string) must load
    /// fine — `validate` skips `validate_project_id` entirely when `id` is
    /// empty, since a freshly-`init`ed-but-never-synced project has none yet.
    #[test]
    fn load_or_default_accepts_missing_id() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "spm-lockfile-test-noid-{}-{nanos}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LOCK_FILE), r#"{"skills":{}}"#).unwrap();
        let lock = Lockfile::load_or_default(&dir).unwrap();
        assert!(lock.id.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_commit_rejects_short_or_uppercase_or_non_hex() {
        assert!(validate_commit(&"a".repeat(40)).is_ok());
        assert!(validate_commit("abc123").is_err(), "too short");
        assert!(
            validate_commit(&"A".repeat(40)).is_err(),
            "uppercase must be rejected (normalize before validating)"
        );
        assert!(
            validate_commit(&"g".repeat(40)).is_err(),
            "non-hex chars must be rejected"
        );
    }

    /// A lockfile whose `store` key doesn't match what `store_key(git, commit)`
    /// would derive is untrusted/hand-edited and must be rejected, even when
    /// the key itself is syntactically a valid single path component (so it
    /// isn't already caught by `validate_store_key`).
    #[test]
    fn validate_rejects_store_key_mismatch() {
        let sha = "b".repeat(40);
        let mut skills = BTreeMap::new();
        skills.insert(
            "greet".to_string(),
            LockedSkill {
                git: "https://example.com/repo.git".into(),
                reference: "branch:main".into(),
                commit: sha.clone(),
                resolved_tag: None,
                path: None,
                store: "totally-not-the-derived-key".into(),
                bundled_skills: Vec::new(),
                requested_by: Vec::new(),
                also_satisfies: Vec::new(),
            },
        );
        let lock = Lockfile {
            id: String::new(),
            skills,
            ..Default::default()
        };
        let err = lock.validate().unwrap_err();
        assert!(format!("{err}").contains("does not match"), "{err}");
    }

    /// A hand-edited `ai.lock` could inject a path separator (or `..`) into a
    /// plugin's `bundled_skills`, which `spm status` later joins onto a vendor
    /// skills dir. Reject such names with the same guarantee as manifest names.
    #[test]
    fn validate_rejects_bundled_skill_with_path_separator() {
        let git = "https://example.com/repo.git";
        let sha = "c".repeat(40);
        let mut plugins = BTreeMap::new();
        plugins.insert(
            "myplugin".to_string(),
            LockedSkill {
                git: git.into(),
                reference: "branch:main".into(),
                commit: sha.clone(),
                resolved_tag: None,
                path: None,
                store: store_key(git, &sha),
                bundled_skills: vec!["../evil".into()],
                requested_by: Vec::new(),
                also_satisfies: Vec::new(),
            },
        );
        let lock = Lockfile {
            plugins,
            ..Default::default()
        };
        let err = lock.validate().unwrap_err();
        assert!(format!("{err:#}").contains("invalid skill name"), "{err:#}");
    }

    /// A hand-edited lock map *key* (skill or plugin name) with a path separator
    /// is equally dangerous — `status` joins it onto the skills dir — so it must
    /// be rejected on load.
    #[test]
    fn validate_rejects_map_key_with_path_separator() {
        let git = "https://example.com/repo.git";
        let sha = "d".repeat(40);
        let mut skills = BTreeMap::new();
        skills.insert(
            "../escape".to_string(),
            LockedSkill {
                git: git.into(),
                reference: "branch:main".into(),
                commit: sha.clone(),
                resolved_tag: None,
                path: None,
                store: store_key(git, &sha),
                bundled_skills: Vec::new(),
                requested_by: Vec::new(),
                also_satisfies: Vec::new(),
            },
        );
        let lock = Lockfile {
            skills,
            ..Default::default()
        };
        let err = lock.validate().unwrap_err();
        assert!(format!("{err:#}").contains("invalid skill name"), "{err:#}");
    }

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

    /// A transitively-resolved skill's `requested_by` provenance round-trips
    /// through serialization, and a directly-declared skill (empty vec) omits it
    /// entirely so ordinary lockfiles stay unchanged.
    #[test]
    fn requested_by_round_trips_and_is_omitted_when_empty() {
        let git = "https://example.com/repo.git";
        let sha = "e".repeat(40);
        let mut skills = BTreeMap::new();
        skills.insert(
            "direct".to_string(),
            LockedSkill {
                git: git.into(),
                reference: "branch:main".into(),
                commit: sha.clone(),
                resolved_tag: None,
                path: None,
                store: store_key(git, &sha),
                bundled_skills: Vec::new(),
                requested_by: Vec::new(),
                also_satisfies: Vec::new(),
            },
        );
        skills.insert(
            "top__leaf-deadbeef".to_string(),
            LockedSkill {
                git: git.into(),
                reference: "branch:main".into(),
                commit: sha.clone(),
                resolved_tag: None,
                path: Some("leaf".into()),
                store: store_key(git, &sha),
                bundled_skills: Vec::new(),
                requested_by: vec!["top".into()],
                also_satisfies: Vec::new(),
            },
        );
        let lock = Lockfile {
            skills,
            ..Default::default()
        };
        let json = serde_json::to_string_pretty(&lock).unwrap();
        // The empty-provenance direct entry omits the key; the transitive one keeps it.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["skills"]["direct"].get("requested_by").is_none());
        assert_eq!(
            parsed["skills"]["top__leaf-deadbeef"]["requested_by"],
            serde_json::json!(["top"])
        );
        // Round-trips back to the same in-memory shape (and passes validation).
        let back: Lockfile = serde_json::from_str(&json).unwrap();
        back.validate().unwrap();
        assert_eq!(
            back.skills["top__leaf-deadbeef"].requested_by,
            vec!["top".to_string()]
        );
        assert!(back.skills["direct"].requested_by.is_empty());
    }

    /// An older `ai.lock` written before the provenance field existed loads
    /// cleanly, defaulting `requested_by` to empty (backward compatibility).
    #[test]
    fn lockfile_without_requested_by_loads() {
        let git = "https://example.com/repo.git";
        let sha = "f".repeat(40);
        let text = format!(
            r#"{{"skills":{{"greet":{{"git":"{git}","reference":"branch:main","commit":"{sha}","store":"{}"}}}}}}"#,
            store_key(git, &sha)
        );
        let lock: Lockfile = serde_json::from_str(&text).unwrap();
        lock.validate().unwrap();
        assert!(lock.skills["greet"].requested_by.is_empty());
    }

    /// A hand-edited `ai.lock` injecting a path separator into `requested_by`
    /// (which `spm list`/`status` render) is rejected on load.
    #[test]
    fn validate_rejects_requested_by_with_path_separator() {
        let git = "https://example.com/repo.git";
        let sha = "a".repeat(40);
        let mut skills = BTreeMap::new();
        skills.insert(
            "leaf".to_string(),
            LockedSkill {
                git: git.into(),
                reference: "branch:main".into(),
                commit: sha.clone(),
                resolved_tag: None,
                path: None,
                store: store_key(git, &sha),
                bundled_skills: Vec::new(),
                requested_by: vec!["../evil".into()],
                also_satisfies: Vec::new(),
            },
        );
        let lock = Lockfile {
            skills,
            ..Default::default()
        };
        let err = lock.validate().unwrap_err();
        assert!(format!("{err:#}").contains("invalid skill name"), "{err:#}");
    }

    fn with_reference(reference: &str, resolved_tag: Option<&str>) -> Lockfile {
        let git = "https://example.com/repo.git";
        let sha = "a".repeat(40);
        let mut skills = BTreeMap::new();
        skills.insert(
            "pdf".to_string(),
            LockedSkill {
                git: git.into(),
                reference: reference.into(),
                commit: sha.clone(),
                resolved_tag: resolved_tag.map(str::to_string),
                path: None,
                store: store_key(git, &sha),
                bundled_skills: Vec::new(),
                requested_by: Vec::new(),
                also_satisfies: Vec::new(),
            },
        );
        Lockfile {
            skills,
            ..Default::default()
        }
    }

    /// A `version:` pin must carry a resolved tag that satisfies its
    /// requirement; no other selector may carry one.
    #[test]
    fn validate_checks_resolved_tag_against_the_reference() {
        with_reference("version:^1.2", Some("v1.4.1"))
            .validate()
            .unwrap();
        with_reference("version:^1.2", Some("1.2.0"))
            .validate()
            .unwrap();
        for (reference, tag, needle) in [
            ("version:^1.2", None, "has no resolved_tag"),
            ("version:^1.2", Some("v2.0.0"), "does not satisfy"),
            ("version:^1.2", Some("v1.1.9"), "does not satisfy"),
            ("version:^1.2", Some("latest"), "not a semver tag"),
            (
                "version:not-semver",
                Some("v1.2.0"),
                "invalid version requirement",
            ),
            (
                "tag:v1.4.1",
                Some("v1.4.1"),
                "only valid for a `version:` reference",
            ),
        ] {
            let err = with_reference(reference, tag).validate().unwrap_err();
            assert!(format!("{err:#}").contains(needle), "{reference}: {err:#}");
        }
    }

    /// Untagged evidence keeps its historical bare-label shape (so existing
    /// locks round-trip byte-for-byte); a range records its resolved tag.
    #[test]
    fn also_satisfies_round_trips_labels_and_tagged_ranges() {
        let json = r#"["branch:main",{"reference":"version:^1","resolved_tag":"v1.0.0"}]"#;
        let parsed: Vec<Satisfied> = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed,
            [
                Satisfied {
                    reference: "branch:main".into(),
                    resolved_tag: None,
                },
                Satisfied {
                    reference: "version:^1".into(),
                    resolved_tag: Some("v1.0.0".into()),
                },
            ]
        );
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
    }

    /// A secondary range is held to the same resolved-tag rule as a primary.
    #[test]
    fn validate_checks_also_satisfies_resolved_tags() {
        let with_evidence = |reference: &str, tag: Option<&str>| {
            let mut lock = with_reference("branch:main", None);
            lock.skills.get_mut("pdf").unwrap().also_satisfies = vec![Satisfied {
                reference: reference.into(),
                resolved_tag: tag.map(str::to_string),
            }];
            lock
        };
        with_evidence("version:^1", Some("v1.0.0"))
            .validate()
            .unwrap();
        for (reference, tag, needle) in [
            ("version:^1", None, "has no resolved_tag"),
            ("version:^1", Some("v2.0.0"), "does not satisfy"),
            ("tag:v1.0.0", Some("v1.0.0"), "only valid for a `version:`"),
        ] {
            let err = with_evidence(reference, tag).validate().unwrap_err();
            let err = format!("{err:#}");
            assert!(err.contains("also_satisfies entry"), "{err}");
            assert!(err.contains(needle), "{reference}: {err}");
        }
    }

    #[test]
    fn display_reference_appends_the_resolved_tag() {
        let lock = with_reference("version:^1.2", Some("v1.4.1"));
        assert_eq!(
            lock.skills["pdf"].display_reference(),
            "version:^1.2 → v1.4.1"
        );
        let lock = with_reference("tag:v1.4.1", None);
        assert_eq!(lock.skills["pdf"].display_reference(), "tag:v1.4.1");
    }
}
