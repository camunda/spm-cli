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
    /// Pinned full-plugin dependencies, parallel to `skills`. A plugin
    /// resolves to a single commit + store checkout like a skill; its `path`
    /// just points at a plugin root rather than a single skill folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<DepName, LockedPlugin>,
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

/// What every `ai.lock` entry pins, whatever its kind: a version selector
/// resolved down to an immutable commit and its store checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub git: String,
    /// The version selector that was requested, stored as its label (e.g.
    /// "tag:v1.2.0", "branch:main").
    pub reference: LockReference,
    /// Resolved immutable commit.
    pub commit: CommitSha,
    /// For a `version:<requirement>` reference: the concrete semver tag the
    /// requirement resolved to (e.g. `v1.4.1`), so `list`/`outdated` can show
    /// `^1.2 → v1.4.1` without a remote lookup. Absent for every other selector.
    pub resolved_tag: Option<String>,
    pub path: Option<String>,
    /// Store directory key: `<repo-name>-<url-hash>@<sha>`.
    pub store: StoreKey,
}

/// A pinned skill: declared in `ai.json`, or pulled in by a nested one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "EntryRepr", into = "EntryRepr")]
pub struct LockedSkill {
    pub pin: Pin,
    pub origin: Origin,
    /// With `resolveTransitive`: other version selectors (`reference` labels)
    /// that nested `ai.json` edges request for this same skill identity and that
    /// resolved to this same `commit`. The deduplicated graph keeps one entry
    /// per identity, so this is the pin evidence that lets those edges be
    /// re-checked without a remote lookup (and lets `install --locked` accept
    /// them). Rebuilt on every sync; sorted and deduplicated.
    pub also_satisfies: Vec<Satisfied>,
}

/// Why a skill is in the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Declared in `ai.json` (written without `requested_by`).
    Direct,
    /// Resolved transitively, for the dependencies that pulled it in.
    Transitive(Requesters),
}

/// Provenance of a transitively-resolved skill: the local name(s) of the
/// dependency(ies) that pulled it in (a top-level manifest key, or another
/// transitive skill's synthesized name). Never empty — an entry without a
/// requester is [`Origin::Direct`]. The walk keeps it sorted and deduplicated
/// (see [`Requesters::normalize`]) so the committed `ai.lock` is stable across
/// runs regardless of traversal order; a loaded one is kept as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requesters(Vec<DepName>);

impl Requesters {
    /// Requested by `first` alone.
    pub fn new(first: DepName) -> Self {
        Self(vec![first])
    }

    /// Requested by `name` as well.
    pub fn push(&mut self, name: DepName) {
        self.0.push(name);
    }

    /// Sort and deduplicate, as the lock records them.
    pub fn normalize(&mut self) {
        self.0.sort();
        self.0.dedup();
    }

    pub fn as_slice(&self) -> &[DepName] {
        &self.0
    }
}

/// A pinned full-plugin dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "EntryRepr", into = "EntryRepr")]
pub struct LockedPlugin {
    pub pin: Pin,
    /// The names of the skills the plugin bundles, recorded at install time so
    /// `spm status` (which never touches the store) still knows the plugin's
    /// materialized component set.
    pub bundled_skills: Vec<DepName>,
}

/// The on-disk shape of every `ai.lock` entry, whatever its kind: one flat
/// object whose kind-specific lists are omitted when empty. The single
/// serialization of [`LockedSkill`] and [`LockedPlugin`], so both keep the
/// historical key set and order.
#[derive(Serialize, Deserialize)]
struct EntryRepr {
    git: String,
    reference: LockReference,
    commit: CommitSha,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    resolved_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    path: Option<String>,
    store: StoreKey,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bundled_skills: Vec<DepName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requested_by: Vec<DepName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    also_satisfies: Vec<Satisfied>,
}

impl EntryRepr {
    fn new(pin: Pin) -> Self {
        let Pin {
            git,
            reference,
            commit,
            resolved_tag,
            path,
            store,
        } = pin;
        Self {
            git,
            reference,
            commit,
            resolved_tag,
            path,
            store,
            bundled_skills: Vec::new(),
            requested_by: Vec::new(),
            also_satisfies: Vec::new(),
        }
    }

    fn pin(&self) -> Pin {
        Pin {
            git: self.git.clone(),
            reference: self.reference.clone(),
            commit: self.commit.clone(),
            resolved_tag: self.resolved_tag.clone(),
            path: self.path.clone(),
            store: self.store.clone(),
        }
    }
}

/// A field of the other kind (a hand-edited lock) was parsed and validated
/// like any other, then dropped: nothing reads it, and the `--locked`
/// whole-lock comparison (on the text as written) still reports it as drift.
impl TryFrom<EntryRepr> for LockedSkill {
    type Error = anyhow::Error;
    fn try_from(r: EntryRepr) -> Result<Self> {
        let pin = r.pin();
        let origin = if r.requested_by.is_empty() {
            Origin::Direct
        } else {
            Origin::Transitive(Requesters(r.requested_by))
        };
        Ok(Self {
            pin,
            origin,
            also_satisfies: r.also_satisfies,
        })
    }
}

impl From<LockedSkill> for EntryRepr {
    fn from(l: LockedSkill) -> Self {
        let mut r = Self::new(l.pin);
        if let Origin::Transitive(Requesters(by)) = l.origin {
            r.requested_by = by;
        }
        r.also_satisfies = l.also_satisfies;
        r
    }
}

impl TryFrom<EntryRepr> for LockedPlugin {
    type Error = anyhow::Error;
    fn try_from(r: EntryRepr) -> Result<Self> {
        // Dropped evidence is held to the rule it would be as a skill's. Serde
        // renders the error with `{}`, so the chain is flattened here.
        validate_evidence(&r.also_satisfies).map_err(|e| anyhow::anyhow!("{e:#}"))?;
        Ok(Self {
            pin: r.pin(),
            bundled_skills: r.bundled_skills,
        })
    }
}

impl From<LockedPlugin> for EntryRepr {
    fn from(l: LockedPlugin) -> Self {
        let mut r = Self::new(l.pin);
        r.bundled_skills = l.bundled_skills;
        r
    }
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

impl Pin {
    /// The requested selector as shown to humans: the `reference` label, plus
    /// the concrete tag a `version` range resolved to (`version:^1.2 → v1.4.1`).
    pub fn display_reference(&self) -> String {
        match &self.resolved_tag {
            Some(tag) => format!("{} → {tag}", self.reference),
            None => self.reference.label(),
        }
    }

    /// Whether `other` pins the very same content: same git + path + commit
    /// (whatever selector each was resolved from).
    pub fn same_content(&self, other: &Pin) -> bool {
        self.git == other.git && self.path == other.path && self.commit == other.commit
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

impl LockedSkill {
    /// A skill `ai.json` declares, pinned at `pin`.
    pub fn direct(pin: Pin, also_satisfies: Vec<Satisfied>) -> Self {
        Self {
            pin,
            origin: Origin::Direct,
            also_satisfies,
        }
    }

    /// A skill pulled in by `requested_by`, pinned at `pin`.
    pub fn transitive(pin: Pin, requested_by: Requesters) -> Self {
        Self {
            pin,
            origin: Origin::Transitive(requested_by),
            also_satisfies: Vec::new(),
        }
    }

    /// Record that `name` pulls this skill in too: a direct one becomes
    /// transitive.
    pub fn add_requester(&mut self, name: DepName) {
        match &mut self.origin {
            Origin::Direct => self.origin = Origin::Transitive(Requesters::new(name)),
            Origin::Transitive(by) => by.push(name),
        }
    }

    /// Who pulled this skill in; empty for a direct one.
    pub fn requested_by(&self) -> &[DepName] {
        match &self.origin {
            Origin::Direct => &[],
            Origin::Transitive(by) => by.as_slice(),
        }
    }

    pub fn is_transitive(&self) -> bool {
        matches!(self.origin, Origin::Transitive(_))
    }
}

/// One `ai.lock` entry of either kind, borrowed: what a kind-agnostic reader
/// (lookups, validation, reports) sees.
#[derive(Debug, Clone, Copy)]
pub enum LockEntry<'a> {
    Skill(&'a LockedSkill),
    Plugin(&'a LockedPlugin),
}

impl<'a> LockEntry<'a> {
    pub fn kind(self) -> DependencyKind {
        match self {
            LockEntry::Skill(_) => DependencyKind::Skill,
            LockEntry::Plugin(_) => DependencyKind::Plugin,
        }
    }

    pub fn pin(self) -> &'a Pin {
        match self {
            LockEntry::Skill(s) => &s.pin,
            LockEntry::Plugin(p) => &p.pin,
        }
    }

    /// A transitive skill's requesters; empty for any other entry.
    pub fn requested_by(self) -> &'a [DepName] {
        match self {
            LockEntry::Skill(s) => s.requested_by(),
            LockEntry::Plugin(_) => &[],
        }
    }

    /// A skill's `also_satisfies` evidence; empty for a plugin.
    pub fn also_satisfies(self) -> &'a [Satisfied] {
        match self {
            LockEntry::Skill(s) => &s.also_satisfies,
            LockEntry::Plugin(_) => &[],
        }
    }

    /// A plugin's bundled skills; empty for a skill.
    pub fn bundled_skills(self) -> &'a [DepName] {
        match self {
            LockEntry::Skill(_) => &[],
            LockEntry::Plugin(p) => &p.bundled_skills,
        }
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
        for (name, entry) in self.dependencies() {
            validate_entry(name, entry)?;
        }
        Ok(())
    }

    /// The pin of the `kind` dependency `name`.
    pub fn get(&self, kind: DependencyKind, name: &str) -> Option<LockEntry<'_>> {
        match kind {
            DependencyKind::Skill => self.skills.get(name).map(LockEntry::Skill),
            DependencyKind::Plugin => self.plugins.get(name).map(LockEntry::Plugin),
        }
    }

    /// Every pin: skills (direct and transitive), then plugins, each in name
    /// order.
    pub fn dependencies(&self) -> impl Iterator<Item = (&DepName, LockEntry<'_>)> {
        let skills = self.skills.iter().map(|(n, l)| (n, LockEntry::Skill(l)));
        let plugins = self.plugins.iter().map(|(n, l)| (n, LockEntry::Plugin(l)));
        skills.chain(plugins)
    }

    /// The pin for `name`, whichever kind it is (a skill wins should a
    /// hand-edited lock repeat a name across kinds).
    pub fn get_dependency(&self, name: &str) -> Option<LockEntry<'_>> {
        DependencyKind::ALL
            .into_iter()
            .find_map(|kind| self.get(kind, name))
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

/// Validate one locked dependency. Names, commits, store keys and references
/// were already validated by their types while parsing; what remains are the
/// cross-field rules. The store key must be exactly what we would derive from
/// the entry's git+commit — a mismatch means the committed (untrusted)
/// `ai.lock` was hand-edited to point elsewhere.
fn validate_entry(name: &DepName, entry: LockEntry) -> Result<()> {
    let (kind, l) = (entry.kind(), entry.pin());
    let expected = StoreKey::derive(&l.git, &l.commit);
    if l.store != expected {
        bail!(
            "{kind} `{name}`: store key `{}` does not match `{expected}` derived from git+commit",
            l.store
        );
    }
    validate_resolved_tag(&l.reference, l.resolved_tag.as_deref())
        .with_context(|| format!("{kind} `{name}`"))?;
    validate_evidence(entry.also_satisfies()).with_context(|| format!("{kind} `{name}`"))
}

/// Every `also_satisfies` entry follows the resolved-tag rule of a primary
/// reference.
fn validate_evidence(also_satisfies: &[Satisfied]) -> Result<()> {
    for s in also_satisfies {
        let r = &s.reference;
        validate_resolved_tag(r, s.resolved_tag.as_deref())
            .with_context(|| format!("also_satisfies entry `{r}`"))?;
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

    /// A valid pin of `git` at `c` repeated into a SHA.
    pub(crate) fn pin(git: &str, c: char, reference: &str) -> Pin {
        let commit = CommitSha::try_from(c.to_string().repeat(40)).unwrap();
        Pin {
            git: git.into(),
            reference: LockReference::try_from(reference).unwrap(),
            store: StoreKey::derive(git, &commit),
            commit,
            resolved_tag: None,
            path: None,
        }
    }

    /// A direct skill pinned like [`pin`].
    pub(crate) fn entry(git: &str, c: char, reference: &str) -> LockedSkill {
        LockedSkill::direct(pin(git, c, reference), Vec::new())
    }

    /// A plugin pinned like [`pin`], bundling nothing.
    pub(crate) fn plugin(git: &str, c: char, reference: &str) -> LockedPlugin {
        LockedPlugin {
            pin: pin(git, c, reference),
            bundled_skills: Vec::new(),
        }
    }

    /// A transitive skill requested by `by`.
    pub(crate) fn requested(mut l: LockedSkill, by: &[&str]) -> LockedSkill {
        let mut by = by.iter().map(|n| dep(n));
        let mut requesters = Requesters::new(by.next().unwrap());
        by.for_each(|n| requesters.push(n));
        l.origin = Origin::Transitive(requesters);
        l
    }

    /// Name lookups span both maps; iteration yields skills before plugins.
    #[test]
    fn dependencies_span_both_kinds_skills_first() {
        let lock = Lockfile {
            skills: BTreeMap::from([
                (dep("zeta"), entry("https://h/s.git", 'a', "branch:main")),
                (dep("alpha"), entry("https://h/s.git", 'b', "branch:main")),
            ]),
            plugins: BTreeMap::from([(dep("kit"), plugin("https://h/p.git", 'c', "tag:v1"))]),
            ..Default::default()
        };
        let all: Vec<_> = lock
            .dependencies()
            .map(|(n, l)| (l.kind(), n.to_string(), l.pin().git.clone()))
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
        assert_eq!(
            lock.get_dependency("kit").unwrap().pin().git,
            "https://h/p.git"
        );
        assert!(lock.contains_dependency("alpha") && lock.contains_dependency("kit"));
        assert!(!lock.contains_dependency("missing"));
    }

    /// Validation errors name the map the bad entry lives in.
    #[test]
    fn validate_names_the_kind_of_a_bad_entry() {
        let mut l = plugin("https://example.com/repo.git", 'b', "branch:main");
        l.pin.store = StoreKey::try_from("not-derived").unwrap();
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
        l.pin.store = StoreKey::try_from("totally-not-the-derived-key").unwrap();
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
        let mut leaf = requested(entry(git, 'e', "branch:main"), &["top"]);
        leaf.pin.path = Some("leaf".into());
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
        assert_eq!(back.skills["top__leaf-deadbeef"].requested_by(), ["top"]);
        assert_eq!(back.skills["direct"].origin, Origin::Direct);
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
        assert_eq!(lock.skills["greet"].origin, Origin::Direct);
    }

    /// The typed fields serialize back to exactly the text they were parsed
    /// from: `ai.lock` stays byte-compatible across the move to newtypes.
    #[test]
    fn lockfile_round_trips_byte_for_byte() {
        let git = "https://example.com/repo.git";
        let mut plugin = plugin(git, 'c', "commit:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC");
        plugin.bundled_skills = vec![dep("b-skill"), dep("a-skill")];
        let mut range = requested(entry(git, 'd', "version:>=1.2, <2"), &["pdf"]);
        range.pin.resolved_tag = Some("v1.4.1".into());
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

    /// An `ai.lock` as spm has always written it — every entry kind, every
    /// optional field — loads into typed entries and saves back to the very
    /// same bytes.
    #[test]
    fn existing_lock_text_round_trips_through_typed_entries() {
        let git = "https://example.com/repo.git";
        let (a, b, c) = (
            CommitSha::try_from("a".repeat(40)).unwrap(),
            CommitSha::try_from("b".repeat(40)).unwrap(),
            CommitSha::try_from("c".repeat(40)).unwrap(),
        );
        let (sa, sb, sc) = (
            StoreKey::derive(git, &a),
            StoreKey::derive(git, &b),
            StoreKey::derive(git, &c),
        );
        let text = format!(
            r#"{{
  "id": "spm-0123abcd",
  "skills": {{
    "pdf": {{
      "git": "{git}",
      "reference": "tag:v1.0.0",
      "commit": "{a}",
      "store": "{sa}",
      "also_satisfies": [
        "branch:main"
      ]
    }},
    "pdf__leaf-0a1b2c3d": {{
      "git": "{git}",
      "reference": "version:^1.2",
      "commit": "{b}",
      "resolved_tag": "v1.4.1",
      "path": "leaf",
      "store": "{sb}",
      "requested_by": [
        "pdf",
        "zip"
      ],
      "also_satisfies": [
        {{
          "reference": "version:^1",
          "resolved_tag": "v1.4.1"
        }}
      ]
    }}
  }},
  "plugins": {{
    "ds": {{
      "git": "{git}",
      "reference": "branch:main",
      "commit": "{c}",
      "path": "pkg",
      "store": "{sc}",
      "bundled_skills": [
        "b-skill",
        "a-skill"
      ]
    }}
  }}
}}
"#
        );
        let lock = load_text(&text).unwrap();
        assert_eq!(lock.skills["pdf"].origin, Origin::Direct);
        assert_eq!(
            lock.skills["pdf__leaf-0a1b2c3d"].requested_by(),
            [dep("pdf"), dep("zip")]
        );
        assert_eq!(
            lock.plugins["ds"].bundled_skills,
            [dep("b-skill"), dep("a-skill")]
        );
        assert_eq!(crate::jsonfile::to_text(&lock).unwrap(), text);
    }

    /// A hand-edited field of the other kind (a plugin's `requested_by` or
    /// `also_satisfies`, a skill's `bundled_skills`) is still validated, then
    /// dropped: no entry can carry another kind's data.
    #[test]
    fn fields_of_the_other_kind_are_validated_then_dropped() {
        use serde_json::json;
        let as_plugin = |text: String| text.replacen(r#"{"skills":"#, r#"{"plugins":"#, 1);
        let lock = load_text(&entry_json("ok", "bundled_skills", json!(["x"]))).unwrap();
        assert_eq!(lock.skills["ok"], entry_from_json());
        for (field, value) in [
            ("requested_by", json!(["top"])),
            ("also_satisfies", json!(["branch:dev"])),
        ] {
            let lock = load_text(&as_plugin(entry_json("ok", field, value))).unwrap();
            assert!(lock.plugins["ok"].bundled_skills.is_empty(), "{field}");
            assert_eq!(lock.plugins["ok"].pin, entry_from_json().pin, "{field}");
            let text = crate::jsonfile::to_text(&lock).unwrap();
            assert!(!text.contains(field), "{field}: {text}");
        }
        for (field, value, needle) in [
            ("requested_by", json!(["a/b"]), "invalid skill name"),
            (
                "also_satisfies",
                json!([{"reference": "version:^1", "resolved_tag": "v2.0.0"}]),
                "does not satisfy",
            ),
            (
                "also_satisfies",
                json!(["version:^1"]),
                "has no resolved_tag",
            ),
        ] {
            let err = load_text(&as_plugin(entry_json("ok", field, value))).unwrap_err();
            assert!(format!("{err:#}").contains(needle), "{field}: {err:#}");
        }
    }

    /// The entry [`entry_json`] writes, without overrides.
    fn entry_from_json() -> LockedSkill {
        entry("https://example.com/repo.git", 'a', "branch:main")
    }

    fn with_reference(reference: &str, resolved_tag: Option<&str>) -> Lockfile {
        let mut l = entry("https://example.com/repo.git", 'a', reference);
        l.pin.resolved_tag = resolved_tag.map(str::to_string);
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
        // A counter, not the clock, names the dir: parallel tests call this.
        let dir = crate::manifest::testutil::Scratch::new();
        std::fs::write(dir.0.join(LOCK_FILE), text).unwrap();
        Lockfile::load_or_default(&dir.0)
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
            lock.skills["pdf"].pin.display_reference(),
            "version:^1.2 → v1.4.1"
        );
        let lock = with_reference("tag:v1.4.1", None);
        assert_eq!(lock.skills["pdf"].pin.display_reference(), "tag:v1.4.1");
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

        fn pinned() -> impl Strategy<Value = Pin> {
            (
                ".{0,16}",
                "[0-9a-f]{40}",
                reference(),
                prop::option::of(".{1,8}"),
            )
                .prop_map(|(git, sha, (reference, resolved_tag), path)| {
                    let commit = CommitSha::try_from(sha).unwrap();
                    Pin {
                        store: StoreKey::derive(&git, &commit),
                        git,
                        reference,
                        commit,
                        resolved_tag,
                        path,
                    }
                })
        }

        fn names() -> impl Strategy<Value = Vec<DepName>> {
            prop::collection::vec(dep_name(), 0..3)
        }

        fn skill() -> impl Strategy<Value = LockedSkill> {
            (pinned(), names(), prop::collection::vec(reference(), 0..3)).prop_map(
                |(pin, requested_by, also)| LockedSkill {
                    pin,
                    origin: if requested_by.is_empty() {
                        Origin::Direct
                    } else {
                        Origin::Transitive(Requesters(requested_by))
                    },
                    also_satisfies: also
                        .into_iter()
                        .map(|(reference, resolved_tag)| Satisfied {
                            reference,
                            resolved_tag,
                        })
                        .collect(),
                },
            )
        }

        fn plugin() -> impl Strategy<Value = LockedPlugin> {
            (pinned(), names()).prop_map(|(pin, bundled_skills)| LockedPlugin {
                pin,
                bundled_skills,
            })
        }

        fn lockfile() -> impl Strategy<Value = Lockfile> {
            (
                prop_oneof![Just(String::new()), "[A-Za-z0-9_-]{1,10}"],
                prop::collection::btree_map(dep_name(), skill(), 0..3),
                prop::collection::btree_map(dep_name(), plugin(), 0..3),
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

            /// Every lock spm can write loads back unchanged: same text, and
            /// every entry keeps its kind and origin.
            #[test]
            fn saved_locks_load_back(lock in lockfile()) {
                let loaded = load_text(&text(&lock));
                prop_assert_eq!(loaded.as_ref().map(text), Some(text(&lock)));
                let loaded = loaded.unwrap();
                prop_assert_eq!(&loaded.skills, &lock.skills);
                prop_assert_eq!(&loaded.plugins, &lock.plugins);
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
