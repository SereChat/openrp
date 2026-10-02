//! The composer: the text field (with input-method preedit), the toolbar
//! with model, reasoning and send, and the slash commands it completes.

use winit::window::CursorIcon;

use super::{Chat, Menu, model_name};
use crate::app::Action;
use crate::paint::{Painter, Rect, fade, mix};
use crate::text::Style;
use crate::theme;
use crate::ui::{Ui, chevron, id};

/// Composer grows up to this many lines, then scrolls.
const MAX_LINES: usize = 8;

/// A command typed as `/name` in the composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    /// Deletes the open chat and starts over.
    Clear,
    /// Starts an out-of-character instruction for the next reply.
    Ooc,
    /// Opens the story's author's note.
    Note,
    /// Opens the story's memories.
    Memory,
    /// Copies the story exactly.
    Duplicate,
    /// Copies the story's setup without what happened.
    Frame,
}

impl Command {
    const ALL: [Self; 6] = [Self::Ooc, Self::Note, Self::Memory, Self::Duplicate, Self::Frame, Self::Clear];

    /// What follows the slash.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Ooc => "ooc",
            Self::Note => "note",
            Self::Memory => "memory",
            Self::Duplicate => "duplicate",
            Self::Frame => "frame",
        }
    }

    /// What it does, shown next to it.
    pub(super) fn detail(self) -> &'static str {
        match self {
            Self::Clear => "Clear this chat's messages",
            Self::Ooc => "Tell the AI something out of character",
            Self::Note => "Author's note for the whole story",
            Self::Memory => "What the story remembers",
            Self::Duplicate => "Copy this story exactly",
            Self::Frame => "New story with this cast and setup",
        }
    }

    /// Commands completing `text`: a slash and part of a name, nothing else.
    pub(super) fn matching(text: &str) -> Vec<Self> {
        let Some(typed) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if typed.contains(char::is_whitespace) {
            return Vec::new();
        }
        Self::ALL.into_iter().filter(|c| c.name().starts_with(typed)).collect()
    }
}

impl Chat {
    /// Draws the composer. Returns its top edge and
    /// the menu anchors: the toolbar's model and reasoning buttons, and the card.
    pub(super) fn draw_composer(&mut self, p: &mut Painter, ui: &mut Ui, main: Rect, actions: &mut Vec<Action>) -> (f32, [Rect; 3]) {
        let t = p.theme;
        let (x, width) = Self::column(main);
        let pad = 12.0;
        let top_pad = 16.0;
        let toolbar_h = 46.0;
        let text_w = width - 2.0 * pad;

        // With an input method composing, show its text at the caret.
        let cursor = self.composer.cursor();
        let (shown, preedit) = match &self.preedit {
            Some((text, _)) => {
                let mut shown = self.composer.text().to_owned();
                shown.insert_str(cursor, text);
                (shown, Some(cursor..cursor + text.len()))
            }
            None => (self.composer.text().to_owned(), None),
        };
        let layout = p.layout(&shown, theme::BODY, Some(text_w));
        let line_h = layout.line_height();
        let visible_h = layout.line_count().min(MAX_LINES) as f32 * line_h;

        let card_h = top_pad + visible_h + toolbar_h;
        let card = Rect::new(x, main.bottom() - 20.0 - card_h, width, card_h);
        let text_area = Rect::new(card.x + pad, card.y + top_pad, text_w, visible_h);

        // Keep the caret inside the visible part of a tall draft.
        let caret_byte = preedit.as_ref().map_or(cursor, |r| {
            let ime_cursor = self.preedit.as_ref().and_then(|(_, c)| *c).map_or(r.end - r.start, |(_, end)| end);
            r.start + ime_cursor
        });
        let (caret_x, caret_y) = layout.caret(caret_byte);
        self.composer_scroll = self.composer_scroll.clamp(caret_y + line_h - visible_h, caret_y).clamp(0.0, (layout.height() - visible_h).max(0.0));
        let origin = (text_area.x, text_area.y - self.composer_scroll);
        self.caret_rect = Some(Rect::new(origin.0 + caret_x, origin.1 + caret_y, 2.0, line_h));

        // Mouse: click to place the caret, drag to select.
        let hit = |ui: &Ui| layout.hit(ui.mouse.0 - origin.0, ui.mouse.1 - origin.1);
        let text_zone = Rect::new(card.x, text_area.y - 4.0, card.w, text_area.h + 8.0);
        if ui.hovered(text_zone) {
            ui.cursor = CursorIcon::Text;
            if ui.pressed && preedit.is_none() {
                self.composer.set_cursor(hit(ui), ui.mods.shift_key());
                if ui.clicks == 2 {
                    self.composer.select_word();
                }
                self.selecting = true;
                ui.last_edit = ui.time;
            }
        }
        if self.selecting {
            if ui.down && ui.clicks < 2 {
                self.composer.set_cursor(hit(ui), true);
            } else if !ui.down {
                self.selecting = false;
            }
        }

        let focus = ui.anim(id("composer-focus"), f32::from(u8::from(ui.focused && self.spotlight.is_none())));
        p.shadow(Rect::new(card.x, card.y + 4.0, card.w, card.h), t.shadow, theme::RADIUS, 14.0);
        p.bordered(card, t.surface, theme::RADIUS, 1.0, mix(t.border_strong, t.border_focus, focus));

        let clip = p.push_clip(Rect::new(text_area.x - 2.0, text_area.y, text_area.w + 4.0, text_area.h));
        let selection = self.composer.selection();
        if !selection.is_empty() && preedit.is_none() {
            // A selected line break shows as a small tail.
            for (x0, x1, y) in layout.selection_spans(selection.start, selection.end, false) {
                p.rect(Rect::new(origin.0 + x0, origin.1 + y, x1 - x0, line_h), t.selection, 2.0);
            }
        }
        if shown.is_empty() {
            p.label("Message OpenRP, or / for commands…", theme::BODY, origin.0, origin.1, t.text_faint);
        } else {
            p.text(&layout, origin.0, origin.1, t.text);
        }
        // The input method's text is underlined until committed.
        if let Some(range) = &preedit {
            for (start, end, y) in layout.line_spans() {
                let (from, to) = (range.start.max(start), range.end.min(end));
                if from < to {
                    let (x0, x1) = (layout.caret(from).0, layout.caret(to).0);
                    p.rect(Rect::new(origin.0 + x0, origin.1 + y + line_h - 5.0, x1 - x0, 1.5), t.accent, 0.0);
                }
            }
        }
        if ui.caret_visible() && (selection.is_empty() || preedit.is_some()) && self.spotlight.is_none() {
            p.rect(Rect::new(origin.0 + caret_x - 1.0, origin.1 + caret_y + 3.0, 2.0, line_h - 6.0), t.accent, 0.0);
        }
        p.set_clip(clip);

        // Toolbar: model and reasoning on the left; send/stop on the right.
        let item_y = card.bottom() - 10.0 - 26.0;
        let name = model_name(&self.models, &self.model).to_owned();
        let model = self.toolbar_button(p, ui, card.x + 6.0, item_y, &name, Menu::Model);
        let reasoning = format!("Reasoning: {}", self.reasoning_in_use().label());
        let reasoning = self.toolbar_button(p, ui, model.right() + 4.0, item_y, &reasoning, Menu::Reasoning);

        let busy = self.current().busy();
        let ready = !self.composer.text().trim().is_empty();
        let send = Rect::new(card.right() - 8.0 - 26.0, item_y, 26.0, 26.0);
        let hovered = ui.hovered(send) && (busy || ready);
        let hover = ui.anim(id("send"), f32::from(u8::from(hovered)));
        if busy {
            p.rect(send, mix(t.hover, t.active, hover), theme::RADIUS_SM);
            p.rect(Rect::new(send.x + 9.0, send.y + 9.0, 8.0, 8.0), t.text, 1.5);
        } else if ready {
            p.rect(send, fade(t.accent, 1.0 - 0.14 * hover), theme::RADIUS_SM);
            p.label_centered("↑", Style::semibold(15.0), send, t.on_accent);
        } else {
            p.rect(send, t.hover, theme::RADIUS_SM);
            p.label_centered("↑", Style::semibold(15.0), send, t.text_faint);
        }
        if hovered {
            ui.cursor = CursorIcon::Pointer;
            if ui.clicked(send) {
                if busy {
                    self.stop(actions);
                } else {
                    self.send(actions);
                }
            }
        }

        self.composer_layout = if preedit.is_none() { Some((layout, origin)) } else { None };
        (card.y, [model, reasoning, card])
    }

    /// A ghost button with a chevron that toggles `menu`. Returns its rect.
    fn toolbar_button(&mut self, p: &mut Painter, ui: &mut Ui, x: f32, y: f32, label: &str, menu: Menu) -> Rect {
        let t = p.theme;
        let text = p.layout(label, theme::SMALL, None);
        let rect = Rect::new(x, y, text.width() + 34.0, 26.0);
        let open = self.menu == Some(menu);
        let hovered = ui.hovered(rect);
        let hover = ui.anim(id(("toolbar", menu == Menu::Model)), f32::from(u8::from(hovered || open)));
        p.rect(rect, fade(t.hover, hover), theme::RADIUS_SM);
        let color = mix(t.text_muted, t.text, hover);
        p.text(&text, rect.x + 8.0, y + (26.0 - text.height()) * 0.5, color);
        chevron(p, rect.right() - 17.0, y + 11.0, true, color);
        if hovered {
            ui.cursor = CursorIcon::Pointer;
            if ui.clicked(rect) {
                self.menu = if open { None } else { Some(menu) };
                self.menu_scroll = 0.0;
            }
        }
        rect
    }
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn commands_complete_a_bare_slash_word() {
        assert_eq!(Command::matching("/").len(), Command::ALL.len());
        assert_eq!(Command::matching("/d"), [Command::Duplicate]);
        assert_eq!(Command::matching("/cl"), [Command::Clear]);
        assert_eq!(Command::matching("/clear"), [Command::Clear]);
        assert!(Command::matching("/clearer").is_empty());
        assert!(Command::matching("/clear it").is_empty(), "text after a command makes it a message");
        assert!(Command::matching("/etc/hosts\n").is_empty());
        assert!(Command::matching("clear").is_empty());
        assert!(Command::matching("").is_empty());
    }
}
