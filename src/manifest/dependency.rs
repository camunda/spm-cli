//! A dependency's own (nested) `ai.json`: a lenient, size-bounded shape that
//! only declares further skills.

use super::de::{deserialize_unique_skills, ObjectOnly};
use super::{SkillSpec, MANIFEST_FILE};
use crate::ids::DepName;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Maximum size (bytes) of a nested dependency `ai.json`. A nested manifest is
/// untrusted and parsed (twice) into memory before any edge budget applies, so
/// the size is bounded up front; this comfortably fits `MAX_EDGES` declarations.
const MAX_NESTED_MANIFEST_BYTES: u64 = 256 * 1024;

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
    /// symlink-resolved path (see `transitive::walk::nested_manifest_path`). Reading
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
mod tests {
    use super::*;
    use crate::manifest::Manifest;

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
