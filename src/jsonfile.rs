//! Reading and writing spm's JSON files (`ai.json`, `ai.lock`, the links file,
//! the ownership record, vendor config): one place for the error context
//! naming the file, the pretty format with its trailing newline, and saving
//! through [`atomic`], so no JSON file is ever written in place.

use crate::atomic::{self, Staged};
use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

/// The text of `path`, or `None` if it does not exist.
pub fn read(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Parse `text`, read from `path` (only used to label errors).
pub fn parse<T: DeserializeOwned>(text: &str, path: &Path) -> Result<T> {
    serde_json::from_str(text).with_context(|| format!("parsing {}", path.display()))
}

/// The text of `path`, which must exist.
pub fn load_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Read and parse `path`, which must exist.
pub fn load<T: DeserializeOwned>(path: &Path) -> Result<T> {
    parse(&load_text(path)?, path)
}

/// Read and parse `path`, or `T::default()` if it does not exist.
pub fn load_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    read(path)?.map_or_else(|| Ok(T::default()), |text| parse(&text, path))
}

/// `value` as spm writes every JSON file: pretty-printed, newline-terminated.
pub fn to_text<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}

/// Replace `path` with `value` (see [`to_text`]) all at once, writing
/// through a symlink at `path` (see [`atomic::replace`]): a failed write
/// leaves the previous file intact and a concurrent reader sees either the
/// old or the new one, never a truncated one.
pub fn save<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let text = to_text(value)?;
    atomic::replace(path, |f| f.write_all(text.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))
}

/// Stage `value` (see [`to_text`]) as the next content of `path`, written
/// through a symlink at `path`, for the caller to publish (see [`save`]).
pub fn stage<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<Staged> {
    let text = to_text(value)?;
    atomic::resolve(path)
        .and_then(|dest| Staged::write(&dest, |f| f.write_all(text.as_bytes())))
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d =
            std::env::temp_dir().join(format!("spm-jsonfile-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn tmp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn saves_pretty_with_a_trailing_newline_and_loads_it_back() {
        let dir = scratch("roundtrip");
        let value = serde_json::json!({"b": [1], "a": {}});
        let p = dir.join("value.json");
        save(&p, &value).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text, serde_json::to_string_pretty(&value).unwrap() + "\n");
        assert_eq!(load::<serde_json::Value>(&p).unwrap(), value);
        assert!(tmp_files(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Only a missing file falls back to the default; every other failure
    /// names the file.
    #[test]
    fn errors_name_the_file_and_only_a_missing_one_is_defaulted() {
        let dir = scratch("errors");
        let missing = dir.join("missing.json");
        assert_eq!(read(&missing).unwrap(), None);
        assert_eq!(
            load_or_default::<Vec<u8>>(&missing).unwrap(),
            Vec::<u8>::new()
        );
        let err = format!("{:#}", load::<Vec<u8>>(&missing).unwrap_err());
        assert!(err.starts_with(&format!("reading {}", missing.display())));

        let unreadable = dir.join("dir.json");
        std::fs::create_dir(&unreadable).unwrap();
        let err = format!("{:#}", load_or_default::<Vec<u8>>(&unreadable).unwrap_err());
        assert!(err.starts_with(&format!("reading {}", unreadable.display())));

        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{").unwrap();
        let err = format!("{:#}", load_or_default::<Vec<u8>>(&bad).unwrap_err());
        assert!(err.starts_with(&format!("parsing {}", bad.display())));

        let err = format!("{:#}", save(&dir.join("no/such.json"), &1).unwrap_err());
        assert!(err.starts_with("writing "), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A symlinked file (e.g. a shared `ai.json`) is written through: the
    /// link stays and what it points at is replaced.
    #[cfg(unix)]
    #[test]
    fn save_writes_through_a_symlink() {
        let dir = scratch("symlink");
        let real = dir.join("real.json");
        let link = dir.join("link.json");
        std::fs::write(&real, "{}").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        save(&link, &1).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "1\n");
        assert!(tmp_files(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
