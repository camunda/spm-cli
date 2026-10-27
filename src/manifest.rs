use crate::ids::DepName;
use crate::policy::Policy;
use crate::scope::Scope;
use crate::version::{parse_version_req, Version};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub const MANIFEST_FILE: &str = "ai.json";

/// Which map of `ai.json`/`ai.lock` a dependency lives in. Skills and plugins
/// share one flat name namespace and the same resolve/pin rules; the kind
/// selects the map and the few kind-specific steps (a plugin's bundled skills,
/// a skill's transitive provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DependencyKind {
    Skill,
    Plugin,
}

impl DependencyKind {
    /// Every kind, in the order dependencies are iterated, resolved and
    /// reported: skills before plugins.
    pub const ALL: [Self; 2] = [Self::Skill, Self::Plugin];

    /// The kind an `add`/`remove` targets: `--plugin` selects plugins.
    pub fn from_plugin_flag(plugin: bool) -> Self {
        if plugin {
            Self::Plugin
        } else {
            Self::Skill
        }
    }

    /// The singular noun used in messages (`skill`, `plugin`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Plugin => "plugin",
        }
    }
}

impl std::fmt::Display for DependencyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The user-authored, VCS-committed declaration file (`ai.json`). Its shape
/// mirrors `schema/ai.schema.json` field for field — unknown keys and explicit
/// `null`s are rejected here too — which the `schema_parity` tests guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Optional editor hint pointing at the schema; kept so saving the
    /// manifest preserves it.
    #[serde(
        rename = "$schema",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub schema: Option<String>,
    /// Target vendors, e.g. `["claude", "copilot"]`.
    pub targets: Vec<String>,
    /// Opt-in gate for transitive dependency resolution. When `true`, spm reads
    /// each resolved skill's own co-located `ai.json` and recursively resolves
    /// the skills it declares. Default `false`: nested manifests are ignored and
    /// behavior is exactly as before. Read **only** from the root project's own
    /// manifest — a dependency's nested `ai.json` cannot re-enable recursion
    /// (that field is rejected there), so a single opt-out at the root is final.
    #[serde(
        default,
        rename = "resolveTransitive",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub resolve_transitive: bool,
    /// Project source allowlist (see `crate::policy`): when set, every skill and
    /// plugin source — direct or transitive — must match one of these patterns
    /// (and the machine policy, if any). Read **only** from the root project's
    /// manifest; a dependency's nested `ai.json` declaring it is rejected.
    #[serde(
        default,
        rename = "allowedSources",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub allowed_sources: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_unique_skills")]
    pub skills: BTreeMap<DepName, SkillSpec>,
    /// Full-plugin dependencies (agents/MCP servers/hooks/scripts + bundled
    /// skills), keyed by local name. Unlike a `skills` entry — which points at a
    /// single `SKILL.md` folder — a plugin `path` points at a Claude Code plugin
    /// root (a dir holding `.claude-plugin/plugin.json`).
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_unique_plugins"
    )]
    pub plugins: BTreeMap<DepName, SkillSpec>,
}

/// Deserialize an optional field that, when present, must hold a value. Serde
/// reads an explicit `null` into an `Option` as absent, a value the schema
/// rejects; with `#[serde(default)]` a missing field is still `None`.
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

/// Deserialize a `T` from a JSON object only. A derived struct impl also
/// accepts a positional array (`["url", "v1"]`), a shape the schema does not
/// have.
struct ObjectOnly<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ObjectOnly<T> {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{value::MapAccessDeserializer, MapAccess, Visitor};
        struct ObjectVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = T;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<M>(self, map: M) -> Result<T, M::Error>
            where
                M: MapAccess<'de>,
            {
                T::deserialize(MapAccessDeserializer::new(map))
            }
        }
        d.deserialize_map(ObjectVisitor(std::marker::PhantomData))
            .map(ObjectOnly)
    }
}

/// Deserialize the `skills` map while rejecting duplicate names. A plain
/// `BTreeMap` (or `serde_json::Value`) silently collapses repeated JSON object
/// keys to last-wins, which would drop an earlier skill without warning. We see
/// both entries only by streaming the map ourselves — so `Manifest::load` must
/// deserialize from the raw text, not from an already-collapsed `Value`.
fn deserialize_unique_skills<'de, D>(d: D) -> Result<BTreeMap<DepName, SkillSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_unique_map(d, DependencyKind::Skill)
}

/// Same duplicate-key rejection as [`deserialize_unique_skills`], for the
/// `plugins` map.
fn deserialize_unique_plugins<'de, D>(d: D) -> Result<BTreeMap<DepName, SkillSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_unique_map(d, DependencyKind::Plugin)
}

/// Stream a `{name: SkillSpec}` map, rejecting duplicate `name` keys. `kind`
/// only shapes the error message. Shared by the `skills` and `plugins` maps so
/// both get identical last-wins-collapse protection.
fn deserialize_unique_map<'de, D>(
    d: D,
    kind: DependencyKind,
) -> Result<BTreeMap<DepName, SkillSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, MapAccess, Visitor};
    struct MapVisitor(DependencyKind);
    impl<'de> Visitor<'de> for MapVisitor {
        type Value = BTreeMap<DepName, SkillSpec>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a map of unique {} names", self.0)
        }
        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let kind = self.0;
            let mut out = BTreeMap::new();
            while let Some(name) = map.next_key::<DepName>()? {
                // Name the entry: serde's own message ("unknown field `x`",
                // "invalid type: null") does not say which one is wrong.
                let ObjectOnly(spec) = map
                    .next_value::<ObjectOnly<SkillSpec>>()
                    .map_err(|e| de::Error::custom(format!("{kind} `{name}`: {e}")))?;
                if out.contains_key(&name) {
                    return Err(de::Error::custom(format!(
                        "duplicate {kind} name `{name}` in the `{kind}s` map; \
                         each {kind} name may appear only once — remove or rename \
                         one of the `{name}` entries"
                    )));
                }
                out.insert(name, spec);
            }
            Ok(out)
        }
    }
    d.deserialize_map(MapVisitor(kind))
}

/// A single skill dependency. Exactly one of tag/branch/commit/version selects
/// the version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSpec {
    pub git: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub tag: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub branch: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub commit: Option<String>,
    /// A semver requirement (`^1.2`, `~1.4`, `>=1.2, <2`) resolved to the
    /// highest matching semver tag. Written as `version` in `ai.json`.
    #[serde(
        rename = "version",
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub range: Option<String>,
    /// Optional subdirectory inside the repo (monorepo of skills).
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub path: Option<String>,
}

impl SkillSpec {
    /// Validate that exactly one version selector is set (and, for `version`,
    /// that it is a valid semver requirement) and return it.
    pub fn version(&self) -> Result<Version> {
        match (&self.tag, &self.branch, &self.commit, &self.range) {
            (Some(t), None, None, None) => Ok(Version::Tag(t.clone())),
            (None, Some(b), None, None) => Ok(Version::Branch(b.clone())),
            (None, None, Some(c), None) => Ok(Version::Commit(c.clone())),
            (None, None, None, Some(r)) => {
                parse_version_req(r).with_context(|| format!("skill `{}`", self.git))?;
                Ok(Version::Range(r.clone()))
            }
            (None, None, None, None) => {
                bail!("skill `{}`: set one of tag/branch/commit/version", self.git)
            }
            _ => bail!(
                "skill `{}`: set only one of tag/branch/commit/version",
                self.git
            ),
        }
    }

    /// Check every field the way the schema's `skill` definition does (plus
    /// the semantic `path`/`version` rules) and return the version selector.
    /// Shared by root and nested manifests: a nested one skips the schema, so
    /// these checks are all that stand between it and git and the store path.
    pub fn validate(&self) -> Result<Version> {
        for (field, value) in [
            ("git", Some(&self.git)),
            ("tag", self.tag.as_ref()),
            ("branch", self.branch.as_ref()),
            ("path", self.path.as_ref()),
        ] {
            if value.is_some_and(|v| v.is_empty()) {
                bail!("`{field}` must be non-empty");
            }
        }
        if let Some(sub) = &self.path {
            validate_subpath(sub)?;
        }
        // The commit is appended to a store path (see `validate_commit_selector`).
        if let Some(commit) = &self.commit {
            validate_commit_selector(commit)?;
        }
        self.version()
    }
}

/// Maximum size (bytes) of a nested dependency `ai.json`. A nested manifest is
/// untrusted and parsed (twice) into memory before any edge budget applies, so
/// the size is bounded up front; this comfortably fits `MAX_EDGES` declarations.
pub const MAX_NESTED_MANIFEST_BYTES: u64 = 256 * 1024;

/// Read a nested manifest, refusing anything over [`MAX_NESTED_MANIFEST_BYTES`].
/// The read itself is capped (not just a metadata check), so a file that grows
/// between check and read still cannot exhaust memory.
fn read_bounded(p: &Path) -> Result<String> {
    use std::io::Read;
    let ctx = || format!("reading nested {MANIFEST_FILE} at {}", p.display());
    let file = std::fs::File::open(p).with_context(ctx)?;
    let mut buf = Vec::new();
    file.take(MAX_NESTED_MANIFEST_BYTES + 1)
        .read_to_end(&mut buf)
        .with_context(ctx)?;
    if buf.len() as u64 > MAX_NESTED_MANIFEST_BYTES {
        bail!(
            "nested {MANIFEST_FILE} at {} is larger than {MAX_NESTED_MANIFEST_BYTES} bytes; \
             refusing to parse it",
            p.display()
        );
    }
    String::from_utf8(buf).with_context(|| {
        format!(
            "nested {MANIFEST_FILE} at {} is not valid UTF-8",
            p.display()
        )
    })
}

/// Reject a `commit` selector that is not a full 40-character hex SHA.
///
/// The root `ai.json` is guarded by the JSON schema, whose `commit` pattern
/// (`^[0-9a-fA-F]{40}$`) is the only thing keeping an untrusted commit string
/// out of the store path: `resolver::resolve` feeds a `commit` selector verbatim
/// into `StoreKey::derive`, which appends it into a directory name that `store::ensure`
/// joins onto the store root and recursively removes/rewrites. A *nested*
/// (`DependencyManifest`) spec bypasses that schema, so a hostile dependency
/// could otherwise smuggle slashes or `..` into the store path via the commit
/// field. Enforce the same shape here (case-insensitively, before the resolver
/// lowercases it) so nested specs get identical protection.
pub fn validate_commit_selector(commit: &str) -> Result<()> {
    if commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid commit `{commit}`: expected a full 40-character hexadecimal SHA");
    }
    Ok(())
}

/// Reject a skill `path` that could escape the fetched repo root. The subdir is
/// joined onto the store checkout (`repo_dir.join(path)`); without this a hostile
/// manifest could use an absolute path or `..` to read arbitrary files off the
/// host (e.g. `/etc`, or a sibling repo's checkout in the shared store).
pub fn validate_subpath(path: &str) -> Result<()> {
    let p = Path::new(path);
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                bail!("invalid path `{path}`: must be relative to the repo root")
            }
            Component::ParentDir => {
                bail!("invalid path `{path}`: `..` components are not allowed")
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    if path.contains('\0') {
        bail!("invalid path `{path}`: must not contain NUL");
    }
    Ok(())
}

impl Manifest {
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(MANIFEST_FILE)
    }

    /// The source policy for this manifest (in `dir`): the machine layer plus
    /// its `allowedSources`.
    pub fn policy(&self, dir: &Path) -> Result<Policy> {
        Policy::load(self.allowed_sources.as_deref(), &Self::path_in(dir))
    }

    /// Load and validate `dir`'s manifest. `scope` only picks the `spm init`
    /// invocation named in the hint when the file is missing.
    pub fn load(dir: &Path, scope: &Scope) -> Result<Self> {
        let p = Self::path_in(dir);
        let text = std::fs::read_to_string(&p).map_err(|e| {
            // Hint only when the file is genuinely absent: for unreadable or
            // unparsable manifests `spm init` would not help.
            let cause = if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!("{e}\nhint: run `{}` to create one", scope.init_command())
            } else {
                anyhow!(e)
            };
            cause.context(format!("no {MANIFEST_FILE} found at {}", p.display()))
        })?;
        Self::parse(&text, &p)
    }

    /// Parse and validate manifest `text` as if read from `p` (used only to
    /// label errors). The single validation path behind [`Manifest::load`], so
    /// an edited in-memory manifest can be checked exactly as the next load
    /// will check it before it is written.
    pub fn parse(text: &str, p: &Path) -> Result<Self> {
        let value: serde_json::Value =
            serde_json::from_str(text).with_context(|| format!("parsing {}", p.display()))?;
        crate::schema::validate(&value).with_context(|| format!("in {}", p.display()))?;
        Self::from_model(text, p)
    }

    /// The model half of [`Manifest::parse`]: deserialize `text` into the
    /// serde model and run its semantic checks, without the JSON schema. Kept
    /// separate so the schema-parity test can hold the two layers against each
    /// other; everything else goes through `parse`, which runs both.
    fn from_model(text: &str, p: &Path) -> Result<Self> {
        // Deserialize from the raw text, not a parsed `serde_json::Value`: a
        // `Value` has already collapsed any duplicate object keys, so duplicate
        // skill names are only detectable by streaming the original bytes
        // through `deserialize_unique_skills`.
        let ObjectOnly(manifest): ObjectOnly<Self> =
            serde_json::from_str(text).with_context(|| format!("parsing {}", p.display()))?;
        if manifest.targets.is_empty() {
            bail!("`targets` in {} must list at least one vendor", p.display());
        }
        let supported = crate::schema::targets()?;
        if let Some(target) = manifest.targets.iter().find(|t| !supported.contains(t)) {
            bail!(
                "unknown target `{target}` in {} (supported: {})",
                p.display(),
                supported.join(", ")
            );
        }
        ensure_unique(&manifest.targets, "targets", p)?;
        if let Some(patterns) = &manifest.allowed_sources {
            ensure_unique(patterns, "allowedSources", p)?;
            crate::policy::validate_patterns(patterns)
                .with_context(|| format!("`allowedSources` in {}", p.display()))?;
        }
        for (kind, name, spec) in manifest.dependencies() {
            spec.validate()
                .with_context(|| format!("{kind} `{name}` in {}", p.display()))?;
            // `skills` and `plugins` share one flat namespace of dependency
            // names (both end up materialized side by side, and commands like
            // `update <name>`/`remove <name>` look a name up across both), so
            // a name declared in both maps is ambiguous, not just a warning.
            if kind == DependencyKind::Plugin && manifest.skills.contains_key(name) {
                bail!(
                    "name collision in {}: `{name}` is declared as both a skill and a plugin — \
                     dependency names must be unique across `skills` and `plugins`",
                    p.display()
                );
            }
        }
        Ok(manifest)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let p = Self::path_in(dir);
        crate::jsonfile::save(&p, self)
    }

    /// The exact text [`Manifest::save`] writes.
    pub fn to_text(&self) -> Result<String> {
        crate::jsonfile::to_text(self)
    }

    pub fn exists(dir: &Path) -> bool {
        Self::path_in(dir).exists()
    }

    /// The map holding dependencies of `kind`.
    pub fn deps(&self, kind: DependencyKind) -> &BTreeMap<DepName, SkillSpec> {
        match kind {
            DependencyKind::Skill => &self.skills,
            DependencyKind::Plugin => &self.plugins,
        }
    }

    /// The map holding dependencies of `kind`, for editing.
    pub fn deps_mut(&mut self, kind: DependencyKind) -> &mut BTreeMap<DepName, SkillSpec> {
        match kind {
            DependencyKind::Skill => &mut self.skills,
            DependencyKind::Plugin => &mut self.plugins,
        }
    }

    /// Every declared dependency with its kind: skills, then plugins, each in
    /// name order.
    pub fn dependencies(&self) -> impl Iterator<Item = (DependencyKind, &DepName, &SkillSpec)> {
        DependencyKind::ALL
            .into_iter()
            .flat_map(move |kind| self.deps(kind).iter().map(move |(n, s)| (kind, n, s)))
    }

    /// The kind `name` is declared as, if it is declared at all. Names are
    /// unique across kinds (enforced by [`Manifest::parse`]); should a
    /// hand-built manifest still repeat one, the skill wins.
    pub fn kind_of(&self, name: &str) -> Option<DependencyKind> {
        DependencyKind::ALL
            .into_iter()
            .find(|&kind| self.deps(kind).contains_key(name))
    }

    /// Whether `name` is declared as a skill or a plugin.
    pub fn contains_dependency(&self, name: &str) -> bool {
        self.kind_of(name).is_some()
    }

    /// Whether any skill or plugin is declared.
    pub fn declares_any(&self) -> bool {
        self.dependencies().next().is_some()
    }
}

/// Reject a repeated entry in the `field` list of the manifest at `p`.
fn ensure_unique(items: &[String], field: &str, p: &Path) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    if let Some(dup) = items.iter().find(|i| !seen.insert(i.as_str())) {
        bail!("`{field}` in {} lists `{dup}` more than once", p.display());
    }
    Ok(())
}

/// A dependency's own `ai.json`, parsed with a deliberately lenient shape.
///
/// A skill repo consumed as a transitive dependency may ship an `ai.json` whose
/// only purpose is to declare *further* skills. Unlike the root project's
/// manifest it is **not** required to list `targets` (transitive skills always
/// inherit the root's targets), and its `plugins` map is ignored in v1 (a plugin
/// pulls a much bigger blast radius — agents/hooks/MCP/scripts — so it is out of
/// scope for automatic transitive fetching). A nested `resolveTransitive` is
/// rejected outright: recursion is governed solely by the *root* project's flag,
/// so a dependency author gets a clear signal the field does nothing here (and a
/// malicious dependency cannot re-enable resolution the root opted out of).
#[derive(Debug, Clone, Deserialize)]
pub struct DependencyManifest {
    #[serde(default, deserialize_with = "deserialize_unique_skills")]
    pub skills: BTreeMap<DepName, SkillSpec>,
}

impl DependencyManifest {
    /// Load the nested manifest from an explicit, already-validated file path
    /// (`<dir>/ai.json`). The caller is responsible for only invoking this when
    /// the file exists — a missing file is a normal "no transitive deps" case,
    /// not an error.
    ///
    /// The transitive resolver validates the nested `ai.json` against the
    /// checkout boundary *before* reading it and gets back a canonical,
    /// symlink-resolved path (see `transitive::nested_manifest_path`). Reading
    /// that exact path — rather than re-deriving `<dir>/ai.json` and letting the
    /// loader follow the on-disk symlink a second time — closes the TOCTOU window
    /// where the symlink could be swapped between the boundary check and the read
    /// to smuggle in an outside, unscanned manifest.
    pub fn load_file(p: &Path) -> Result<Self> {
        let text = read_bounded(p)?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing nested {}", p.display()))?;
        if value.get("resolveTransitive").is_some() {
            bail!(
                "nested {} at {}: `resolveTransitive` is not allowed in a dependency's manifest — \
                 transitive resolution is controlled only by the root project's ai.json; \
                 remove it here (it has no effect)",
                MANIFEST_FILE,
                p.display()
            );
        }
        // Same rule for the source policy: only the root project (and the
        // machine policy) decides where content may come from — a dependency
        // must not appear to widen or narrow it.
        if value.get("allowedSources").is_some() {
            bail!(
                "nested {} at {}: `allowedSources` is not allowed in a dependency's manifest — \
                 the source policy is set only by the root project's ai.json and the machine \
                 policy; remove it here",
                MANIFEST_FILE,
                p.display()
            );
        }
        // Deserialize from the raw text (not `value`) so duplicate skill keys in
        // the nested manifest are still rejected, exactly as `Manifest::load` does.
        let ObjectOnly(dep): ObjectOnly<Self> = serde_json::from_str(&text)
            .with_context(|| format!("parsing nested {}", p.display()))?;
        // The same field checks the root schema and model apply (non-empty
        // strings, a contained `path`, a full-SHA `commit`, exactly one valid
        // selector): a nested manifest skips the schema, so without them a
        // malformed spec would only fail deep in git or reach the store path.
        for (name, spec) in &dep.skills {
            spec.validate()
                .with_context(|| format!("skill `{name}` in nested {}", p.display()))?;
        }
        Ok(dep)
    }
}

#[cfg(test)]
mod schema_parity;

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(git: &str, tag: Option<&str>, branch: Option<&str>, commit: Option<&str>) -> SkillSpec {
        SkillSpec {
            git: git.to_string(),
            tag: tag.map(str::to_string),
            branch: branch.map(str::to_string),
            commit: commit.map(str::to_string),
            range: None,
            path: None,
        }
    }

    /// The schema requires exactly one of tag/branch/commit on any manifest
    /// loaded from disk, but `SkillSpec::version` is also called on
    /// programmatically-constructed specs (e.g. `add --all`'s container spec)
    /// where that invariant isn't schema-enforced — so both error arms must be
    /// checked directly.
    #[test]
    fn version_rejects_no_selector() {
        let s = spec("u", None, None, None);
        let Err(err) = s.version() else {
            panic!("expected an error");
        };
        assert!(format!("{err}").contains("set one of tag/branch/commit/version"));
    }

    #[test]
    fn version_rejects_multiple_selectors() {
        let s = spec("u", Some("v1"), Some("main"), None);
        let Err(err) = s.version() else {
            panic!("expected an error");
        };
        assert!(format!("{err}").contains("set only one of tag/branch/commit/version"));

        let sha = "a".repeat(40);
        let all_three = spec("u", Some("v1"), Some("main"), Some(sha.as_str()));
        assert!(all_three.version().is_err());

        // `version` conflicts with each of the other three selectors.
        for mut s in [
            spec("u", Some("v1"), None, None),
            spec("u", None, Some("main"), None),
            spec("u", None, None, Some(sha.as_str())),
        ] {
            s.range = Some("^1.2".into());
            let err = s.version().unwrap_err();
            assert!(format!("{err}").contains("set only one"), "{err}");
        }
    }

    #[test]
    fn version_accepts_a_semver_range_and_rejects_an_invalid_one() {
        let mut s = spec("u", None, None, None);
        s.range = Some(">=1.2, <2".into());
        assert_eq!(s.version().unwrap(), Version::Range(">=1.2, <2".into()));
        for bad in ["", "not-a-version", "^v1.2", "1.2.3.4"] {
            s.range = Some(bad.into());
            let err = s.version().unwrap_err();
            assert!(
                format!("{err:#}").contains("invalid version requirement"),
                "`{bad}`: {err:#}"
            );
        }
        s.range = Some("^v1.2".into());
        assert!(
            format!("{:#}", s.version().unwrap_err()).contains("drop the `v`"),
            "a `v`-prefixed requirement gets a targeted hint"
        );
    }

    /// `1.2` is `^1.2` to the semver crate, which is exactly why ranges get
    /// their own `version` field: an existing exact `"tag": "1.2"` must keep
    /// meaning the literal tag `1.2`.
    #[test]
    fn tag_stays_exact_while_version_is_a_range() {
        assert_eq!(
            spec("u", Some("1.2"), None, None).version().unwrap(),
            Version::Tag("1.2".into())
        );
        let req = parse_version_req("1.2").unwrap();
        assert!(req.matches(&semver::Version::new(1, 9, 0)));
    }

    #[test]
    fn version_accepts_each_single_selector() {
        assert!(matches!(
            spec("u", Some("v1"), None, None).version().unwrap(),
            Version::Tag(t) if t == "v1"
        ));
        assert!(matches!(
            spec("u", None, Some("main"), None).version().unwrap(),
            Version::Branch(b) if b == "main"
        ));
        let sha = "a".repeat(40);
        assert!(matches!(
            spec("u", None, None, Some(&sha)).version().unwrap(),
            Version::Commit(c) if c == sha
        ));
    }

    /// The kind-agnostic view: skills then plugins, each in name order, with
    /// name lookups spanning both maps and edits landing in the kind's map.
    #[test]
    fn dependencies_span_both_kinds_skills_first() {
        let dep = r#"{"git":"u","branch":"main"}"#;
        let text = format!(
            r#"{{"targets":["claude"],"skills":{{"b":{dep},"a":{dep}}},"plugins":{{"0p":{dep}}}}}"#
        );
        let mut m = Manifest::parse(&text, Path::new("ai.json")).unwrap();
        let all: Vec<_> = m
            .dependencies()
            .map(|(k, n, _)| (k, n.to_string()))
            .collect();
        assert_eq!(
            all,
            [
                (DependencyKind::Skill, "a".to_string()),
                (DependencyKind::Skill, "b".to_string()),
                (DependencyKind::Plugin, "0p".to_string()),
            ]
        );
        assert!(m.declares_any());
        assert!(m.contains_dependency("0p") && m.contains_dependency("a"));
        assert!(!m.contains_dependency("missing"));
        assert_eq!(m.kind_of("missing"), None);

        let spec = m.deps_mut(DependencyKind::Plugin).remove("0p").unwrap();
        assert_eq!(m.kind_of("0p"), None);
        m.deps_mut(DependencyKind::Skill)
            .insert(crate::ids::dep("0p"), spec);
        assert_eq!(m.kind_of("0p"), Some(DependencyKind::Skill));
        assert_eq!(m.deps(DependencyKind::Plugin).len(), 0);

        m.deps_mut(DependencyKind::Skill).clear();
        assert!(!m.declares_any());
    }

    /// Root manifests reject invalid dependency names in either map, and a
    /// name declared both as a skill and a plugin.
    #[test]
    fn parse_rejects_bad_names_and_cross_map_collisions() {
        let p = Path::new("ai.json");
        let dep = r#"{"git":"u","branch":"main"}"#;
        let ok =
            format!(r#"{{"targets":["claude"],"skills":{{"a":{dep}}},"plugins":{{"b":{dep}}}}}"#);
        let m = Manifest::parse(&ok, p).unwrap();
        assert_eq!(m.kind_of("a"), Some(DependencyKind::Skill));
        assert_eq!(m.kind_of("b"), Some(DependencyKind::Plugin));
        for (json, needle) in [
            (
                format!(
                    r#"{{"targets":["claude"],"skills":{{"x":{dep}}},"plugins":{{"x":{dep}}}}}"#
                ),
                "name collision",
            ),
            (
                format!(r#"{{"targets":["claude"],"skills":{{"a:b":{dep}}}}}"#),
                "invalid skill name",
            ),
            (
                format!(r#"{{"targets":["claude"],"plugins":{{"..":{dep}}}}}"#),
                "invalid skill name",
            ),
            (
                format!(r#"{{"targets":["claude"],"plugins":{{"aux":{dep}}}}}"#),
                "reserved device name",
            ),
        ] {
            let err = Manifest::parse(&json, p).unwrap_err();
            assert!(format!("{err:#}").contains(needle), "{json}: {err:#}");
        }
    }

    #[test]
    fn validate_subpath_rejects_nul_byte() {
        let err = validate_subpath("foo\0bar").unwrap_err();
        assert!(format!("{err}").contains("NUL"), "{err}");
    }

    #[test]
    fn validate_subpath_accepts_plain_relative_path() {
        assert!(validate_subpath("skills/greet").is_ok());
        assert!(validate_subpath(".").is_ok());
    }

    /// A nested manifest bypasses the root JSON schema, so a `commit` selector
    /// that is not a full hex SHA (here one carrying path separators aimed at
    /// the store path) must be rejected by `DependencyManifest::load_file` before it
    /// can reach `resolver::resolve`/`StoreKey::derive`.
    #[test]
    fn dependency_manifest_rejects_non_sha_commit() {
        let dir = std::env::temp_dir().join(format!(
            "spm-depman-badcommit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Manifest::path_in(&dir),
            r#"{"skills":{"evil":{"git":"u","commit":"../../../../etc/evil"}}}"#,
        )
        .unwrap();
        let err = DependencyManifest::load_file(&Manifest::path_in(&dir)).unwrap_err();
        assert!(
            format!("{err:#}").contains("40-character hexadecimal SHA"),
            "{err:#}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The root schema requires `git`/`tag`/`branch`/`path` to be non-empty
    /// (`minLength: 1`); a nested manifest bypasses the schema, so `load_file`
    /// must enforce the same so an empty selector fails locally instead of
    /// surfacing as a confusing `refs/heads/` or a silent repo-root pin.
    #[test]
    fn dependency_manifest_rejects_empty_selector_fields() {
        let base = std::env::temp_dir().join(format!(
            "spm-depman-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for (json, needle) in [
            (
                r#"{"skills":{"leaf":{"git":"","branch":"main"}}}"#,
                "`git` must be non-empty",
            ),
            (
                r#"{"skills":{"leaf":{"git":"u","tag":""}}}"#,
                "`tag` must be non-empty",
            ),
            (
                r#"{"skills":{"leaf":{"git":"u","branch":""}}}"#,
                "`branch` must be non-empty",
            ),
            (
                r#"{"skills":{"leaf":{"git":"u","branch":"main","path":""}}}"#,
                "`path` must be non-empty",
            ),
            (
                r#"{"skills":{"leaf":{"git":"u","version":""}}}"#,
                "invalid version requirement",
            ),
            (
                r#"{"skills":{"leaf":{"git":"u","version":"^1","tag":"v1.0.0"}}}"#,
                "set only one of",
            ),
        ] {
            let dir = base.join(needle.replace([' ', '`'], "_"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(Manifest::path_in(&dir), json).unwrap();
            let err = DependencyManifest::load_file(&Manifest::path_in(&dir)).unwrap_err();
            assert!(format!("{err:#}").contains(needle), "{json}: {err:#}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn dependency_manifest_rejects_oversized_file_before_parsing() {
        let dir = std::env::temp_dir().join(format!(
            "spm-depman-big-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Valid JSON, but padded past the cap: must fail on size, not parse.
        let pad = " ".repeat(MAX_NESTED_MANIFEST_BYTES as usize);
        std::fs::write(Manifest::path_in(&dir), format!("{{\"skills\":{{}}}}{pad}")).unwrap();
        let err = DependencyManifest::load_file(&Manifest::path_in(&dir)).unwrap_err();
        assert!(format!("{err:#}").contains("larger than"), "{err:#}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_commit_selector_accepts_both_cases_and_rejects_bad() {
        assert!(validate_commit_selector(&"a".repeat(40)).is_ok());
        assert!(
            validate_commit_selector(&"A".repeat(40)).is_ok(),
            "case-insensitive"
        );
        assert!(validate_commit_selector("abc123").is_err(), "too short");
        assert!(
            validate_commit_selector(&"g".repeat(40)).is_err(),
            "non-hex"
        );
        assert!(
            validate_commit_selector(&format!("{}/x", "a".repeat(38))).is_err(),
            "path separator"
        );
    }

    /// A dependency's own `ai.json` parses leniently: no `targets` required, and
    /// `plugins`/`targets` keys are ignored (transitive skills inherit the root's
    /// targets, and nested plugins are out of scope in v1).
    #[test]
    fn dependency_manifest_parses_skills_and_ignores_targets_and_plugins() {
        let dir = std::env::temp_dir().join(format!(
            "spm-depman-lenient-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Manifest::path_in(&dir),
            r#"{"targets":["claude"],"plugins":{"p":{"git":"u","branch":"main"}},"skills":{"leaf":{"git":"u","branch":"main"}}}"#,
        )
        .unwrap();
        let dep = DependencyManifest::load_file(&Manifest::path_in(&dir)).unwrap();
        assert_eq!(dep.skills.len(), 1);
        assert!(dep.skills.contains_key("leaf"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Writes `body` as a nested `ai.json` in a fresh dir and loads it.
    fn load_nested(tag: &str, body: &str) -> (std::path::PathBuf, Result<DependencyManifest>) {
        let dir = std::env::temp_dir().join(format!(
            "spm-depman-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Manifest::path_in(&dir);
        std::fs::write(&path, body).unwrap();
        let res = DependencyManifest::load_file(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        (path, res)
    }

    /// Only the nested manifest's top level is lenient: a skill entry is held
    /// to the root spec shape, so an unknown key inside it is rejected, with an
    /// error naming the file and the offending key.
    #[test]
    fn dependency_manifest_rejects_unknown_key_in_skill_entry() {
        let (path, res) = load_nested(
            "unknown-key",
            r#"{"skills":{"a":{"git":"u","tag":"v1","x":1}}}"#,
        );
        let err = format!("{:#}", res.unwrap_err());
        assert!(err.contains(&path.display().to_string()), "{err}");
        assert!(err.contains("skill `a`: unknown field `x`"), "{err}");
    }

    /// An explicit `null` for a spec field is rejected in a nested skill entry
    /// exactly as in the root manifest, naming the file and the entry.
    #[test]
    fn dependency_manifest_rejects_explicit_null_in_skill_entry() {
        let (path, res) = load_nested(
            "null-field",
            r#"{"skills":{"a":{"git":"u","tag":null,"branch":"main"}}}"#,
        );
        let err = format!("{:#}", res.unwrap_err());
        assert!(err.contains(&path.display().to_string()), "{err}");
        assert!(err.contains("skill `a`: invalid type: null"), "{err}");
    }

    /// Unknown keys at the nested manifest's top level (e.g. `$schema` or
    /// a key from a newer spm) stay accepted and ignored.
    #[test]
    fn dependency_manifest_accepts_unknown_top_level_keys() {
        let (_, res) = load_nested(
            "top-level",
            r#"{"$schema":"https://example.invalid/ai.schema.json","futureKey":{"a":1},"skills":{"a":{"git":"u","tag":"v1"}}}"#,
        );
        let dep = res.unwrap();
        assert!(dep.skills.contains_key("a"));
    }

    /// A nested `resolveTransitive` is a hard error — recursion is controlled
    /// only by the root project's manifest.
    #[test]
    fn dependency_manifest_rejects_nested_resolve_transitive() {
        let dir = std::env::temp_dir().join(format!(
            "spm-depman-nested-flag-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Manifest::path_in(&dir),
            r#"{"resolveTransitive":true,"skills":{}}"#,
        )
        .unwrap();
        let err = DependencyManifest::load_file(&Manifest::path_in(&dir)).unwrap_err();
        assert!(format!("{err:#}").contains("resolveTransitive"), "{err:#}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `load_file` reads the exact path it is handed — not a `<dir>/ai.json`
    /// re-derived from a directory. The transitive resolver relies on this to
    /// read the boundary-checked, symlink-resolved manifest path instead of
    /// re-following the on-disk `ai.json` symlink (which a swap could redirect
    /// to an outside, unscanned manifest after the boundary check).
    #[test]
    fn dependency_manifest_load_file_reads_the_given_path() {
        let dir = std::env::temp_dir().join(format!(
            "spm-depman-loadfile-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // The real, validated manifest lives at a non-default filename; the
        // default `ai.json` slot holds a decoy the loader must NOT read.
        let validated = dir.join("validated.json");
        std::fs::write(
            &validated,
            r#"{"skills":{"real":{"git":"u","branch":"main"}}}"#,
        )
        .unwrap();
        std::fs::write(
            Manifest::path_in(&dir),
            r#"{"skills":{"decoy":{"git":"u","branch":"main"}}}"#,
        )
        .unwrap();
        let dep = DependencyManifest::load_file(&validated).unwrap();
        assert!(dep.skills.contains_key("real"), "reads the given path");
        assert!(!dep.skills.contains_key("decoy"), "ignores <dir>/ai.json");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
