//! The worlds, characters and personas pages: an index of everything saved,
//! and a form to create, edit or duplicate one, with its portrait. A persona
//! is someone the user plays: a story asking who they are offers them, and
//! fills in the starred one by itself (see `chat/dialog.rs`). Worlds are
//! played from here: Play starts a story set in that world, and deleting a
//! world deletes its stories; a world's form lists them, to open one. A
//! character's Play asks which world to meet them in.
//!
//! The index narrows as you type (names, tags and descriptions), by the tag
//! chips under the filter, and to favourites, which are listed first; the
//! star on a card sets one. Characters come in from character cards
//! (Import: PNG or JSON, several at once) and go out as one (Export card in
//! the form's ⋯ menu, beside Duplicate and Delete).
//!
//! The form keeps what is rarely touched in sections that open on click: a
//! character's greetings and example dialogue, and the lore of both, which
//! a lorebook can be imported into.
//!
//! The app reads the records from `~/.openrp/worlds/`, `characters/` and
//! `personas/` when the chat screen opens and saves them through its writer
//! thread; this page keeps the copy everything else reads.
//!
//! Portraits are never deleted from here: stories copy them from the
//! library, so a sweep at startup removes the ones nothing uses any more
//! (see `serechat::Portraits::collect_garbage`).
//!
//! Nothing typed is lost by looking elsewhere: a form with unsaved changes
//! is kept as a draft (one per page) and reopens with the page. Going back
//! or pressing Esc discards it, after a second press. Deleting asks twice
//! too. A record saved by a newer version of the app is shown but cannot
//! be saved over.

use std::collections::HashMap;
use std::path::PathBuf;

use arboard::Clipboard;
use serechat::{Character, LoreEntry, Persona, Portraits, World, new_id, unix_now};
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use crate::app::Action;
use crate::editor::Editor;
use crate::form::{FIELD_PAD, Fields};
use crate::image::Lookup;
use crate::paint::{Painter, Rect, fade, mix};
use crate::text::{Align, Style};
use crate::theme;
use crate::ui::{ButtonStyle, Ui, button, chevron, id};

/// Widest the page's column gets.
const CONTENT_WIDTH: f32 = 680.0;
/// Height of a record's card in the index.
const CARD_H: f32 = 80.0;
/// Side of a card's portrait.
const CARD_PORTRAIT: f32 = 56.0;
/// Side of the form's portrait.
const FORM_PORTRAIT: f32 = 132.0;
/// Height of a story's row on its world's page.
const STORY_H: f32 = 40.0;
/// Height of a section's header row in the form.
const SECTION_H: f32 = 44.0;
/// Side of the button removing a greeting or a lore entry.
const REMOVE: f32 = 28.0;
/// Smallest height of a greeting's field.
const GREETING_MIN_H: f32 = 88.0;
/// Smallest height of the example dialogue's field.
const EXAMPLES_MIN_H: f32 = 140.0;
/// Smallest height of a lore entry's text field.
const LORE_MIN_H: f32 = 68.0;
/// Height of a tag chip above the index.
const CHIP_H: f32 = 26.0;
/// Most tags offered as chips; chosen ones always show.
const TAG_CHIPS: usize = 12;
/// Longest name, in chars.
pub const NAME_LIMIT: usize = 80;
/// Longest list of tags or keys, in chars.
const LIST_LIMIT: usize = 1000;
/// Smallest height of a description field.
pub const DESCRIPTION_MIN_H: f32 = 220.0;
/// Longest comment, in chars.
const COMMENT_LIMIT: usize = 300;
/// The form's fields: name, tags, comment, description and example
/// dialogue, then each greeting, then each lore entry's keys and text.
const NAME: usize = 0;
const TAGS: usize = 1;
const COMMENT: usize = 2;
const DESCRIPTION: usize = 3;
const EXAMPLES: usize = 4;
/// How many fields come before the greetings.
const FIXED: usize = 5;

/// Which of the three pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Settings stories take place in.
    World,
    /// People the model plays.
    Character,
    /// People the user plays.
    Persona,
}

impl Kind {
    /// Every page, in the sidebar's order.
    pub const ALL: [Self; 3] = [Self::World, Self::Character, Self::Persona];

    /// The page's name.
    #[must_use]
    pub fn plural(self) -> &'static str {
        match self {
            Self::World => "Worlds",
            Self::Character => "Characters",
            Self::Persona => "Personas",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Character => "character",
            Self::Persona => "persona",
        }
    }

    /// What the page holds, in a sentence.
    fn blurb(self) -> &'static str {
        match self {
            Self::World => "Settings for your stories, like Panem or a galaxy far, far away. Click one to play it.",
            Self::Character => "People the AI plays, ready to cast in any story. A story keeps its own copy.",
            Self::Persona => "Who you play, ready for any story that asks. The starred one fills in by itself.",
        }
    }

    /// Placeholder of the description field.
    fn hint(self) -> &'static str {
        match self {
            Self::World => "The premise, places, factions, rules and tone of this world…",
            Self::Character => "Who they are: background, personality, appearance and how they speak…",
            Self::Persona => "Who you are: background, appearance, how you speak, what you want…",
        }
    }

    /// The form's sections, in order.
    fn sections(self) -> &'static [Section] {
        match self {
            Self::World => &[Section::Lore],
            Self::Character => &[Section::Greetings, Section::Examples, Section::Lore],
            Self::Persona => &[],
        }
    }
}

/// A part of the form that opens on click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Greetings,
    Examples,
    Lore,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Self::Greetings => "Greetings",
            Self::Examples => "Example dialogue",
            Self::Lore => "Lore",
        }
    }
}

/// What the pages ask the chat screen to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Start a story in this world.
    Play(String),
    /// Choose a world to play the character with this id in; the menu
    /// opens below the rectangle.
    PlayCharacter(String, Rect),
    /// Open the form's ⋯ menu below the rectangle (see [`LibraryView::form_menu`]).
    FormMenu(Rect),
    /// Open the story (conversation) with this id.
    Open(u64),
    /// Ask the generator for a character matching this idea (empty: any).
    Generate(String),
}

/// A row of the form's ⋯ menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormAction {
    /// Save, then open an unsaved copy.
    Duplicate,
    /// Save, then write a character card.
    Export,
    /// Delete the record (a world, with its stories).
    Delete,
}

/// A story played in the world being edited, for its page to list.
#[derive(Clone, Debug)]
pub struct Story {
    /// The conversation's id.
    pub id: u64,
    /// Its title; empty for none yet.
    pub title: String,
    /// Last change, seconds since the Unix epoch.
    pub updated: u64,
}

/// A world or character as the app shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// Identifier, also its file name.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What it is like, for the model.
    pub description: String,
    /// Portrait file name; empty for none.
    pub portrait: String,
    /// A character's opening messages, the one used first.
    pub greetings: Vec<String>,
    /// A character's example dialogue.
    pub examples: String,
    /// Labels to find it by.
    pub tags: Vec<String>,
    /// The user's own note to tell it apart; never sent to the model.
    pub comment: String,
    /// Starred: listed first.
    pub favorite: bool,
    /// Background the model reads when the story mentions it.
    pub lore: Vec<LoreEntry>,
    created: u64,
    updated: u64,
    /// Saved by a newer version of the app: shown, but never saved over.
    locked: bool,
}

impl From<World> for Record {
    fn from(w: World) -> Self {
        let locked = w.is_newer();
        let World { id, name, description, portrait, comment, tags, favorite, lore, created, updated, .. } = w;
        Self { id, name, description, portrait, tags, comment, favorite, lore, created, updated, locked, ..Self::default() }
    }
}

impl From<Character> for Record {
    fn from(c: Character) -> Self {
        let locked = c.is_newer();
        let Character { id, name, description, portrait, comment, greetings, examples, tags, favorite, lore, created, updated, .. } = c;
        Self { id, name, description, portrait, greetings, examples, tags, comment, favorite, lore, created, updated, locked }
    }
}

impl From<Persona> for Record {
    fn from(p: Persona) -> Self {
        let locked = p.is_newer();
        let Persona { id, name, description, portrait, comment, tags, favorite, created, updated, .. } = p;
        Self { id, name, description, portrait, tags, comment, favorite, created, updated, locked, ..Self::default() }
    }
}

impl Record {
    /// This record as a library character.
    #[must_use]
    pub fn character(&self) -> Character {
        let r = self.clone();
        Character {
            version: Character::VERSION,
            id: r.id,
            name: r.name,
            description: r.description,
            portrait: r.portrait,
            comment: r.comment,
            greetings: r.greetings,
            examples: r.examples,
            tags: r.tags,
            favorite: r.favorite,
            lore: r.lore,
            created: r.created,
            updated: r.updated,
        }
    }

    /// The action that saves this record as a `kind`.
    fn save_action(&self, kind: Kind) -> Action {
        match kind {
            Kind::World => {
                let r = self.clone();
                Action::SaveWorld(World {
                    version: World::VERSION,
                    id: r.id,
                    name: r.name,
                    description: r.description,
                    portrait: r.portrait,
                    comment: r.comment,
                    tags: r.tags,
                    favorite: r.favorite,
                    lore: r.lore,
                    created: r.created,
                    updated: r.updated,
                })
            }
            Kind::Character => Action::SaveCharacter(self.character()),
            Kind::Persona => {
                let r = self.clone();
                Action::SavePersona(Persona {
                    version: Persona::VERSION,
                    id: r.id,
                    name: r.name,
                    description: r.description,
                    portrait: r.portrait,
                    comment: r.comment,
                    tags: r.tags,
                    favorite: r.favorite,
                    created: r.created,
                    updated: r.updated,
                })
            }
        }
    }
}

/// A name field limited to one line of [`NAME_LIMIT`] chars.
#[must_use]
pub fn name_editor(text: &str) -> Editor {
    let mut editor = Editor::restricted(NAME_LIMIT, |c| !c.is_control());
    editor.insert(text);
    editor
}

/// A multi-line field holding `text`.
#[must_use]
pub fn text_editor(text: &str) -> Editor {
    let mut editor = Editor::default();
    editor.insert(text);
    editor
}

/// A one-line note that wraps.
fn comment_editor(text: &str) -> Editor {
    let mut editor = Editor::restricted(COMMENT_LIMIT, |c| !c.is_control());
    editor.insert(text);
    editor
}

/// A comma-separated list (tags, keys): one line that wraps.
fn list_editor(items: &[String]) -> Editor {
    let mut editor = Editor::restricted(LIST_LIMIT, |c| !c.is_control());
    editor.insert(&items.join(", "));
    editor
}

/// The items of a comma-separated list: trimmed, without empty ones or
/// repeats (in any case).
fn parse_list(text: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for item in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !items.iter().any(|i| i.to_lowercase() == item.to_lowercase()) {
            items.push(item.to_owned());
        }
    }
    items
}

/// Creating or editing one record.
struct Form {
    kind: Kind,
    id: String,
    created: u64,
    /// Not saved yet, so nothing to delete or play.
    new: bool,
    /// See [`NAME`] for the order.
    fields: Fields,
    /// How many greetings follow the fixed fields.
    greetings: usize,
    /// Each lore entry's constant flag; its fields follow the greetings.
    constant: Vec<bool>,
    /// Which sections are open.
    open: Vec<Section>,
    favorite: bool,
    /// Portrait file name; empty for none.
    portrait: String,
    /// The image picker is open.
    picking: bool,
    /// Why the last portrait could not be used.
    error: Option<String>,
    /// Back or Esc was pressed once with unsaved changes, and discards
    /// them when pressed again.
    confirm_discard: bool,
    /// What it holds as last saved, to tell edits apart.
    original: Record,
    /// Saved by a newer version of the app: it cannot be saved here.
    locked: bool,
    /// The generator request whose answer fills the form.
    generating: Option<u64>,
    /// Why the last generation failed.
    generate_error: Option<String>,
    /// The lorebook picker is open.
    importing: bool,
    /// Why the last lorebook could not be read.
    import_error: Option<String>,
}

impl Form {
    /// A form for `record`, or for a new one when `None`.
    fn new(kind: Kind, record: Option<Record>) -> Self {
        let new = record.is_none();
        let record = record.unwrap_or_else(|| Record { id: new_id(), created: unix_now(), ..Record::default() });
        let mut fields = Fields::new(
            [
                name_editor(&record.name),
                list_editor(&record.tags),
                comment_editor(&record.comment),
                text_editor(&record.description),
                text_editor(&record.examples),
            ],
            [false, true, true, true, true],
        );
        for greeting in &record.greetings {
            fields.insert_field(fields.count(), text_editor(greeting), true);
        }
        for entry in &record.lore {
            fields.insert_field(fields.count(), list_editor(&entry.keys), true);
            fields.insert_field(fields.count(), text_editor(&entry.content), true);
        }
        fields.focus(NAME);
        let mut form = Self {
            kind,
            id: record.id,
            created: record.created,
            new,
            fields,
            greetings: record.greetings.len(),
            constant: record.lore.iter().map(|e| e.constant).collect(),
            open: Vec::new(),
            favorite: record.favorite,
            portrait: record.portrait,
            picking: false,
            error: None,
            confirm_discard: false,
            original: Record::default(),
            locked: record.locked,
            generating: None,
            generate_error: None,
            importing: false,
            import_error: None,
        };
        form.original = form.snapshot();
        form
    }

    /// The field of greeting `k`.
    fn greeting(k: usize) -> usize {
        FIXED + k
    }

    /// The keys field of lore entry `k`; its text is the next one.
    fn lore_keys(&self, k: usize) -> usize {
        FIXED + self.greetings + 2 * k
    }

    /// What the form holds now, as a record last changed at `0`.
    fn snapshot(&self) -> Record {
        let text = |index: usize| self.fields.text(index).trim().to_owned();
        let lore = (0..self.constant.len())
            .map(|k| LoreEntry { keys: parse_list(self.fields.text(self.lore_keys(k))), content: text(self.lore_keys(k) + 1), constant: self.constant[k] })
            .filter(|e| !e.content.is_empty())
            .collect();
        Record {
            id: self.id.clone(),
            name: text(NAME),
            description: text(DESCRIPTION),
            portrait: self.portrait.clone(),
            greetings: (0..self.greetings).map(|k| text(Self::greeting(k))).filter(|g| !g.is_empty()).collect(),
            examples: if self.kind == Kind::Character { text(EXAMPLES) } else { String::new() },
            tags: parse_list(self.fields.text(TAGS)),
            comment: text(COMMENT),
            favorite: self.favorite,
            lore,
            created: self.created,
            updated: 0,
            locked: self.locked,
        }
    }

    /// Whether it holds changes that were not saved.
    fn dirty(&self) -> bool {
        self.snapshot() != self.original
    }

    /// What the generator is asked for: the name and description typed so
    /// far; empty for anyone.
    fn idea(&self) -> String {
        let (name, description) = (self.fields.text(NAME).trim(), self.fields.text(DESCRIPTION).trim());
        match (name.is_empty(), description.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!("Their name: {name}"),
            (true, false) => description.to_owned(),
            (false, false) => format!("Their name: {name}\n\n{description}"),
        }
    }

    /// The record the form holds, or `None` while it has no name.
    fn record(&self) -> Option<Record> {
        let record = self.snapshot();
        (!record.name.is_empty()).then(|| Record { updated: unix_now(), ..record })
    }

    /// Adds an empty greeting, focused, and shows it.
    fn add_greeting(&mut self) {
        self.fields.insert_field(Self::greeting(self.greetings), text_editor(""), true);
        self.greetings += 1;
        self.show(Section::Greetings);
    }

    /// Adds `entries` to the lore and shows it, the first of them focused.
    fn add_lore(&mut self, entries: &[LoreEntry]) {
        let first = self.lore_keys(self.constant.len());
        for entry in entries {
            let at = self.lore_keys(self.constant.len());
            self.fields.insert_field(at, list_editor(&entry.keys), true);
            self.fields.insert_field(at + 1, text_editor(&entry.content), true);
            self.constant.push(entry.constant);
        }
        self.fields.focus(first);
        self.show(Section::Lore);
    }

    /// Removes greeting `k`.
    fn remove_greeting(&mut self, k: usize) {
        if k < self.greetings {
            self.fields.remove(Self::greeting(k));
            self.greetings -= 1;
        }
    }

    /// Removes lore entry `k`.
    fn remove_lore(&mut self, k: usize) {
        if k < self.constant.len() {
            let keys = self.lore_keys(k);
            self.fields.remove(keys);
            self.fields.remove(keys);
            self.constant.remove(k);
        }
    }

    fn show(&mut self, section: Section) {
        if !self.open.contains(&section) {
            self.open.push(section);
        }
    }

    /// Whether field `index` is on screen: its section, if any, is open.
    fn shown(&self, index: usize) -> bool {
        let section = match index {
            NAME | TAGS | COMMENT | DESCRIPTION => return true,
            EXAMPLES => Section::Examples,
            i if i < FIXED + self.greetings => Section::Greetings,
            _ => Section::Lore,
        };
        self.kind.sections().contains(&section) && self.open.contains(&section)
    }

    /// Moves the focus to the next field on screen (the one before, when
    /// `back`).
    fn tab(&mut self, back: bool) {
        let n = self.fields.count();
        let step = if back { n - 1 } else { 1 };
        let mut next = self.fields.focused();
        for _ in 0..n {
            next = (next + step) % n;
            if self.shown(next) {
                break;
            }
        }
        self.fields.focus(next);
    }
}

/// What the index's rows were worked out for: the page, the filter text,
/// the tags and favourites chosen, and the records' revision.
type FilterKey = (Kind, String, Vec<String>, bool, u64);

/// State of the worlds and characters pages, and the records everything
/// else looks up.
#[derive(Default)]
pub struct LibraryView {
    worlds: Vec<Record>,
    characters: Vec<Record>,
    personas: Vec<Record>,
    /// Where portraits are; `None` until loaded or without a home directory.
    portraits: Option<Portraits>,
    /// The records have been read from disk.
    loaded: bool,
    /// The record being created or edited; the index shows otherwise.
    form: Option<Form>,
    /// Forms with unsaved changes left for another page, at most one per
    /// kind; each reopens with its page.
    drafts: Vec<Form>,
    scroll: f32,
    /// Content height measured last frame, for clamping the scroll.
    content_h: f32,
    /// The index's filter field, made when first shown.
    filter: Option<Fields>,
    /// Tags the index is narrowed to, lower-cased: a record needs them all.
    tags: Vec<String>,
    /// The index shows favourites only.
    favorites: bool,
    /// The page shown last: another starts unfiltered.
    shown: Option<Kind>,
    /// Counts changes to the records, so the index knows to filter again.
    revision: u64,
    /// The index's rows (favourites first) and tag chips, and what they
    /// were worked out for.
    cache: Option<(FilterKey, Vec<usize>, Vec<String>)>,
    /// The card picker is open.
    importing: bool,
}

impl LibraryView {
    /// Shows the page of `kind`: the draft left there, if there is one,
    /// or the index. A form with unsaved changes is kept as a draft.
    pub fn show(&mut self, kind: Kind) {
        self.stash();
        self.form = self.drafts.iter().position(|d| d.kind == kind).map(|i| self.drafts.remove(i));
        self.scroll = 0.0;
        if self.shown != Some(kind) {
            self.clear_filter();
            self.shown = Some(kind);
        }
    }

    /// Shows every record again.
    fn clear_filter(&mut self) {
        if let Some(filter) = &mut self.filter {
            filter.replace(0, "");
        }
        self.tags.clear();
        self.favorites = false;
    }

    /// Whether the index is narrowed at all.
    fn filtered(&self) -> bool {
        self.favorites || !self.tags.is_empty() || self.filter.as_ref().is_some_and(|f| !f.text(0).trim().is_empty())
    }

    /// Keeps the open form as a draft if it has unsaved changes (replacing
    /// an older draft of its kind), and closes it.
    fn stash(&mut self) {
        if let Some(form) = self.form.take().filter(Form::dirty) {
            self.drafts.retain(|d| d.kind != form.kind);
            self.drafts.push(form);
        }
    }

    /// Stores the records read from disk, most recently edited first.
    pub fn loaded(&mut self, worlds: Vec<World>, characters: Vec<Character>, personas: Vec<Persona>, portraits: Portraits) {
        let sorted = |mut records: Vec<Record>| {
            records.sort_by_key(|r| std::cmp::Reverse(r.updated));
            records
        };
        self.worlds = sorted(worlds.into_iter().map(Record::from).collect());
        self.characters = sorted(characters.into_iter().map(Record::from).collect());
        self.personas = sorted(personas.into_iter().map(Record::from).collect());
        self.portraits = Some(portraits);
        self.loaded = true;
        self.revision += 1;
    }

    /// Every record of `kind`, most recently edited first.
    #[must_use]
    pub fn list(&self, kind: Kind) -> &[Record] {
        match kind {
            Kind::World => &self.worlds,
            Kind::Character => &self.characters,
            Kind::Persona => &self.personas,
        }
    }

    /// The persona a new story starts with: the starred one, if any.
    #[must_use]
    pub fn default_persona(&self) -> Option<&Record> {
        self.personas.iter().find(|p| p.favorite)
    }

    /// The persona called `name` (in any case), if there is one.
    #[must_use]
    pub fn persona_named(&self, name: &str) -> Option<&Record> {
        let name = name.trim().to_lowercase();
        self.personas.iter().find(|p| p.name.trim().to_lowercase() == name)
    }

    /// Saves who the user plays as a persona: updates the one of that name,
    /// or adds one. One saved by a newer version is left alone.
    pub fn save_persona(&mut self, name: &str, description: &str, portrait: &str, actions: &mut Vec<Action>) {
        let existing = self.persona_named(name).cloned();
        if existing.as_ref().is_some_and(|r| r.locked) || name.trim().is_empty() {
            return;
        }
        let base = existing.unwrap_or_else(|| Record { id: new_id(), created: unix_now(), ..Record::default() });
        let record = Record {
            name: name.trim().chars().take(NAME_LIMIT).collect(),
            description: description.trim().to_owned(),
            portrait: portrait.to_owned(),
            updated: unix_now(),
            ..base
        };
        self.put(Kind::Persona, record, actions);
    }

    /// The record of `kind` with this id, if it exists.
    #[must_use]
    pub fn get(&self, kind: Kind, id: &str) -> Option<&Record> {
        self.list(kind).iter().find(|r| r.id == id)
    }

    fn list_mut(&mut self, kind: Kind) -> &mut Vec<Record> {
        // Whoever asks may change them: the index filters again.
        self.revision += 1;
        match kind {
            Kind::World => &mut self.worlds,
            Kind::Character => &mut self.characters,
            Kind::Persona => &mut self.personas,
        }
    }

    /// The file a portrait name points at, if it is a valid one.
    #[must_use]
    pub fn portrait_path(&self, portrait: &str) -> Option<String> {
        self.portraits.as_ref()?.path(portrait).map(|p| p.to_string_lossy().into_owned())
    }

    /// Text committed by an input method: to the open form, or the filter.
    ///
    /// ponytail: forms show no preedit (text still being composed), only
    /// what is committed; port the composer's preedit drawing if CJK users
    /// miss it.
    pub fn insert(&mut self, text: &str) {
        if let Some(form) = &mut self.form {
            form.fields.insert(text);
        } else if let Some(filter) = &mut self.filter {
            filter.insert(text);
        }
    }

    /// The open form waits for generator request `request`.
    pub fn generating(&mut self, request: u64) {
        if let Some(form) = &mut self.form {
            (form.generating, form.generate_error) = (Some(request), None);
        }
    }

    /// The generator answered `request` with a name and description, which
    /// replace the form's, or why it could not. Answers for a form closed
    /// since are dropped.
    pub fn generated(&mut self, request: u64, made: Result<(String, String), String>) {
        let Some(form) = self.form.as_mut().filter(|f| f.generating == Some(request)) else { return };
        form.generating = None;
        match made {
            Ok((name, description)) => {
                form.fields.replace(NAME, &name);
                form.fields.replace(DESCRIPTION, &description);
            }
            Err(e) => form.generate_error = Some(e),
        }
    }

    /// Where the input method's candidate window should appear.
    #[must_use]
    pub fn caret(&self) -> Option<Rect> {
        match &self.form {
            Some(form) => form.fields.caret(),
            None => self.filter.as_ref().and_then(Fields::caret),
        }
    }

    /// The image picker closed: `Ok(Some(name))` is a portrait copied in.
    pub fn portrait_picked(&mut self, result: Result<Option<String>, String>) {
        // A form closed while the picker was open leaves an unused copy,
        // which the startup sweep removes.
        let Some(form) = &mut self.form else { return };
        form.picking = false;
        match result {
            Ok(Some(name)) => {
                form.error = None;
                form.portrait = name;
            }
            Ok(None) => {}
            Err(e) => form.error = Some(e),
        }
    }

    /// Character cards were read (none when the picker was cancelled): each
    /// becomes a saved character, listed first. One alone opens, to review.
    pub fn imported(&mut self, characters: Vec<Character>, actions: &mut Vec<Action>) {
        self.importing = false;
        let now = unix_now();
        let count = characters.len();
        let mut last = None;
        for character in characters {
            let mut record = Record::from(Character { id: new_id(), created: now, updated: now, version: Character::VERSION, ..character });
            record.name = record.name.chars().take(NAME_LIMIT).collect();
            last = Some(record.clone());
            self.put(Kind::Character, record, actions);
        }
        if let Some(record) = last.filter(|_| count == 1) {
            self.stash();
            self.form = Some(Form::new(Kind::Character, Some(record)));
            self.scroll = 0.0;
        }
    }

    /// The lorebook picker closed: its entries join the open form's lore
    /// (unsaved until Save), or why they could not be read.
    pub fn lore_imported(&mut self, result: Result<Vec<LoreEntry>, String>) {
        // The form may have been left as a draft meanwhile.
        let Some(form) = self.form.iter_mut().chain(&mut self.drafts).find(|f| f.importing) else { return };
        form.importing = false;
        match result {
            // Cancelled.
            Ok(entries) if entries.is_empty() => {}
            Ok(entries) => {
                form.import_error = None;
                form.add_lore(&entries);
            }
            Err(e) => form.import_error = Some(e),
        }
    }

    /// Keyboard input. Returns `false` for keys the page does not use, so
    /// the caller may handle them.
    pub fn key(&mut self, event: &KeyEvent, mods: ModifiersState, cb: &mut Option<Clipboard>, actions: &mut Vec<Action>) -> bool {
        let Some(form) = &mut self.form else {
            // The index: typing filters it, and Esc clears the filter first.
            if event.logical_key == Key::Named(NamedKey::Escape) {
                let filtered = self.filtered();
                self.clear_filter();
                return filtered;
            }
            return self.filter.as_mut().is_some_and(|f| f.key(event, mods, cb));
        };
        let primary = if cfg!(target_os = "macos") { mods.super_key() } else { mods.control_key() };
        match &event.logical_key {
            // Unsaved changes go only on a second Esc.
            Key::Named(NamedKey::Escape) if form.dirty() && !form.confirm_discard => form.confirm_discard = true,
            Key::Named(NamedKey::Escape) => self.form = None,
            Key::Character(c) if primary && c.eq_ignore_ascii_case("s") => self.save(actions),
            // Fields in closed sections are skipped.
            Key::Named(NamedKey::Tab) => form.tab(mods.shift_key()),
            _ => {
                form.fields.key(event, mods, cb);
            }
        }
        true
    }

    /// Saves the open form and returns to the index; does nothing while
    /// the form has no name, or holds a record from a newer version.
    fn save(&mut self, actions: &mut Vec<Action>) {
        let Some(form) = self.form.take_if(|f| f.record().is_some() && !f.locked) else {
            return;
        };
        let Some(record) = form.record() else { return };
        self.put(form.kind, record, actions);
    }

    /// Saves the open form's edits but keeps it open, and returns what it
    /// holds: `None` while it has no name. A record from a newer version
    /// is returned as shown, unsaved.
    fn commit(&mut self, actions: &mut Vec<Action>) -> Option<Record> {
        let form = self.form.as_ref()?;
        let (kind, record) = (form.kind, form.record()?);
        if !form.locked && (form.new || form.dirty()) {
            self.put(kind, record.clone(), actions);
            if let Some(form) = &mut self.form {
                form.new = false;
                form.original = form.snapshot();
            }
        }
        Some(record)
    }

    /// Saves `record`, replacing any with its id, as the most recent.
    fn put(&mut self, kind: Kind, record: Record, actions: &mut Vec<Action>) {
        actions.push(record.save_action(kind));
        let records = self.list_mut(kind);
        records.retain(|r| r.id != record.id);
        records.insert(0, record);
    }

    /// Stars or unstars record `id`, keeping its place and last edit.
    fn toggle_favorite(&mut self, kind: Kind, id: &str, actions: &mut Vec<Action>) {
        let Some(record) = self.list_mut(kind).iter_mut().find(|r| r.id == id && !r.locked) else { return };
        record.favorite = !record.favorite;
        actions.push(record.save_action(kind));
    }

    /// Stores a story's cast member as library character `id`: updates the
    /// one it was cast from (keeping its greetings, tags and star), or adds
    /// a new one.
    pub fn store_character(&mut self, member: &serechat::CastMember, actions: &mut Vec<Action>) {
        let existing = self.get(Kind::Character, &member.id).cloned();
        // One saved by a newer version is never saved over.
        if existing.as_ref().is_some_and(|r| r.locked) {
            return;
        }
        let base = existing.unwrap_or_else(|| Record { created: unix_now(), ..Record::default() });
        let record = Record {
            id: member.id.clone(),
            name: member.name.clone(),
            description: member.description.clone(),
            portrait: member.portrait.clone(),
            examples: member.examples.clone(),
            lore: member.lore.clone(),
            updated: unix_now(),
            locked: false,
            ..base
        };
        self.put(Kind::Character, record, actions);
    }

    /// Opens the form of record `id`, as clicking its card does: its draft,
    /// if one was left. A form open with unsaved changes becomes a draft.
    pub fn open_form(&mut self, kind: Kind, id: &str) {
        if self.form.as_ref().is_some_and(|f| f.kind == kind && f.id == id) {
            return;
        }
        self.stash();
        self.form = match self.drafts.iter().position(|d| d.kind == kind && d.id == id) {
            Some(index) => Some(self.drafts.remove(index)),
            None => self.get(kind, id).cloned().map(|r| Form::new(kind, Some(r))),
        };
    }

    /// Opens an empty form for a new record of `kind`.
    pub fn new_form(&mut self, kind: Kind) {
        self.stash();
        self.form = Some(Form::new(kind, None));
        self.scroll = 0.0;
    }

    /// The saved world whose form is open, whose stories it lists.
    #[must_use]
    pub fn open_world(&self) -> Option<&str> {
        self.form.as_ref().filter(|f| f.kind == Kind::World && !f.new).map(|f| f.id.as_str())
    }

    /// The rows of the open form's ⋯ menu; the last deletes.
    #[must_use]
    pub fn form_menu(&self) -> Vec<FormAction> {
        match &self.form {
            Some(form) if form.kind == Kind::Character => vec![FormAction::Duplicate, FormAction::Export, FormAction::Delete],
            Some(_) => vec![FormAction::Duplicate, FormAction::Delete],
            None => Vec::new(),
        }
    }

    /// Does what row `action` of the form's menu says. Returns the id of a
    /// saved world that was deleted, whose stories must go too.
    pub fn form_menu_picked(&mut self, action: FormAction, actions: &mut Vec<Action>) -> Option<String> {
        match action {
            FormAction::Duplicate => self.duplicate(actions),
            FormAction::Export => {
                if let Some(record) = self.commit(actions) {
                    let portrait = self.portrait_path(&record.portrait).map(PathBuf::from);
                    actions.push(Action::ExportCard { character: record.character(), portrait });
                }
            }
            FormAction::Delete => return self.delete(actions),
        }
        None
    }

    /// Saves the open form, like Play, then opens a new, unsaved copy of it.
    fn duplicate(&mut self, actions: &mut Vec<Action>) {
        let Some(form) = &self.form else { return };
        let (kind, Some(record)) = (form.kind, form.record()) else { return };
        if !form.locked {
            self.put(kind, record.clone(), actions);
        }
        let copy = Record { id: new_id(), name: format!("{} (copy)", record.name), created: unix_now(), locked: false, ..record };
        let mut form = Form::new(kind, Some(copy));
        form.new = true;
        self.form = Some(form);
        self.scroll = 0.0;
    }

    /// Deletes the open form's record and returns to the index. Returns the
    /// id of a saved world that went, whose stories must go too.
    fn delete(&mut self, actions: &mut Vec<Action>) -> Option<String> {
        let form = self.form.take()?;
        self.list_mut(form.kind).retain(|r| r.id != form.id);
        if form.new {
            return None;
        }
        actions.push(Action::DeleteRecord(form.kind, form.id.clone()));
        (form.kind == Kind::World).then_some(form.id)
    }

    /// Draws the page for `kind` into `area` (the whole main area).
    /// `stories` are those of the world being edited (see [`Self::open_world`]).
    pub fn draw(&mut self, p: &mut Painter, ui: &mut Ui, area: Rect, kind: Kind, stories: &[Story], actions: &mut Vec<Action>) -> Option<Event> {
        if self.form.is_some() { self.draw_form(p, ui, area, stories, actions) } else { self.draw_index(p, ui, area, kind, actions) }
    }

    /// Scrolls the body below the header and returns it with the content
    /// column's x and width.
    fn body(&mut self, ui: &mut Ui, area: Rect) -> (Rect, f32, f32) {
        let body = Rect::new(area.x, area.y + theme::HEADER_HEIGHT, area.w, area.h - theme::HEADER_HEIGHT);
        if ui.hovered(body) {
            self.scroll += ui.scroll;
        }
        self.scroll = self.scroll.clamp(0.0, (self.content_h - body.h).max(0.0));
        let width = CONTENT_WIDTH.min(body.w - 64.0);
        (body, body.x + ((body.w - width) * 0.5).round(), width)
    }

    /// The index of `kind` as filtered now: its rows (indices into its
    /// list, favourites first) and the tags offered as chips (most used
    /// first, then any chosen). Worked out again only when something changed.
    fn rows(&mut self, kind: Kind) -> (Vec<usize>, Vec<String>) {
        let query = self.filter.as_ref().map(|f| f.text(0).trim().to_lowercase()).unwrap_or_default();
        let key: FilterKey = (kind, query, self.tags.clone(), self.favorites, self.revision);
        if let Some((cached, rows, chips)) = &self.cache
            && *cached == key
        {
            return (rows.clone(), chips.clone());
        }
        let records = self.list(kind);
        let words: Vec<&str> = key.1.split_whitespace().collect();
        let lower_tags = |r: &Record| r.tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>();
        let mut rows: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                let tags = lower_tags(r);
                let text = || format!("{}\n{}\n{}\n{}", r.name, r.tags.join(" "), r.comment, r.description).to_lowercase();
                (!self.favorites || r.favorite)
                    && self.tags.iter().all(|t| tags.contains(t))
                    && (words.is_empty() || words.iter().all(|w| text().contains(w)))
            })
            .map(|(i, _)| i)
            .collect();
        rows.sort_by_key(|&i| !records[i].favorite);
        let mut counts: HashMap<String, (String, usize)> = HashMap::new();
        for tag in records.iter().flat_map(|r| &r.tags) {
            counts.entry(tag.to_lowercase()).or_insert_with(|| (tag.clone(), 0)).1 += 1;
        }
        let mut ranked: Vec<(String, (String, usize))> = counts.into_iter().collect();
        ranked.sort_by(|(a, (_, n)), (b, (_, m))| m.cmp(n).then_with(|| a.cmp(b)));
        let mut chips: Vec<String> = ranked.iter().take(TAG_CHIPS).map(|(_, (tag, _))| tag.clone()).collect();
        for tag in &self.tags {
            if !chips.iter().any(|c| c.to_lowercase() == *tag) {
                chips.push(tag.clone());
            }
        }
        self.cache = Some((key, rows.clone(), chips.clone()));
        (rows, chips)
    }

    /// Every saved record of `kind`, favourites first, then newest, narrowed
    /// by the filter, or an invitation to create (or import) the first.
    fn draw_index(&mut self, p: &mut Painter, ui: &mut Ui, area: Rect, kind: Kind, actions: &mut Vec<Action>) -> Option<Event> {
        let t = p.theme;
        if self.shown != Some(kind) {
            self.clear_filter();
            self.shown = Some(kind);
        }
        let new = Rect::new(area.right() - 16.0 - 130.0, area.y + (theme::HEADER_HEIGHT - 30.0) * 0.5, 130.0, 30.0);
        let mut create = button(p, ui, new, &format!("New {}", kind.noun()), ButtonStyle::Primary, true);
        let (mut import, mut buttons_x) = (false, new.x);
        if kind == Kind::Character {
            let (label, import_w) = if self.importing { ("Importing…", 108.0) } else { ("Import", 80.0) };
            let import_rect = Rect::new(new.x - 8.0 - import_w, new.y, import_w, 30.0);
            import = button(p, ui, import_rect, label, ButtonStyle::Secondary, !self.importing);
            buttons_x = import_rect.x;
        }
        header(p, area, kind.plural(), area.x + 16.0, buttons_x - 12.0);
        let (body, x, width) = self.body(ui, area);
        // Reading the files takes milliseconds; a spinner would only flash.
        if !self.loaded {
            return None;
        }
        let interactive = body.contains(ui.mouse);
        let scroll = self.scroll;
        if self.list(kind).is_empty() {
            let top = body.y + (body.h * 0.5 - 90.0).max(32.0);
            let title = p.layout(&format!("No {} yet", kind.plural().to_lowercase()), theme::TITLE, None);
            p.text_aligned(&title, x, top, Align::Center, width, t.text);
            let blurb = p.layout(kind.blurb(), theme::SMALL, Some(width));
            p.text_aligned(&blurb, x, top + 40.0, Align::Center, width, t.text_muted);
            let y = top + 64.0 + blurb.height();
            let label = format!("Create a {}", kind.noun());
            if kind == Kind::Character {
                // Or bring them from another app, as character cards.
                create |= button(p, ui, Rect::new(x + width * 0.5 - 178.0, y, 170.0, 34.0), &label, ButtonStyle::Secondary, true);
                import |= button(p, ui, Rect::new(x + width * 0.5 + 8.0, y, 170.0, 34.0), "Import a card", ButtonStyle::Secondary, !self.importing);
            } else {
                create |= button(p, ui, Rect::new(x + (width - 170.0) * 0.5, y, 170.0, 34.0), &label, ButtonStyle::Secondary, true);
            }
        }
        if create {
            self.stash();
            self.form = Some(Form::new(kind, None));
            self.scroll = 0.0;
            return None;
        }
        if import {
            self.importing = true;
            actions.push(Action::ImportCards);
        }
        if self.list(kind).is_empty() {
            return None;
        }

        let (rows, tag_chips) = self.rows(kind);
        let clip = p.push_clip(body);
        let top = body.y + 24.0 - scroll.round();
        p.label(kind.blurb(), theme::SMALL, x, top, t.text_muted);
        let mut y = top + 30.0;

        // The filter: typing anywhere on the page goes here.
        let total = self.list(kind).len();
        let filter = self.filter.get_or_insert_with(|| Fields::new([Editor::restricted(120, |c| !c.is_control())], [false]));
        let field = Rect::new(x, y, width, 40.0);
        let layout = filter.layout(p, 0, width);
        let hint = format!("Filter by name, tag, comment or description ({total})");
        filter.draw(0, p, ui, field, layout, &hint, interactive);
        let narrowed = self.favorites || !self.tags.is_empty() || !filter.text(0).trim().is_empty();
        if narrowed {
            let count = p.layout(&format!("{} of {total}", rows.len()), theme::SMALL, None);
            p.text(&count, field.right() - FIELD_PAD.0 - count.width(), field.y + (field.h - count.height()) * 0.5, t.text_faint);
        }
        y += field.h + 10.0;

        // Chips: favourites, then the tags most used.
        let mut chip_clicked = None;
        let any_favorite = self.favorites || self.list(kind).iter().any(|r| r.favorite);
        let labels = any_favorite.then(|| "★ Favorites".to_owned()).into_iter().chain(tag_chips.iter().cloned());
        let mut at = (x, y);
        let mut drawn = false;
        for (k, label) in labels.enumerate() {
            let favorite = any_favorite && k == 0;
            let selected = if favorite { self.favorites } else { self.tags.contains(&label.to_lowercase()) };
            let mut text = p.layout(&label, theme::SMALL, None);
            text.truncate(p.fonts, 200.0);
            let chip_w = text.width() + 22.0;
            if at.0 + chip_w > x + width && at.0 > x {
                at = (x, at.1 + CHIP_H + 6.0);
            }
            let chip_rect = Rect::new(at.0, at.1, chip_w, CHIP_H);
            at.0 += chip_w + 6.0;
            drawn = true;
            let hovered = interactive && ui.hovered(chip_rect);
            let hover = ui.anim(id(("chip", &label)), f32::from(u8::from(hovered)));
            let (fill, border) = if selected { (fade(t.accent, 0.16 + 0.06 * hover), fade(t.accent, 0.7)) } else { (fade(t.hover, hover), t.border_strong) };
            p.bordered(chip_rect, fill, CHIP_H * 0.5, 1.0, border);
            let color = if selected { t.text } else { mix(t.text_muted, t.text, hover) };
            p.text(&text, chip_rect.x + 11.0, chip_rect.y + (CHIP_H - text.height()) * 0.5, color);
            if hovered {
                ui.cursor = CursorIcon::Pointer;
                if ui.clicked(chip_rect) {
                    chip_clicked = Some((favorite, label.to_lowercase()));
                }
            }
        }
        if drawn {
            y = at.1 + CHIP_H + 14.0;
        }

        let (mut opened, mut play, mut star, mut clear) = (None, None, None, false);
        if rows.is_empty() {
            p.label(&format!("No {} match.", kind.plural().to_lowercase()), theme::SMALL, x, y + 8.0, t.text_muted);
            clear = button(p, ui, Rect::new(x + width - 110.0, y + 2.0, 110.0, 30.0), "Clear filter", ButtonStyle::Ghost, interactive);
            y += 40.0;
        }
        for &index in &rows {
            let record = &self.list(kind)[index];
            let card = Rect::new(x, y, width, CARD_H);
            y += CARD_H + 8.0;
            if card.bottom() < body.y || card.y > body.bottom() {
                continue;
            }
            let hovered = interactive && ui.hovered(card);
            let hover = ui.anim(id(("record", &record.id)), f32::from(u8::from(hovered)));
            p.bordered(card, mix(t.surface, t.hover, hover), theme::RADIUS, 1.0, mix(t.border, t.border_strong, hover));
            let face = Rect::new(card.x + 12.0, card.y + (CARD_H - CARD_PORTRAIT) * 0.5, CARD_PORTRAIT, CARD_PORTRAIT);
            portrait(p, self.portrait_path(&record.portrait).as_deref(), &record.name, face, theme::RADIUS_SM);
            let text_x = face.right() + 14.0;

            // Edit on the right, for worlds: clicking the card plays instead.
            let mut right = card.right() - 14.0;
            let mut on_control = false;
            if kind == Kind::World {
                let edit_button = Rect::new(right - 72.0, card.y + (CARD_H - 30.0) * 0.5, 72.0, 30.0);
                on_control = edit_button.contains(ui.mouse);
                // Inert only where the mouse is over this button through the header
                // (it is scrolled beneath): hovering the header or sidebar elsewhere must not dim it.
                if button(p, ui, edit_button, "Edit", ButtonStyle::Primary, interactive || !on_control) {
                    opened = Some(index);
                }
                right = edit_button.x - 6.0;
            }
            // The star: always on a favourite, on hover otherwise.
            let star_rect = Rect::new(right - 30.0, card.y + (CARD_H - 30.0) * 0.5, 30.0, 30.0);
            let on_star = hovered && star_rect.contains(ui.mouse);
            on_control |= on_star;
            if record.favorite || hovered {
                if on_star {
                    p.rect(star_rect, t.active, theme::RADIUS_SM);
                    ui.cursor = CursorIcon::Pointer;
                }
                let color = if record.favorite { t.accent } else if on_star { t.text } else { t.text_faint };
                p.label_centered(if record.favorite { "★" } else { "☆" }, Style::regular(16.0), star_rect, color);
                if on_star && ui.clicked(star_rect) {
                    star = Some(record.id.clone());
                }
            }
            right = star_rect.x - 8.0;

            let mut name = p.layout(&record.name, theme::LABEL, None);
            name.truncate(p.fonts, right - text_x);
            p.text(&name, text_x, card.y + 17.0, t.text);
            // Its first tags, beside the name while they fit.
            let mut pill_x = text_x + name.width() + 10.0;
            for tag in record.tags.iter().take(3) {
                let text = p.layout(tag, theme::TINY, None);
                let pill = Rect::new(pill_x, card.y + 16.0, text.width() + 14.0, 19.0);
                if pill.right() > right {
                    break;
                }
                p.bordered(pill, t.bg, 9.5, 1.0, t.border);
                p.text(&text, pill.x + 7.0, pill.y + (pill.h - text.height()) * 0.5, t.text_muted);
                pill_x = pill.right() + 4.0;
            }
            // The user's comment tells it apart best; else its description.
            let summary = if record.comment.is_empty() {
                record.description.lines().find(|l| !l.trim().is_empty()).unwrap_or("No description yet")
            } else {
                record.comment.as_str()
            };
            let mut summary = p.layout(summary, theme::SMALL, None);
            summary.truncate(p.fonts, right - text_x);
            p.text(&summary, text_x, card.y + 43.0, t.text_muted);
            if hovered && !on_control {
                ui.cursor = CursorIcon::Pointer;
                if ui.clicked(card) {
                    if kind == Kind::World {
                        play = Some(record.id.clone());
                    } else {
                        opened = Some(index);
                    }
                }
            }
        }
        p.set_clip(clip);
        self.content_h = y - top + 24.0;

        if let Some((favorite, tag)) = chip_clicked {
            if favorite {
                self.favorites = !self.favorites;
            } else if let Some(at) = self.tags.iter().position(|t| *t == tag) {
                self.tags.remove(at);
            } else {
                self.tags.push(tag);
            }
        }
        if clear {
            self.clear_filter();
        }
        if let Some(id) = star {
            self.toggle_favorite(kind, &id, actions);
        }
        if let Some(index) = opened {
            let record = self.list(kind)[index].clone();
            self.form = Some(Form::new(kind, Some(record)));
            self.scroll = 0.0;
        }
        play.map(Event::Play)
    }

    /// The open form: portrait, name, tags and description, the sections,
    /// and for a saved world the stories played in it; Save, Play and (for
    /// characters) Generate in the header, the rest in its ⋯ menu.
    fn draw_form(&mut self, p: &mut Painter, ui: &mut Ui, area: Rect, stories: &[Story], actions: &mut Vec<Action>) -> Option<Event> {
        let t = p.theme;
        let (body, x, width) = self.body(ui, area);
        let scroll = self.scroll;
        let portrait_path = self.form.as_ref().and_then(|f| self.portrait_path(&f.portrait));
        let form = self.form.as_mut()?;
        let kind = form.kind;

        // Header: back, title, and the form's buttons on the right.
        let back = Rect::new(area.x + 8.0, area.y + (theme::HEADER_HEIGHT - 30.0) * 0.5, 112.0, 30.0);
        let title = match form.fields.text(NAME).trim() {
            _ if form.new => format!("New {}", kind.noun()),
            "" => "Untitled".to_owned(),
            name => name.to_owned(),
        };
        // With unsaved changes, going back discards them on a second click.
        let go_back = if form.confirm_discard {
            button(p, ui, back, "Discard changes", ButtonStyle::Danger, true)
        } else {
            button(p, ui, back, &format!("← {}", kind.plural()), ButtonStyle::Ghost, true)
        };
        if !go_back && ui.released && !back.contains(ui.press_pos) {
            form.confirm_discard = false;
        }
        let save = Rect::new(area.right() - 16.0 - 80.0, back.y, 80.0, 30.0);
        let has_name = !form.fields.text(NAME).trim().is_empty();
        let saved = button(p, ui, save, "Save", ButtonStyle::Primary, has_name && !form.locked);
        let mut right = save.x - 8.0;
        let mut play = None;
        // Personas are played from a story's "who are you?".
        if !form.new && kind != Kind::Persona {
            let play_button = Rect::new(right - 72.0, back.y, 72.0, 30.0);
            if button(p, ui, play_button, "Play", ButtonStyle::Secondary, has_name) {
                play = Some(play_button);
            }
            right = play_button.x - 8.0;
        }
        let mut generate = false;
        if kind == Kind::Character {
            let generate_w = if form.generating.is_some() { 120.0 } else { 96.0 };
            let generate_button = Rect::new(right - generate_w, back.y, generate_w, 30.0);
            let label = if form.generating.is_some() { "Generating…" } else { "Generate" };
            generate = button(p, ui, generate_button, label, ButtonStyle::Secondary, form.generating.is_none());
            right = generate_button.x - 8.0;
        }
        let mut menu = None;
        if !form.new {
            let dots = Rect::new(right - 34.0, back.y, 34.0, 30.0);
            if dots_button(p, ui, dots) && has_name {
                menu = Some(dots);
            }
            right = dots.x - 8.0;
        }
        // The title takes what the buttons leave.
        header(p, area, &title, back.right() + 12.0, right);

        let interactive = body.contains(ui.mouse);
        let clip = p.push_clip(body);
        let top = body.y + 32.0 - scroll.round();

        // Portrait on the left; name and tags beside it.
        let face = Rect::new(x, top, FORM_PORTRAIT, FORM_PORTRAIT);
        let hovered = interactive && ui.hovered(face) && !form.picking;
        let hover = ui.anim(id("form-portrait"), f32::from(u8::from(hovered)));
        portrait(p, portrait_path.as_deref(), form.fields.text(NAME), face, theme::RADIUS);
        p.bordered(face, [0.0; 4], theme::RADIUS, 1.0, mix(t.border, t.border_focus, hover));
        if hovered || form.portrait.is_empty() || form.picking {
            let strip = Rect::new(face.x + 1.0, face.bottom() - 27.0, face.w - 2.0, 26.0);
            p.rect(strip, fade(t.bg, 0.82), theme::RADIUS - 1.0);
            let label = if form.picking {
                "Choosing…"
            } else if form.portrait.is_empty() {
                "Add portrait"
            } else {
                "Change"
            };
            p.label_centered(label, theme::CAPTION, strip, t.text);
        }
        if hovered {
            ui.cursor = CursorIcon::Pointer;
            if ui.clicked(face) {
                form.picking = true;
                form.error = None;
                actions.push(Action::PickPortrait);
            }
        }
        let mut below = face.bottom() + 6.0;
        if !form.portrait.is_empty() && !form.picking {
            let remove = Rect::new(face.x, below, face.w, 24.0);
            if button(p, ui, remove, "Remove", ButtonStyle::Ghost, interactive) {
                form.portrait.clear();
            }
            below += 24.0;
        }
        if let Some(error) = &form.error {
            let text = p.layout(error, theme::TINY, Some(face.w));
            p.text(&text, face.x, below + 4.0, t.danger);
            below += text.height() + 8.0;
        }

        let (fx, fw) = (face.right() + 20.0, width - FORM_PORTRAIT - 20.0);
        let mut fy = top;
        p.label("Name", theme::LABEL, fx, fy, t.text);
        let name = form.fields.layout(p, NAME, fw);
        let name_h = name.line_height() + 2.0 * FIELD_PAD.1;
        form.fields.draw(NAME, p, ui, Rect::new(fx, fy + 24.0, fw, name_h), name, &format!("Name this {}", kind.noun()), interactive);
        fy += 24.0 + name_h + 14.0;
        // Tags and the private comment side by side, under the name.
        let half = ((fw - 12.0) * 0.5).floor();
        let comment_x = fx + half + 12.0;
        p.label("Tags", theme::LABEL, fx, fy, t.text);
        p.label("Comment", theme::LABEL, comment_x, fy, t.text);
        let private = p.layout("Only you see it", theme::TINY, None);
        p.text(&private, fx + fw - private.width(), fy + 2.0, t.text_faint);
        let tags = form.fields.layout(p, TAGS, half);
        let comment = form.fields.layout(p, COMMENT, half);
        let row_h = tags.height().max(comment.height()) + 2.0 * FIELD_PAD.1;
        form.fields.draw(TAGS, p, ui, Rect::new(fx, fy + 24.0, half, row_h), tags, "e.g. fantasy, mentor", interactive);
        form.fields.draw(COMMENT, p, ui, Rect::new(comment_x, fy + 24.0, half, row_h), comment, "To tell it apart", interactive);
        fy += 24.0 + row_h + 10.0;
        let line = |text: &str, painter: &mut Painter, color| {
            let mut text = painter.layout(text, theme::TINY, None);
            text.truncate(painter.fonts, fw);
            painter.text(&text, fx, fy, color);
        };
        if let Some(error) = &form.generate_error {
            line(error, p, t.danger);
        } else if form.locked {
            line(&format!("Saved by a newer version of OpenRP: update OpenRP to change this {}.", kind.noun()), p, t.danger);
        } else if form.confirm_discard {
            line("Unsaved changes: Esc again discards them.", p, t.danger);
        } else {
            let save = if cfg!(target_os = "macos") { "Cmd+S" } else { "Ctrl+S" };
            let tip = if kind == Kind::Character {
                format!("{save} saves  ·  Esc discards  ·  Generate starts from what you typed")
            } else {
                format!("{save} saves  ·  Esc goes back without saving")
            };
            line(&tip, p, t.text_faint);
        }
        fy += 16.0;

        let mut y = below.max(fy) + 24.0;
        p.label("Description", theme::LABEL, x, y, t.text);
        y += 24.0;
        let description = form.fields.layout(p, DESCRIPTION, width);
        let description_h = (description.height() + 2.0 * FIELD_PAD.1).max(DESCRIPTION_MIN_H);
        form.fields.draw(DESCRIPTION, p, ui, Rect::new(x, y, width, description_h), description, kind.hint(), interactive);
        y += description_h + 20.0;

        // The sections, each a row that opens and closes it.
        let mut changes = Changes::default();
        for &section in kind.sections() {
            let open = form.open.contains(&section);
            let summary = match section {
                Section::Greetings => match form.greetings {
                    0 => "None yet".to_owned(),
                    1 => "First message".to_owned(),
                    2 => "First message and 1 alternate".to_owned(),
                    count => format!("First message and {} alternates", count - 1),
                },
                Section::Examples => match form.fields.text(EXAMPLES).lines().filter(|l| !l.trim().is_empty()).count() {
                    0 => "None yet".to_owned(),
                    1 => "1 line".to_owned(),
                    count => format!("{count} lines"),
                },
                Section::Lore => match form.constant.len() {
                    0 => "None yet".to_owned(),
                    1 => "1 entry".to_owned(),
                    count => format!("{count} entries"),
                },
            };
            let row = Rect::new(x, y, width, SECTION_H);
            if section_row(p, ui, row, section.title(), &summary, open, interactive) {
                changes.toggle = Some(section);
            }
            y += SECTION_H;
            if open {
                y += 8.0;
                y = match section {
                    Section::Greetings => draw_greetings(form, p, ui, (x, y, width), interactive, &mut changes),
                    Section::Examples => draw_examples(form, p, ui, (x, y, width), interactive),
                    Section::Lore => draw_lore(form, p, ui, (x, y, width), interactive, &mut changes, actions),
                } + 12.0;
            }
        }
        p.rect(Rect::new(x, y, width, 1.0), t.border, 0.0);

        // A saved world lists the stories played in it, newest first.
        let mut opened = None;
        if kind == Kind::World && !form.new {
            y += 28.0;
            p.label("Stories", theme::LABEL, x, y, t.text);
            if !stories.is_empty() {
                let count = p.layout(&stories.len().to_string(), theme::SMALL, None);
                p.text(&count, x + width - count.width(), y + 1.0, t.text_faint);
            }
            y += 28.0;
            if stories.is_empty() {
                p.label("None yet: press Play to start one.", theme::SMALL, x, y, t.text_muted);
                y += 20.0;
            }
            let now = unix_now();
            for story in stories {
                let row = Rect::new(x, y, width, STORY_H);
                y += STORY_H + 4.0;
                let hovered = interactive && ui.hovered(row);
                let hover = ui.anim(id(("story", story.id)), f32::from(u8::from(hovered)));
                p.bordered(row, mix(t.surface, t.hover, hover), theme::RADIUS_SM, 1.0, mix(t.border, t.border_strong, hover));
                let age = p.layout(&crate::chat::ago(now, story.updated), theme::TINY, None);
                p.text(&age, row.right() - 12.0 - age.width(), row.y + (STORY_H - age.height()) * 0.5, t.text_faint);
                let mut title = p.layout(if story.title.is_empty() { "Untitled story" } else { &story.title }, theme::SMALL, None);
                title.truncate(p.fonts, row.w - 36.0 - age.width());
                p.text(&title, row.x + 12.0, row.y + (STORY_H - title.height()) * 0.5, t.text);
                if hovered {
                    ui.cursor = CursorIcon::Pointer;
                    if ui.clicked(row) {
                        opened = Some(story.id);
                    }
                }
            }
        }
        p.set_clip(clip);
        self.content_h = y - top + 64.0;
        changes.apply(form);

        let id = form.id.clone();
        if generate {
            return Some(Event::Generate(form.idea()));
        }
        if let Some(anchor) = menu {
            return Some(Event::FormMenu(anchor));
        }
        if saved {
            self.save(actions);
        } else if let Some(story) = opened {
            // Unsaved edits wait as a draft.
            self.stash();
            return Some(Event::Open(story));
        } else if go_back {
            // Unsaved changes go only on a second click.
            match self.form.as_mut() {
                Some(form) if form.dirty() && !form.confirm_discard => form.confirm_discard = true,
                _ => self.form = None,
            }
        } else if let Some(anchor) = play {
            // Play what is shown: unsaved edits are saved first.
            if kind == Kind::World {
                self.save(actions);
                return Some(Event::Play(id));
            }
            self.commit(actions);
            return Some(Event::PlayCharacter(id, anchor));
        }
        None
    }
}

/// Changes to the form asked for while it was drawn, made once it is.
#[derive(Default)]
struct Changes {
    toggle: Option<Section>,
    add_greeting: bool,
    remove_greeting: Option<usize>,
    add_lore: bool,
    remove_lore: Option<usize>,
    constant: Option<usize>,
}

impl Changes {
    fn apply(self, form: &mut Form) {
        if let Some(section) = self.toggle {
            match form.open.iter().position(|s| *s == section) {
                Some(at) => {
                    form.open.remove(at);
                }
                None => form.open.push(section),
            }
        }
        if self.add_greeting {
            form.add_greeting();
        }
        if let Some(k) = self.remove_greeting {
            form.remove_greeting(k);
        }
        if self.add_lore {
            form.add_lore(&[LoreEntry::default()]);
        }
        if let Some(k) = self.remove_lore {
            form.remove_lore(k);
        }
        if let Some(flag) = self.constant.and_then(|k| form.constant.get_mut(k)) {
            *flag = !*flag;
        }
        // Typing never goes to a field out of sight.
        if !form.shown(form.fields.focused()) {
            form.fields.focus(DESCRIPTION);
        }
    }
}

/// Draws the open Greetings section from `y` in a column at `x`, `width`
/// wide; returns where it ends.
fn draw_greetings(form: &mut Form, p: &mut Painter, ui: &mut Ui, (x, y, width): (f32, f32, f32), interactive: bool, changes: &mut Changes) -> f32 {
    let t = p.theme;
    let mut y = y;
    let field_w = width - REMOVE - 8.0;
    for k in 0..form.greetings {
        let label = if k == 0 { "First message".to_owned() } else { format!("Alternate {k}") };
        p.label(&label, theme::CAPTION, x, y, t.text_faint);
        y += 20.0;
        let index = Form::greeting(k);
        let layout = form.fields.layout(p, index, field_w);
        let field_h = (layout.height() + 2.0 * FIELD_PAD.1).max(GREETING_MIN_H);
        let hint = if k == 0 { "*She looks up from the bar.* What'll it be, {{user}}?" } else { "Another way the story could open" };
        form.fields.draw(index, p, ui, Rect::new(x, y, field_w, field_h), layout, hint, interactive);
        if button(p, ui, Rect::new(x + field_w + 8.0, y + 6.0, REMOVE, REMOVE), "×", ButtonStyle::Ghost, interactive) {
            changes.remove_greeting = Some(k);
        }
        y += field_h + 12.0;
    }
    changes.add_greeting |= button(p, ui, Rect::new(x - 8.0, y, 130.0, 30.0), "+ Add greeting", ButtonStyle::Ghost, interactive);
    y + 30.0 + hint_line(p, "Their first message opens a story they join before anyone speaks; the others can be swiped to. {{user}} is your character, {{char}} theirs.", (x, y + 36.0, width))
        + 6.0
}

/// Draws the open Example dialogue section; returns where it ends.
fn draw_examples(form: &mut Form, p: &mut Painter, ui: &mut Ui, (x, y, width): (f32, f32, f32), interactive: bool) -> f32 {
    let layout = form.fields.layout(p, EXAMPLES, width);
    let h = (layout.height() + 2.0 * FIELD_PAD.1).max(EXAMPLES_MIN_H);
    let hint = "<START>\n{{user}}: Seen anything strange?\n{{char}}: *leans in* Only you, stranger.";
    form.fields.draw(EXAMPLES, p, ui, Rect::new(x, y, width, h), layout, hint, interactive);
    y + h + hint_line(p, "Sample lines in their voice. The AI learns how they talk from them; they never happen in the story.", (x, y + h + 8.0, width)) + 8.0
}

/// Draws the open Lore section; returns where it ends.
fn draw_lore(form: &mut Form, p: &mut Painter, ui: &mut Ui, (x, y, width): (f32, f32, f32), interactive: bool, changes: &mut Changes, actions: &mut Vec<Action>) -> f32 {
    let t = p.theme;
    let mut y = y;
    let toggle_w = 86.0;
    let column = width - REMOVE - 8.0;
    let keys_w = column - toggle_w - 8.0;
    for k in 0..form.constant.len() {
        let keys = form.lore_keys(k);
        let constant = form.constant[k];
        let layout = form.fields.layout(p, keys, keys_w);
        let keys_h = layout.height() + 2.0 * FIELD_PAD.1;
        let hint = if constant { "Keys not needed: always read" } else { "Keys that bring it in, e.g. Lantern, the inn" };
        form.fields.draw(keys, p, ui, Rect::new(x, y, keys_w, keys_h), layout, hint, interactive);
        // Always: read whatever is said.
        let toggle = Rect::new(x + keys_w + 8.0, y + (keys_h - 30.0) * 0.5, toggle_w, 30.0);
        let (label, style) = if constant { ("✓ Always", ButtonStyle::Secondary) } else { ("Always", ButtonStyle::Ghost) };
        if button(p, ui, toggle, label, style, interactive) {
            changes.constant = Some(k);
        }
        if button(p, ui, Rect::new(x + column + 8.0, y + (keys_h - REMOVE) * 0.5, REMOVE, REMOVE), "×", ButtonStyle::Ghost, interactive) {
            changes.remove_lore = Some(k);
        }
        y += keys_h + 6.0;
        let layout = form.fields.layout(p, keys + 1, column);
        let content_h = (layout.height() + 2.0 * FIELD_PAD.1).max(LORE_MIN_H);
        form.fields.draw(keys + 1, p, ui, Rect::new(x, y, column, content_h), layout, "What the AI should know when it comes up", interactive);
        y += content_h + 18.0;
    }
    changes.add_lore |= button(p, ui, Rect::new(x - 8.0, y, 112.0, 30.0), "+ Add entry", ButtonStyle::Ghost, interactive);
    let (label, import_w) = if form.importing { ("Importing…", 116.0) } else { ("Import lorebook…", 140.0) };
    if button(p, ui, Rect::new(x + 108.0, y, import_w, 30.0), label, ButtonStyle::Ghost, interactive && !form.importing) {
        form.importing = true;
        form.import_error = None;
        actions.push(Action::ImportLore);
    }
    y += 36.0;
    if let Some(error) = &form.import_error {
        let text = p.layout(error, theme::TINY, Some(width));
        p.text(&text, x, y, t.danger);
        y += text.height() + 6.0;
    }
    y + hint_line(p, "The AI reads an entry only while the latest turns mention one of its keys, so a big world costs little. A SillyTavern lorebook or a card's book can be imported.", (x, y, width))
}

/// Draws a faint hint at `(x, y)`, wrapped to `width`; returns its height.
fn hint_line(p: &mut Painter, text: &str, (x, y, width): (f32, f32, f32)) -> f32 {
    let text = p.layout(text, theme::TINY, Some(width));
    p.text(&text, x, y, p.theme.text_faint);
    text.height()
}

/// Draws a section's header row in `rect`: a hairline above, a chevron, its
/// `title` and a `summary` on the right. Returns whether it was clicked.
fn section_row(p: &mut Painter, ui: &mut Ui, rect: Rect, title: &str, summary: &str, open: bool, interactive: bool) -> bool {
    let t = p.theme;
    p.rect(Rect::new(rect.x, rect.y, rect.w, 1.0), t.border, 0.0);
    let row = Rect::new(rect.x - 8.0, rect.y + 6.0, rect.w + 16.0, rect.h - 8.0);
    let hovered = interactive && ui.hovered(row);
    let hover = ui.anim(id(("section", title)), f32::from(u8::from(hovered)));
    p.rect(row, fade(t.hover, hover), theme::RADIUS_SM);
    let color = mix(t.text_muted, t.text, hover.max(f32::from(u8::from(open))));
    let (cx, cy) = if open { (rect.x, row.y + row.h * 0.5 - 2.0) } else { (rect.x + 2.0, row.y + row.h * 0.5 - 3.5) };
    chevron(p, cx, cy, open, color);
    let label = p.layout(title, theme::LABEL, None);
    p.text(&label, rect.x + 18.0, row.y + (row.h - label.height()) * 0.5, t.text);
    let mut summary = p.layout(summary, theme::SMALL, None);
    summary.truncate(p.fonts, rect.w - label.width() - 40.0);
    p.text(&summary, rect.right() - summary.width(), row.y + (row.h - summary.height()) * 0.5, t.text_faint);
    if hovered {
        ui.cursor = CursorIcon::Pointer;
    }
    hovered && ui.clicked(row)
}

/// A ghost button showing three dots, for a menu. Returns whether it was clicked.
fn dots_button(p: &mut Painter, ui: &mut Ui, rect: Rect) -> bool {
    let clicked = button(p, ui, rect, "", ButtonStyle::Ghost, true);
    let color = if ui.hovered(rect) { p.theme.text } else { p.theme.text_muted };
    for i in 0..3 {
        let dot = Rect::new(rect.x + rect.w * 0.5 - 6.25 + 5.0 * i as f32, rect.y + rect.h * 0.5 - 1.25, 2.5, 2.5);
        p.rect(dot, color, 1.25);
    }
    clicked
}

/// Draws a portrait filling `rect`, or the first letter of `name` on a
/// tinted square while there is none (or it is still loading).
pub fn portrait(p: &mut Painter, path: Option<&str>, name: &str, rect: Rect, radius: f32) {
    if let Some(path) = path
        && let Lookup::Ready(_) = p.image(path, rect, radius)
    {
        return;
    }
    let t = p.theme;
    p.rect(rect, fade(t.accent, 0.16), radius);
    let initial: String = name.trim().chars().next().map(char::to_uppercase).into_iter().flatten().collect();
    p.label_centered(if initial.is_empty() { "?" } else { &initial }, Style::semibold(rect.h * 0.42), rect, t.accent);
}

/// The page's title bar over `area`, its title between `title_x` and
/// `title_right` (where its buttons start).
fn header(p: &mut Painter, area: Rect, title: &str, title_x: f32, title_right: f32) {
    let t = p.theme;
    let bar = Rect::new(area.x, area.y, area.w, theme::HEADER_HEIGHT);
    p.rect(Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0), t.border, 0.0);
    let mut title = p.layout(title, theme::LABEL, None);
    title.truncate(p.fonts, (title_right - title_x).max(24.0));
    p.text(&title, title_x, bar.y + (bar.h - title.height()) * 0.5, t.text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_and_deleting_records() {
        let mut view = LibraryView::default();
        let old = World { id: "a".into(), name: "Old".into(), updated: 1, ..World::default() };
        let new = World { id: "b".into(), name: "New".into(), updated: 2, ..World::default() };
        view.loaded(vec![old, new], Vec::new(), Vec::new(), Portraits::at("p".into()));
        assert_eq!(view.worlds.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["b", "a"], "newest first");

        // A nameless form is not saved.
        let mut actions = Vec::new();
        view.form = Some(Form::new(Kind::Character, None));
        view.save(&mut actions);
        assert!(actions.is_empty() && view.form.is_some());

        // Editing moves the record to the top and saves it as its kind,
        // with the portrait picked meanwhile.
        view.form = Some(Form::new(Kind::World, Some(view.worlds[1].clone())));
        view.portrait_picked(Ok(Some("face.png".into())));
        view.form.as_mut().unwrap().fields.editor(DESCRIPTION).insert("  Twelve districts.  ");
        view.form.as_mut().unwrap().fields.editor(TAGS).insert(" dystopia, Dystopia,, games ");
        view.save(&mut actions);
        assert!(view.form.is_none());
        let [Action::SaveWorld(w)] = &actions[..] else { panic!("not saved") };
        assert_eq!((w.id.as_str(), w.description.as_str(), w.portrait.as_str(), w.created), ("a", "Twelve districts.", "face.png", 0));
        assert_eq!(w.tags, ["dystopia", "games"], "trimmed, without repeats");
        assert_eq!(view.worlds[0].id, "a");

        // Unsaved changes survive looking elsewhere, as a draft.
        view.form = Some(Form::new(Kind::World, Some(view.worlds[0].clone())));
        view.form.as_mut().unwrap().fields.insert("Draft ");
        view.show(Kind::Character);
        assert!(view.form.is_none() && view.drafts.len() == 1);
        view.show(Kind::World);
        assert!(view.form.as_ref().is_some_and(|f| f.fields.text(NAME).contains("Draft")) && view.drafts.is_empty());
        view.open_form(Kind::World, "b");
        assert_eq!(view.drafts.len(), 1, "opening another keeps the draft");
        view.open_form(Kind::World, "a");
        assert!(view.form.as_ref().is_some_and(Form::dirty), "and opening it again brings it back");
        view.form = None;
        view.drafts.clear();

        // Deleting a new record only drops the form; a saved world reports
        // itself so its stories go too.
        let mut actions = Vec::new();
        view.form = Some(Form::new(Kind::World, None));
        assert_eq!(view.form_menu_picked(FormAction::Delete, &mut actions), None);
        assert!(actions.is_empty());
        view.form = Some(Form::new(Kind::World, Some(view.worlds[0].clone())));
        assert_eq!(view.form_menu(), [FormAction::Duplicate, FormAction::Delete], "worlds are not exported");
        assert_eq!(view.form_menu_picked(FormAction::Delete, &mut actions).as_deref(), Some("a"));
        assert!(matches!(&actions[..], [Action::DeleteRecord(Kind::World, id)] if id == "a"));
        assert_eq!(view.worlds.len(), 1);

        // Duplicating saves what is shown, then opens an unsaved copy.
        let mut actions = Vec::new();
        view.form = Some(Form::new(Kind::World, Some(view.worlds[0].clone())));
        assert_eq!(view.open_world(), Some("b"));
        view.form.as_mut().unwrap().fields.insert("er");
        view.form_menu_picked(FormAction::Duplicate, &mut actions);
        assert!(matches!(&actions[..], [Action::SaveWorld(w)] if w.id == "b" && w.name == "Newer"));
        let copy = view.form.as_ref().unwrap();
        assert!(copy.new && copy.id != "b" && copy.fields.text(NAME) == "Newer (copy)");
        assert_eq!(view.open_world(), None, "an unsaved world has no stories");
        view.save(&mut actions);
        assert_eq!(view.worlds.len(), 2);

        // A pick landing after the form closed changes nothing.
        view.portrait_picked(Ok(Some("late.png".into())));
        assert!(view.form.is_none());

        // A world from a newer version is shown but never saved over.
        let newer = World { id: "n".into(), name: "Newer".into(), version: World::VERSION + 1, ..World::default() };
        view.loaded(vec![newer], Vec::new(), Vec::new(), Portraits::at("p".into()));
        let mut actions = Vec::new();
        view.open_form(Kind::World, "n");
        view.form.as_mut().unwrap().fields.insert("x");
        view.save(&mut actions);
        assert!(actions.is_empty() && view.form.is_some());
    }

    #[test]
    fn greetings_and_lore_are_edited_in_sections() {
        let lore = vec![LoreEntry { keys: vec!["inn".into()], content: "Warm.".into(), constant: false }];
        let mira = Character { id: "m".into(), name: "Mira".into(), greetings: vec!["Hi.".into()], lore, ..Character::default() };
        let mut view = LibraryView::default();
        view.loaded(Vec::new(), vec![mira], Vec::new(), Portraits::at("p".into()));
        view.open_form(Kind::Character, "m");
        let form = view.form.as_mut().unwrap();
        assert!(!form.dirty(), "opened as saved");
        assert_eq!((form.fields.text(Form::greeting(0)), form.fields.text(form.lore_keys(0)), form.fields.text(form.lore_keys(0) + 1)), ("Hi.", "inn", "Warm."));

        // A greeting added and filled, the entry made constant, a new one left empty.
        form.add_greeting();
        form.fields.insert("Back again?");
        form.constant[0] = true;
        form.add_lore(&[LoreEntry::default()]);
        assert_eq!(form.fields.text(form.lore_keys(0) + 1), "Warm.", "lore moved along with the new greeting");
        let record = form.record().unwrap();
        assert_eq!(record.greetings, ["Hi.", "Back again?"]);
        assert!(record.lore.len() == 1 && record.lore[0].constant, "the empty entry is dropped");

        // Tab skips the fields of closed sections.
        form.open.clear();
        form.fields.focus(DESCRIPTION);
        form.tab(false);
        assert_eq!(form.fields.focused(), NAME, "past the hidden greetings and lore, back to the top");
        form.tab(true);
        assert_eq!(form.fields.focused(), DESCRIPTION);

        // Removing shifts what follows.
        form.remove_greeting(0);
        form.remove_lore(1);
        let record = form.record().unwrap();
        assert!(record.greetings == ["Back again?"] && record.lore[0].content == "Warm.");

        // A lorebook's entries join the open form, unsaved.
        form.importing = true;
        view.lore_imported(Ok(vec![LoreEntry { keys: vec!["dragon".into()], content: "Big.".into(), constant: false }]));
        let form = view.form.as_ref().unwrap();
        assert!(form.record().unwrap().lore.len() == 2 && form.open.contains(&Section::Lore));
        let mut actions = Vec::new();
        assert_eq!(view.form_menu(), [FormAction::Duplicate, FormAction::Export, FormAction::Delete]);
        view.form_menu_picked(FormAction::Export, &mut actions);
        assert!(matches!(&actions[..], [Action::SaveCharacter(c), Action::ExportCard { character, .. }] if c.lore.len() == 2 && character.greetings == ["Back again?"]));
        assert!(!view.form.as_ref().unwrap().dirty(), "export saved it first");
    }

    #[test]
    fn the_index_filters_and_stars() {
        let character = |id: &str, name: &str, tags: &[&str], updated| Character {
            id: id.into(),
            name: name.into(),
            description: format!("About {name}."),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            updated,
            ..Character::default()
        };
        let mut view = LibraryView::default();
        let cast = vec![character("a", "Aria", &["Elf", "mage"], 3), character("b", "Bran", &["dwarf"], 2), character("c", "Cole", &["elf"], 1)];
        view.loaded(Vec::new(), cast, Vec::new(), Portraits::at("p".into()));
        view.show(Kind::Character);
        assert_eq!(view.rows(Kind::Character), (vec![0, 1, 2], vec!["Elf".to_owned(), "dwarf".to_owned(), "mage".to_owned()]), "most used tag first, as first written");

        view.filter = Some(Fields::new([Editor::default()], [false]));
        view.filter.as_mut().unwrap().insert("about c");
        assert_eq!(view.rows(Kind::Character).0, [2], "every word, in the description too");
        view.clear_filter();
        view.tags.push("elf".into());
        assert_eq!(view.rows(Kind::Character).0, [0, 2], "tags in any case");

        // A star lists it first and saves it, without moving it otherwise.
        let mut actions = Vec::new();
        view.toggle_favorite(Kind::Character, "c", &mut actions);
        assert!(matches!(&actions[..], [Action::SaveCharacter(c)] if c.id == "c" && c.favorite && c.updated == 1));
        assert_eq!(view.rows(Kind::Character).0, [2, 0]);
        view.favorites = true;
        assert_eq!(view.rows(Kind::Character).0, [2]);

        // Another page starts unfiltered.
        view.show(Kind::World);
        assert!(!view.filtered());
    }

    #[test]
    fn cards_are_imported_and_cast_members_stored() {
        let mut view = LibraryView { importing: true, ..LibraryView::default() };
        let mut actions = Vec::new();
        let card = Character { name: "N".repeat(200), greetings: vec!["Hello.".into()], ..Character::default() };
        view.imported(vec![card], &mut actions);
        assert!(!view.importing);
        let [Action::SaveCharacter(saved)] = &actions[..] else { panic!("not saved") };
        assert!(!saved.id.is_empty() && saved.name.chars().count() == NAME_LIMIT && saved.version == Character::VERSION);
        assert!(view.form.as_ref().is_some_and(|f| f.id == saved.id), "one card opens to review");

        // Storing a cast member keeps the library's greetings and tags.
        let id = saved.id.clone();
        let member = serechat::CastMember { id: id.clone(), name: "Nia".into(), examples: "Hm.".into(), ..serechat::CastMember::default() };
        view.store_character(&member, &mut actions);
        let stored = view.get(Kind::Character, &id).unwrap();
        assert!(stored.name == "Nia" && stored.greetings == ["Hello."] && stored.examples == "Hm.");

        // Several cards stay in the index.
        view.form = None;
        view.imported(vec![Character { name: "A".into(), ..Character::default() }, Character { name: "B".into(), ..Character::default() }], &mut actions);
        assert!(view.form.is_none() && view.characters.len() == 3);
    }

    #[test]
    fn comments_find_records_but_stay_private() {
        let mira = Character { id: "a".into(), name: "Mira".into(), comment: "v2, darker".into(), ..Character::default() };
        let mut view = LibraryView::default();
        view.loaded(Vec::new(), vec![mira, Character { id: "b".into(), name: "Mira".into(), ..Character::default() }], Vec::new(), Portraits::at("p".into()));
        view.show(Kind::Character);
        view.filter = Some(Fields::new([Editor::default()], [false]));
        view.filter.as_mut().unwrap().insert("darker");
        assert_eq!(view.rows(Kind::Character).0, [0]);

        view.open_form(Kind::Character, "a");
        let form = view.form.as_mut().unwrap();
        assert_eq!(form.fields.text(COMMENT), "v2, darker");
        form.fields.replace(COMMENT, "  my secret tag  ");
        let record = form.record().unwrap();
        assert_eq!(record.comment, "my secret tag");
        assert!(!serechat::card_json(&record.character()).contains("secret"), "never in a card");
    }

    #[test]
    fn personas_are_saved_by_name() {
        let mut view = LibraryView::default();
        let starred = Persona { id: "p".into(), name: "Gale".into(), favorite: true, ..Persona::default() };
        view.loaded(Vec::new(), Vec::new(), vec![Persona { id: "q".into(), name: "Ana".into(), ..Persona::default() }, starred], Portraits::at("p".into()));
        assert_eq!(view.default_persona().map(|p| p.id.as_str()), Some("p"), "the starred one");

        // Saving under a name kept already updates that persona, star and all.
        let mut actions = Vec::new();
        view.save_persona(" gale ", "A hunter.", "gale.png", &mut actions);
        assert!(matches!(&actions[..], [Action::SavePersona(p)] if p.id == "p" && p.favorite && p.description == "A hunter." && p.name == "gale"));
        view.save_persona("Rue", "Quick.", "", &mut actions);
        assert_eq!(view.list(Kind::Persona).len(), 3);
        view.save_persona("  ", "Nobody.", "", &mut actions);
        assert_eq!(actions.len(), 2, "a persona needs a name");
        assert!(view.persona_named("RUE").is_some_and(|p| p.description == "Quick."));
    }

    #[test]
    fn generating_a_character() {
        let mut view = LibraryView { form: Some(Form::new(Kind::Character, None)), ..LibraryView::default() };
        assert_eq!(view.form.as_ref().unwrap().idea(), "", "nothing typed: anyone");
        view.form.as_mut().unwrap().fields.insert("Rue");
        assert_eq!(view.form.as_ref().unwrap().idea(), "Their name: Rue");

        // Only the answer to the pending request fills the form.
        view.generating(7);
        view.generated(6, Ok(("Cato".into(), "A career.".into())));
        assert_eq!(view.form.as_ref().unwrap().fields.text(NAME), "Rue", "a stale answer is dropped");
        view.generated(7, Err("No.".into()));
        let form = view.form.as_ref().unwrap();
        assert!(form.generating.is_none() && form.generate_error.as_deref() == Some("No."));
        view.generating(8);
        view.generated(8, Ok(("Rue".into(), "A tribute from District 11.".into())));
        let form = view.form.as_ref().unwrap();
        assert!(form.generate_error.is_none() && form.fields.text(DESCRIPTION) == "A tribute from District 11.");
    }
}
