//! Drop-down menus: model and reasoning (above the composer toolbar),
//! slash commands (above the composer), ways to add to the cast (below the
//! cast strip's add button), what to do with a cast member (below its
//! chip's dots) and with a session (duplicate, export or delete, below its
//! sidebar row's dots), the background model (from the settings page), on
//! the library pages the open form's ⋯ (duplicate, export, delete) and the
//! world a character's Play starts a story in, and the personas to play as
//! (from the player form, when there are more than its chips show).
//!
//! Long lists (characters, worlds, personas, models) are searched: typing
//! while one is open narrows it (names first, then comments and tags), the
//! arrows move through what is left and Enter picks; Esc clears the search,
//! then closes. Deleting from a menu takes a second click on the same row.

use arboard::Clipboard;
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use super::{Chat, Menu, MenuItem, Page, Reasoning, group_digits, price};
use crate::app::Action;
use crate::library::{FormAction, Kind, portrait};
use crate::paint::{Painter, Rect, fade};
use crate::text::TextLayout;
use crate::theme;
use crate::ui::{Ui, edit_key, id};

/// Height of a menu's row.
const ROW_H: f32 = 30.0;
/// Side of a row's portrait.
const FACE: f32 = 22.0;

/// What an open menu shows.
struct Content {
    /// What it opens next to.
    anchor: Rect,
    width: f32,
    /// It opens below its anchor, else above.
    below: bool,
    /// Its title (a searched menu's placeholder) and a note on the right.
    header: (String, String),
    items: Vec<MenuItem>,
    /// Shown while it has no rows.
    empty: String,
}

impl Menu {
    /// Whether it lists what may be many (characters, worlds, personas,
    /// models), searched by typing.
    pub(super) fn searchable(self) -> bool {
        matches!(self, Self::Model | Self::UtilityModel | Self::CastLibrary | Self::PlayIn | Self::Personas)
    }
}

/// The rows of `items` holding every word of `query` (in any case), as
/// indices into it: those whose label does first, then those matching with
/// their detail and search text. All of them, in order, for no query.
fn search(items: &[MenuItem], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return (0..items.len()).collect();
    }
    let holds = |text: &str| {
        let text = text.to_lowercase();
        words.iter().all(|w| text.contains(w.as_str()))
    };
    let (mut named, mut other) = (Vec::new(), Vec::new());
    for (index, item) in items.iter().enumerate() {
        if holds(&item.label) {
            named.push(index);
        } else if holds(&format!("{} {} {}", item.label, item.detail, item.search)) {
            other.push(index);
        }
    }
    named.extend(other);
    named
}

impl Chat {
    /// What `menu` shows now; `anchors` holds the model and reasoning
    /// buttons, the composer card and the cast strip's add button or open
    /// member's dots.
    fn menu_content(&mut self, menu: Menu, anchors: [Rect; 4]) -> Content {
        let confirming = self.confirming == Some(menu);
        // The row that deletes, while it waits for its second click.
        let delete_row = |label: &str, detail: &str| {
            if confirming { MenuItem::new("Click again to delete", "Can't be undone") } else { MenuItem::new(label, detail) }
        };
        let content = |anchor, width, below, header: (&str, &str), items| Content {
            anchor,
            width,
            below,
            header: (header.0.to_owned(), header.1.to_owned()),
            items,
            empty: "Loading…".to_owned(),
        };
        let models = |count: usize| format!("Search {} models", group_digits(count as u64));
        match menu {
            Menu::Model => {
                let items = self
                    .models
                    .iter()
                    .map(|m| MenuItem { selected: m.id == self.model, search: m.id.clone(), ..MenuItem::new(model_label(m), &price(m)) })
                    .collect();
                let mut content = content(anchors[0], 360.0, false, (&models(self.models.len()), "Input / output per 1M"), items);
                if let Some(error) = &self.models_error {
                    content.empty = format!("{error} Trying again soon.");
                }
                content
            }
            Menu::Reasoning => {
                let in_use = self.reasoning_in_use();
                let items = Reasoning::choices(self.selected_model())
                    .into_iter()
                    .map(|r| MenuItem { selected: r == in_use, ..MenuItem::new(r.label(), r.detail()) })
                    .collect();
                content(anchors[1], 280.0, false, ("Reasoning effort", ""), items)
            }
            Menu::Commands => {
                let items = self.commands().into_iter().map(|c| MenuItem::new(&format!("/{}", c.name()), c.detail())).collect();
                content(anchors[2], 320.0, false, ("Commands", "Tab to complete"), items)
            }
            Menu::Cast => content(anchors[3], 240.0, true, ("Add to the cast", ""), Self::cast_menu()),
            Menu::CastLibrary => {
                let items = self.library_menu();
                let placeholder = format!("Search {} characters", group_digits(items.len().saturating_sub(1) as u64));
                content(anchors[3], 320.0, true, (&placeholder, "Click each to add"), items)
            }
            Menu::Member(index) => {
                let title = self.current().cast.get(index).map(|m| m.name.clone()).unwrap_or_default();
                let mut items = self.member_menu(index);
                if let Some(row) = items.get_mut(2) {
                    *row = delete_row("Delete", "From this story");
                }
                content(anchors[3], 260.0, true, (&title, ""), items)
            }
            Menu::Session(_) => {
                let items = vec![
                    MenuItem::new("Duplicate", "Exact copy"),
                    MenuItem::new("Duplicate frame", "Cast and setup only"),
                    MenuItem::new("Export as text", "Readable transcript"),
                    MenuItem::new("Export as JSONL", "SillyTavern chat"),
                    delete_row("Delete", ""),
                ];
                content(self.session_menu, 280.0, true, ("Session", ""), items)
            }
            Menu::Record => {
                let stories = self.world_stories().len();
                let items = self
                    .library
                    .form_menu()
                    .into_iter()
                    .map(|action| match action {
                        FormAction::Duplicate => MenuItem::new("Duplicate", "Saves this first"),
                        FormAction::Export => MenuItem::new("Export card…", "PNG or JSON"),
                        FormAction::Delete => match stories {
                            0 => delete_row("Delete", ""),
                            1 => delete_row("Delete", "With its story"),
                            n => delete_row("Delete", &format!("With its {n} stories")),
                        },
                    })
                    .collect();
                content(self.menu_anchor, 260.0, true, ("Options", ""), items)
            }
            Menu::PlayIn => {
                // Every world, then a way to make one.
                let worlds = self.library.list(Kind::World);
                let placeholder = format!("Search {} worlds", group_digits(worlds.len() as u64));
                let mut items: Vec<MenuItem> = worlds.iter().map(MenuItem::record).collect();
                items.push(MenuItem::new("New world…", ""));
                content(self.menu_anchor, 320.0, true, (&placeholder, "Play in"), items)
            }
            Menu::Personas => {
                let items: Vec<MenuItem> = self.persona_choices().into_iter().map(MenuItem::record).collect();
                let placeholder = format!("Search {} personas", group_digits(items.len() as u64));
                content(self.menu_anchor, 320.0, true, (&placeholder, "Play as"), items)
            }
            Menu::UtilityModel => {
                let same = MenuItem { selected: self.utility_model.is_none(), ..MenuItem::new("Same as the story", "") };
                let items = std::iter::once(same)
                    .chain(self.models.iter().map(|m| MenuItem {
                        selected: self.utility_model.as_deref() == Some(m.id.as_str()),
                        search: m.id.clone(),
                        ..MenuItem::new(model_label(m), &price(m))
                    }))
                    .collect();
                content(self.utility_anchor, 360.0, true, (&models(self.models.len()), "Background model"), items)
            }
        }
    }

    /// Draws the open menu (if any) and applies a choice. `anchors`: see
    /// [`Chat::menu_content`].
    pub(super) fn draw_open_menu(&mut self, p: &mut Painter, ui: &mut Ui, anchors: [Rect; 4], actions: &mut Vec<Action>) {
        let Some(menu) = self.menu else {
            (self.menu_rect, self.confirming, self.menu_seen) = (None, None, None);
            return;
        };
        // A menu opens with nothing searched.
        if self.menu_seen != Some(menu) {
            self.menu_seen = Some(menu);
            self.menu_query.take();
            (self.menu_pick, self.menu_keyed) = (0, false);
        }
        if self.confirming.is_some_and(|m| m != menu) {
            self.confirming = None;
        }
        let content = self.menu_content(menu, anchors);
        let rows = if menu.searchable() { search(&content.items, self.menu_query.text()) } else { (0..content.items.len()).collect() };
        if let Some(row) = self.draw_menu(p, ui, &content, &rows, menu.searchable()) {
            ui.released = false;
            self.choose(menu, rows[row], content.items.len(), actions);
            return;
        }
        let inside = self.menu_rect.is_some_and(|r| r.contains(ui.press_pos)) || content.anchor.contains(ui.press_pos);
        if ui.released && !inside {
            self.close_menu();
        }
    }

    /// Keys for an open searched menu: typing searches, the arrows move,
    /// Enter picks, Esc clears the search and then closes. Returns `false`
    /// for keys it does not use.
    pub(super) fn menu_key(&mut self, event: &KeyEvent, mods: ModifiersState, cb: &mut Option<Clipboard>, actions: &mut Vec<Action>) -> bool {
        let Some(menu) = self.menu.filter(|m| m.searchable()) else { return false };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) if !self.menu_query.text().is_empty() => {
                self.menu_query.take();
                (self.menu_pick, self.menu_keyed, self.menu_scroll) = (0, false, 0.0);
            }
            Key::Named(NamedKey::Escape) => self.close_menu(),
            Key::Named(NamedKey::Enter) => {
                let content = self.menu_content(menu, [Rect::default(); 4]);
                let rows = search(&content.items, self.menu_query.text());
                if let Some(&index) = rows.get(self.menu_pick) {
                    self.choose(menu, index, content.items.len(), actions);
                }
            }
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) => {
                let content = self.menu_content(menu, [Rect::default(); 4]);
                let count = search(&content.items, self.menu_query.text()).len();
                if count > 0 {
                    let step = if *key == NamedKey::ArrowUp { count - 1 } else { 1 };
                    // The first press lands on the first row.
                    self.menu_pick = if self.menu_keyed { (self.menu_pick.min(count - 1) + step) % count } else { 0 };
                    (self.menu_keyed, self.menu_follow) = (true, true);
                }
            }
            _ => {
                let before = self.menu_query.text().to_owned();
                if !edit_key(&mut self.menu_query, event, mods, cb) {
                    return false;
                }
                if self.menu_query.text() != before {
                    (self.menu_pick, self.menu_scroll) = (0, 0.0);
                    self.menu_keyed = !self.menu_query.text().trim().is_empty();
                }
            }
        }
        true
    }

    /// Picks row `index` of `menu`, one of `count` rows (before searching).
    fn choose(&mut self, menu: Menu, index: usize, count: usize, actions: &mut Vec<Action>) {
        // Deleting asks for a second click on the row: always the last.
        let deletes = index + 1 == count && matches!(menu, Menu::Session(_) | Menu::Member(_) | Menu::Record);
        if deletes && self.confirming != Some(menu) {
            self.confirming = Some(menu);
            return;
        }
        self.menu = None;
        self.confirming = None;
        match menu {
            Menu::Model => {
                if let Some(model) = self.models.get(index) {
                    self.model.clone_from(&model.id);
                    actions.push(Action::SelectModel(model.id.clone()));
                }
            }
            Menu::Reasoning => {
                if let Some(choice) = Reasoning::choices(self.selected_model()).get(index) {
                    self.reasoning = *choice;
                    actions.push(Action::SetReasoning(self.reasoning));
                }
            }
            Menu::Commands => {
                if let Some(command) = self.commands().get(index) {
                    self.run_command(*command, actions);
                }
            }
            Menu::Cast => self.cast_menu_picked(index),
            Menu::CastLibrary => self.library_menu_picked(index, actions),
            Menu::Member(member) => self.member_menu_picked(member, index, actions),
            Menu::Session(id) => match index {
                0 | 1 => self.duplicate(id, index == 1, actions),
                2 | 3 => self.export(id, index == 3, actions),
                _ => self.delete_conversation(id, actions),
            },
            Menu::Record => {
                let picked = self.library.form_menu().get(index).copied();
                if let Some(world) = picked.and_then(|action| self.library.form_menu_picked(action, actions)) {
                    self.delete_world_stories(&world, actions);
                }
            }
            Menu::PlayIn => {
                if let Some(world) = self.library.list(Kind::World).get(index).map(|w| w.id.clone()) {
                    self.play_character_in(world, actions);
                } else {
                    self.show(Page::Library(Kind::World));
                    self.library.new_form(Kind::World);
                }
            }
            Menu::Personas => self.play_as(index),
            Menu::UtilityModel => {
                let model = index.checked_sub(1).and_then(|i| self.models.get(i)).map(|m| m.id.clone());
                self.utility_model.clone_from(&model);
                actions.push(Action::SetUtilityModel(model));
            }
        }
    }

    /// Draws `content` as a menu next to its anchor (below it, or above),
    /// showing `rows` of its items, and a search field above them when
    /// `searched`. Returns the clicked row (in `rows`).
    fn draw_menu(&mut self, p: &mut Painter, ui: &mut Ui, content: &Content, rows: &[usize], searched: bool) -> Option<usize> {
        let t = p.theme;
        let (header_h, pad) = (if searched { 38.0 } else { 30.0 }, 4.0);
        let content_h = rows.len().max(1) as f32 * ROW_H;
        let view_h = p.clip().bottom();
        let anchor = content.anchor;
        // Scrolls when the window is too short for every row.
        let room = if content.below { view_h - anchor.bottom() - 16.0 } else { anchor.y - 16.0 };
        let height = (header_h + content_h + 2.0 * pad).min(room.max(header_h + ROW_H * 3.0));
        let y = if content.below { anchor.bottom() + 6.0 } else { anchor.y - 6.0 - height };
        // Kept inside the window when the anchor sits near its right edge.
        let x = anchor.x.min(p.clip().right() - content.width - 8.0).max(8.0);
        let area = Rect::new(x, y, content.width, height);
        self.menu_rect = Some(area);

        p.shadow(Rect::new(area.x, area.y + 6.0, area.w, area.h), t.shadow, theme::RADIUS, 18.0);
        p.bordered(area, t.surface, theme::RADIUS, 1.0, t.border_strong);
        if searched {
            // The search: what is typed while the menu is open lands here.
            let query = self.menu_query.text();
            let note = if query.is_empty() { content.header.1.clone() } else { group_digits(rows.len() as u64) };
            let note = p.layout(&note, theme::CAPTION, None);
            p.text(&note, area.right() - 12.0 - note.width(), area.y + (header_h - note.height()) * 0.5, t.text_faint);
            let field_w = area.w - 36.0 - note.width();
            let (text, color) = if query.is_empty() { (content.header.0.as_str(), t.text_faint) } else { (query, t.text) };
            let mut layout = p.layout(text, theme::SMALL, None);
            let caret_x = if query.is_empty() { 0.0 } else { layout.caret(self.menu_query.cursor()).0 };
            layout.truncate(p.fonts, field_w);
            let origin = (area.x + 12.0, area.y + (header_h - layout.height()) * 0.5);
            p.text(&layout, origin.0, origin.1, color);
            if ui.caret_visible() && caret_x <= field_w {
                p.rect(Rect::new(origin.0 + caret_x - 1.0, origin.1 + 2.0, 2.0, layout.line_height() - 4.0), t.accent, 0.0);
            }
        } else {
            p.label(&content.header.0, theme::CAPTION, area.x + 12.0, area.y + 9.0, t.text_faint);
            let right = p.layout(&content.header.1, theme::CAPTION, None);
            p.text(&right, area.right() - 12.0 - right.width(), area.y + 9.0, t.text_faint);
        }
        p.rect(Rect::new(area.x, area.y + header_h, area.w, 1.0), t.border, 0.0);

        let list = Rect::new(area.x, area.y + header_h + pad, area.w, area.h - header_h - 2.0 * pad);
        if rows.is_empty() {
            let empty = if content.items.is_empty() { content.empty.clone() } else { "Nothing matches.".to_owned() };
            let mut text = p.layout(&empty, theme::SMALL, None);
            text.truncate(p.fonts, list.w - 24.0);
            p.text(&text, list.x + 12.0, list.y + 7.0, t.text_muted);
            return None;
        }
        if ui.hovered(area) {
            self.menu_scroll += ui.scroll;
            ui.scroll = 0.0;
        }
        // The row picked with the keyboard stays in view.
        if std::mem::take(&mut self.menu_follow) {
            let top = self.menu_pick as f32 * ROW_H;
            self.menu_scroll = self.menu_scroll.min(top).max(top + ROW_H - list.h);
        }
        self.menu_scroll = self.menu_scroll.clamp(0.0, (content_h - list.h).max(0.0));

        let faces = content.items.iter().any(|item| item.face);
        let clip = p.push_clip(list);
        let mut chosen = None;
        for (i, &index) in rows.iter().enumerate() {
            let row = Rect::new(area.x + 4.0, list.y + i as f32 * ROW_H - self.menu_scroll, area.w - 8.0, ROW_H);
            if row.bottom() < list.y || row.y > list.bottom() {
                continue;
            }
            let item = &content.items[index];
            let hovered = ui.hovered(row) && list.contains(ui.mouse);
            // The row Enter would pick is highlighted.
            let picked = (self.menu == Some(Menu::Commands) && i == self.command_pick) || (searched && self.menu_keyed && i == self.menu_pick);
            let hover = ui.anim(id(("menu", &content.header.0, i)), f32::from(u8::from(hovered || picked)));
            p.rect(row, fade(t.hover, hover), theme::RADIUS_SM);

            let centre = |layout: &TextLayout| row.y + (row.h - layout.height()) * 0.5;
            // People and worlds show their portrait (or initial); every
            // label in such a list lines up after it.
            let face = Rect::new(row.x + 6.0, row.y + (ROW_H - FACE) * 0.5, FACE, FACE);
            let label_x = if faces { face.right() + 10.0 } else { row.x + 24.0 };
            if item.face {
                portrait(p, self.library.portrait_path(&item.image).as_deref(), &item.label, face, FACE * 0.5);
            }
            if item.selected {
                p.label_centered("✓", theme::SMALL, Rect::new(row.x + 4.0, row.y, 16.0, row.h), t.accent);
            }
            let mut detail = p.layout(&item.detail, theme::SMALL, None);
            detail.truncate(p.fonts, (row.w * 0.5).max(60.0));
            p.text(&detail, row.right() - 10.0 - detail.width(), centre(&detail), t.text_faint);
            let mut label = p.layout(&item.label, theme::SMALL, None);
            label.truncate(p.fonts, row.right() - label_x - 26.0 - detail.width());
            p.text(&label, label_x, centre(&label), t.text);

            if hovered {
                ui.cursor = CursorIcon::Pointer;
                if ui.clicked(row) {
                    chosen = Some(i);
                }
            }
        }
        p.set_clip(clip);
        chosen
    }
}

/// A model's name in a menu: its name, or its id without one.
fn model_label(model: &serechat::Model) -> &str {
    if model.name.is_empty() { &model.id } else { &model.name }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Reasoning;

    #[test]
    fn menus_are_searched_by_name_first() {
        let item = |label: &str, detail: &str, search: &str| MenuItem { search: search.to_owned(), ..MenuItem::new(label, detail) };
        let items = [item("Mira", "barkeep, v2", "fantasy"), item("Rex", "", "clone army"), item("Old Mira", "", ""), item("Kira", "mira's sister", "")];
        assert_eq!(search(&items, ""), [0, 1, 2, 3]);
        assert_eq!(search(&items, "mira"), [0, 2, 3], "names first, then comments");
        assert_eq!(search(&items, "MIRA fantasy"), [0], "every word, across name and tags");
        assert_eq!(search(&items, "army"), [1]);
        assert!(search(&items, "nobody").is_empty());
    }

    #[test]
    fn the_cast_is_picked_from_a_search() {
        let character = |id: &str, name: &str, comment: &str| serechat::Character { id: id.into(), name: name.into(), comment: comment.into(), ..Default::default() };
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        let library = vec![character("a", "Mira", "barkeep v1"), character("b", "Mira", "barkeep v2, darker"), character("c", "Rex", "")];
        chat.library_loaded(Vec::new(), library, Vec::new(), serechat::Portraits::at("p".into()));
        chat.play("w".into());
        chat.current().player = Some(serechat::Player { name: "Gale".into(), ..Default::default() });
        chat.menu = Some(Menu::CastLibrary);
        chat.menu_query.insert("mira DARKER");
        let content = chat.menu_content(Menu::CastLibrary, [Rect::default(); 4]);
        let rows = search(&content.items, chat.menu_query.text());
        assert_eq!(rows.len(), 1);
        assert_eq!(content.items[rows[0]].detail, "barkeep v2, darker", "the comment tells the two apart");
        let mut actions = Vec::new();
        chat.choose(Menu::CastLibrary, rows[0], content.items.len(), &mut actions);
        assert!(chat.current().cast.iter().any(|m| m.id == "b"), "the one searched for joins");
        assert!(chat.menu == Some(Menu::CastLibrary), "and the menu stays open for more");
    }
}
