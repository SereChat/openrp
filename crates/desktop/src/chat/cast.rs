//! Stories: Play starts one in a world, and its cast is the characters
//! taking part. A cast member is present (in the current scene) or absent
//! (part of the story, elsewhere); the strip under the header toggles that,
//! opens each member's menu (store in Characters, edit, delete; see
//! `dialog.rs`) and adds characters: from the library, created or
//! generated. Members are
//! copies: what the library does later never changes a story. The strip
//! also shows who the user plays, and opens the player form, and above
//! the cast, the scene (set by the model, or by clicking it), beside the
//! buttons that open the story's memories and author's note.
//!
//! A story nobody has written in yet opens with a greeting: the first
//! message of the first character in the scene whose library character has
//! one, the others to swipe to (see [`Chat::greet`]).

use serechat::{CastMember, StoredMessage, macros};
use winit::window::CursorIcon;

use super::{Chat, Entry, Load, Menu, MenuItem, Page, tools};
use crate::app::Action;
use crate::library::{Kind, Record, portrait};
use crate::paint::{Painter, Rect, fade, mix};
use crate::theme;
use crate::ui::{ButtonStyle, Ui, button, id};

/// Height of one row of cast chips.
const ROW_H: f32 = 34.0;
/// Height of a chip.
const CHIP_H: f32 = 26.0;
/// Height of the scene row above the chips, always shown.
const SCENE_H: f32 = 26.0;

impl Chat {
    /// Starts a story in `world` and shows it.
    pub(super) fn play(&mut self, world: String) {
        self.new_conversation();
        self.current().world = Some(world);
    }

    /// Starts a story in `world` with the character Play was pressed for
    /// in the scene; their greeting opens it once the user says who they are.
    pub(super) fn play_character_in(&mut self, world: String, actions: &mut Vec<Action>) {
        let id = std::mem::take(&mut self.play_character);
        let Some(member) = self.library.get(Kind::Character, &id).map(cast_member) else { return };
        self.play(world);
        self.change_cast(actions, |cast| cast.push(member));
    }

    /// Opens the story with a greeting, if nobody has written in it yet:
    /// the first message of the first member in the scene whose library
    /// character has one, as their bubble (their other greetings are kept
    /// to swipe to). `{{user}}` and `{{char}}` are filled in.
    pub(super) fn greet(&mut self, actions: &mut Vec<Action>) {
        let id = self.next_id();
        let library = &self.library;
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == self.current) else { return };
        let Some(player) = conversation.player.as_ref().map(|p| p.name.clone()) else { return };
        if !conversation.entries.is_empty() || conversation.world.is_none() || conversation.load != Load::Loaded {
            return;
        }
        let greeter = conversation.cast.iter().filter(|m| m.present).find_map(|m| {
            let greetings = &library.get(Kind::Character, &m.id)?.greetings;
            (!greetings.is_empty()).then(|| (m.name.clone(), greetings.clone()))
        });
        let Some((name, greetings)) = greeter else { return };
        let mut opening: Vec<StoredMessage> = greetings.iter().map(|g| tools::greeting(&name, &macros(g, Some(&name), &player))).collect();
        let mut first = opening.remove(0);
        first.swipes = opening.into_iter().map(|m| vec![m]).collect();
        conversation.entries.push(Entry::new(id, first));
        actions.push(Action::SaveSession(conversation.to_session()));
    }

    /// The system prompt for conversation `id`: its world (as the library
    /// has it now), who the user plays, and its cast.
    pub(super) fn instructions(&self, id: u64) -> String {
        let Some(conversation) = self.conversations.iter().find(|c| c.id == id) else {
            return String::new();
        };
        // A story stays a story even before the library has loaded its world.
        let world = conversation.world.as_deref().map(|w| {
            self.library.get(Kind::World, w).map_or(("this world", "", &[][..]), |w| (w.name.as_str(), w.description.as_str(), w.lore.as_slice()))
        });
        super::stream::story_prompt(world, conversation.player.as_ref(), &conversation.cast)
    }

    /// The story state sent after the latest message of conversation `id`:
    /// its scene, who is in it, the lore its latest turns mention, memories
    /// and author's note. `None` outside a story.
    pub(super) fn state(&self, id: u64) -> Option<String> {
        let conversation = self.conversations.iter().find(|c| c.id == id)?;
        let world = self.library.get(Kind::World, conversation.world.as_deref()?);
        let world_lore = world.map_or(&[][..], |w| w.lore.as_slice());
        let user = conversation.player.as_ref().map_or("the user", |p| p.name.as_str());
        // Only a story with lore reads its latest turns for it.
        let lore = if world_lore.is_empty() && conversation.cast.iter().all(|m| m.lore.is_empty()) {
            Vec::new()
        } else {
            let from = super::stream::lore_start(&conversation.entries);
            let recent: Vec<StoredMessage> = conversation.entries[from..].iter().map(|e| e.message.clone()).collect();
            let text = format!("{}\n{}", conversation.scene, tools::transcript(&recent, user, usize::MAX));
            super::stream::lore_in_play(world_lore, &conversation.cast, &text, user)
        };
        Some(super::stream::story_state(&conversation.cast, &lore, &conversation.memories, &conversation.scene, &conversation.note))
    }

    /// Library characters that can still join the open story, favourites
    /// first.
    pub(super) fn cast_choices(&self) -> Vec<&Record> {
        let Some(conversation) = self.conversations.iter().find(|c| c.id == self.current) else { return Vec::new() };
        let cast = |id: &str| conversation.cast.iter().any(|m| m.id == id);
        let mut choices: Vec<&Record> = self.library.list(Kind::Character).iter().filter(|c| !cast(&c.id)).collect();
        choices.sort_by_key(|c| !c.favorite);
        choices
    }

    /// Rows of the add-to-cast menu.
    pub(super) fn cast_menu() -> Vec<MenuItem> {
        vec![MenuItem::new("From characters…", "Your library"), MenuItem::new("Create character…", ""), MenuItem::new("Generate character…", "AI")]
    }

    /// Applies row `index` of the add-to-cast menu.
    pub(super) fn cast_menu_picked(&mut self, index: usize) {
        match index {
            0 => {
                self.menu = Some(Menu::CastLibrary);
                self.menu_scroll = 0.0;
            }
            1 => self.create_member(),
            _ => self.generate_member(),
        }
    }

    /// Rows of the library menu: the characters that can join, then a way
    /// to the characters page.
    pub(super) fn library_menu(&self) -> Vec<MenuItem> {
        let mut items: Vec<MenuItem> = self.cast_choices().into_iter().map(MenuItem::record).collect();
        let more = if self.library.list(Kind::Character).is_empty() { "Create a character…" } else { "Manage characters…" };
        items.push(MenuItem::new(more, ""));
        items
    }

    /// Applies row `index` of the library menu, which stays open while
    /// there is anyone left to add.
    pub(super) fn library_menu_picked(&mut self, index: usize, actions: &mut Vec<Action>) {
        let picked = self.cast_choices().into_iter().nth(index).map(cast_member);
        match picked {
            Some(member) => {
                self.change_cast(actions, |cast| cast.push(member));
                // The first to join a story nobody wrote in yet may open it.
                self.greet(actions);
                // Stays open to add more; whoever joined leaves the list.
                if !self.cast_choices().is_empty() {
                    self.menu = Some(Menu::CastLibrary);
                }
            }
            None => self.show(Page::Library(Kind::Character)),
        }
    }

    /// Rows of cast member `member`'s menu: storing them updates the library
    /// character they were cast from, if there is one.
    pub(super) fn member_menu(&mut self, member: usize) -> Vec<MenuItem> {
        let id = self.current().cast.get(member).map(|m| m.id.clone()).unwrap_or_default();
        let store = if self.library.get(Kind::Character, &id).is_some() { "Update in Characters" } else { "Store in Characters" };
        vec![MenuItem::new(store, "For other stories"), MenuItem::new("Edit…", ""), MenuItem::new("Delete", "From this story")]
    }

    /// Applies row `index` of cast member `member`'s menu.
    pub(super) fn member_menu_picked(&mut self, member: usize, index: usize, actions: &mut Vec<Action>) {
        match index {
            0 => {
                if let Some(m) = self.current().cast.get(member).cloned() {
                    self.library.store_character(&m, actions);
                }
            }
            1 => self.edit_member(member),
            _ if member < self.current().cast.len() => self.change_cast(actions, |cast| {
                cast.remove(member);
            }),
            _ => {}
        }
    }

    /// Changes the open story's cast, saving it unless nothing was written yet.
    fn change_cast(&mut self, actions: &mut Vec<Action>, change: impl FnOnce(&mut Vec<CastMember>)) {
        let conversation = self.current();
        change(&mut conversation.cast);
        if !conversation.is_fresh() && conversation.load == super::Load::Loaded {
            actions.push(Action::SaveSession(conversation.to_session()));
        }
    }

    /// Draws the cast strip under the header, for a story in a world.
    /// Returns where the strip ends and what anchors the open cast menu:
    /// the add button, or the dots of the member whose menu is open.
    pub(super) fn draw_cast(&mut self, p: &mut Painter, ui: &mut Ui, main: Rect, actions: &mut Vec<Action>) -> (f32, Rect) {
        let top = theme::HEADER_HEIGHT;
        let conversation = self.current();
        if conversation.world.is_none() || conversation.load != super::Load::Loaded {
            return (top, Rect::default());
        }
        let t = p.theme;
        let cast = conversation.cast.clone();
        let player = conversation.player.as_ref().map(|p| (p.name.clone(), p.portrait.clone()));
        let scene = conversation.scene.clone();
        let (memories, has_note) = (conversation.memories.len(), !conversation.note.trim().is_empty());
        let reviewing = conversation.reviewing.is_some();

        // Where the story is, above the cast; clicking it edits it.
        let (left, right) = (main.x + 16.0, main.right() - 16.0);
        let label = p.layout("Cast", theme::CAPTION, None);
        let scene_label = p.layout("Scene", theme::CAPTION, None);
        let row_start = left + label.width().max(scene_label.width()) + 12.0;
        // Right of the scene: what the story remembers, and the author's note.
        let updating = if reviewing { " …" } else { "" };
        let memory_label = if memories == 0 { format!("Memory{updating}") } else { format!("Memory · {memories}{updating}") };
        let note_label = if has_note { "Note ✓" } else { "Note" };
        let widths = [&memory_label as &str, note_label].map(|l| p.layout(l, theme::LABEL, None).width() + 20.0);
        let note_button = Rect::new(right - widths[1], top + 5.0, widths[1], SCENE_H);
        let memory_button = Rect::new(note_button.x - 4.0 - widths[0], top + 5.0, widths[0], SCENE_H);
        let open_memories = button(p, ui, memory_button, &memory_label, ButtonStyle::Ghost, true);
        let open_note = button(p, ui, note_button, note_label, ButtonStyle::Ghost, true);
        let right_of_scene = memory_button.x - 8.0;
        let scene_row = Rect::new(row_start - 6.0, top + 5.0, right_of_scene - row_start + 6.0, SCENE_H);
        let scene_hovered = ui.hovered(scene_row);
        let scene_hover = ui.anim(id("cast-scene"), f32::from(u8::from(scene_hovered)));
        p.rect(scene_row, fade(t.hover, scene_hover), theme::RADIUS_SM);
        p.text(&scene_label, left, scene_row.y + (SCENE_H - scene_label.height()) * 0.5, t.text_faint);
        let (shown, color) = if scene.is_empty() {
            ("Not set yet: the AI sets it as the story starts, or click to set it".to_owned(), t.text_faint)
        } else {
            (scene.split_whitespace().collect::<Vec<_>>().join(" "), mix(t.text_muted, t.text, scene_hover))
        };
        let mut text = p.layout(&shown, theme::SMALL, None);
        text.truncate(p.fonts, scene_row.w - 12.0);
        p.text(&text, row_start, scene_row.y + (SCENE_H - text.height()) * 0.5, color);
        let mut edit_scene = false;
        if scene_hovered {
            ui.cursor = CursorIcon::Pointer;
            edit_scene = ui.clicked(scene_row);
        }

        // Lay the chips out first, wrapping, to know the strip's height.
        let top = top + SCENE_H;
        let mut at_x = row_start;
        let mut at_y = top + (ROW_H - CHIP_H) * 0.5 + 5.0;
        let mut place = |width: f32| {
            if at_x + width > right && at_x > row_start {
                (at_x, at_y) = (row_start, at_y + ROW_H);
            }
            let rect = Rect::new(at_x, at_y, width, CHIP_H);
            at_x += width + 6.0;
            rect
        };
        // Who the user plays comes first, with their portrait if they have
        // one; clicking it edits them.
        let you = player.map(|(name, portrait)| {
            let mut text = p.layout(&format!("You · {name}"), theme::SMALL, None);
            text.truncate(p.fonts, 180.0);
            let path = self.library.portrait_path(&portrait);
            let photo = if path.is_some() { CHIP_H - 4.0 } else { 0.0 };
            (place(text.width() + 22.0 + photo), text, path)
        });
        let mut chips = Vec::with_capacity(cast.len());
        for member in &cast {
            let mut text = p.layout(&member.name, theme::SMALL, None);
            text.truncate(p.fonts, 160.0);
            chips.push((place(text.width() + CHIP_H + 30.0), text, self.library.portrait_path(&member.portrait)));
        }
        let add_w = 74.0;
        let add = place(add_w);
        let bottom = add.y + CHIP_H + (ROW_H - CHIP_H) * 0.5 + 5.0;
        p.rect(Rect::new(main.x, bottom - 1.0, main.w, 1.0), t.border, 0.0);
        p.text(&label, left, top + 5.0 + (ROW_H - label.height()) * 0.5, t.text_faint);

        let mut edit_you = false;
        if let Some((chip, text, path)) = &you {
            let hovered = ui.hovered(*chip);
            let hover = ui.anim(id("cast-you"), f32::from(u8::from(hovered)));
            p.rect(*chip, fade(t.accent, 0.14 + 0.1 * hover), CHIP_H * 0.5);
            let mut text_x = chip.x + 11.0;
            if let Some(path) = path {
                let photo = Rect::new(chip.x + 3.0, chip.y + 3.0, CHIP_H - 6.0, CHIP_H - 6.0);
                portrait(p, Some(path), "", photo, photo.w * 0.5);
                text_x = photo.right() + 7.0;
            }
            p.text(text, text_x, chip.y + (CHIP_H - text.height()) * 0.5, t.text);
            if hovered {
                ui.cursor = CursorIcon::Pointer;
                edit_you = ui.clicked(*chip);
            }
        }

        let (mut toggle, mut open_member) = (None, None);
        let dots_of = |chip: Rect| Rect::new(chip.right() - 22.0, chip.y + 3.0, 19.0, 20.0);
        for (index, ((chip, text, path), member)) in chips.iter().zip(&cast).enumerate() {
            let hovered = ui.hovered(*chip);
            let menu_open = self.menu == Some(Menu::Member(index));
            let hover = ui.anim(id(("cast", &member.id)), f32::from(u8::from(hovered || menu_open)));
            let border = if member.present { mix(fade(t.accent, 0.55), t.accent, hover) } else { mix(t.border, t.border_strong, hover) };
            p.bordered(*chip, mix(t.surface, t.hover, hover), CHIP_H * 0.5, 1.0, border);
            let photo = Rect::new(chip.x + 3.0, chip.y + 3.0, CHIP_H - 6.0, CHIP_H - 6.0);
            portrait(p, path.as_deref(), &member.name, photo, photo.w * 0.5);
            if !member.present {
                // Absent: the face fades into the chip.
                p.rect(photo, fade(t.surface, 0.6), photo.w * 0.5);
            }
            let color = if member.present { t.text } else { t.text_faint };
            p.text(text, photo.right() + 7.0, chip.y + (CHIP_H - text.height()) * 0.5, color);
            // The dots open the member's menu.
            let dots = dots_of(*chip);
            let on_dots = hovered && dots.contains(ui.mouse);
            if hovered || menu_open {
                let color = if on_dots || menu_open { t.text } else { t.text_faint };
                for i in 0..3 {
                    let dot = Rect::new(dots.x + 4.5 + 4.0 * i as f32, dots.y + dots.h * 0.5 - 1.25, 2.5, 2.5);
                    p.rect(dot, color, 1.25);
                }
            }
            if hovered {
                ui.cursor = CursorIcon::Pointer;
                if ui.clicked(*chip) {
                    if on_dots {
                        open_member = Some(index);
                    } else {
                        toggle = Some(index);
                    }
                }
            }
        }
        if cast.is_empty() {
            let hint = p.layout("No one yet: add characters, or just say who you meet and the AI creates them.", theme::SMALL, None);
            p.text(&hint, add.right() + 12.0, add.y + (CHIP_H - hint.height()) * 0.5, t.text_faint);
        } else if chips.iter().any(|(chip, ..)| ui.hovered(*chip)) {
            let hint = p.layout("Click to move in or out of the scene", theme::TINY, None);
            if add.right() + 12.0 + hint.width() < right {
                p.text(&hint, add.right() + 12.0, add.y + (CHIP_H - hint.height()) * 0.5, t.text_faint);
            }
        }

        let open = self.menu == Some(Menu::Cast);
        let hovered = ui.hovered(add);
        let hover = ui.anim(id("cast-add"), f32::from(u8::from(hovered || open)));
        p.bordered(add, fade(t.hover, hover), CHIP_H * 0.5, 1.0, t.border_strong);
        p.label_centered("+ Add", theme::SMALL, add, mix(t.text_muted, t.text, hover));
        if hovered {
            ui.cursor = CursorIcon::Pointer;
            if ui.clicked(add) {
                self.menu = if open { None } else { Some(Menu::Cast) };
                self.menu_scroll = 0.0;
            }
        }

        if edit_scene {
            self.edit_scene();
        } else if open_memories {
            self.edit_memories();
        } else if open_note {
            self.edit_note();
        } else if edit_you {
            self.edit_player();
        } else if let Some(index) = toggle {
            self.change_cast(actions, |cast| cast[index].present = !cast[index].present);
        } else if let Some(index) = open_member {
            self.menu = if self.menu == Some(Menu::Member(index)) { None } else { Some(Menu::Member(index)) };
            self.menu_scroll = 0.0;
        }
        // Chosen after the clicks above, so a menu opened this frame is anchored
        // (and its click counted as inside it) right away.
        let anchor = match self.menu {
            Some(Menu::Member(index)) => chips.get(index).map(|(chip, ..)| dots_of(*chip)),
            _ => None,
        };
        (bottom, anchor.unwrap_or(add))
    }
}

/// A library character as a story's cast member, in the scene: a copy, so
/// the story keeps them whatever the library does later.
fn cast_member(character: &Record) -> CastMember {
    CastMember {
        id: character.id.clone(),
        name: character.name.clone(),
        description: character.description.clone(),
        portrait: character.portrait.clone(),
        examples: character.examples.clone(),
        lore: character.lore.clone(),
        present: true,
        ..CastMember::default()
    }
}

#[cfg(test)]
mod tests {
    use serechat::{Character, Portraits, World};

    use super::*;
    use crate::chat::Reasoning;

    #[test]
    fn play_casts_and_prompts() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        let world = World { id: "w".into(), name: "Panem".into(), description: "Twelve districts.".into(), ..World::default() };
        let katniss = Character { id: "k".into(), name: "Katniss".into(), description: "A hunter.".into(), ..Character::default() };
        let peeta = Character { id: "p".into(), name: "Peeta".into(), description: "A baker.".into(), ..Character::default() };
        chat.library_loaded(vec![world], vec![katniss, peeta], Vec::new(), Portraits::at("portraits".into()));
        assert!(!chat.current().playable(), "nothing to write in before Play");

        chat.play("w".into());
        assert!(chat.page == Page::Chat && !chat.current().playable(), "first: who does the user play");
        chat.current().player = Some(serechat::Player { name: "Gale".into(), description: "A hunter too.".into(), ..serechat::Player::default() });
        assert!(chat.current().playable());
        let mut actions = Vec::new();
        chat.library_menu_picked(0, &mut actions);
        assert!(chat.menu == Some(Menu::CastLibrary), "stays open to add more");
        chat.menu = None;
        chat.library_menu_picked(0, &mut actions);
        assert!(chat.menu.is_none(), "closes once everyone is added");
        assert_eq!(actions.len(), 2, "a begun story is saved with every change");
        assert_eq!(chat.cast_choices().len(), 0, "everyone is cast");
        chat.library_menu_picked(0, &mut actions);
        assert!(chat.page == Page::Library(Kind::Character), "the last row leads to the characters");
        chat.page = Page::Chat;
        chat.change_cast(&mut actions, |cast| cast[1].present = false);

        // The cast holds copies: the library losing Katniss changes nothing.
        let world = World { id: "w".into(), name: "Panem".into(), description: "Twelve districts.".into(), ..World::default() };
        chat.library_loaded(vec![world], Vec::new(), Vec::new(), Portraits::at("portraits".into()));
        let id = chat.current().id;
        let prompt = chat.instructions(id);
        assert!(prompt.contains("# World: Panem\n\nTwelve districts.") && prompt.contains("# The user's character: Gale\n\nA hunter too."));
        assert!(prompt.contains("## Katniss\n\nA hunter.") && prompt.contains("## Peeta\n\nA baker."), "the whole cast, described");
        // Who is where is the story state's.
        let state = chat.state(id).unwrap();
        assert!(state.contains("# In the scene\n\nKatniss") && state.contains("Peeta") && !state.contains("A baker"));

        // Sending saves the story with its world, cast and player.
        actions.clear();
        chat.composer.insert("I step into the arena.");
        chat.send(&mut actions);
        let Some(Action::SaveSession(session)) = actions.first() else { panic!("not saved") };
        assert_eq!(session.world.as_deref(), Some("w"));
        assert_eq!(session.cast.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Katniss", "Peeta"]);
        assert!(session.player.is_some());
        let Some(Action::Send(job)) = actions.last() else { panic!("not sent") };
        assert_eq!(job.instructions, prompt);
        assert!(job.tools, "stories offer the story's tools");
    }

    #[test]
    fn a_characters_greeting_opens_the_story() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        let world = World { id: "w".into(), name: "Ard".into(), ..World::default() };
        let greetings = vec!["Hi, {{user}}.".into(), "Back, {{user}}?".into()];
        let mira = Character { id: "m".into(), name: "Mira".into(), greetings, examples: "Hm.".into(), ..Character::default() };
        chat.library_loaded(vec![world], vec![mira], Vec::new(), Portraits::at("portraits".into()));

        // Play on a character: a story in the world picked, with her in the scene.
        chat.play_character = "m".into();
        let mut actions = Vec::new();
        chat.play_character_in("w".into(), &mut actions);
        assert!(chat.current().cast.iter().any(|m| m.name == "Mira" && m.examples == "Hm." && m.present), "cast with what the prompt needs");
        chat.greet(&mut actions);
        assert!(chat.current().entries.is_empty(), "not before the user says who they are");

        // Once they do, she opens it; her other greeting can be swiped to.
        chat.current().player = Some(serechat::Player { name: "Gale".into(), ..serechat::Player::default() });
        chat.greet(&mut actions);
        let said = |chat: &mut Chat| {
            let entry = chat.current().entries.first_mut().unwrap();
            entry.refresh_display(false, true);
            entry.display.clone()
        };
        assert_eq!(said(&mut chat), "**Mira**: Hi, Gale.");
        assert_eq!(chat.current().greetings(), Some((0, 2)));
        assert!(matches!(actions.last(), Some(Action::SaveSession(s)) if s.messages.len() == 1 && s.messages[0].swipes.len() == 1));
        chat.greet(&mut actions);
        assert_eq!(chat.current().entries.len(), 1, "only once");
        chat.swipe_greeting(1, &mut actions);
        assert_eq!((said(&mut chat), chat.current().greetings()), ("**Mira**: Back, Gale?".to_owned(), Some((1, 2))));
        chat.swipe_greeting(0, &mut actions);
        assert_eq!(said(&mut chat), "**Mira**: Hi, Gale.");

        // The model reads her greeting as her turn; once the user writes, it stays.
        chat.composer.insert("Hello.");
        chat.send(&mut actions);
        assert!(chat.current().greetings().is_none());
        let Some(Action::Send(job)) = actions.pop() else { panic!("not sent") };
        assert!(job.instructions.contains("### How Mira talks"));
        assert!(matches!(&job.input()[1], serechat::InputItem::ToolCall(call) if call.arguments.contains("Hi, Gale.")));
    }

    #[test]
    fn stories_are_exported_once_read() {
        let session = serechat::Session {
            id: "s".into(),
            title: "Night".into(),
            world: Some("w".into()),
            messages: vec![serechat::StoredMessage::new(serechat::Role::User, "Hi.".into())],
            ..serechat::Session::default()
        };
        let mut chat = Chat::new(None, Reasoning::Auto, vec![session.summary()]);
        let world = World { id: "w".into(), name: "Ard".into(), ..World::default() };
        chat.library_loaded(vec![world], Vec::new(), Vec::new(), Portraits::at("portraits".into()));
        let id = chat.conversations.iter().find(|c| c.session_id == "s").unwrap().id;
        let mut actions = Vec::new();
        chat.export(id, true, &mut actions);
        assert!(matches!(&actions[..], [Action::LoadSession { .. }]), "read first");
        let mut actions = Vec::new();
        chat.session_loaded(id, Ok(session), &mut actions);
        assert!(matches!(&actions[..], [Action::ExportStory { session, world, jsonl: true }] if session.messages.len() == 1 && world == "Ard"));
    }

    #[test]
    fn members_are_stored_in_characters() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        chat.play("w".into());
        let rue = CastMember { id: "r".into(), name: "Rue".into(), description: "Small.".into(), portrait: "rue.png".into(), present: true, ..CastMember::default() };
        chat.current().cast.push(rue);
        assert_eq!(chat.member_menu(0)[0].label, "Store in Characters");
        let mut actions = Vec::new();
        chat.member_menu_picked(0, 0, &mut actions);
        assert!(matches!(&actions[..], [Action::SaveCharacter(c)] if c.id == "r" && c.description == "Small." && c.portrait == "rue.png"));
        assert_eq!(chat.member_menu(0)[0].label, "Update in Characters", "now cast from the library");

        // Storing again updates that character; the story itself is untouched.
        chat.current().cast[0].description = "Quick.".into();
        chat.member_menu_picked(0, 0, &mut actions);
        let records = chat.library.list(Kind::Character);
        assert!(records.len() == 1 && records[0].description == "Quick.");
        assert!(actions.iter().all(|a| matches!(a, Action::SaveCharacter(_))));
    }

    #[test]
    fn deleting_a_world_deletes_its_stories() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        for world in ["a", "b"] {
            chat.play(world.into());
            chat.current().player = Some(serechat::Player { name: "Gale".into(), ..serechat::Player::default() });
            chat.composer.insert("Hello.");
            chat.send(&mut Vec::new());
        }
        // A world's page lists its stories.
        let world = |id: &str| serechat::World { id: id.into(), name: id.into(), ..serechat::World::default() };
        chat.library_loaded(vec![world("a"), world("b")], Vec::new(), Vec::new(), Portraits::at("portraits".into()));
        assert!(chat.world_stories().is_empty(), "no world open");
        chat.library.open_form(Kind::World, "a");
        let stories = chat.world_stories();
        assert!(stories.len() == 1 && chat.conversations.iter().any(|c| c.id == stories[0].id && c.world.as_deref() == Some("a")));

        let mut actions = Vec::new();
        chat.delete_world_stories("a", &mut actions);
        assert_eq!(actions.iter().filter(|a| matches!(a, Action::DeleteSession(_))).count(), 1);
        assert!(chat.conversations.iter().all(|c| c.world.as_deref() != Some("a")));
        assert!(chat.conversations.iter().any(|c| c.world.as_deref() == Some("b")));
    }
}
