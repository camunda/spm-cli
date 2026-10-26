//! Loading, parsing (schema + model checks) and saving of the root `ai.json`.

use super::de::ObjectOnly;
use super::validate::ensure_unique;
use super::{DependencyKind, Manifest, MANIFEST_FILE};
use crate::policy::Policy;
use crate::scope::Scope;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

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
    pub(super) fn from_model(text: &str, p: &Path) -> Result<Self> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

    mod props {
        use super::*;
        use crate::manifest::testutil::{dep_name, edited, Scratch};
        use crate::manifest::{DependencyManifest, SkillSpec};
        use proptest::prelude::*;
        use std::collections::BTreeMap;

        fn spec() -> impl Strategy<Value = SkillSpec> {
            #[derive(Debug, Clone)]
            enum Selector {
                Tag(String),
                Branch(String),
                Commit(String),
                Range(&'static str),
            }
            let selector = prop_oneof![
                ".{1,8}".prop_map(Selector::Tag),
                ".{1,8}".prop_map(Selector::Branch),
                "[0-9a-fA-F]{40}".prop_map(Selector::Commit),
                prop::sample::select(&["^1.2", "~1.4.0", ">=1.2, <2", "1.2", "*"][..])
                    .prop_map(Selector::Range),
            ];
            let path = prop::option::of("[a-z][a-z.]{0,3}(/[a-z]{1,4}){0,2}");
            (".{1,16}", selector, path).prop_map(|(git, selector, path)| {
                let mut spec = SkillSpec {
                    git,
                    tag: None,
                    branch: None,
                    commit: None,
                    range: None,
                    path,
                };
                match selector {
                    Selector::Tag(t) => spec.tag = Some(t),
                    Selector::Branch(b) => spec.branch = Some(b),
                    Selector::Commit(c) => spec.commit = Some(c),
                    Selector::Range(r) => spec.range = Some(r.into()),
                }
                spec
            })
        }

        /// A manifest the schema and the model both accept.
        fn manifest() -> impl Strategy<Value = Manifest> {
            let targets =
                prop::sample::subsequence(crate::schema::targets().unwrap(), 1..=3).prop_shuffle();
            let sources = prop::option::of(prop::sample::subsequence(
                vec![
                    "github.com/org/repo".to_string(),
                    "github.com/org/*".into(),
                    "gitlab.com/*".into(),
                    "file://*".into(),
                ],
                0..=2,
            ));
            let deps = prop::collection::btree_map(dep_name(), (any::<bool>(), spec()), 0..4);
            (
                prop::option::of(".{0,8}"),
                targets,
                any::<bool>(),
                sources,
                deps,
            )
                .prop_map(
                    |(schema, targets, resolve_transitive, allowed_sources, deps)| {
                        let (plugins, skills) = deps
                            .into_iter()
                            .partition::<BTreeMap<_, _>, _>(|(_, (p, _))| *p);
                        let strip = |m: BTreeMap<_, (bool, SkillSpec)>| {
                            m.into_iter().map(|(n, (_, s))| (n, s)).collect()
                        };
                        Manifest {
                            schema,
                            targets,
                            resolve_transitive,
                            allowed_sources,
                            skills: strip(skills),
                            plugins: strip(plugins),
                        }
                    },
                )
        }

        /// Parse `text`; an accepted manifest saves to a text that parses
        /// back to the same text.
        fn parse(text: &str) -> Option<String> {
            let p = Path::new("ai.json");
            let saved = Manifest::parse(text, p).ok()?.to_text().unwrap();
            let again = Manifest::parse(&saved, p).unwrap().to_text().unwrap();
            assert_eq!(again, saved);
            Some(saved)
        }

        proptest! {
            #![proptest_config(crate::proptest_config(128))]

            /// Every manifest spm can write parses back unchanged.
            #[test]
            fn saved_manifests_parse_back(m in manifest()) {
                let text = m.to_text().unwrap();
                prop_assert_eq!(parse(&text), Some(text));
            }

            /// A hand-edited `ai.json`, or a dependency's nested one, is an
            /// error or a manifest, never a panic; an accepted root manifest
            /// round-trips.
            #[test]
            fn edited_manifests_parse_or_fail_cleanly(
                text in manifest().prop_flat_map(|m| edited(m.to_text().unwrap())),
            ) {
                parse(&text);
                let dir = Scratch::new();
                let file = dir.0.join(MANIFEST_FILE);
                std::fs::write(&file, &text).unwrap();
                let _ = DependencyManifest::load_file(&file);
            }
        }
    }
}
