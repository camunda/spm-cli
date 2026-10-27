use crate::ids::{CommitSha, DepName, LockReference, StoreKey};
use crate::manifest::{DependencyKind, SkillSpec};
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
    pub skills: BTreeMap<DepName, LockedSkill>,
    /// Pinned full-plugin dependencies, parallel to `skills`. Same locked shape
    /// (a plugin resolves to a single commit + store checkout); the `path` just
    /// points at a plugin root rather than a single skill folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<DepName, LockedSkill>,
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
    /// The version selector that was requested, stored as its label (e.g.
    /// "tag:v1.2.0", "branch:main").
    pub reference: LockReference,
    /// Resolved immutable commit.
    pub commit: CommitSha,
    /// For a `version:<requirement>` reference: the concrete semver tag the
    /// requirement resolved to (e.g. `v1.4.1`), so `list`/`outdated` can show
    /// `^1.2 → v1.4.1` without a remote lookup. Absent for every other selector.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resolved_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    /// Store directory key: `<repo-name>-<url-hash>@<sha>`.
    pub store: StoreKey,
    /// For a full-plugin lock entry: the names of the skills the plugin bundles,
    /// recorded at install time so `spm status` (which never touches the store)
    /// still knows the plugin's materialized component set. Always empty for an
    /// ordinary `skills` entry, so it is omitted from those.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bundled_skills: Vec<DepName>,
    /// Provenance for a transitively-resolved skill: the local name(s) of the
    /// direct dependency(ies) that pulled it in (a top-level manifest key, or
    /// another transitive skill's synthesized name). Empty for a normal,
    /// directly-declared skill, so it is omitted from those. Always kept sorted
    /// and deduplicated so the committed `ai.lock` is stable across runs
    /// regardless of traversal order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_by: Vec<DepName>,
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
#[serde(try_from = "SatisfiedRepr", into = "SatisfiedRepr")]
pub struct Satisfied {
    pub reference: LockReference,
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

/// The reference is parsed after the untagged match (rather than typed in
/// `SatisfiedRepr`), so an invalid label reports why instead of serde's generic
/// "did not match any variant".
impl TryFrom<SatisfiedRepr> for Satisfied {
    type Error = anyhow::Error;
    fn try_from(r: SatisfiedRepr) -> Result<Self> {
        let (reference, resolved_tag) = match r {
            SatisfiedRepr::Label(reference) => (reference, None),
            SatisfiedRepr::Tagged {
                reference,
                resolved_tag,
            } => (reference, Some(resolved_tag)),
        };
        // No `.context` here: serde renders the error with `{}`, which would
        // show only the outermost message.
        let reference = LockReference::try_from(reference)?;
        Ok(Self {
            reference,
            resolved_tag,
        })
    }
}

impl From<Satisfied> for SatisfiedRepr {
    fn from(s: Satisfied) -> Self {
        let reference = s.reference.label();
        match s.resolved_tag {
            None => Self::Label(reference),
            Some(resolved_tag) => Self::Tagged {
                reference,
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
            None => self.reference.label(),
        }
    }

    /// Compare this pin against the request it would satisfy. `None` when the
    /// pin was resolved from exactly `spec`'s git + selector + path, so it can
    /// be reused without re-resolving; otherwise a human-readable description of
    /// what changed. The single canonical reuse rule for direct skills, plugins
    /// and transitive children.
    pub fn drift_from(&self, spec: &SkillSpec) -> Result<Option<String>> {
        let reference = LockReference::from(spec.version()?);
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
        let lock: Self = crate::jsonfile::load(&p)?;
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
        for kind in DependencyKind::ALL {
            validate_locked_map(self.deps(kind), kind)?;
        }
        Ok(())
    }

    /// The map holding pins of `kind`.
    pub fn deps(&self, kind: DependencyKind) -> &BTreeMap<DepName, LockedSkill> {
        match kind {
            DependencyKind::Skill => &self.skills,
            DependencyKind::Plugin => &self.plugins,
        }
    }

    /// The map holding pins of `kind`, for editing.
    pub fn deps_mut(&mut self, kind: DependencyKind) -> &mut BTreeMap<DepName, LockedSkill> {
        match kind {
            DependencyKind::Skill => &mut self.skills,
            DependencyKind::Plugin => &mut self.plugins,
        }
    }

    /// Every pin with its kind: skills (direct and transitive), then plugins,
    /// each in name order.
    pub fn dependencies(&self) -> impl Iterator<Item = (DependencyKind, &DepName, &LockedSkill)> {
        DependencyKind::ALL
            .into_iter()
            .flat_map(move |kind| self.deps(kind).iter().map(move |(n, l)| (kind, n, l)))
    }

    /// The pin for `name`, whichever kind it is (a skill wins should a
    /// hand-edited lock repeat a name across kinds).
    pub fn get_dependency(&self, name: &str) -> Option<&LockedSkill> {
        DependencyKind::ALL
            .into_iter()
            .find_map(|kind| self.deps(kind).get(name))
    }

    /// Whether `name` is pinned as a skill or a plugin.
    pub fn contains_dependency(&self, name: &str) -> bool {
        self.get_dependency(name).is_some()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let p = Self::path_in(dir);
        crate::jsonfile::save(&p, self)
    }
}

/// Validate one locked-dependency map (`skills` or `plugins`). `kind` only
/// shapes the error text. Names, commits, store keys and references were
/// already validated by their types while parsing; what remains are the
/// cross-field rules. The store key must be exactly what we would derive from
/// the entry's git+commit — a mismatch means the committed (untrusted)
/// `ai.lock` was hand-edited to point elsewhere.
fn validate_locked_map(map: &BTreeMap<DepName, LockedSkill>, kind: DependencyKind) -> Result<()> {
    for (name, l) in map {
        let expected = StoreKey::derive(&l.git, &l.commit);
        if l.store != expected {
            bail!(
                "{kind} `{name}`: store key `{}` does not match `{expected}` derived from git+commit",
                l.store
            );
        }
        validate_resolved_tag(&l.reference, l.resolved_tag.as_deref())
            .with_context(|| format!("{kind} `{name}`"))?;
        for s in &l.also_satisfies {
            let r = &s.reference;
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
fn validate_resolved_tag(reference: &LockReference, resolved_tag: Option<&str>) -> Result<()> {
    use crate::version::{parse_version_req, semver_of_tag, Version};
    match (reference.version(), resolved_tag) {
        (Version::Range(req), Some(tag)) => {
            let req = parse_version_req(req)?;
            match semver_of_tag(tag) {
                Some(v) if req.matches(&v) => Ok(()),
                Some(_) => bail!("resolved_tag `{tag}` does not satisfy `{reference}`"),
                None => bail!("resolved_tag `{tag}` is not a semver tag"),
            }
        }
        (Version::Range(_), None) => bail!("`{reference}` has no resolved_tag"),
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ids::dep;

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

    /// A valid entry for `git` pinned at `c` repeated into a SHA.
    pub(crate) fn entry(git: &str, c: char, reference: &str) -> LockedSkill {
        let commit = CommitSha::try_from(c.to_string().repeat(40)).unwrap();
        LockedSkill {
            git: git.into(),
            reference: LockReference::try_from(reference).unwrap(),
            store: StoreKey::derive(git, &commit),
            commit,
            resolved_tag: None,
            path: None,
            bundled_skills: Vec::new(),
            requested_by: Vec::new(),
            also_satisfies: Vec::new(),
        }
    }

    /// Name lookups span both maps; iteration yields skills before plugins.
    #[test]
    fn dependencies_span_both_kinds_skills_first() {
        let lock = Lockfile {
            skills: BTreeMap::from([
                (dep("zeta"), entry("https://h/s.git", 'a', "branch:main")),
                (dep("alpha"), entry("https://h/s.git", 'b', "branch:main")),
            ]),
            plugins: BTreeMap::from([(dep("kit"), entry("https://h/p.git", 'c', "tag:v1"))]),
            ..Default::default()
        };
        let all: Vec<_> = lock
            .dependencies()
            .map(|(k, n, l)| (k, n.to_string(), l.git.clone()))
            .collect();
        assert_eq!(
            all,
            [
                (
                    DependencyKind::Skill,
                    "alpha".into(),
                    "https://h/s.git".into()
                ),
                (
                    DependencyKind::Skill,
                    "zeta".into(),
                    "https://h/s.git".into()
                ),
                (
                    DependencyKind::Plugin,
                    "kit".into(),
                    "https://h/p.git".into()
                ),
            ]
        );
        assert_eq!(lock.get_dependency("kit").unwrap().git, "https://h/p.git");
        assert!(lock.contains_dependency("alpha") && lock.contains_dependency("kit"));
        assert!(!lock.contains_dependency("missing"));
    }

    /// Validation errors name the map the bad entry lives in.
    #[test]
    fn validate_names_the_kind_of_a_bad_entry() {
        let mut l = entry("https://example.com/repo.git", 'b', "branch:main");
        l.store = StoreKey::try_from("not-derived").unwrap();
        let lock = Lockfile {
            plugins: BTreeMap::from([(dep("kit"), l)]),
            ..Default::default()
        };
        let err = format!("{}", lock.validate().unwrap_err());
        assert!(err.starts_with("plugin `kit`: store key"), "{err}");
    }

    /// A lockfile whose `store` key doesn't match what `StoreKey::derive`
    /// would produce for its git+commit is untrusted/hand-edited and must be
    /// rejected, even when the key itself is a valid single path component.
    #[test]
    fn validate_rejects_store_key_mismatch() {
        let mut l = entry("https://example.com/repo.git", 'b', "branch:main");
        l.store = StoreKey::try_from("totally-not-the-derived-key").unwrap();
        let lock = Lockfile {
            skills: BTreeMap::from([(dep("greet"), l)]),
            ..Default::default()
        };
        let err = lock.validate().unwrap_err();
        assert!(format!("{err}").contains("does not match"), "{err}");
    }

    /// A transitively-resolved skill's `requested_by` provenance round-trips
    /// through serialization, and a directly-declared skill (empty vec) omits it
    /// entirely so ordinary lockfiles stay unchanged.
    #[test]
    fn requested_by_round_trips_and_is_omitted_when_empty() {
        let git = "https://example.com/repo.git";
        let mut leaf = entry(git, 'e', "branch:main");
        leaf.path = Some("leaf".into());
        leaf.requested_by = vec![dep("top")];
        let lock = Lockfile {
            skills: BTreeMap::from([
                (dep("direct"), entry(git, 'e', "branch:main")),
                (dep("top__leaf-deadbeef"), leaf),
            ]),
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
        assert_eq!(back.skills["top__leaf-deadbeef"].requested_by, ["top"]);
        assert!(back.skills["direct"].requested_by.is_empty());
    }

    /// An older `ai.lock` written before the provenance field existed loads
    /// cleanly, defaulting `requested_by` to empty (backward compatibility).
    #[test]
    fn lockfile_without_requested_by_loads() {
        let git = "https://example.com/repo.git";
        let sha = CommitSha::try_from("f".repeat(40)).unwrap();
        let text = format!(
            r#"{{"skills":{{"greet":{{"git":"{git}","reference":"branch:main","commit":"{sha}","store":"{}"}}}}}}"#,
            StoreKey::derive(git, &sha)
        );
        let lock = load_text(&text).unwrap();
        assert!(lock.skills["greet"].requested_by.is_empty());
    }

    /// The typed fields serialize back to exactly the text they were parsed
    /// from: `ai.lock` stays byte-compatible across the move to newtypes.
    #[test]
    fn lockfile_round_trips_byte_for_byte() {
        let git = "https://example.com/repo.git";
        let mut plugin = entry(git, 'c', "commit:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC");
        plugin.bundled_skills = vec![dep("b-skill"), dep("a-skill")];
        let mut range = entry(git, 'd', "version:>=1.2, <2");
        range.resolved_tag = Some("v1.4.1".into());
        range.requested_by = vec![dep("pdf")];
        range.also_satisfies = vec![
            Satisfied {
                reference: LockReference::try_from("branch:feat/x:y").unwrap(),
                resolved_tag: None,
            },
            Satisfied {
                reference: LockReference::try_from("version:^1").unwrap(),
                resolved_tag: Some("v1.4.1".into()),
            },
        ];
        let lock = Lockfile {
            id: "spm-0123abcd".into(),
            skills: BTreeMap::from([
                (dep("pdf"), entry(git, 'a', "tag:v1.0.0")),
                (dep("pdf__leaf-0a1b2c3d"), range),
            ]),
            plugins: BTreeMap::from([(dep("ds"), plugin)]),
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("spm-lockfile-bytes-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        lock.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(LOCK_FILE)).unwrap();
        let back = Lockfile::load_or_default(&dir).unwrap();
        back.save(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(LOCK_FILE)).unwrap(), text);
        std::fs::remove_dir_all(&dir).unwrap();
        // The labels and names are written as plain strings, not structured.
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["skills"]["pdf"]["reference"], "tag:v1.0.0");
        assert_eq!(
            v["skills"]["pdf__leaf-0a1b2c3d"]["also_satisfies"],
            serde_json::json!([
                "branch:feat/x:y",
                {"reference": "version:^1", "resolved_tag": "v1.4.1"}
            ])
        );
        assert_eq!(
            v["plugins"]["ds"]["bundled_skills"],
            serde_json::json!(["b-skill", "a-skill"])
        );
    }

    fn with_reference(reference: &str, resolved_tag: Option<&str>) -> Lockfile {
        let mut l = entry("https://example.com/repo.git", 'a', reference);
        l.resolved_tag = resolved_tag.map(str::to_string);
        Lockfile {
            skills: BTreeMap::from([(dep("pdf"), l)]),
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
                    reference: LockReference::try_from("branch:main").unwrap(),
                    resolved_tag: None,
                },
                Satisfied {
                    reference: LockReference::try_from("version:^1").unwrap(),
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
                reference: LockReference::try_from(reference).unwrap(),
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

    /// Load `text` as an on-disk `ai.lock`, exactly as commands do.
    fn load_text(text: &str) -> Result<Lockfile> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("spm-lockfile-text-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LOCK_FILE), text).unwrap();
        let out = Lockfile::load_or_default(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        out
    }

    /// One `skills` entry as JSON, with `field` overridden to `value`
    /// (`key` overrides the map key).
    fn entry_json(key: &str, field: &str, value: serde_json::Value) -> String {
        let git = "https://example.com/repo.git";
        let sha = CommitSha::try_from("a".repeat(40)).unwrap();
        let mut entry = serde_json::json!({
            "git": git,
            "reference": "branch:main",
            "commit": sha,
            "store": StoreKey::derive(git, &sha),
        });
        if !field.is_empty() {
            entry[field] = value;
        }
        serde_json::json!({ "skills": { key: entry } }).to_string()
    }

    /// Every untrusted value in a hand-edited `ai.lock` that reaches a path or
    /// a selector is rejected on load, whatever layer catches it.
    #[test]
    fn load_rejects_tampered_values() {
        use serde_json::json;
        load_text(&entry_json("greet", "", json!(null))).expect("baseline entry loads");
        for (key, field, value, needle) in [
            ("../escape", "", json!(null), "invalid skill name"),
            ("con", "", json!(null), "reserved device name"),
            ("ok", "commit", json!("A".repeat(40)), "invalid commit"),
            ("ok", "commit", json!("abc123"), "invalid commit"),
            ("ok", "store", json!("../x"), "invalid store key"),
            ("ok", "store", json!("a/b@c"), "invalid store key"),
            (
                "ok",
                "store",
                json!("/home/victim/.config/autostart"),
                "invalid store key",
            ),
            ("ok", "store", json!("other-0@x"), "does not match"),
            (
                "ok",
                "bundled_skills",
                json!(["../evil"]),
                "invalid skill name",
            ),
            ("ok", "requested_by", json!(["a/b"]), "invalid skill name"),
            ("ok", "also_satisfies", json!(["rev:abc"]), "rev:abc"),
            ("ok", "also_satisfies", json!(["no-colon"]), "no-colon"),
        ] {
            let err = load_text(&entry_json(key, field, value)).unwrap_err();
            let err = format!("{err:#}");
            assert!(err.contains(needle), "{key}/{field}: {err}");
        }
        // The project id names a directory, so a forged one never loads.
        let err = load_text(r#"{"id":"../../evil","skills":{}}"#).unwrap_err();
        assert!(format!("{err:#}").contains("invalid project id"), "{err:#}");
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

    mod props {
        use super::*;
        use crate::manifest::testutil::{dep_name, edited, Scratch};
        use proptest::prelude::*;

        /// A selector label and the tag it needs: none, except a `version`
        /// range, which records a tag satisfying it.
        fn reference() -> impl Strategy<Value = (LockReference, Option<String>)> {
            prop_oneof![
                ("tag|branch|commit", ".{0,8}").prop_map(|(kind, value)| {
                    let label = format!("{kind}:{value}");
                    (LockReference::try_from(label).unwrap(), None)
                }),
                prop::sample::select(
                    &[
                        ("^1.2", "v1.4.1"),
                        ("~1.4.0", "1.4.9"),
                        (">=1.2, <2", "v1.2.0"),
                        ("*", "0.1.0"),
                    ][..]
                )
                .prop_map(|(req, tag)| {
                    let label = format!("version:{req}");
                    (LockReference::try_from(label).unwrap(), Some(tag.into()))
                }),
            ]
        }

        fn locked() -> impl Strategy<Value = LockedSkill> {
            let names = || prop::collection::vec(dep_name(), 0..3);
            (
                (".{0,16}", "[0-9a-f]{40}", reference()),
                (prop::option::of(".{1,8}"), names(), names()),
                prop::collection::vec(reference(), 0..3),
            )
                .prop_map(
                    |(
                        (git, sha, (reference, resolved_tag)),
                        (path, bundled_skills, requested_by),
                        also,
                    )| {
                        let commit = CommitSha::try_from(sha).unwrap();
                        LockedSkill {
                            store: StoreKey::derive(&git, &commit),
                            git,
                            reference,
                            commit,
                            resolved_tag,
                            path,
                            bundled_skills,
                            requested_by,
                            also_satisfies: also
                                .into_iter()
                                .map(|(reference, resolved_tag)| Satisfied {
                                    reference,
                                    resolved_tag,
                                })
                                .collect(),
                        }
                    },
                )
        }

        fn lockfile() -> impl Strategy<Value = Lockfile> {
            let deps = || prop::collection::btree_map(dep_name(), locked(), 0..3);
            (
                prop_oneof![Just(String::new()), "[A-Za-z0-9_-]{1,10}"],
                deps(),
                deps(),
            )
                .prop_map(|(id, skills, plugins)| Lockfile {
                    id,
                    skills,
                    plugins,
                })
        }

        /// The text `save` writes for `lock`.
        fn text(lock: &Lockfile) -> String {
            crate::jsonfile::to_text(lock).unwrap()
        }

        /// Load `ai.lock` holding `contents`; an accepted lock saves and
        /// reloads to the same text.
        fn load_text(contents: &str) -> Option<Lockfile> {
            let dir = Scratch::new();
            std::fs::write(Lockfile::path_in(&dir.0), contents).unwrap();
            let lock = Lockfile::load_or_default(&dir.0).ok()?;
            lock.save(&dir.0).unwrap();
            let reloaded = Lockfile::load_or_default(&dir.0).unwrap();
            assert_eq!(text(&reloaded), text(&lock));
            Some(lock)
        }

        proptest! {
            #![proptest_config(crate::proptest_config(128))]

            /// Every lock spm can write loads back unchanged.
            #[test]
            fn saved_locks_load_back(lock in lockfile()) {
                let loaded = load_text(&text(&lock));
                prop_assert_eq!(loaded.as_ref().map(text), Some(text(&lock)));
            }

            /// A hand-edited or hostile `ai.lock` is an error or a lock, never
            /// a panic; whatever is accepted round-trips.
            #[test]
            fn edited_locks_load_or_fail_cleanly(
                contents in lockfile().prop_flat_map(|lock| edited(text(&lock))),
            ) {
                load_text(&contents);
            }
        }
    }
}
