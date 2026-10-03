//! Chat sessions saved as one JSON file each in `~/.openrp/sessions/`.
//!
//! Next to them, `.index.json` holds a [`SessionSummary`] per session so the
//! sidebar and usage totals load without reading any message bodies. The
//! index is a cache: [`SessionStore::list`] reconciles it against the files
//! on disk (a directory listing plus modification times, no reads) and
//! re-reads only sessions that are new or changed since the index was
//! written, so a crash between the two writes of a save heals itself.
//!
//! Files are written atomically with the same private permissions as the
//! config. Fields added later must be `#[serde(default)]` so older files keep
//! loading.

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::{Config, write_private};
use crate::error::{Error, Result};
use crate::responses::{Role, Usage};

/// Name of the index file. The leading dot keeps it out of session scans,
/// since `.` is not a valid id character.
const INDEX_FILE: &str = ".index.json";

/// One saved conversation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Identifier, also the file name without `.json`.
    pub id: String,
    /// Sidebar title, taken from the first prompt.
    pub title: String,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Last activity, seconds since the Unix epoch.
    pub updated: u64,
    /// Id of the [`World`](crate::World) this story is played in; `None` for
    /// a plain chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
    /// The characters taking part in this story.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<CastMember>,
    /// Who the user plays; `None` until they say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<Player>,
    /// Where the story is now and what it is like (place, time, mood), as
    /// the model last set it; empty until it does.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub scene: String,
    /// What the story must not forget (promises, secrets, injuries), one
    /// fact each, kept by the model and editable by the user.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<String>,
    /// The user's guidance for the whole story (tone, pacing, limits),
    /// sent with every request.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// How many of `messages` the last memory review had read, so the next
    /// one covers only what came after.
    #[serde(skip_serializing_if = "is_default")]
    pub reviewed: usize,
    /// Every turn, oldest first.
    pub messages: Vec<StoredMessage>,
}

/// A character taking part in a story.
///
/// A copy, not a reference: taken from the [`Character`](crate::Character)
/// library when cast (or made up by the model during the story), so later
/// edits or deletions in the library never change a story.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CastMember {
    /// The library character's id, or a fresh one for a character the
    /// model created.
    #[serde(alias = "character")]
    pub id: String,
    /// Display name, also how the model refers to them.
    pub name: String,
    /// Who they are, for the model.
    pub description: String,
    /// Portrait file name in [`Portraits`](crate::Portraits); empty for none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub portrait: String,
    /// In the current scene; absent characters belong to the story but are
    /// elsewhere.
    pub present: bool,
}

/// The character the user plays in a story.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Player {
    /// Their name.
    pub name: String,
    /// Who they are, for the model.
    pub description: String,
}

/// A tool call a reply made, with the result sent back to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    /// The call as the model made it.
    #[serde(flatten)]
    pub call: crate::responses::ToolCall,
    /// What the model was told about it.
    #[serde(default)]
    pub output: String,
}

impl Session {
    /// Total spent on this session in USD.
    #[must_use]
    pub fn cost(&self) -> f64 {
        self.messages.iter().map(|m| m.cost).sum()
    }

    /// Total tokens billed for this session.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.messages.iter().map(|m| m.usage.input_tokens + m.usage.output_tokens).sum()
    }

    /// The index entry for this session.
    #[must_use]
    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            created: self.created,
            updated: self.updated,
            world: self.world.clone(),
            cost: self.cost(),
            tokens: self.tokens(),
        }
    }
}

/// What the sidebar and usage totals need to know about a session, without
/// its messages.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionSummary {
    /// Session id.
    pub id: String,
    /// Sidebar title.
    pub title: String,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Last activity, seconds since the Unix epoch.
    pub updated: u64,
    /// The world played in, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
    /// Total spent in USD.
    pub cost: f64,
    /// Total tokens billed.
    pub tokens: u64,
}

/// One turn of a saved session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    /// Author.
    pub role: Role,
    /// Text of the prompt or reply (or the error, when `failed`).
    pub content: String,
    /// The model's reasoning, for replies from thinking models.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    /// How long the model reasoned before answering, in milliseconds
    /// (0 when unknown or it did not reason).
    #[serde(default, skip_serializing_if = "is_default")]
    pub reasoning_ms: u64,
    /// Model that wrote a reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tokens billed for a reply.
    #[serde(default, skip_serializing_if = "is_default")]
    pub usage: Usage,
    /// What a reply cost in USD, at the prices when it was written.
    #[serde(default, skip_serializing_if = "is_default")]
    pub cost: f64,
    /// The turn is an error message, not model output.
    #[serde(default, skip_serializing_if = "is_default")]
    pub failed: bool,
    /// Tools a reply called, with their results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolResult>,
    /// A summary that replaces every earlier message when talking to the
    /// model, written when the conversation outgrew the context window.
    /// The earlier messages stay in the session for the user.
    #[serde(default, skip_serializing_if = "is_default")]
    pub compaction: bool,
    /// What a reply's tool calls changed in its story, so deleting or
    /// regenerating it can undo that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<StoryChange>,
}

/// What a reply changed in its story: each cast member it touched, as they
/// were before (`None` when it added them) and after, the scene before and
/// after when it set a new one, and the memories it added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoryChange {
    /// (before, after) of each member it added or changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<(Option<CastMember>, CastMember)>,
    /// (before, after) of the scene, if it set one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<(String, String)>,
    /// Memories it added.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<String>,
}

impl StoryChange {
    /// What turned `cast` and `scene` into `cast_after` and `scene_after`
    /// while `remembered` was added to the memories; `None` when nothing
    /// changed. Tools never remove members or memories.
    #[must_use]
    pub fn between(cast: &[CastMember], scene: &str, cast_after: &[CastMember], scene_after: &str, remembered: &[String]) -> Option<Self> {
        let changed = cast_after.iter().filter_map(|after| {
            let before = cast.iter().find(|m| m.id == after.id);
            (before != Some(after)).then(|| (before.cloned(), after.clone()))
        });
        let change = Self {
            cast: changed.collect(),
            scene: (scene != scene_after).then(|| (scene.to_owned(), scene_after.to_owned())),
            memories: remembered.to_vec(),
        };
        (change != Self::default()).then_some(change)
    }

    /// Undoes the change in `cast`, `scene` and `memories`, except where
    /// something changed them again since (the user, or a later reply not
    /// undone).
    pub fn undo(&self, cast: &mut Vec<CastMember>, scene: &mut String, memories: &mut Vec<String>) {
        for memory in &self.memories {
            if let Some(index) = memories.iter().rposition(|m| m == memory) {
                memories.remove(index);
            }
        }
        for (before, after) in self.cast.iter().rev() {
            let Some(index) = cast.iter().position(|m| m == after) else { continue };
            match before {
                Some(before) => cast[index] = before.clone(),
                None => {
                    cast.remove(index);
                }
            }
        }
        if let Some((before, after)) = &self.scene
            && scene == after
        {
            scene.clone_from(before);
        }
    }
}

impl StoredMessage {
    /// A plain message without reply metadata.
    #[must_use]
    pub fn new(role: Role, content: String) -> Self {
        Self {
            role,
            content,
            reasoning: String::new(),
            reasoning_ms: 0,
            model: None,
            usage: Usage::default(),
            cost: 0.0,
            failed: false,
            tool_calls: Vec::new(),
            compaction: false,
            change: None,
        }
    }
}

/// A message matching a content search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Session id.
    pub session: String,
    /// Session title.
    pub title: String,
    /// The matching line, shortened around the match.
    pub snippet: String,
    /// Session's last activity, for ranking.
    pub updated: u64,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// Seconds since the Unix epoch.
#[must_use]
pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A fresh, file-name-safe id for a session, world or character (the
/// creation time in hex nanoseconds).
#[must_use]
pub fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{nanos:x}")
}

/// Ids become file names, so only a conservative character set is allowed.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// On-disk shape of [`INDEX_FILE`].
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Index {
    version: u32,
    sessions: Vec<SessionSummary>,
}

/// Reads and writes the session files in one directory, keeping the index
/// up to date.
#[derive(Debug)]
pub struct SessionStore {
    dir: PathBuf,
    /// Summaries, newest first. Valid once `indexed` is set.
    index: Vec<SessionSummary>,
    indexed: bool,
}

impl SessionStore {
    /// The store in `~/.openrp/sessions`.
    ///
    /// # Errors
    /// [`Error::NoHomeDir`] if the platform reports no home directory.
    pub fn open() -> Result<Self> {
        Ok(Self::at(Config::dir()?.join("sessions")))
    }

    /// A store in an explicit directory (created on first save). Cheap: no
    /// I/O happens until a method needs it.
    #[must_use]
    pub fn at(dir: PathBuf) -> Self {
        Self { dir, index: Vec::new(), indexed: false }
    }

    /// The directory holding the session files.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        if valid_id(id) {
            Ok(self.dir.join(format!("{id}.json")))
        } else {
            Err(Error::Io(std::io::Error::new(ErrorKind::InvalidInput, "invalid session id")))
        }
    }

    /// Summaries of every session, most recently updated first, plus the
    /// errors of session files that could not be read (those files are left
    /// untouched and not listed).
    ///
    /// Reads only the index, re-reading session files that are missing from
    /// it or changed after it was written.
    ///
    /// # Errors
    /// The directory exists but cannot be listed.
    pub fn list(&mut self) -> Result<(Vec<SessionSummary>, Vec<Error>)> {
        let index_path = self.dir.join(INDEX_FILE);
        let (mut index, index_time) = match fs::read(&index_path) {
            // A corrupt index is rebuilt from the session files.
            Ok(bytes) => match serde_json::from_slice::<Index>(&bytes) {
                Ok(index) => (index.sessions, fs::metadata(&index_path).and_then(|m| m.modified()).ok()),
                Err(_) => (Vec::new(), None),
            },
            Err(e) if e.kind() == ErrorKind::NotFound => (Vec::new(), None),
            Err(e) => return Err(e.into()),
        };
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => Some(entries),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        let (mut on_disk, mut errors, mut changed) = (HashSet::new(), Vec::new(), false);
        for entry in entries.into_iter().flatten() {
            let entry = entry?;
            let path = entry.path();
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).filter(|id| valid_id(id)) else {
                continue;
            };
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            on_disk.insert(id.to_owned());
            let known = index.iter().position(|s| s.id == id);
            let modified = entry.metadata().and_then(|m| m.modified()).ok();
            let stale = match (known, index_time, modified) {
                (Some(_), Some(indexed), Some(modified)) => modified > indexed,
                _ => true,
            };
            if !stale {
                continue;
            }
            match self.load(id) {
                Ok(session) => {
                    let summary = session.summary();
                    match known {
                        Some(i) => index[i] = summary,
                        None => index.push(summary),
                    }
                    changed = true;
                }
                Err(e) => {
                    // Keep a stale entry out of the list rather than show it.
                    if let Some(i) = known {
                        index.remove(i);
                    }
                    errors.push(e);
                }
            }
        }
        let before = index.len();
        index.retain(|s| on_disk.contains(&s.id));
        changed |= index.len() != before;

        index.sort_by_key(|s| std::cmp::Reverse(s.updated));
        self.index = index;
        self.indexed = true;
        if changed && let Err(e) = self.write_index() {
            // Listing still worked; the next save retries the write.
            errors.push(e);
        }
        Ok((self.index.clone(), errors))
    }

    /// Reads one session with all its messages.
    ///
    /// # Errors
    /// An invalid id, a missing or unreadable file, or malformed JSON.
    pub fn load(&self, id: &str) -> Result<Session> {
        let mut session: Session = serde_json::from_slice(&fs::read(self.path(id)?)?)?;
        // The file name is authoritative; it is what `delete` removes.
        id.clone_into(&mut session.id);
        Ok(session)
    }

    /// Writes `session` atomically and updates the index.
    ///
    /// # Errors
    /// An invalid id or any I/O failure.
    pub fn save(&mut self, session: &Session) -> Result<()> {
        write_private(&self.path(&session.id)?, &serde_json::to_vec(session)?)?;
        self.ensure_indexed()?;
        let summary = session.summary();
        match self.index.iter_mut().find(|s| s.id == session.id) {
            Some(entry) => *entry = summary,
            None => self.index.push(summary),
        }
        self.index.sort_by_key(|s| std::cmp::Reverse(s.updated));
        self.write_index()
    }

    /// Case-insensitive search of every session's messages for `query`,
    /// newest sessions first, at most one hit per session. Reads every file,
    /// so run it off the UI thread.
    ///
    /// # Errors
    /// The directory exists but cannot be listed.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let needle = query.trim().to_lowercase();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut hits = Vec::new();
        if needle.is_empty() {
            return Ok(hits);
        }
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).filter(|id| valid_id(id)) else {
                continue;
            };
            // Unreadable files are skipped; `list` reports them.
            let Ok(session) = self.load(id) else { continue };
            let found = session
                .messages
                .iter()
                .find_map(|m| m.content.lines().find(|line| line.to_lowercase().contains(&needle)).map(|line| snippet(line, &needle)));
            if let Some(snippet) = found {
                hits.push(SearchHit { session: session.id, title: session.title, snippet, updated: session.updated });
            }
        }
        hits.sort_by_key(|h| std::cmp::Reverse(h.updated));
        hits.truncate(limit);
        Ok(hits)
    }

    /// Deletes a session's file and its index entry; deleting a missing
    /// session succeeds.
    ///
    /// # Errors
    /// An invalid id or any I/O failure other than "not found".
    pub fn delete(&mut self, id: &str) -> Result<()> {
        match fs::remove_file(self.path(id)?) {
            Err(e) if e.kind() != ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        self.ensure_indexed()?;
        self.index.retain(|s| s.id != id);
        self.write_index()
    }

    /// Loads the index before the first change, so writing it back never
    /// drops sessions this store has not listed yet.
    fn ensure_indexed(&mut self) -> Result<()> {
        if !self.indexed {
            self.list()?;
        }
        Ok(())
    }

    fn write_index(&self) -> Result<()> {
        let index = Index { version: 1, sessions: self.index.clone() };
        write_private(&self.dir.join(INDEX_FILE), &serde_json::to_vec(&index)?)
    }
}

/// About 90 characters of `line` around the first match of `needle`
/// (already lower-cased), with ellipses where cut.
fn snippet(line: &str, needle: &str) -> String {
    const CONTEXT: usize = 40;
    let chars: Vec<char> = line.trim().chars().collect();
    let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase()).collect();
    let needle: Vec<char> = needle.chars().collect();
    // Case folding can change lengths; fall back to the start if it did.
    let at = if lower.len() == chars.len() { lower.windows(needle.len()).position(|w| w == needle.as_slice()).unwrap_or(0) } else { 0 };
    let start = at.saturating_sub(CONTEXT);
    let end = (at + needle.len() + CONTEXT).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    if start > 0 {
        out.insert(0, '…');
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (PathBuf, SessionStore) {
        // `sessions/` inside a folder of its own, removed afterwards.
        let dir = std::env::temp_dir().join(format!("openrp-{name}-{}", std::process::id())).join("sessions");
        let _ = fs::remove_dir_all(dir.parent().unwrap());
        (dir.clone(), SessionStore::at(dir))
    }

    fn session(id: &str, updated: u64, cost: f64) -> Session {
        let mut reply = StoredMessage::new(Role::Assistant, "hello".into());
        reply.usage = Usage::new(10, 5);
        reply.cost = cost;
        Session {
            id: id.into(),
            title: format!("Title {id}"),
            updated,
            messages: vec![StoredMessage::new(Role::User, "hi".into()), reply],
            ..Session::default()
        }
    }

    #[test]
    fn save_list_load_delete() {
        let (dir, mut store) = temp_store("sessions");
        assert_eq!(store.list().unwrap().0.len(), 0);

        let (old, mut new) = (session("a1", 1, 0.5), session("b2", 2, 0.25));
        new.world = Some("w1".into());
        new.cast = vec![CastMember { id: "c1".into(), name: "Katniss".into(), present: true, ..CastMember::default() }];
        new.player = Some(Player { name: "Gale".into(), description: "A hunter.".into() });
        store.save(&old).unwrap();
        store.save(&new).unwrap();
        fs::write(dir.join("broken.json"), "{").unwrap();

        // A fresh store (a new app launch) lists from the index alone.
        let mut store = SessionStore::at(dir.clone());
        let (listed, errors) = store.list().unwrap();
        assert_eq!(listed, [new.summary(), old.summary()]);
        assert_eq!(errors.len(), 1, "the broken file is reported, not listed");
        assert_eq!(listed[0].tokens, 15);
        assert_eq!(store.load("b2").unwrap(), new);

        store.delete("a1").unwrap();
        store.delete("a1").unwrap();
        assert_eq!(SessionStore::at(dir.clone()).list().unwrap().0, [new.summary()]);
        assert!(store.save(&Session { id: "../evil".into(), ..Session::default() }).is_err());
        assert!(store.delete("..").is_err());
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn index_heals_from_the_files() {
        let (dir, mut store) = temp_store("index");
        let (kept, edited) = (session("k1", 1, 0.1), session("e2", 2, 0.2));
        store.save(&kept).unwrap();
        store.save(&edited).unwrap();

        // Simulate a crash between writes and outside edits: one file changed
        // without the index, one appeared, and one vanished.
        let mut changed = edited.clone();
        changed.title = "Renamed".into();
        fs::write(dir.join("e2.json"), serde_json::to_vec(&changed).unwrap()).unwrap();
        let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        fs::File::options().write(true).open(dir.join(INDEX_FILE)).unwrap().set_modified(old).unwrap();
        let added = session("n3", 3, 0.3);
        fs::write(dir.join("n3.json"), serde_json::to_vec(&added).unwrap()).unwrap();
        fs::remove_file(dir.join("k1.json")).unwrap();

        let (listed, errors) = SessionStore::at(dir.clone()).list().unwrap();
        assert!(errors.is_empty());
        assert_eq!(listed, [added.summary(), changed.summary()]);

        // A corrupt index is rebuilt rather than trusted.
        fs::write(dir.join(INDEX_FILE), "garbage").unwrap();
        assert_eq!(SessionStore::at(dir.clone()).list().unwrap().0, listed);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn search_finds_message_text() {
        let (dir, mut store) = temp_store("search");
        let mut s = session("s1", 5, 0.0);
        s.messages.insert(0, StoredMessage::new(Role::User, "first line\nThe Quick brown fox jumps".into()));
        store.save(&s).unwrap();
        store.save(&session("s2", 6, 0.0)).unwrap();

        let hits = store.search("quick BROWN", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session, "s1");
        assert_eq!(hits[0].snippet, "The Quick brown fox jumps");
        assert_eq!(store.search("  ", 10).unwrap().len(), 0);
        assert_eq!(snippet(&"x".repeat(100), "xx").chars().last(), Some('…'));
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn tool_results_and_older_casts_load() {
        let mut reply = StoredMessage::new(Role::Assistant, "The arena hums.".into());
        reply.tool_calls.push(ToolResult {
            call: crate::responses::ToolCall { call_id: "c1".into(), name: "speak".into(), arguments: "{}".into() },
            output: "Spoken.".into(),
        });
        let json = serde_json::to_string(&reply).unwrap();
        assert!(json.contains(r#""call_id":"c1""#) && json.contains(r#""output":"Spoken.""#));
        assert_eq!(serde_json::from_str::<StoredMessage>(&json).unwrap(), reply);

        // Casts saved before members were copies still load.
        let old: CastMember = serde_json::from_str(r#"{"character":"k","present":true}"#).unwrap();
        assert_eq!((old.id.as_str(), old.name.as_str(), old.present), ("k", "", true));
    }

    #[test]
    fn compact_json_for_plain_messages() {
        let json = serde_json::to_string(&StoredMessage::new(Role::User, "hi".into())).unwrap();
        assert_eq!(json, r#"{"role":"user","content":"hi"}"#);
        assert!(valid_id(&new_id()));
    }

    #[test]
    fn story_changes_undo_what_nobody_changed_since() {
        let member = |id: &str, present: bool| CastMember { id: id.into(), name: id.into(), present, ..CastMember::default() };
        let cast = vec![member("a", true), member("b", false)];
        // The reply moved b in, added c and set the scene.
        let after = vec![member("a", true), member("b", true), member("c", true)];
        let change = StoryChange::between(&cast, "", &after, "Dusk.", &["b owes a.".to_owned()]).unwrap();
        assert_eq!(change.cast.len(), 2);
        assert!(StoryChange::between(&cast, "x", &cast, "x", &[]).is_none());
        assert!(StoryChange::between(&cast, "x", &cast, "x", &["m".to_owned()]).is_some(), "remembering is a change");

        let (mut undone, mut scene) = (after.clone(), "Dusk.".to_owned());
        let mut memories = vec!["Old.".to_owned(), "b owes a.".to_owned()];
        change.undo(&mut undone, &mut scene, &mut memories);
        assert_eq!((undone, scene.as_str(), memories), (cast.clone(), "", vec!["Old.".to_owned()]));

        // The user moved c out, set the scene and deleted the memory since: those stay.
        let (mut edited, mut scene) = (vec![member("a", true), member("b", true), member("c", false)], "Night.".to_owned());
        let mut memories = vec!["Old.".to_owned()];
        change.undo(&mut edited, &mut scene, &mut memories);
        assert_eq!((edited, scene.as_str()), (vec![member("a", true), member("b", false), member("c", false)], "Night."));
        assert_eq!(memories, ["Old."]);

        let mut message = StoredMessage::new(Role::Assistant, String::new());
        message.change = Some(change);
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(serde_json::from_str::<StoredMessage>(&json).unwrap(), message);
    }
}
