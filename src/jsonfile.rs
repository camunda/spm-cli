//! Reading and writing spm's JSON files (`ai.json`, `ai.lock`, the links file,
//! the ownership record, vendor config): one place for the error context
//! naming the file, the pretty format with its trailing newline, and the one
//! atomic-replace implementation.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

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

/// Write `value` to `path` (see [`to_text`]) in place.
pub fn save<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    std::fs::write(path, to_text(value)?).with_context(|| format!("writing {}", path.display()))
}

/// Replace the entry at `path` with `value` (see [`to_text`]) all at once
/// (see [`Staged`]), so a failed write leaves the previous file intact and a
/// concurrent reader sees either the old or the new one, never a truncated
/// one. A symlink at `path` is replaced, not followed: resolve it first to
/// write through it.
pub fn save_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let text = to_text(value)?;
    replace_with(path, |f| f.write_all(text.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))
}

/// [`Staged::write`] then [`Staged::publish`].
pub fn replace_with(
    path: &Path,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    Staged::write(path, write)?.publish()
}

/// A complete replacement for a file, staged beside it as a hidden `.tmp`
/// file (which readers listing the directory for `.json` files skip) until
/// [`Staged::publish`] renames it over the destination. Dropped unpublished,
/// it is removed.
#[must_use = "a staged file is removed unless published"]
pub struct Staged {
    tmp: PathBuf,
    dest: PathBuf,
    published: bool,
}

impl Staged {
    /// Stage what `write` writes as the next content of `dest`, flushed to
    /// disk. `dest`'s directory must exist.
    pub fn write(
        dest: &Path,
        write: impl FnOnce(&mut File) -> std::io::Result<()>,
    ) -> std::io::Result<Self> {
        let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a file path", dest.display()),
            ));
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let tmp = dir.join(format!(
            ".{}.{}-{nanos}.tmp",
            name.to_string_lossy(),
            std::process::id()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        let staged = Self {
            tmp,
            dest: dest.to_path_buf(),
            published: false,
        };
        write(&mut file)?;
        file.sync_all()?;
        Ok(staged)
    }

    /// Atomically replace the destination with the staged file.
    pub fn publish(mut self) -> std::io::Result<()> {
        std::fs::rename(&self.tmp, &self.dest)?;
        self.published = true;
        Ok(())
    }

    /// Where the replacement is staged.
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.tmp
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for (i, save) in [save::<serde_json::Value>, save_atomic].iter().enumerate() {
            let p = dir.join(format!("{i}.json"));
            save(&p, &value).unwrap();
            let text = std::fs::read_to_string(&p).unwrap();
            assert_eq!(text, serde_json::to_string_pretty(&value).unwrap() + "\n");
            assert_eq!(load::<serde_json::Value>(&p).unwrap(), value);
        }
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

    /// A failed write (e.g. a full disk) or an abandoned stage leaves the
    /// previous file intact and no staging file behind.
    #[test]
    fn a_failed_replacement_keeps_the_previous_file() {
        let dir = scratch("failed");
        let p = dir.join("state.json");
        std::fs::write(&p, "old").unwrap();
        let err = replace_with(&p, |f| {
            f.write_all(b"ne")?;
            Err(std::io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "disk full");
        drop(Staged::write(&p, |f| f.write_all(b"new")).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        assert!(tmp_files(&dir).is_empty(), "{:?}", tmp_files(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Until published, readers see the old file and the staged one beside
    /// it; publishing replaces the entry at the destination.
    #[test]
    fn publishing_replaces_the_destination_at_once() {
        let dir = scratch("publish");
        let p = dir.join("state.json");
        std::fs::write(&p, "old").unwrap();
        let staged = Staged::write(&p, |f| f.write_all(b"new")).unwrap();
        assert!(staged.path().is_file());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        staged.publish().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        assert!(tmp_files(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A symlink at the destination is replaced, not written through.
    #[cfg(unix)]
    #[test]
    fn save_atomic_replaces_a_symlink_rather_than_following_it() {
        let dir = scratch("symlink");
        let real = dir.join("real.json");
        let link = dir.join("link.json");
        std::fs::write(&real, "{}").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        save_atomic(&link, &1).unwrap();
        assert!(!std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "{}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
