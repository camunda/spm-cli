//! Validated identifiers for security-relevant strings.
//!
//! Dependency names, commit SHAs and store keys all end up as path components
//! (`skills_dir.join(name)`, `store_root.join(key)`), and lock references are
//! re-read from the committed, untrusted `ai.lock`. Each is a newtype that can
//! only be built through its validating constructor — deserialization included
//! (`#[serde(try_from = "String")]`) — so a value of the type is proof it was
//! checked, and the check lives in exactly one place instead of at every
//! boundary that reads one.

use crate::manifest::Version;
use anyhow::{bail, Error, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::path::Path;

/// The shared surface of the string newtypes: read access as `&str`/`&Path`,
/// `Display`, lookups by `&str` in maps and sets keyed by the newtype
/// (`Borrow<str>`, sound because `Eq`/`Ord`/`Hash` are the inner string's),
/// and validating conversions from strings.
macro_rules! string_id {
    ($ty:ident) => {
        impl $ty {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::ops::Deref for $ty {
            type Target = str;
            fn deref(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<Path> for $ty {
            fn as_ref(&self) -> &Path {
                Path::new(&self.0)
            }
        }

        impl Borrow<str> for $ty {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.pad(&self.0)
            }
        }

        impl From<$ty> for String {
            fn from(v: $ty) -> String {
                v.0
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = Error;
            fn try_from(s: &str) -> Result<Self> {
                Self::try_from(s.to_string())
            }
        }

        impl TryFrom<&String> for $ty {
            type Error = Error;
            fn try_from(s: &String) -> Result<Self> {
                Self::try_from(s.clone())
            }
        }

        impl PartialEq<&str> for $ty {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $ty {
            fn eq(&self, other: &String) -> bool {
                &self.0 == other
            }
        }

        impl PartialEq<$ty> for &str {
            fn eq(&self, other: &$ty) -> bool {
                *self == other.0
            }
        }

        impl PartialEq<$ty> for String {
            fn eq(&self, other: &$ty) -> bool {
                *self == other.0
            }
        }
    };
}

/// A dependency name: an `ai.json`/`ai.lock` map key, a plugin's bundled skill,
/// or a transitive skill's synthesized name. It becomes a directory component
/// under each vendor's skills dir, so it is safe and portable as one.
///
/// Two classes are rejected:
///   1. **Path escapes** — `..`, `/`, `\`, or NUL would let a hostile manifest
///      write outside the intended directory (`skills_dir.join(name)`).
///   2. **Platform-invalid / reserved names** — characters and reserved device
///      names that are illegal, silently rewritten, or map to device files on
///      Windows (which is in CI and a supported platform). Rejecting them here
///      fails fast with a clear local error instead of surfacing as a confusing
///      filesystem error deep inside materialization on another OS — and does so
///      *before* a nested dependency is synthesized or fetched.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DepName(String);
string_id!(DepName);

impl TryFrom<String> for DepName {
    type Error = Error;
    fn try_from(name: String) -> Result<Self> {
        let path_bad = name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
            || name.contains('\0');
        if path_bad {
            bail!(
                "invalid skill name `{name}`: names must be non-empty and must not contain \
                 path separators, `.`, `..`, or NUL"
            );
        }
        if let Some(c) = name
            .chars()
            .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control())
        {
            bail!(
                "invalid skill name `{name}`: character {c:?} is not allowed — names become \
                 directory components, and `< > : \" | ? *` and control characters are invalid \
                 in file names on Windows"
            );
        }
        if name.ends_with('.') || name.ends_with(' ') {
            bail!(
                "invalid skill name `{name}`: names must not end with a space or `.` — Windows \
                 silently strips trailing spaces and dots, which would collide two distinct names"
            );
        }
        // Windows reserves device names matched on the stem before the first
        // dot, so `CON`, `con.txt`, and `aux.anything` all resolve to a device,
        // not a file.
        let stem = name.split('.').next().unwrap_or(&name);
        if is_windows_reserved_device_name(stem) {
            bail!(
                "invalid skill name `{name}`: `{stem}` is a reserved device name on Windows — \
                 choose another name"
            );
        }
        Ok(Self(name))
    }
}

/// Whether `stem` is a Windows reserved device name (case-insensitive):
/// `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, or `LPT1`–`LPT9`.
fn is_windows_reserved_device_name(stem: &str) -> bool {
    let s = stem.to_ascii_uppercase();
    matches!(s.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((s.starts_with("COM") || s.starts_with("LPT"))
            && s.len() == 4
            && matches!(s.as_bytes()[3], b'1'..=b'9'))
}

/// A resolved commit: a full 40-character lowercase hex SHA. Full SHAs keep the
/// store key unambiguous and let `git::is_at_commit` compare against the
/// lowercase `rev-parse HEAD` without spurious refetches; they also prevent
/// path injection through the SHA, which is appended verbatim into the store
/// key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitSha(String);
string_id!(CommitSha);

impl CommitSha {
    /// The abbreviated form shown to humans.
    pub fn short(&self) -> &str {
        &self.0[..8]
    }
}

impl TryFrom<String> for CommitSha {
    type Error = Error;
    fn try_from(commit: String) -> Result<Self> {
        let ok = commit.len() == 40
            && commit
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        if !ok {
            bail!("invalid commit `{commit}`: expected a full 40-character lowercase hex SHA");
        }
        Ok(Self(commit))
    }
}

/// The directory naming one checkout beneath the global store root
/// (`<repo-name>-<url-hash>@<sha>`). Always a single path component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StoreKey(String);
string_id!(StoreKey);

impl StoreKey {
    /// Derive the filesystem-safe, **bounded-length** key for `url` at
    /// `commit`. Sanitizing the entire URL into the directory name would, for
    /// long URLs (e.g. `file://` paths on CI), push the full
    /// `.../store/<key>/.git/objects/...` path past Windows' 260-char limit. So
    /// the key is a short human-readable repo-name hint plus a hash of the full
    /// URL: bounded regardless of URL length while staying collision-safe.
    pub fn derive(url: &str, commit: &CommitSha) -> Self {
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
        Self(format!("{hint}-{h:016x}@{commit}"))
    }
}

impl TryFrom<String> for StoreKey {
    type Error = Error;
    fn try_from(key: String) -> Result<Self> {
        let bad = key.is_empty()
            || key.contains('/')
            || key.contains('\\')
            || key.contains('\0')
            || key.split('@').any(|seg| seg == "." || seg == "..");
        if bad {
            bail!("invalid store key `{key}`: must be a single path component");
        }
        Ok(Self(key))
    }
}

/// The version selector a lock entry was resolved from, as recorded in
/// `ai.lock` (`reference`, `also_satisfies`): a [`Version`] written as its
/// stable label — `tag:<t>`, `branch:<b>`, `commit:<sha>` or
/// `version:<requirement>`. Parsed once on load; a label of any other shape is
/// rejected.
///
/// Ordered by label, so lists of references sort exactly as their serialized
/// strings do and `ai.lock` stays byte-stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LockReference(Version);

impl LockReference {
    pub fn version(&self) -> &Version {
        &self.0
    }

    /// The stable label stored in `ai.lock`.
    pub fn label(&self) -> String {
        self.0.label()
    }
}

impl From<Version> for LockReference {
    fn from(v: Version) -> Self {
        Self(v)
    }
}

impl From<LockReference> for String {
    fn from(r: LockReference) -> String {
        r.label()
    }
}

impl TryFrom<String> for LockReference {
    type Error = Error;
    fn try_from(label: String) -> Result<Self> {
        Self::try_from(label.as_str())
    }
}

impl TryFrom<&str> for LockReference {
    type Error = Error;
    fn try_from(label: &str) -> Result<Self> {
        let parsed = label.split_once(':').and_then(|(kind, value)| {
            let value = value.to_string();
            Some(match kind {
                "tag" => Version::Tag(value),
                "branch" => Version::Branch(value),
                "commit" => Version::Commit(value),
                "version" => Version::Range(value),
                _ => return None,
            })
        });
        match parsed {
            Some(v) => Ok(Self(v)),
            None => bail!(
                "invalid reference `{label}`: expected a version selector \
                 (`tag:`, `branch:`, `commit:` or `version:`)"
            ),
        }
    }
}

impl fmt::Display for LockReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.label())
    }
}

impl PartialOrd for LockReference {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LockReference {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.label().cmp(&other.label())
    }
}

/// Test shorthand for a known-valid dependency name.
#[cfg(test)]
pub fn dep(name: &str) -> DepName {
    DepName::try_from(name).expect("valid test dependency name")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn dep_name_accepts_ordinary_names() {
        for name in [
            "foo", "my-skill", "foo.bar", "com0", "lpt10", "comic", "console", "a b", ".hidden",
            "ünï",
        ] {
            assert!(
                DepName::try_from(name).is_ok(),
                "`{name}` should be accepted"
            );
        }
    }

    #[test]
    fn dep_name_rejects_path_escapes() {
        for name in ["", ".", "..", "a/b", "../x", "a\\b", "nul\0byte", "/abs"] {
            let err = DepName::try_from(name).unwrap_err();
            assert!(
                format!("{err}").contains("path separators"),
                "`{name:?}` should be rejected: {err}"
            );
        }
    }

    #[test]
    fn dep_name_rejects_windows_invalid_chars() {
        for name in [
            "foo:bar",
            "a*b",
            "what?",
            "pipe|d",
            "lt<gt>",
            "quo\"te",
            "ctrl\u{1}x",
            "tab\tx",
        ] {
            let err = DepName::try_from(name).unwrap_err();
            assert!(
                format!("{err}").contains("not allowed"),
                "`{name}` should be rejected: {err}"
            );
        }
    }

    #[test]
    fn dep_name_rejects_trailing_dot_or_space() {
        for name in ["foo.", "foo ", "bar. ", "..."] {
            let err = DepName::try_from(name).unwrap_err();
            assert!(
                format!("{err}").contains("space or `.`"),
                "`{name}` should be rejected: {err}"
            );
        }
    }

    #[test]
    fn dep_name_rejects_windows_reserved_device_names() {
        for name in [
            "con", "CON", "Prn", "aux", "nul", "com1", "LPT9", "con.txt", "aux.md",
        ] {
            let err = DepName::try_from(name).unwrap_err();
            assert!(
                format!("{err}").contains("reserved device name"),
                "`{name}` should be rejected: {err}"
            );
        }
    }

    /// Deserializing is the validating constructor: a bad map key or value is
    /// rejected while parsing, never materialized as a `DepName`.
    #[test]
    fn dep_name_validates_on_deserialize_as_value_and_map_key() {
        let ok: BTreeMap<DepName, Vec<DepName>> =
            serde_json::from_str(r#"{"a":["b","c"]}"#).unwrap();
        assert_eq!(ok["a"], ["b", "c"], "looked up by &str through Borrow");
        let err = serde_json::from_str::<BTreeMap<DepName, u8>>(r#"{"../x":1}"#).unwrap_err();
        assert!(err.to_string().contains("invalid skill name"), "{err}");
        let err = serde_json::from_str::<Vec<DepName>>(r#"["ok","a/b"]"#).unwrap_err();
        assert!(err.to_string().contains("invalid skill name"), "{err}");
        assert_eq!(
            serde_json::to_string(&DepName::try_from("x").unwrap()).unwrap(),
            r#""x""#
        );
    }

    /// Maps keyed by `DepName` iterate in the same order as the `String`-keyed
    /// maps they replace, keeping serialized files byte-stable.
    #[test]
    fn dep_name_orders_like_its_string() {
        let mut names: Vec<DepName> = ["b", "B", "a-b", "a", "ä"]
            .iter()
            .map(|n| DepName::try_from(*n).unwrap())
            .collect();
        names.sort();
        let mut strings = vec!["b", "B", "a-b", "a", "ä"];
        strings.sort();
        assert_eq!(names, strings);
    }

    #[test]
    fn commit_sha_requires_full_lowercase_hex() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(CommitSha::try_from(sha).unwrap().short(), "01234567");
        for bad in [
            String::new(),
            "abc123".into(),
            "A".repeat(40),
            "g".repeat(40),
            "a".repeat(39),
            "a".repeat(41),
            format!("{}/x", "a".repeat(38)),
            format!("{}..", "a".repeat(38)),
        ] {
            let err = CommitSha::try_from(bad.as_str()).unwrap_err();
            assert!(
                format!("{err}").contains("40-character lowercase hex"),
                "`{bad}`: {err}"
            );
        }
        let err = serde_json::from_str::<CommitSha>(&format!(r#""{}""#, "F".repeat(40)));
        assert!(err.is_err(), "validated on deserialize");
    }

    #[test]
    fn store_key_rejects_anything_but_one_component() {
        for ok in ["repo-0123@abc", "x", "a.b@c", "..x"] {
            assert!(StoreKey::try_from(ok).is_ok(), "`{ok}` should be accepted");
        }
        for bad in [
            "", "a/b", "a\\b", "a\0b", ".", "..", "x@..", "..@x", "a@.@b",
        ] {
            let err = StoreKey::try_from(bad).unwrap_err();
            assert!(
                format!("{err}").contains("single path component"),
                "`{bad:?}`: {err}"
            );
        }
        assert!(serde_json::from_str::<StoreKey>(r#""../escape""#).is_err());
    }

    #[test]
    fn store_key_derive_is_bounded_safe_and_stable() {
        let sha = CommitSha::try_from("0123456789abcdef0123456789abcdef01234567").unwrap();
        // A pathologically long URL (e.g. a `file://` temp path on CI) must
        // still yield a short, filesystem-safe key so paths stay under
        // Windows' limit.
        let long = format!(
            "file://{}/skill",
            "C_Users_runner_AppData_Local_Temp".repeat(20)
        );
        let key = StoreKey::derive(&long, &sha);
        StoreKey::try_from(key.as_str()).expect("a derived key passes validation");
        // hint(<=32) + '-' + 16 hex + '@' + 40 sha == 90 max.
        assert!(key.len() <= 90, "key too long: {} ({key})", key.len());
        assert!(key.ends_with(&format!("@{sha}")));
        assert_eq!(key, StoreKey::derive(&long, &sha), "deterministic");
        assert_ne!(
            StoreKey::derive("https://github.com/a/repo", &sha),
            StoreKey::derive("https://github.com/b/repo", &sha),
            "distinct URLs do not collide on the same sha"
        );
        // Hostile URL characters never reach the key.
        let key = StoreKey::derive("https://h/../..\\evil.git", &sha);
        assert!(key.starts_with("evil-"), "{key}");
    }

    #[test]
    fn lock_reference_round_trips_every_selector_label() {
        for label in [
            "tag:v1.2.0",
            "branch:feat/x:y",
            "branch:",
            &format!("commit:{}", "A".repeat(40)),
            "version:>=1.2, <2",
        ] {
            let r = LockReference::try_from(label).unwrap();
            assert_eq!(r.label(), label);
            assert_eq!(r.to_string(), label);
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(json, serde_json::to_string(label).unwrap());
            assert_eq!(serde_json::from_str::<LockReference>(&json).unwrap(), r);
        }
        assert_eq!(
            LockReference::try_from("branch:feat/x:y")
                .unwrap()
                .version(),
            &Version::Branch("feat/x:y".into()),
            "only the first `:` separates the kind"
        );
    }

    #[test]
    fn lock_reference_rejects_unknown_shapes() {
        for bad in ["rev:abc", "no-colon", "", ":x", "Tag:v1"] {
            let err = LockReference::try_from(bad).unwrap_err();
            assert!(
                format!("{err}").contains("version selector"),
                "`{bad}`: {err}"
            );
            assert!(serde_json::from_str::<LockReference>(&format!("{bad:?}")).is_err());
        }
    }

    /// References sort by their serialized label, not by `Version` variant, so
    /// sorted lists (`also_satisfies`) serialize in the same order as before.
    #[test]
    fn lock_reference_orders_by_label() {
        let mut refs: Vec<LockReference> = ["version:^1", "tag:v1", "commit:a", "branch:z"]
            .iter()
            .map(|l| LockReference::try_from(*l).unwrap())
            .collect();
        refs.sort();
        let labels: Vec<String> = refs.iter().map(LockReference::label).collect();
        assert_eq!(labels, ["branch:z", "commit:a", "tag:v1", "version:^1"]);
    }
}
