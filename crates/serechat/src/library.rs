//! Worlds, characters and personas, saved as one JSON file each in
//! `~/.openrp/worlds/`, `~/.openrp/characters/` and `~/.openrp/personas/`, and
//! their portraits in `~/.openrp/portraits/`.
//!
//! A world is a setting to play in (say, Panem or a galaxy far away); a
//! character is someone the model plays and the user talks to; a persona is
//! someone the user plays, kept to start stories as. There are few
//! of them and they are small, so listing reads every file; no index.
//!
//! Both can hold lore: [`LoreEntry`]s the model reads only while the story
//! mentions one of their keys (or always, when constant), so a large setting
//! costs context only where it matters. Characters also carry what character
//! cards do (see `card.rs`): greetings that open a story and example dialogue.
//!
//! Portraits are the app's own copies of PNG or JPEG files, named
//! `<id>.png` / `<id>.jpg`, and shared: a story's cast copies them from the
//! library. Records name them by file name only; a name that
//! is anything else (say, a hand-edited `../secret`) is ignored.
//!
//! Files are written atomically with the same private permissions as the
//! config. Fields added later must be `#[serde(default)]` so older files keep
//! loading. Each record notes the format version that wrote it: one from a
//! newer version of the app is shown but must not be saved over (see
//! [`World::is_newer`]), since that would drop what this version does not know.

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
    /// The format version that wrote the file; `0` for files from before
    /// versions were recorded.
    pub version: u32,
    /// Identifier, also the file name without `.json`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What the world is like: its premise, places, rules and tone.
    pub description: String,
    /// File name of its portrait in [`Portraits`]; empty for none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub portrait: String,
    /// The user's own note to tell it apart (say, which version of a card
    /// it is): never sent to the model, nor written into an exported card.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// Labels to find it by, as the user typed them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Starred: listed first.
    #[serde(skip_serializing_if = "is_false")]
    pub favorite: bool,
    /// Places, factions, history: read by the model when the story mentions them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lore: Vec<LoreEntry>,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Last edit, seconds since the Unix epoch.
    pub updated: u64,
}

/// Someone the model plays.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Character {
    /// The format version that wrote the file, as for [`World::version`].
    pub version: u32,
    /// Identifier, also the file name without `.json`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Who they are: background, personality, appearance, voice.
    pub description: String,
    /// File name of their portrait in [`Portraits`]; empty for none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub portrait: String,
    /// The user's own note to tell it apart (say, which version of a card
    /// it is): never sent to the model, nor written into an exported card.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// What they say to open a story they are cast in: the first, or
    /// another swiped to. `{{user}}` and `{{char}}` stand for the user's
    /// character and theirs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub greetings: Vec<String>,
    /// Example dialogue showing how they talk, for the model.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub examples: String,
    /// Labels to find them by, as the user typed them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Starred: listed first.
    #[serde(skip_serializing_if = "is_false")]
    pub favorite: bool,
    /// What they know about: read by the model when the story mentions it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lore: Vec<LoreEntry>,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Last edit, seconds since the Unix epoch.
    pub updated: u64,
}

/// Someone the user plays, kept to start stories as.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Persona {
    /// The format version that wrote the file, as for [`World::version`].
    pub version: u32,
    /// Identifier, also the file name without `.json`.
    pub id: String,
    /// Their name.
    pub name: String,
    /// Who they are, for the model.
    pub description: String,
    /// File name of their portrait in [`Portraits`]; empty for none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub portrait: String,
    /// The user's own note to tell it apart (say, which version of a card
    /// it is): never sent to the model, nor written into an exported card.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// Labels to find them by, as the user typed them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Starred: listed first, and filled in when a story asks who the user is.
    #[serde(skip_serializing_if = "is_false")]
    pub favorite: bool,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Last edit, seconds since the Unix epoch.
    pub updated: u64,
}

impl Persona {
    /// The format this version of the app writes.
    pub const VERSION: u32 = 1;

    /// Saved by a newer version of the app: show it, but don't save over it.
    #[must_use]
    pub fn is_newer(&self) -> bool {
        self.version > Self::VERSION
    }
}

/// A piece of lore: background the model reads only while the latest turns
/// mention one of its keys, or always when it is constant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoreEntry {
    /// Words or names that bring it in, matched as whole words in any case
    /// (with a plural ending).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    /// What the model reads.
    pub content: String,
    /// Always read, whatever is said.
    #[serde(skip_serializing_if = "is_false")]
    pub constant: bool,
}

impl LoreEntry {
    /// Whether `text` (already lower-cased) mentions one of the keys as a
    /// whole word. Keys in scripts without spaces between words (Chinese,
    /// Japanese) match anywhere.
    ///
    /// ponytail: keys are plain words: SillyTavern's `/regex/` keys and
    /// secondary keys are read as words or dropped. Add them if imported
    /// books need them.
    #[must_use]
    pub fn mentioned_in(&self, text: &str) -> bool {
        self.keys.iter().any(|key| {
            let key = key.trim().to_lowercase();
            !key.is_empty() && text.match_indices(key.as_str()).any(|(at, _)| whole_word(text, at, at + key.len()))
        })
    }
}

/// Whether `text[start..end]` stands alone: no letter or digit runs into an
/// ASCII letter or digit at its edges, except a plural ending (`s`, `es`,
/// `'s`) after it.
fn whole_word(text: &str, start: usize, end: usize) -> bool {
    let (word, joined) = (|c: char| c.is_ascii_alphanumeric(), |s: &str| s.chars().next().is_some_and(char::is_alphanumeric));
    let found = &text[start..end];
    if found.chars().next().is_some_and(word) && text[..start].chars().next_back().is_some_and(char::is_alphanumeric) {
        return false;
    }
    let after = &text[end..];
    !found.chars().next_back().is_some_and(word) || ["", "s", "es", "'s"].iter().any(|ending| after.strip_prefix(ending).is_some_and(|rest| !joined(rest)))
}

/// For `skip_serializing_if`.
#[expect(clippy::trivially_copy_pass_by_ref, reason = "serde passes a reference")]
fn is_false(value: &bool) -> bool {
    !*value
}

impl World {
    /// The format this version of the app writes; 2 added tags, the
    /// favourite star and lore.
    pub const VERSION: u32 = 2;

    /// Saved by a newer version of the app: show it, but don't save over it.
    #[must_use]
    pub fn is_newer(&self) -> bool {
        self.version > Self::VERSION
    }
}

impl Character {
    /// The format this version of the app writes; 2 added greetings,
    /// examples, tags, the favourite star and lore.
    pub const VERSION: u32 = 2;

    /// Saved by a newer version of the app: show it, but don't save over it.
    #[must_use]
    pub fn is_newer(&self) -> bool {
        self.version > Self::VERSION
    }
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

    /// The user's personas in `~/.openrp/personas`.
    ///
    /// # Errors
    /// [`Error::NoHomeDir`] if the platform reports no home directory.
    pub fn personas() -> Result<Self> {
        Ok(Self::at(Config::dir()?.join("personas")))
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

/// An error for data that is not what it should be, saying why.
pub(crate) fn invalid(message: &str) -> Error {
    Error::Io(std::io::Error::new(ErrorKind::InvalidData, message.to_owned()))
}

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
        if fs::metadata(source)?.len() > MAX_PORTRAIT {
            return Err(invalid("The image is larger than 20 MB."));
        }
        self.add(&fs::read(source)?)
    }

    /// Saves the PNG or JPEG `bytes` under a fresh name and returns that name.
    ///
    /// # Errors
    /// The image is larger than 20 MB, is not a PNG or JPEG, or cannot be
    /// written.
    pub fn add(&self, bytes: &[u8]) -> Result<String> {
        if bytes.len() as u64 > MAX_PORTRAIT {
            return Err(invalid("The image is larger than 20 MB."));
        }
        let ext = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "png"
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "jpg"
        } else {
            return Err(invalid("Only PNG and JPEG images can be portraits."));
        };
        let name = format!("{}.{ext}", crate::session::new_id());
        write_private(&self.dir.join(&name), bytes)?;
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
    fn lore_is_found_by_whole_words() {
        let entry = |keys: &[&str]| LoreEntry { keys: keys.iter().map(|k| (*k).to_owned()).collect(), ..LoreEntry::default() };
        let text = "the dragons of valyria. a cat's toy, 龍が来た.";
        assert!(entry(&["Dragon"]).mentioned_in(text), "plural, any case");
        assert!(entry(&["valyria"]).mentioned_in(text) && entry(&["cat"]).mentioned_in(text));
        assert!(!entry(&["drag"]).mentioned_in(text) && !entry(&["ria"]).mentioned_in(text), "not inside a word");
        assert!(entry(&["龍"]).mentioned_in(text), "scripts without spaces match anywhere");
        assert!(!entry(&["", "  "]).mentioned_in(text) && !entry(&[]).mentioned_in(text));
        assert!(entry(&["toy,"]).mentioned_in(text), "punctuation in a key");
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
