//! Worlds and characters, saved as one JSON file each in
//! `~/.openrp/worlds/` and `~/.openrp/characters/`, and their portraits in
//! `~/.openrp/portraits/`.
//!
//! A world is a setting to play in (say, Panem or a galaxy far away); a
//! character is someone the model plays and the user talks to. There are few
//! of them and they are small, so listing reads every file; no index.
//!
//! Portraits are the app's own copies of PNG or JPEG files, named
//! `<id>.png` / `<id>.jpg`, and shared: a story's cast copies them from the
//! library. Records name them by file name only; a name that
//! is anything else (say, a hand-edited `../secret`) is ignored.
//!
//! Files are written atomically with the same private permissions as the
//! config. Fields added later must be `#[serde(default)]` so older files keep
//! loading.

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::{Config, write_private};
use crate::error::{Error, Result};
use crate::session::valid_id;

/// A setting stories take place in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct World {
    /// Identifier, also the file name without `.json`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What the world is like: its premise, places, rules and tone.
    pub description: String,
    /// File name of its portrait in [`Portraits`]; empty for none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub portrait: String,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Last edit, seconds since the Unix epoch.
    pub updated: u64,
}

/// Someone the model plays.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Character {
    /// Identifier, also the file name without `.json`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Who they are: background, personality, appearance, voice.
    pub description: String,
    /// File name of their portrait in [`Portraits`]; empty for none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub portrait: String,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Last edit, seconds since the Unix epoch.
    pub updated: u64,
}

/// A folder of records of one kind, one `<id>.json` file each.
#[derive(Debug, Clone)]
pub struct Library {
    dir: PathBuf,
}

impl Library {
    /// The worlds in `~/.openrp/worlds`.
    ///
    /// # Errors
    /// [`Error::NoHomeDir`] if the platform reports no home directory.
    pub fn worlds() -> Result<Self> {
        Ok(Self::at(Config::dir()?.join("worlds")))
    }

    /// The characters in `~/.openrp/characters`.
    ///
    /// # Errors
    /// [`Error::NoHomeDir`] if the platform reports no home directory.
    pub fn characters() -> Result<Self> {
        Ok(Self::at(Config::dir()?.join("characters")))
    }

    /// A library in an explicit directory (created on first save).
    #[must_use]
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The directory holding the files.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        if valid_id(id) {
            Ok(self.dir.join(format!("{id}.json")))
        } else {
            Err(Error::Io(std::io::Error::new(ErrorKind::InvalidInput, "invalid id")))
        }
    }

    /// Every record, in no particular order, plus the errors of files that
    /// could not be read (those are skipped). A missing folder is empty.
    ///
    /// # Errors
    /// The directory exists but cannot be listed.
    pub fn list<T: DeserializeOwned>(&self) -> Result<(Vec<T>, Vec<Error>)> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
            Err(e) => return Err(e.into()),
        };
        let (mut records, mut errors) = (Vec::new(), Vec::new());
        for entry in entries {
            let path = entry?.path();
            let named = path.file_stem().and_then(|s| s.to_str()).is_some_and(valid_id);
            if !named || path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            match fs::read(&path).map_err(Error::from).and_then(|bytes| Ok(serde_json::from_slice(&bytes)?)) {
                Ok(record) => records.push(record),
                Err(e) => errors.push(e),
            }
        }
        Ok((records, errors))
    }

    /// Writes `record` atomically as `<id>.json`.
    ///
    /// # Errors
    /// An invalid id or any I/O failure.
    pub fn save<T: Serialize>(&self, id: &str, record: &T) -> Result<()> {
        write_private(&self.path(id)?, &serde_json::to_vec_pretty(record)?)
    }

    /// Deletes `<id>.json`; deleting a missing record succeeds.
    ///
    /// # Errors
    /// An invalid id or any I/O failure other than "not found".
    pub fn delete(&self, id: &str) -> Result<()> {
        match fs::remove_file(self.path(id)?) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// Largest portrait accepted, in bytes.
const MAX_PORTRAIT: u64 = 20 << 20;

/// The portrait images, in one folder.
#[derive(Debug, Clone)]
pub struct Portraits {
    dir: PathBuf,
}

impl Portraits {
    /// The portraits in `~/.openrp/portraits`.
    ///
    /// # Errors
    /// [`Error::NoHomeDir`] if the platform reports no home directory.
    pub fn open() -> Result<Self> {
        Ok(Self::at(Config::dir()?.join("portraits")))
    }

    /// Portraits in an explicit directory (created on first import).
    #[must_use]
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Where the portrait called `name` lives; `None` for an empty or
    /// malformed name.
    #[must_use]
    pub fn path(&self, name: &str) -> Option<PathBuf> {
        let (stem, ext) = name.rsplit_once('.')?;
        (valid_id(stem) && matches!(ext, "png" | "jpg")).then(|| self.dir.join(name))
    }

    /// Copies the PNG or JPEG at `source` in under a fresh name and returns
    /// that name.
    ///
    /// # Errors
    /// The file cannot be read, is larger than 20 MB, is not a PNG or JPEG,
    /// or cannot be written.
    pub fn import(&self, source: &Path) -> Result<String> {
        let invalid = |message: &str| Error::Io(std::io::Error::new(ErrorKind::InvalidData, message.to_owned()));
        if fs::metadata(source)?.len() > MAX_PORTRAIT {
            return Err(invalid("The image is larger than 20 MB."));
        }
        let bytes = fs::read(source)?;
        let ext = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "png"
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "jpg"
        } else {
            return Err(invalid("Only PNG and JPEG images can be portraits."));
        };
        let name = format!("{}.{ext}", crate::session::new_id());
        write_private(&self.dir.join(&name), &bytes)?;
        Ok(name)
    }

    /// Deletes every portrait not named in `keep` and last changed more than
    /// `grace` ago (so one just picked for an unsaved form survives).
    /// Portraits are shared (stories copy them from the library), so
    /// nothing deletes one directly; this runs once at startup instead.
    /// Returns how many were deleted.
    ///
    /// # Errors
    /// The folder exists but cannot be listed.
    pub fn collect_garbage(&self, keep: &HashSet<String>, grace: Duration) -> Result<usize> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let mut deleted = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > grace);
            // Only the app's own files: anything else in the folder stays.
            if old && !keep.contains(&name) && self.path(&name).is_some() && fs::remove_file(entry.path()).is_ok() {
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portraits_import_and_collect() {
        let dir = std::env::temp_dir().join(format!("openrp-portraits-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let portraits = Portraits::at(dir.join("portraits"));
        let (png, text) = (dir.join("face.png"), dir.join("notes.txt"));
        fs::write(&png, b"\x89PNG\r\n\x1a\nrest").unwrap();
        fs::write(&text, b"hello").unwrap();

        let name = portraits.import(&png).unwrap();
        assert_eq!(Path::new(&name).extension().and_then(|e| e.to_str()), Some("png"));
        let copy = portraits.path(&name).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), fs::read(&png).unwrap());
        assert!(portraits.import(&text).is_err(), "not an image");

        for bad in ["", "x", "../evil.png", "a.gif", "a/b.png"] {
            assert!(portraits.path(bad).is_none(), "{bad}");
        }

        // Unreferenced portraits go once old enough; kept and foreign files stay.
        let kept = portraits.import(&png).unwrap();
        let foreign = dir.join("portraits").join("notes.txt");
        fs::write(&foreign, b"mine").unwrap();
        let keep = HashSet::from([kept.clone()]);
        assert_eq!(portraits.collect_garbage(&keep, Duration::from_secs(3600)).unwrap(), 0, "too new");
        assert_eq!(portraits.collect_garbage(&keep, Duration::ZERO).unwrap(), 1);
        assert!(!copy.exists() && portraits.path(&kept).unwrap().exists() && foreign.exists());
        assert_eq!(Portraits::at(dir.join("missing")).collect_garbage(&keep, Duration::ZERO).unwrap(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_list_delete() {
        let dir = std::env::temp_dir().join(format!("openrp-library-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let library = Library::at(dir.clone());
        assert!(library.list::<World>().unwrap().0.is_empty(), "a missing folder is empty");

        let world =
            World { id: "w1".into(), name: "Panem".into(), description: "Twelve districts.".into(), created: 1, updated: 2, ..World::default() };
        library.save(&world.id, &world).unwrap();
        fs::write(dir.join("broken.json"), "{").unwrap();
        fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let (worlds, errors) = library.list::<World>().unwrap();
        assert_eq!(worlds, [world]);
        assert_eq!(errors.len(), 1, "the broken file is reported, not listed");

        library.delete("w1").unwrap();
        library.delete("w1").unwrap();
        assert_eq!(library.list::<World>().unwrap().0.len(), 0);
        assert!(library.save("../evil", &Character::default()).is_err());
        assert!(library.delete("..").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
