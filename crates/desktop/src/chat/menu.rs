//! Drop-down menus: model and reasoning (above the composer toolbar),
//! slash commands (above the composer), ways to add to the cast (below the
//! cast strip's add button), what to do with a cast member (below its
//! chip's dots) and with a session (duplicate or delete, below its sidebar
//! row's dots), and the background model (from the settings page).
//!
//! Deleting from a menu takes a second click on the same row.

use winit::window::CursorIcon;

use super::{Chat, Menu, MenuItem, Reasoning, price};
use crate::app::Action;
use crate::paint::{Painter, Rect, fade};
use crate::text::TextLayout;
use crate::theme;
use crate::ui::{Ui, id};

impl Chat {
    /// Draws the open menu (if any) and applies a choice. `anchors` holds
    /// the model and reasoning buttons, the composer card and the cast
    /// strip's add button or open member's dots.
    pub(super) fn draw_open_menu(&mut self, p: &mut Painter, ui: &mut Ui, anchors: [Rect; 4], actions: &mut Vec<Action>) {
        let Some(menu) = self.menu else {
            self.menu_rect = None;
            self.confirming = None;
            return;
        };
        if self.confirming.is_some_and(|m| m != menu) {
            self.confirming = None;
        }
        let confirming = self.confirming == Some(menu);
        // The row that deletes, while it waits for its second click.
        let delete_row = |label: &'static str| if confirming { ("Click again to delete", "Can't be undone") } else { (label, "") };
        let title;
        let mut empty = "Loading…".to_owned();
        let (anchor, width, below, header, items) = match menu {
            Menu::Model => {
                let items = self
                    .models
                    .iter()
                    .map(|m| MenuItem {
                        label: if m.name.is_empty() { m.id.clone() } else { m.name.clone() },
                        detail: price(m),
                        selected: m.id == self.model,
                    })
                    .collect::<Vec<_>>();
                if let Some(error) = &self.models_error {
                    empty = format!("{error} Trying again soon.");
                }
                (anchors[0], 360.0, false, ("Model", "Input / output per 1M tokens"), items)
            }
            Menu::Reasoning => {
                let in_use = self.reasoning_in_use();
                let items = Reasoning::choices(self.selected_model())
                    .into_iter()
                    .map(|r| MenuItem { label: r.label().to_owned(), detail: r.detail().to_owned(), selected: r == in_use })
                    .collect();
                (anchors[1], 280.0, false, ("Reasoning effort", ""), items)
            }
            Menu::Commands => {
                let items = self
                    .commands()
                    .into_iter()
                    .map(|c| MenuItem { label: format!("/{}", c.name()), detail: c.detail().to_owned(), selected: false })
                    .collect();
                (anchors[2], 320.0, false, ("Commands", "Tab to complete"), items)
            }
            Menu::Cast => (anchors[3], 240.0, true, ("Add to the cast", ""), Self::cast_menu()),
            Menu::CastLibrary => (anchors[3], 280.0, true, ("From characters", "Click each to add"), self.library_menu()),
            Menu::Member(index) => {
                title = self.current().cast.get(index).map(|m| m.name.clone()).unwrap_or_default();
                let mut items = self.member_menu(index);
                if let Some(row) = items.get_mut(2) {
                    let (label, detail) = delete_row("Delete");
                    (row.label, row.detail) = (label.to_owned(), if confirming { detail.to_owned() } else { "From this story".to_owned() });
                }
                (anchors[3], 260.0, true, (title.as_str(), ""), items)
            }
            Menu::Session(_) => {
                let row = |label: &str, detail: &str| MenuItem { label: label.to_owned(), detail: detail.to_owned(), selected: false };
                let (label, detail) = delete_row("Delete");
                let items = vec![row("Duplicate", "Exact copy"), row("Duplicate frame", "Cast and setup only"), row(label, detail)];
                (self.session_menu, 280.0, true, ("Session", ""), items)
            }
            Menu::UtilityModel => {
                let same = MenuItem { label: "Same as the story".to_owned(), detail: String::new(), selected: self.utility_model.is_none() };
                let models = self.models.iter().map(|m| MenuItem {
                    label: if m.name.is_empty() { m.id.clone() } else { m.name.clone() },
                    detail: price(m),
                    selected: self.utility_model.as_deref() == Some(m.id.as_str()),
                });
                (self.utility_anchor, 360.0, true, ("Background model", "Input / output per 1M tokens"), std::iter::once(same).chain(models).collect())
            }
        };
        if let Some(index) = self.draw_menu(p, ui, anchor, width, below, header, &items, &empty) {
            ui.released = false;
            // Deleting asks for a second click on the row.
            let deletes = index == 2 && matches!(menu, Menu::Session(_) | Menu::Member(_));
            if deletes && !confirming {
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
                Menu::Session(id) if index == 2 => self.delete_conversation(id, actions),
                Menu::Session(id) => self.duplicate(id, index == 1, actions),
                Menu::UtilityModel => {
                    let model = index.checked_sub(1).and_then(|i| self.models.get(i)).map(|m| m.id.clone());
                    self.utility_model.clone_from(&model);
                    actions.push(Action::SetUtilityModel(model));
                }
            }
            return;
        }
        let inside = self.menu_rect.is_some_and(|r| r.contains(ui.press_pos)) || anchor.contains(ui.press_pos);
        if ui.released && !inside {
            self.close_menu();
        }
    }

    /// Draws a menu next to `anchor` (below it, or above when `below` is
    /// false), showing `empty` while it has no rows. Returns the clicked row.
    #[allow(clippy::too_many_arguments, reason = "menu geometry and content; a struct would only rename them")]
    fn draw_menu(
        &mut self,
        p: &mut Painter,
        ui: &mut Ui,
        anchor: Rect,
        width: f32,
        below: bool,
        header: (&str, &str),
        items: &[MenuItem],
        empty: &str,
    ) -> Option<usize> {
        let t = p.theme;
        let (row_h, header_h, pad) = (30.0, 30.0, 4.0);
        let content_h = items.len().max(1) as f32 * row_h;
        let view_h = p.clip().bottom();
        // Scrolls when the window is too short for every row.
        let room = if below { view_h - anchor.bottom() - 16.0 } else { anchor.y - 16.0 };
        let height = (header_h + content_h + 2.0 * pad).min(room.max(header_h + row_h * 3.0));
        let y = if below { anchor.bottom() + 6.0 } else { anchor.y - 6.0 - height };
        // Kept inside the window when the anchor sits near its right edge.
        let x = anchor.x.min(p.clip().right() - width - 8.0).max(8.0);
        let area = Rect::new(x, y, width, height);
        self.menu_rect = Some(area);

        p.shadow(Rect::new(area.x, area.y + 6.0, area.w, area.h), t.shadow, theme::RADIUS, 18.0);
        p.bordered(area, t.surface, theme::RADIUS, 1.0, t.border_strong);
        p.label(header.0, theme::CAPTION, area.x + 12.0, area.y + 9.0, t.text_faint);
        let right = p.layout(header.1, theme::CAPTION, None);
        p.text(&right, area.right() - 12.0 - right.width(), area.y + 9.0, t.text_faint);
        p.rect(Rect::new(area.x, area.y + header_h, area.w, 1.0), t.border, 0.0);

        let list = Rect::new(area.x, area.y + header_h + pad, area.w, area.h - header_h - 2.0 * pad);
        if items.is_empty() {
            let mut text = p.layout(empty, theme::SMALL, None);
            text.truncate(p.fonts, list.w - 24.0);
            p.text(&text, list.x + 12.0, list.y + 7.0, t.text_muted);
            return None;
        }
        if ui.hovered(area) {
            self.menu_scroll += ui.scroll;
            ui.scroll = 0.0;
        }
        self.menu_scroll = self.menu_scroll.clamp(0.0, (content_h - list.h).max(0.0));

        let clip = p.push_clip(list);
        let mut chosen = None;
        for (i, item) in items.iter().enumerate() {
            let row = Rect::new(area.x + 4.0, list.y + i as f32 * row_h - self.menu_scroll, area.w - 8.0, row_h);
            if row.bottom() < list.y || row.y > list.bottom() {
                continue;
            }
            let hovered = ui.hovered(row) && list.contains(ui.mouse);
            // The command Enter would run is highlighted.
            let picked = self.menu == Some(Menu::Commands) && i == self.command_pick;
            let hover = ui.anim(id(("menu", header.0, i)), f32::from(u8::from(hovered || picked)));
            p.rect(row, fade(t.hover, hover), theme::RADIUS_SM);

            let centre = |layout: &TextLayout| row.y + (row.h - layout.height()) * 0.5;
            if item.selected {
                p.label_centered("✓", theme::SMALL, Rect::new(row.x + 4.0, row.y, 16.0, row.h), t.accent);
            }
            let mut detail = p.layout(&item.detail, theme::SMALL, None);
            detail.truncate(p.fonts, (row.w * 0.5).max(60.0));
            p.text(&detail, row.right() - 10.0 - detail.width(), centre(&detail), t.text_faint);
            let mut label = p.layout(&item.label, theme::SMALL, None);
            label.truncate(p.fonts, row.w - 50.0 - detail.width());
            p.text(&label, row.x + 24.0, centre(&label), t.text);

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
