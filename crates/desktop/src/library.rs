//! The worlds and characters pages: an index of everything saved, and a
//! form to create or edit one, with its portrait. Worlds are played from
//! here: Play starts a story set in that world, and deleting a world
//! deletes its stories.
//!
//! The app reads the records from `~/.openrp/worlds/` and
//! `~/.openrp/characters/` when the chat screen opens and saves them through
//! its writer thread; this page keeps the copy everything else reads.
//!
//! Portraits are never deleted from here: stories copy them from the
//! library, so a sweep at startup removes the ones nothing uses any more
//! (see `serechat::Portraits::collect_garbage`).

use arboard::Clipboard;
use serechat::{Character, Portraits, World, new_id, unix_now};
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
use crate::ui::{ButtonStyle, Ui, button, id};

/// Widest the page's column gets.
const CONTENT_WIDTH: f32 = 680.0;
/// Height of a record's card in the index.
const CARD_H: f32 = 80.0;
/// Side of a card's portrait.
const CARD_PORTRAIT: f32 = 56.0;
/// Side of the form's portrait.
const FORM_PORTRAIT: f32 = 132.0;
/// Longest name, in chars.
pub const NAME_LIMIT: usize = 80;
/// Smallest height of a description field.
pub const DESCRIPTION_MIN_H: f32 = 220.0;
/// The form's fields.
const NAME: usize = 0;
const DESCRIPTION: usize = 1;

/// Which of the two pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Settings stories take place in.
    World,
    /// People the model plays.
    Character,
}

impl Kind {
    /// The page's name.
    #[must_use]
    pub fn plural(self) -> &'static str {
        match self {
            Self::World => "Worlds",
            Self::Character => "Characters",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Character => "character",
        }
    }

    /// What the page holds, in a sentence.
    fn blurb(self) -> &'static str {
        match self {
            Self::World => "Settings for your stories, like Panem or a galaxy far, far away. Press Play to start one.",
            Self::Character => "People the AI plays, ready to cast in any story. A story keeps its own copy.",
        }
    }

    /// Placeholder of the description field.
    fn hint(self) -> &'static str {
        match self {
            Self::World => "The premise, places, factions, rules and tone of this world…",
            Self::Character => "Who they are: background, personality, appearance and how they speak…",
        }
    }
}

/// What the pages ask the chat screen to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Start a story in this world.
    Play(String),
    /// This world was deleted: its stories go too.
    WorldDeleted(String),
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
    created: u64,
    updated: u64,
}

impl From<World> for Record {
    fn from(w: World) -> Self {
        Self { id: w.id, name: w.name, description: w.description, portrait: w.portrait, created: w.created, updated: w.updated }
    }
}

impl From<Character> for Record {
    fn from(c: Character) -> Self {
        Self { id: c.id, name: c.name, description: c.description, portrait: c.portrait, created: c.created, updated: c.updated }
    }
}

impl Record {
    /// The action that saves this record as a `kind`.
    fn save_action(self, kind: Kind) -> Action {
        let Self { id, name, description, portrait, created, updated } = self;
        match kind {
            Kind::World => Action::SaveWorld(World { id, name, description, portrait, created, updated }),
            Kind::Character => Action::SaveCharacter(Character { id, name, description, portrait, created, updated }),
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

/// Creating or editing one record.
struct Form {
    kind: Kind,
    id: String,
    created: u64,
    /// Not saved yet, so nothing to delete or play.
    new: bool,
    /// Name and description.
    fields: Fields<2>,
    /// Portrait file name; empty for none.
    portrait: String,
    /// The image picker is open.
    picking: bool,
    /// Why the last portrait could not be used.
    error: Option<String>,
    /// Delete was clicked once and waits for a second click.
    confirm_delete: bool,
}

impl Form {
    /// A form for `record`, or for a new one when `None`.
    fn new(kind: Kind, record: Option<Record>) -> Self {
        let new = record.is_none();
        let record = record.unwrap_or_else(|| Record { id: new_id(), created: unix_now(), ..Record::default() });
        Self {
            kind,
            id: record.id,
            created: record.created,
            new,
            fields: Fields::new([name_editor(&record.name), text_editor(&record.description)], [false, true]),
            portrait: record.portrait,
            picking: false,
            error: None,
            confirm_delete: false,
        }
    }

    /// The record the form holds, or `None` while it has no name.
    fn record(&self) -> Option<Record> {
        let name = self.fields.text(NAME).trim();
        (!name.is_empty()).then(|| Record {
            id: self.id.clone(),
            name: name.to_owned(),
            description: self.fields.text(DESCRIPTION).trim().to_owned(),
            portrait: self.portrait.clone(),
            created: self.created,
            updated: unix_now(),
        })
    }
}

/// State of the worlds and characters pages, and the records everything
/// else looks up.
#[derive(Default)]
pub struct LibraryView {
    worlds: Vec<Record>,
    characters: Vec<Record>,
    /// Where portraits are; `None` until loaded or without a home directory.
    portraits: Option<Portraits>,
    /// The records have been read from disk.
    loaded: bool,
    /// The record being created or edited; the index shows otherwise.
    form: Option<Form>,
    scroll: f32,
    /// Content height measured last frame, for clamping the scroll.
    content_h: f32,
}

impl LibraryView {
    /// Shows the index, dropping any unsaved form.
    pub fn show_index(&mut self) {
        self.form = None;
        self.scroll = 0.0;
    }

    /// Stores the records read from disk, most recently edited first.
    pub fn loaded(&mut self, worlds: Vec<World>, characters: Vec<Character>, portraits: Portraits) {
        let sorted = |mut records: Vec<Record>| {
            records.sort_by_key(|r| std::cmp::Reverse(r.updated));
            records
        };
        self.worlds = sorted(worlds.into_iter().map(Record::from).collect());
        self.characters = sorted(characters.into_iter().map(Record::from).collect());
        self.portraits = Some(portraits);
        self.loaded = true;
    }

    /// Every record of `kind`, most recently edited first.
    #[must_use]
    pub fn list(&self, kind: Kind) -> &[Record] {
        match kind {
            Kind::World => &self.worlds,
            Kind::Character => &self.characters,
        }
    }

    /// The record of `kind` with this id, if it exists.
    #[must_use]
    pub fn get(&self, kind: Kind, id: &str) -> Option<&Record> {
        self.list(kind).iter().find(|r| r.id == id)
    }

    fn list_mut(&mut self, kind: Kind) -> &mut Vec<Record> {
        match kind {
            Kind::World => &mut self.worlds,
            Kind::Character => &mut self.characters,
        }
    }

    /// The file a portrait name points at, if it is a valid one.
    #[must_use]
    pub fn portrait_path(&self, portrait: &str) -> Option<String> {
        self.portraits.as_ref()?.path(portrait).map(|p| p.to_string_lossy().into_owned())
    }

    /// Text committed by an input method.
    ///
    /// ponytail: forms show no preedit (text still being composed), only
    /// what is committed; port the composer's preedit drawing if CJK users
    /// miss it.
    pub fn insert(&mut self, text: &str) {
        if let Some(form) = &mut self.form {
            form.fields.insert(text);
        }
    }

    /// Where the input method's candidate window should appear.
    #[must_use]
    pub fn caret(&self) -> Option<Rect> {
        self.form.as_ref().and_then(|f| f.fields.caret())
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

    /// Keyboard input. Returns `false` for keys the page does not use, so
    /// the caller may handle them.
    pub fn key(&mut self, event: &KeyEvent, mods: ModifiersState, cb: &mut Option<Clipboard>, actions: &mut Vec<Action>) -> bool {
        let Some(form) = &mut self.form else {
            return false;
        };
        let primary = if cfg!(target_os = "macos") { mods.super_key() } else { mods.control_key() };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.form = None,
            Key::Character(c) if primary && c.eq_ignore_ascii_case("s") => self.save(actions),
            _ => {
                form.fields.key(event, mods, cb);
            }
        }
        true
    }

    /// Saves the open form and returns to the index; does nothing while
    /// the form has no name.
    fn save(&mut self, actions: &mut Vec<Action>) {
        let Some(form) = self.form.take_if(|f| f.record().is_some()) else {
            return;
        };
        let Some(record) = form.record() else { return };
        let records = self.list_mut(form.kind);
        records.retain(|r| r.id != record.id);
        records.insert(0, record.clone());
        actions.push(record.save_action(form.kind));
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
    pub fn draw(&mut self, p: &mut Painter, ui: &mut Ui, area: Rect, kind: Kind, actions: &mut Vec<Action>) -> Option<Event> {
        if self.form.is_some() { self.draw_form(p, ui, area, actions) } else { self.draw_index(p, ui, area, kind).map(Event::Play) }
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

    /// Every saved record of `kind`, newest first, or an invitation to
    /// create the first. Returns a world to play.
    fn draw_index(&mut self, p: &mut Painter, ui: &mut Ui, area: Rect, kind: Kind) -> Option<String> {
        let t = p.theme;
        let bar = header(p, area, kind.plural(), area.x + 16.0);
        let label = format!("New {}", kind.noun());
        let new = Rect::new(bar.right() - 16.0 - 130.0, bar.y + (bar.h - 30.0) * 0.5, 130.0, 30.0);
        if button(p, ui, new, &label, ButtonStyle::Primary, true) {
            self.form = Some(Form::new(kind, None));
            self.scroll = 0.0;
            return None;
        }
        let (body, x, width) = self.body(ui, area);
        // Reading the files takes milliseconds; a spinner would only flash.
        if !self.loaded {
            return None;
        }
        let interactive = body.contains(ui.mouse);
        let scroll = self.scroll;
        let records = self.list(kind);
        if records.is_empty() {
            let top = body.y + (body.h * 0.5 - 90.0).max(32.0);
            let title = p.layout(&format!("No {} yet", kind.plural().to_lowercase()), theme::TITLE, None);
            p.text_aligned(&title, x, top, Align::Center, width, t.text);
            let blurb = p.layout(kind.blurb(), theme::SMALL, Some(width));
            p.text_aligned(&blurb, x, top + 40.0, Align::Center, width, t.text_muted);
            let create = Rect::new(x + (width - 170.0) * 0.5, top + 64.0 + blurb.height(), 170.0, 34.0);
            if button(p, ui, create, &format!("Create a {}", kind.noun()), ButtonStyle::Secondary, true) {
                self.form = Some(Form::new(kind, None));
            }
            return None;
        }

        let clip = p.push_clip(body);
        let top = body.y + 24.0 - scroll.round();
        p.label(kind.blurb(), theme::SMALL, x, top, t.text_muted);
        let mut y = top + 32.0;
        let (mut opened, mut play) = (None, None);
        for (index, record) in records.iter().enumerate() {
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

            // Play on the right, for worlds.
            let mut right = card.right() - 14.0;
            let mut on_play = false;
            if kind == Kind::World {
                let play_button = Rect::new(right - 72.0, card.y + (CARD_H - 30.0) * 0.5, 72.0, 30.0);
                on_play = play_button.contains(ui.mouse);
                if button(p, ui, play_button, "Play", ButtonStyle::Primary, interactive) {
                    play = Some(record.id.clone());
                }
                right = play_button.x - 14.0;
            }
            let mut name = p.layout(&record.name, theme::LABEL, None);
            name.truncate(p.fonts, right - text_x);
            p.text(&name, text_x, card.y + 17.0, t.text);
            let summary = record.description.lines().find(|l| !l.trim().is_empty()).unwrap_or("No description yet");
            let mut summary = p.layout(summary, theme::SMALL, None);
            summary.truncate(p.fonts, right - text_x);
            p.text(&summary, text_x, card.y + 43.0, t.text_muted);
            if hovered && !on_play {
                ui.cursor = CursorIcon::Pointer;
                if ui.clicked(card) {
                    opened = Some(index);
                }
            }
        }
        p.set_clip(clip);
        self.content_h = y - top + 24.0;
        if let Some(index) = opened {
            let record = self.list(kind)[index].clone();
            self.form = Some(Form::new(kind, Some(record)));
            self.scroll = 0.0;
        }
        play
    }

    /// The open form: portrait, name and description, with save, delete
    /// and (for a saved world) play.
    fn draw_form(&mut self, p: &mut Painter, ui: &mut Ui, area: Rect, actions: &mut Vec<Action>) -> Option<Event> {
        let t = p.theme;
        let (body, x, width) = self.body(ui, area);
        let scroll = self.scroll;
        let portrait_path = self.form.as_ref().and_then(|f| self.portrait_path(&f.portrait));
        let form = self.form.as_mut()?;
        let kind = form.kind;

        // Header: back, title, and delete, play and save on the right.
        let back = Rect::new(area.x + 8.0, area.y + (theme::HEADER_HEIGHT - 30.0) * 0.5, 112.0, 30.0);
        let title = match form.fields.text(NAME).trim() {
            _ if form.new => format!("New {}", kind.noun()),
            "" => "Untitled".to_owned(),
            name => name.to_owned(),
        };
        let bar = header(p, area, &title, back.right() + 12.0);
        let go_back = button(p, ui, back, &format!("← {}", kind.plural()), ButtonStyle::Ghost, true);
        let save = Rect::new(bar.right() - 16.0 - 80.0, back.y, 80.0, 30.0);
        let can_save = !form.fields.text(NAME).trim().is_empty();
        let saved = button(p, ui, save, "Save", ButtonStyle::Primary, can_save);
        let mut right = save.x - 8.0;
        let mut play = false;
        if kind == Kind::World && !form.new {
            let play_button = Rect::new(right - 72.0, back.y, 72.0, 30.0);
            play = button(p, ui, play_button, "Play", ButtonStyle::Secondary, true);
            right = play_button.x - 8.0;
        }
        let mut deleted = false;
        if !form.new {
            // Deleting a world takes its stories along; the label says so.
            let (label, w) = match (form.confirm_delete, kind) {
                (false, _) => ("Delete", 90.0),
                (true, Kind::World) => ("Delete world and its stories", 220.0),
                (true, Kind::Character) => ("Confirm delete", 130.0),
            };
            let delete = Rect::new(right - w, back.y, w, 30.0);
            if button(p, ui, delete, label, ButtonStyle::Danger, true) {
                deleted = form.confirm_delete;
                form.confirm_delete = true;
            } else if ui.released && !delete.contains(ui.press_pos) {
                form.confirm_delete = false;
            }
        }

        let interactive = body.contains(ui.mouse);
        let clip = p.push_clip(body);
        let top = body.y + 32.0 - scroll.round();

        // Portrait on the left, name beside it.
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
        p.label("Name", theme::LABEL, fx, top, t.text);
        let name = form.fields.layout(p, NAME, fw);
        let name_h = name.line_height() + 2.0 * FIELD_PAD.1;
        form.fields.draw(NAME, p, ui, Rect::new(fx, top + 24.0, fw, name_h), name, &format!("Name this {}", kind.noun()), interactive);
        let tip = format!("{} saves  ·  Esc goes back without saving", if cfg!(target_os = "macos") { "Cmd+S" } else { "Ctrl+S" });
        p.label(&tip, theme::TINY, fx, top + 24.0 + name_h + 10.0, t.text_faint);

        let mut y = below.max(top + 24.0 + name_h) + 28.0;
        p.label("Description", theme::LABEL, x, y, t.text);
        y += 24.0;
        let description = form.fields.layout(p, DESCRIPTION, width);
        let description_h = (description.height() + 2.0 * FIELD_PAD.1).max(DESCRIPTION_MIN_H);
        form.fields.draw(DESCRIPTION, p, ui, Rect::new(x, y, width, description_h), description, kind.hint(), interactive);
        y += description_h;
        p.set_clip(clip);
        self.content_h = y - top + 64.0;

        let id = form.id.clone();
        if saved {
            self.save(actions);
        } else if deleted {
            return self.delete(actions).map(Event::WorldDeleted);
        } else if go_back {
            self.form = None;
        } else if play {
            // Play what is shown: unsaved edits are saved first.
            self.save(actions);
            return Some(Event::Play(id));
        }
        None
    }
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

/// The page's title bar over `area`, its title starting at `title_x`.
fn header(p: &mut Painter, area: Rect, title: &str, title_x: f32) -> Rect {
    let t = p.theme;
    let bar = Rect::new(area.x, area.y, area.w, theme::HEADER_HEIGHT);
    p.rect(Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0), t.border, 0.0);
    let mut title = p.layout(title, theme::LABEL, None);
    title.truncate(p.fonts, (bar.w * 0.4).max(60.0));
    p.text(&title, title_x, bar.y + (bar.h - title.height()) * 0.5, t.text);
    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_and_deleting_records() {
        let mut view = LibraryView::default();
        let old = World { id: "a".into(), name: "Old".into(), updated: 1, ..World::default() };
        let new = World { id: "b".into(), name: "New".into(), updated: 2, ..World::default() };
        view.loaded(vec![old, new], Vec::new(), Portraits::at("p".into()));
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
        view.save(&mut actions);
        assert!(view.form.is_none());
        let [Action::SaveWorld(w)] = &actions[..] else { panic!("not saved") };
        assert_eq!((w.id.as_str(), w.description.as_str(), w.portrait.as_str(), w.created), ("a", "Twelve districts.", "face.png", 0));
        assert_eq!(view.worlds[0].id, "a");

        // Deleting a new record only drops the form; a saved world reports
        // itself so its stories go too.
        let mut actions = Vec::new();
        view.form = Some(Form::new(Kind::World, None));
        assert_eq!(view.delete(&mut actions), None);
        assert!(actions.is_empty());
        view.form = Some(Form::new(Kind::World, Some(view.worlds[0].clone())));
        assert_eq!(view.delete(&mut actions).as_deref(), Some("a"));
        assert!(matches!(&actions[..], [Action::DeleteRecord(Kind::World, id)] if id == "a"));
        assert_eq!(view.worlds.len(), 1);

        // A pick landing after the form closed changes nothing.
        view.portrait_picked(Ok(Some("late.png".into())));
        assert!(view.form.is_none());
    }
}
