//! Transitive identity: the normalized `(git, path)` pair every walk structure
//! (cycle stack, dedup map, conflict key) keys on, and the deterministic names
//! synthesized from it for transitively-resolved skills.

use super::MAX_SYNTH_NAME_BYTES;
use std::path::{Component, Path};

/// Identity of a resolvable skill for transitive bookkeeping: the normalized git
/// URL plus the optional in-repo subpath. Two entries with the same git but a
/// different `path` are different skills (the monorepo-of-skills layout) and
/// never conflict.
pub(super) type Key = (String, Option<String>);

/// Normalize a git URL for transitive identity purposes only (cycle stack,
/// dedup map, conflict key, and the name-synthesis hash). This intentionally
/// does **not** feed [`StoreKey::derive`](crate::ids::StoreKey::derive)/store
/// dedup — that is an orthogonal, pre-existing behavior. The normalization:
/// strip a trailing `/`, strip a trailing `.git` **only for remote sources**,
/// then lowercase the scheme + authority while preserving the path's case
/// (paths are case-sensitive on the server; hosts are not).
///
/// The `.git` suffix is a *remote*-repo spelling convention: `github.com/o/r`
/// and `github.com/o/r.git` name the same server-side repository. For a local
/// source — a `file://` URL or a bare filesystem path — `repo.git` and `repo`
/// can be two genuinely distinct directories, so stripping the suffix there
/// would fuse unrelated skills into one transitive identity (silently deduping
/// them or flagging a phantom conflict). We therefore preserve `.git` for local
/// and `file://` sources and only strip it for remote URL forms.
///
/// Transport-equivalent spellings of the *same* remote are also folded to one
/// identity: `https://host/o/r`, `ssh://git@host/o/r` and scp-style
/// `git@host:o/r` all normalize to the canonical HTTPS form via
/// [`crate::remote::remote_identity`], reusing the exact HTTPS/SSH equivalence
/// that `--protocol` rewriting relies on, so a dependency cannot evade `(git, path)`
/// deduplication or version-conflict detection by requesting a repo through a
/// different protocol.
pub(super) fn normalize_git(url: &str) -> String {
    let s = url.trim().trim_end_matches('/');

    // A remote with a cross-protocol equivalent collapses to its canonical HTTPS
    // identity (with the `.git` convention stripped), regardless of the protocol
    // spelling supplied. Sources with no such equivalent (`file://`, a port,
    // userinfo, a non-`git` SSH user, a local path) return `None` and keep their
    // literal, protocol-specific identity below.
    if let Some(identity) = crate::remote::remote_identity(s) {
        return strip_git_suffix(&identity);
    }

    if let Some(idx) = s.find("://") {
        // scheme://authority/path
        let scheme = &s[..idx];
        let rest = &s[idx + 3..];
        let (authority, path) = match rest.find('/') {
            Some(p) => (&rest[..p], &rest[p..]),
            None => (rest, ""),
        };
        // Preserve `.git` for local `file://` sources; strip it for remote ones.
        let path = if scheme.eq_ignore_ascii_case("file") {
            path.to_string()
        } else {
            strip_git_suffix(path)
        };
        format!(
            "{}://{}{}",
            scheme.to_ascii_lowercase(),
            authority.to_ascii_lowercase(),
            path
        )
    } else if let Some(idx) = s.find(':') {
        // scp-style `user@host:path` (no scheme). Only treat as scp when the
        // colon precedes the first slash — otherwise it's a plain path.
        let before_colon = &s[..idx];
        if is_windows_drive_prefix(s) {
            // A bare Windows path like `C:\work\repo.git`: the single-letter
            // "authority" is a drive letter, not an scp host. Treat it as a local
            // path so `repo.git` and `repo` stay distinct identities.
            s.to_string()
        } else if !before_colon.contains('/') {
            // A remote scp target — strip the `.git` convention.
            let authority = before_colon;
            let path = strip_git_suffix(&s[idx..]);
            format!("{}{}", authority.to_ascii_lowercase(), path)
        } else {
            // A bare local path that happens to contain a colon — keep `.git`.
            s.to_string()
        }
    } else {
        // A bare local filesystem path — `repo.git` and `repo` are distinct
        // directories, so preserve the suffix.
        s.to_string()
    }
}

/// Strip a single trailing `.git` (and any trailing `/`) from a **remote** repo
/// path component. Applied only to remote sources — see [`normalize_git`] for
/// why local/`file://` sources must keep the suffix.
fn strip_git_suffix(path: &str) -> String {
    let p = path.trim_end_matches('/');
    p.strip_suffix(".git").unwrap_or(p).to_string()
}

/// Does `s` start with a Windows drive-letter prefix (`C:\…` or `C:/…`)?
///
/// A bare local Windows path like `C:\work\repo.git` otherwise reaches the
/// scp-style branch of [`normalize_git`] with a single-letter "authority" (`C`)
/// and gets its `.git` suffix stripped, fusing `C:\work\repo.git` and
/// `C:\work\repo` into one identity. Detecting the drive prefix keeps it a local
/// path (suffix preserved). Requiring a path separator after the colon avoids
/// misclassifying a genuine single-letter scp host (`h:path`).
fn is_windows_drive_prefix(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(letter), Some(':'), Some('\\' | '/')) if letter.is_ascii_alphabetic()
    )
}
/// Canonicalize a validated subpath into a stable lexical identity string.
///
/// `validate_subpath` (run before any spec reaches here) already rejects
/// absolute paths and `..`, but it *permits* `.` components and repeated or
/// trailing separators — so `skills/x`, `skills/./x`, `skills//x`, and
/// `skills/x/` all name the same content while producing different raw strings.
/// Using the raw string as the transitive identity would let those spellings
/// dodge dedup/version-conflict detection and materialize the same skill twice.
/// Fold every root-equivalent form (`.`, empty, `None`) to `None` and join the
/// remaining `Normal` components with `/` so identity is spelling-independent.
fn canonical_subpath(path: &Option<String>) -> Option<String> {
    let raw = path.as_deref()?;
    let parts: Vec<std::borrow::Cow<str>> = Path::new(raw)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy()),
            // CurDir dropped; validate_subpath already rejected the rest.
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

pub(super) fn key_of(git: &str, path: &Option<String>) -> Key {
    (normalize_git(git), canonical_subpath(path))
}

/// Short, stable hex hash of a skill's normalized identity, for name synthesis.
fn short_hash(key: &Key) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let feed = |h: &mut u64, bytes: &[u8]| {
        for b in bytes {
            *h ^= *b as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(&mut h, key.0.as_bytes());
    feed(&mut h, b"\0");
    feed(&mut h, key.1.as_deref().unwrap_or("").as_bytes());
    format!("{:08x}", h & 0xffff_ffff)
}

/// Synthesize a transitive skill's materialized name:
/// `{requester}__{declared}-{short_hash}`.
///
/// Names nest (a requester may itself be synthesized), so the result is bounded
/// to [`MAX_SYNTH_NAME_BYTES`] by truncating the requester and declared parts;
/// the identity hash suffix is always kept intact and keeps names distinct.
pub(super) fn synth_name(requester: &str, declared: &str, key: &Key) -> String {
    let suffix = format!("-{}", short_hash(key));
    let budget = MAX_SYNTH_NAME_BYTES - suffix.len() - 2;
    // Give `declared` priority up to half the budget; the requester gets the rest.
    let declared_max = declared.len().min(budget / 2);
    let requester = truncate_to(requester, budget - declared_max);
    let declared = truncate_to(declared, declared_max);
    format!("{requester}__{declared}{suffix}")
}

fn truncate_to(s: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::DepName;
    use crate::transitive::MAX_DEPTH;

    #[test]
    fn normalize_strips_git_suffix_and_trailing_slash() {
        assert_eq!(
            normalize_git("https://github.com/Org/Repo.git/"),
            "https://github.com/Org/Repo"
        );
        assert_eq!(
            normalize_git("https://github.com/Org/Repo"),
            "https://github.com/Org/Repo"
        );
    }

    #[test]
    fn normalize_lowercases_scheme_and_host_but_not_path() {
        assert_eq!(
            normalize_git("HTTPS://GitHub.COM/Org/Repo"),
            "https://github.com/Org/Repo"
        );
    }

    #[test]
    fn normalize_handles_scp_style() {
        // scp-style folds to the canonical HTTPS identity (host lowercased,
        // `.git` stripped, path case preserved).
        assert_eq!(
            normalize_git("git@GitHub.com:Org/Repo.git"),
            "https://github.com/Org/Repo"
        );
    }

    #[test]
    fn normalize_folds_transport_equivalent_remote_spellings() {
        // HTTPS, `ssh://` and scp-style spellings of the same remote must share
        // one identity, or a dependency could evade `(git, path)` dedup and
        // version-conflict detection by requesting a repo over a different
        // protocol. They all canonicalize to the HTTPS form.
        let https = normalize_git("https://github.com/o/r");
        assert_eq!(https, "https://github.com/o/r");
        assert_eq!(normalize_git("git@github.com:o/r.git"), https);
        assert_eq!(normalize_git("ssh://git@github.com/o/r"), https);
        assert_eq!(normalize_git("git@github.com:o/r"), https);
        assert_eq!(normalize_git("HTTPS://GitHub.com/o/r.git/"), https);
        // Schemes are case-insensitive: an uppercase `SSH://` must still fold to
        // the HTTPS identity. (Uppercase `HTTPS://` happens to collapse via the
        // generic lowercasing branch, but `SSH://` keeps its distinct `ssh://`
        // scheme + `git@` there, so only a case-insensitive remote parser folds
        // it — see `git::transport::split_remote`.)
        assert_eq!(normalize_git("SSH://git@GitHub.com/o/r"), https);
        assert_eq!(normalize_git("Ssh://git@github.com/o/r.git"), https);
    }

    #[test]
    fn normalize_treats_windows_drive_path_as_local() {
        // A bare Windows path must NOT be misread as an scp target: its drive
        // letter is a single-letter "authority", so stripping `.git` would fuse
        // `C:\work\repo.git` and `C:\work\repo` into one identity.
        assert_eq!(
            normalize_git(r"C:\work\repo.git"),
            r"C:\work\repo.git",
            "drive-letter path keeps its .git suffix and case"
        );
        assert_ne!(
            normalize_git(r"C:\work\repo.git"),
            normalize_git(r"C:\work\repo"),
            "distinct local dirs stay distinct identities"
        );
        // Forward-slash drive paths (`C:/…`) are treated the same.
        assert_eq!(normalize_git("D:/work/repo.git"), "D:/work/repo.git");
        // A genuine single-letter scp host (no path separator after the colon)
        // is still scp — the `.git` convention is stripped.
        assert_eq!(normalize_git("h:Org/Repo.git"), "h:Org/Repo");
    }

    #[test]
    fn normalize_treats_three_spellings_as_one_identity() {
        let a = normalize_git("https://github.com/o/r");
        let b = normalize_git("https://github.com/o/r.git");
        let c = normalize_git("https://GitHub.com/o/r/");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn normalize_leaves_local_path_case() {
        assert_eq!(normalize_git("/home/User/Repo"), "/home/User/Repo");
        assert_eq!(
            normalize_git("file:///home/User/Repo"),
            "file:///home/User/Repo"
        );
    }

    #[test]
    fn normalize_preserves_git_suffix_for_local_and_file_sources() {
        // `repo.git` and `repo` can be two distinct local directories, so the
        // `.git` suffix must survive for both bare paths and `file://` URLs —
        // otherwise unrelated skills fuse into one transitive identity.
        assert_eq!(normalize_git("/tmp/repo.git"), "/tmp/repo.git");
        assert_ne!(normalize_git("/tmp/repo.git"), normalize_git("/tmp/repo"));
        assert_eq!(
            normalize_git("file:///tmp/repo.git"),
            "file:///tmp/repo.git"
        );
        assert_ne!(
            normalize_git("file:///tmp/repo.git"),
            normalize_git("file:///tmp/repo")
        );
        // A trailing slash is still trimmed, but the suffix stays.
        assert_eq!(
            normalize_git("file:///tmp/repo.git/"),
            "file:///tmp/repo.git"
        );
    }

    #[test]
    fn normalize_still_strips_git_suffix_for_remote_sources() {
        assert_eq!(
            normalize_git("https://github.com/o/r.git"),
            normalize_git("https://github.com/o/r")
        );
        assert_eq!(
            normalize_git("git@github.com:o/r.git"),
            "https://github.com/o/r"
        );
    }

    #[test]
    fn synth_name_is_stable_and_separator_free() {
        let key = (
            "https://github.com/o/r".to_string(),
            Some("skills/x".to_string()),
        );
        let a = synth_name("foo", "helper", &key);
        let b = synth_name("foo", "helper", &key);
        assert_eq!(a, b, "deterministic");
        assert!(
            DepName::try_from(a.as_str()).is_ok(),
            "must be a valid skill name: {a}"
        );
        assert!(a.starts_with("foo__helper-"));
    }

    #[test]
    fn synth_name_stays_within_length_cap_at_max_depth() {
        let key = ("https://github.com/o/r".to_string(), None);
        let mut name = "r".repeat(100);
        for _ in 0..MAX_DEPTH {
            name = synth_name(&name, &"d".repeat(100), &key);
            assert!(name.len() <= MAX_SYNTH_NAME_BYTES, "{} bytes", name.len());
            assert!(DepName::try_from(name.as_str()).is_ok());
            assert!(name.ends_with(&format!("-{}", short_hash(&key))));
        }
    }

    #[test]
    fn synth_name_truncation_respects_char_boundaries() {
        let key = ("https://github.com/o/r".to_string(), None);
        let name = synth_name(&"é".repeat(300), &"ü".repeat(300), &key);
        assert!(name.len() <= MAX_SYNTH_NAME_BYTES);
        assert!(DepName::try_from(name.as_str()).is_ok());
    }

    #[test]
    fn different_identities_hash_differently() {
        let k1 = ("https://github.com/o/r".to_string(), None);
        let k2 = ("https://github.com/o/other".to_string(), None);
        assert_ne!(short_hash(&k1), short_hash(&k2));
    }

    #[test]
    fn canonical_subpath_folds_equivalent_spellings() {
        let want = Some("skills/x".to_string());
        assert_eq!(canonical_subpath(&Some("skills/x".into())), want);
        assert_eq!(canonical_subpath(&Some("skills/./x".into())), want);
        assert_eq!(canonical_subpath(&Some("skills//x".into())), want);
        assert_eq!(canonical_subpath(&Some("skills/x/".into())), want);
        assert_eq!(canonical_subpath(&Some("./skills/x".into())), want);
    }

    #[test]
    fn canonical_subpath_folds_root_equivalent_forms_to_none() {
        assert_eq!(canonical_subpath(&None), None);
        assert_eq!(canonical_subpath(&Some(".".into())), None);
        assert_eq!(canonical_subpath(&Some("".into())), None);
        assert_eq!(canonical_subpath(&Some("./".into())), None);
    }

    /// `skills/x` and `skills/./x` name the same content, so their transitive
    /// identity key must be equal — otherwise dedup/version-conflict detection
    /// can be bypassed and the same skill materialized twice.
    #[test]
    fn key_of_dedups_dot_component_paths() {
        let git = "https://github.com/o/r";
        assert_eq!(
            key_of(git, &Some("skills/x".into())),
            key_of(git, &Some("skills/./x".into()))
        );
    }
}
