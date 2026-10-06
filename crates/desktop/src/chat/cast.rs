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

use serechat::CastMember;
use winit::window::CursorIcon;

use super::{Chat, Menu, MenuItem, Page};
use crate::app::Action;
use crate::library::{Kind, portrait};
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

    /// The system prompt for conversation `id`: its world (as the library
    /// has it now), who the user plays, and its cast.
    pub(super) fn instructions(&self, id: u64) -> String {
        let Some(conversation) = self.conversations.iter().find(|c| c.id == id) else {
            return String::new();
        };
        // A story stays a story even before the library has loaded its world.
        let world = conversation
            .world
            .as_deref()
            .map(|w| self.library.get(Kind::World, w).map_or(("this world", ""), |w| (w.name.as_str(), w.description.as_str())));
        let present: Vec<_> = conversation.cast.iter().filter(|m| m.present).map(|m| (m.name.as_str(), m.description.as_str())).collect();
        let absent: Vec<_> = conversation.cast.iter().filter(|m| !m.present).map(|m| m.name.as_str()).collect();
        let player = conversation.player.as_ref().map(|p| (p.name.as_str(), p.description.as_str()));
        let (memories, scene, note) = (&conversation.memories, &conversation.scene, &conversation.note);
        super::stream::story_prompt(world, player, &present, &absent, memories, scene, note)
    }

    /// Library characters that can still join the open story: (id, name).
    pub(super) fn cast_choices(&mut self) -> Vec<(String, String)> {
        let cast: Vec<String> = self.current().cast.iter().map(|m| m.id.clone()).collect();
        self.library.list(Kind::Character).iter().filter(|c| !cast.contains(&c.id)).map(|c| (c.id.clone(), c.name.clone())).collect()
    }

    /// Rows of the add-to-cast menu.
    pub(super) fn cast_menu() -> Vec<MenuItem> {
        let row = |label: &str, detail: &str| MenuItem { label: label.to_owned(), detail: detail.to_owned(), selected: false };
        vec![row("From characters…", "Your library"), row("Create character…", ""), row("Generate character…", "AI")]
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
    pub(super) fn library_menu(&mut self) -> Vec<MenuItem> {
        let mut items: Vec<MenuItem> =
            self.cast_choices().into_iter().map(|(_, name)| MenuItem { label: name, detail: String::new(), selected: false }).collect();
        let more = if self.library.list(Kind::Character).is_empty() { "Create a character…" } else { "Manage characters…" };
        items.push(MenuItem { label: more.to_owned(), detail: String::new(), selected: false });
        items
    }

    /// Applies row `index` of the library menu, which stays open while
    /// there is anyone left to add.
    pub(super) fn library_menu_picked(&mut self, index: usize, actions: &mut Vec<Action>) {
        let picked = self.cast_choices().into_iter().nth(index).and_then(|(id, _)| self.library.get(Kind::Character, &id));
        match picked {
            // A copy: the story keeps it whatever the library does later.
            Some(character) => {
                let member = CastMember {
                    id: character.id.clone(),
                    name: character.name.clone(),
                    description: character.description.clone(),
                    portrait: character.portrait.clone(),
                    present: true,
                };
                self.change_cast(actions, |cast| cast.push(member));
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
        let row = |label: &str, detail: &str| MenuItem { label: label.to_owned(), detail: detail.to_owned(), selected: false };
        let id = self.current().cast.get(member).map(|m| m.id.clone()).unwrap_or_default();
        let store = if self.library.get(Kind::Character, &id).is_some() { "Update in Characters" } else { "Store in Characters" };
        vec![row(store, "For other stories"), row("Edit…", ""), row("Delete", "From this story")]
    }

    /// Applies row `index` of cast member `member`'s menu.
    pub(super) fn member_menu_picked(&mut self, member: usize, index: usize, actions: &mut Vec<Action>) {
        match index {
            0 => {
                if let Some(m) = self.current().cast.get(member).cloned() {
                    self.library.store_character(&m.id, &m.name, &m.description, &m.portrait, actions);
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
        let player = conversation.player.as_ref().map(|p| p.name.clone());
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
        // Who the user plays comes first; clicking it edits them.
        let you = player.map(|name| {
            let mut text = p.layout(&format!("You · {name}"), theme::SMALL, None);
            text.truncate(p.fonts, 180.0);
            (place(text.width() + 22.0), text)
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
        if let Some((chip, text)) = &you {
            let hovered = ui.hovered(*chip);
            let hover = ui.anim(id("cast-you"), f32::from(u8::from(hovered)));
            p.rect(*chip, fade(t.accent, 0.14 + 0.1 * hover), CHIP_H * 0.5);
            p.text(text, chip.x + 11.0, chip.y + (CHIP_H - text.height()) * 0.5, t.text);
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
        chat.library_loaded(vec![world], vec![katniss, peeta], Portraits::at("portraits".into()));
        assert!(!chat.current().playable(), "nothing to write in before Play");

        chat.play("w".into());
        assert!(chat.page == Page::Chat && !chat.current().playable(), "first: who does the user play");
        chat.current().player = Some(serechat::Player { name: "Gale".into(), description: "A hunter too.".into() });
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
        chat.library_loaded(vec![world], Vec::new(), Portraits::at("portraits".into()));
        let id = chat.current().id;
        let prompt = chat.instructions(id);
        assert!(prompt.contains("# World: Panem\n\nTwelve districts.") && prompt.contains("# The user's character: Gale\n\nA hunter too."));
        let (here, away) = prompt.split_once("# Characters elsewhere").unwrap();
        assert!(here.contains("## Katniss\n\nA hunter.") && away.contains("- Peeta") && !away.contains("A baker"), "names only for the absent");

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
    fn members_are_stored_in_characters() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        chat.play("w".into());
        let rue = CastMember { id: "r".into(), name: "Rue".into(), description: "Small.".into(), portrait: "rue.png".into(), present: true };
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
            chat.current().player = Some(serechat::Player { name: "Gale".into(), description: String::new() });
            chat.composer.insert("Hello.");
            chat.send(&mut Vec::new());
        }
        // A world's page lists its stories.
        let world = |id: &str| serechat::World { id: id.into(), name: id.into(), ..serechat::World::default() };
        chat.library_loaded(vec![world("a"), world("b")], Vec::new(), Portraits::at("portraits".into()));
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
