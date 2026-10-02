//! The message list: Markdown replies, prompts, the
//! reasoning block, selection and scrolling.

use winit::window::CursorIcon;

use super::stream::MAX_RETRIES;
use serechat::{CastMember, Role};

use super::tools::{self, Part};
use super::{Chat, Entry, Load, PRIMARY_KEY, Page, ReasoningView, SelPos, model_name, turns, usage_caption};
use crate::app::Action;
use crate::doc::{Doc, INK_MUTED, INK_TEXT};
use crate::form::FIELD_PAD;
use crate::library::{Kind, LibraryView, portrait};
use crate::paint::{Painter, Rect, fade, mix};
use crate::text::{Align, TextLayout};
use crate::theme;
use crate::ui::{ButtonStyle, Ui, button, chevron, id, keycap, logo};

/// Vertical space between messages.
const MESSAGE_GAP: f32 = 20.0;
/// Inner padding of boxed messages.
const BOX_PAD: (f32, f32) = (12.0, 10.0);
/// Height of the caption row under a reply.
const META_H: f32 = 30.0;
/// Height of the "Reasoning" toggle above a reply.
const REASONING_ROW: f32 = 30.0;
/// Height of the retry status or Continue button under the messages.
const FOOTER_H: f32 = 44.0;
/// Height of the Save and Cancel row under replies being edited.
const EDIT_BAR: f32 = 46.0;
/// Smallest height of a reply being edited.
const EDIT_MIN_H: f32 = 60.0;
/// Smallest height of a character's bubble being edited.
const BUBBLE_EDIT_MIN_H: f32 = 44.0;
/// Size of a character's portrait beside their bubble.
const AVATAR: f32 = 34.0;
/// Space between a portrait and its bubble.
const AVATAR_GAP: f32 = 12.0;
/// Height of the name row above a bubble.
const NAME_H: f32 = 22.0;
/// Vertical space between the parts of a story reply.
const PART_GAP: f32 = 14.0;

/// Where a laid-out document was drawn, for hit-testing after the frame.
struct Target {
    entry: usize,
    doc: u8,
    origin: (f32, f32),
    rect: Rect,
}

impl Entry {
    /// Waiting for the first words of a live reply.
    fn thinking(&self, live: bool) -> bool {
        live && self.display.is_empty()
    }

    /// Whether the reasoning header row is drawn: while a live reply
    /// thinks, and above a finished reply that reasoned.
    fn reasoning_row(&self, live: bool, view: ReasoningView) -> bool {
        view != ReasoningView::Hidden && (self.thinking(live) || !self.message.reasoning.is_empty())
    }

    /// Whether the entry shows as character bubbles and notes.
    fn bubbles(&self) -> bool {
        !self.parts.is_empty() && !self.boxed() && !self.message.compaction
    }

    /// Lays out stale parts at `width`; returns their height.
    fn measure_parts(&mut self, p: &Painter, width: f32) -> f32 {
        let (bubble_wrap, note_wrap) = (width - AVATAR - AVATAR_GAP - 2.0 * BOX_PAD.0, width - AVATAR - AVATAR_GAP);
        self.part_docs.truncate(self.parts.len());
        let mut height = 0.0;
        for (k, part) in self.parts.iter().enumerate() {
            let wrap = if matches!(part, Part::Note(_)) { note_wrap } else { bubble_wrap };
            if !self.part_docs.get(k).is_some_and(|(built, doc)| built == part && doc.fits(wrap, p.scale)) {
                let previous = self.part_docs.get_mut(k).map(|(_, doc)| std::mem::take(doc));
                let doc = match part {
                    // While a reply streams, only its last bubble grows.
                    Part::Said { text, .. } => Doc::speech(p.fonts, text, wrap, p.scale, previous),
                    Part::Note(note) => Doc::plain(p.fonts, note, theme::SMALL, wrap, p.scale),
                };
                if k < self.part_docs.len() {
                    self.part_docs[k] = (part.clone(), doc);
                } else {
                    self.part_docs.push((part.clone(), doc));
                }
            }
            let gap = if k > 0 { PART_GAP } else { 0.0 };
            height += gap + part_height(part, &self.part_docs[k].1);
        }
        height
    }

    /// Rebuilds stale documents and returns the entry's height at `width`.
    fn measure(&mut self, p: &Painter, width: f32, live: bool, story: bool, view: ReasoningView) -> f32 {
        self.refresh_display(live, story);
        let boxed = self.boxed();
        let summary = self.message.compaction && !boxed;
        let wrap = if boxed {
            width - 2.0 * BOX_PAD.0
        } else if summary {
            width - 14.0
        } else {
            width
        };
        let key = (self.display.len(), boxed);
        let doc_h = if self.bubbles() {
            self.doc = None;
            self.measure_parts(p, width)
        } else {
            self.part_docs.clear();
            if self.doc.as_ref().is_none_or(|d| !d.fits(wrap, p.scale)) || self.doc_key != key {
                let previous = self.doc.take();
                self.doc = Some(if story && self.message.role == Role::User && !self.message.failed {
                    // The user's *actions* are muted, as the characters' are.
                    Doc::speech(p.fonts, &self.display, wrap, p.scale, previous)
                } else if boxed {
                    Doc::plain(p.fonts, &self.display, theme::BODY, wrap, p.scale)
                } else {
                    Doc::markdown(p.fonts, &self.display, wrap, p.scale, INK_TEXT, previous)
                });
                self.doc_key = key;
            }
            self.doc.as_ref().map_or(0.0, |d| d.height)
        };
        if boxed {
            return doc_h + 2.0 * BOX_PAD.1;
        }
        if summary {
            let open = !live && self.reasoning_open == Some(true);
            return REASONING_ROW + if open { doc_h + 12.0 } else { 0.0 };
        }

        let mut height = 0.0;
        let thinking = self.thinking(live);
        if self.reasoning_row(live, view) {
            height += REASONING_ROW;
            if self.reasoning_shown(view) {
                let rwrap = width - 14.0;
                if self.reasoning_doc.as_ref().is_none_or(|d| !d.fits(rwrap, p.scale)) || self.reasoning_len != self.message.reasoning.len() {
                    let previous = self.reasoning_doc.take();
                    self.reasoning_doc = Some(Doc::markdown(p.fonts, &self.message.reasoning, rwrap, p.scale, INK_MUTED, previous));
                    self.reasoning_len = self.message.reasoning.len();
                }
                height += self.reasoning_doc.as_ref().map_or(0.0, |d| d.height) + 12.0;
            }
        } else if thinking {
            // The bare "thinking" dots.
            height += 18.0;
        }
        if !thinking {
            height += doc_h;
        }
        if !live {
            height += META_H;
        }
        height
    }
}

impl Chat {
    pub(super) fn draw_messages(&mut self, p: &mut Painter, ui: &mut Ui, view: Rect, actions: &mut Vec<Action>) {
        let t = p.theme;
        let (x, width) = Self::column(Rect::new(view.x, 0.0, view.w, 0.0));
        let dt = ui.dt;
        let copied = self.copied.filter(|(_, _, at)| ui.time - at < 1.5);
        if ui.pressed && !view.contains(ui.mouse) {
            self.selection = None;
        }
        let selection = self.selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        let current = self.current;
        let reasoning_view = self.reasoning_view;
        let models = &self.models;
        let library = &self.library;
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == current) else {
            return;
        };

        match &conversation.load {
            // Reading a file takes milliseconds; a spinner would only flash.
            Load::Summary | Load::Loading => return,
            Load::Failed(message) => {
                let layout = p.layout(message, theme::SMALL, Some(width));
                p.text_aligned(&layout, x, view.y + view.h * 0.4, Align::Center, width, t.danger);
                return;
            }
            Load::Loaded if conversation.entries.is_empty() => {
                let world = conversation.world.as_deref().map(|id| self.library.get(Kind::World, id));
                let path = world.flatten().and_then(|w| self.library.portrait_path(&w.portrait));
                let story = world.map(|w| (w.map_or("A deleted world", |w| w.name.as_str()), path.as_deref()));
                if draw_empty(p, ui, view, story) {
                    self.show(Page::Library(Kind::World));
                }
                return;
            }
            Load::Loaded => {}
        }
        let live_entry = conversation.stream.as_ref().map(|s| s.entry);
        let story = conversation.world.is_some();
        let started = conversation.stream.as_ref().map(|s| s.started);
        // Under the messages: a retry in progress, or the Continue button.
        let retry = conversation.retry.as_ref().map(|r| format!("Retrying ({} of {MAX_RETRIES}): {}", r.attempt, r.error));
        let resumable = conversation.resumable();
        let footer_h = if retry.is_some() || resumable { FOOTER_H } else { 0.0 };
        let busy = conversation.busy();

        // Each turn's actions sit under its anchor: entry index -> its turn.
        let mut anchors = vec![None; conversation.entries.len()];
        let mut next = 0;
        while next < conversation.entries.len() {
            let turn = turns::turn(&conversation.entries, next);
            anchors[turns::anchor(&conversation.entries, &turn)] = Some(turn.clone());
            next = turn.end.max(next + 1);
        }
        // Replies being edited show a field per character bubble (or one for
        // a reply without characters); the last reply has the buttons.
        let mut edit = self.turn_edit.as_mut().filter(|e| e.conversation == current);
        let slots: Vec<(u64, Option<String>)> =
            edit.as_ref().map(|e| e.slots.iter().map(|s| (s.entry, s.character.clone())).collect()).unwrap_or_default();
        let field_w = |character: &Option<String>| if character.is_some() { width - AVATAR - AVATAR_GAP } else { width };
        let mut edit_layouts: Vec<Option<TextLayout>> = edit
            .as_ref()
            .map(|e| slots.iter().enumerate().map(|(k, (_, character))| Some(e.fields.layout(p, k, field_w(character)))).collect())
            .unwrap_or_default();
        let field_heights: Vec<f32> = edit_layouts
            .iter()
            .zip(&slots)
            .map(|(layout, (_, character))| {
                let min = if character.is_some() { BUBBLE_EDIT_MIN_H } else { EDIT_MIN_H };
                layout.as_ref().map_or(0.0, |l| l.height() + 2.0 * FIELD_PAD.1).max(min)
            })
            .collect();
        let last_edited = slots.last().map(|s| s.0);
        // An edited reply's height: its fields (under names), then the buttons.
        let edit_height = |id: u64| -> Option<f32> {
            let rows = slots.iter().zip(&field_heights).filter(|((entry, _), _)| *entry == id);
            let rows: Vec<f32> = rows.map(|((_, character), field)| field + if character.is_some() { NAME_H } else { 0.0 }).collect();
            let gaps = PART_GAP * rows.len().saturating_sub(1) as f32;
            (!rows.is_empty()).then(|| rows.iter().sum::<f32>() + gaps + if last_edited == Some(id) { EDIT_BAR } else { 0.0 })
        };
        let confirming = self.confirm_turn;
        let editable: Vec<bool> = anchors.iter().map(|t| t.as_ref().is_some_and(|t| turns::has_editable(&conversation.entries, t))).collect();
        // Regen sends the last prompt again: there must be one.
        let regen_ok = conversation.entries.iter().any(|e| e.message.role == Role::User);

        // Measure everything (layouts are cached) to know the scroll range.
        let mut content_h = 24.0 + footer_h;
        for (index, entry) in conversation.entries.iter_mut().enumerate() {
            let live = live_entry == Some(entry.id);
            content_h += entry_height(entry, edit_height(entry.id), anchors[index].is_some(), p, width, live, story, reasoning_view) + MESSAGE_GAP;
        }
        let max_scroll = (content_h - view.h).max(0.0);

        if ui.hovered(view) && ui.scroll != 0.0 {
            self.scroll_target = (self.scroll_target + ui.scroll).clamp(0.0, max_scroll);
            self.stick_to_bottom = self.scroll_target >= max_scroll - 1.0;
        }
        // Dragging a selection past the edges scrolls.
        if self.dragging && ui.down {
            let over = if ui.mouse.1 < view.y {
                ui.mouse.1 - view.y
            } else if ui.mouse.1 > view.bottom() {
                ui.mouse.1 - view.bottom()
            } else {
                0.0
            };
            if over != 0.0 {
                self.scroll_target = (self.scroll_target + over.clamp(-40.0, 40.0) * 0.5).clamp(0.0, max_scroll);
                self.stick_to_bottom = false;
                ui.animating = true;
            }
        }
        // Scrollbar: drag the thumb, or press the track to jump there.
        let thumb_h = (view.h * view.h / content_h).max(32.0);
        let travel = (view.h - thumb_h).max(1.0);
        let track = Rect::new(view.right() - 12.0, view.y, 12.0, view.h);
        if max_scroll > 0.0 && ui.pressed && ui.hovered(track) {
            let thumb_y = view.y + travel * (self.scroll / max_scroll);
            if !(thumb_y..thumb_y + thumb_h).contains(&ui.mouse.1) {
                self.scroll = ((ui.mouse.1 - view.y - thumb_h * 0.5) / travel * max_scroll).clamp(0.0, max_scroll);
            }
            self.bar_drag = Some(self.scroll);
        }
        if let Some(start) = self.bar_drag {
            if !ui.down {
                self.bar_drag = None;
            } else if ui.mouse.1 > f32::MIN {
                // (`f32::MIN` means the pointer left the window: hold still.)
                self.scroll = (start + (ui.mouse.1 - ui.press_pos.1) * max_scroll / travel).clamp(0.0, max_scroll);
                self.scroll_target = self.scroll;
                self.stick_to_bottom = self.scroll >= max_scroll - 1.0;
            }
        }
        if self.stick_to_bottom {
            self.scroll_target = max_scroll;
        }
        self.scroll_target = self.scroll_target.min(max_scroll);
        self.scroll += (self.scroll_target - self.scroll) * (1.0 - (-dt * 18.0).exp());
        if (self.scroll_target - self.scroll).abs() < 0.5 {
            self.scroll = self.scroll_target;
        } else {
            ui.animating = true;
        }

        // Widgets scrolled under the header must not react.
        let in_view = view.contains(ui.mouse) && ui.hovered(view);
        let clip = p.push_clip(view);
        let mut y = view.y + 24.0 - self.scroll.round();
        let mut targets = Vec::new();
        let mut effects = Effects::default();
        let entry_count = conversation.entries.len();
        let mut turn_top = y;
        for (index, entry) in conversation.entries.iter_mut().enumerate() {
            let live = live_entry == Some(entry.id);
            let editing = edit_height(entry.id);
            let anchor = anchors[index].clone();
            let height = entry_height(entry, editing, anchor.is_some(), p, width, live, story, reasoning_view);
            let area = Rect::new(x, y, width, height);
            y += height + MESSAGE_GAP;
            if entry.message.role == Role::User || index == 0 {
                turn_top = area.y;
            }
            if area.bottom() < view.y || area.y > view.bottom() {
                continue;
            }
            // A turn's actions, while it is hovered and nothing is on its
            // way or being edited.
            let turn_hovered = in_view && ui.hovered(Rect::new(x, turn_top, width, area.bottom() - turn_top));
            let actions_for = anchor.filter(|_| !busy && slots.is_empty() && turn_hovered).map(|turn| TurnButtons {
                edit: editable[index],
                regen: turn.end == entry_count && regen_ok,
                confirming: confirming == Some(entry.id),
            });
            effects.confirm_shown |= actions_for.as_ref().is_some_and(|b| b.confirming);
            let sel = |doc: u8, d: &Doc| selected_range(selection, index, doc, d);
            if editing.is_some() {
                let mut top = area.y;
                for (k, (_, character)) in slots.iter().enumerate().filter(|(_, (id, _))| *id == entry.id) {
                    let field = if let Some(name) = character {
                        // Where the bubble was: under the name, beside the portrait.
                        let column = speaker(p, (&conversation.cast, library), name, (x, top), width);
                        Rect::new(column, top + NAME_H, width - AVATAR - AVATAR_GAP, field_heights[k])
                    } else {
                        Rect::new(x, top, width, field_heights[k])
                    };
                    if let (Some(edit), Some(layout)) = (edit.as_deref_mut(), edit_layouts[k].take()) {
                        let hint = if character.is_some() { "Empty: they say nothing in this reply" } else { "Empty: this reply is removed" };
                        edit.fields.draw(k, p, ui, field, layout, hint, in_view);
                    }
                    top = field.bottom() + PART_GAP;
                }
                if last_edited == Some(entry.id) {
                    let bar_y = area.bottom() - EDIT_BAR + 10.0;
                    let save = Rect::new(area.right() - 90.0, bar_y, 90.0, 30.0);
                    effects.save_edit |= button(p, ui, save, "Save", ButtonStyle::Primary, in_view);
                    effects.cancel_edit |= button(p, ui, Rect::new(save.x - 96.0, bar_y, 90.0, 30.0), "Cancel", ButtonStyle::Ghost, in_view);
                    let tip = p.layout(&format!("{PRIMARY_KEY}+Enter to save, Esc to cancel"), theme::TINY, None);
                    p.text(&tip, x, bar_y + (30.0 - tip.height()) * 0.5, t.text_faint);
                }
                continue;
            }
            if entry.boxed() {
                let (fill, border, color) =
                    if entry.message.failed { (fade(t.danger, 0.08), fade(t.danger, 0.4), t.danger) } else { (t.surface, t.border, t.text) };
                let boxed = Rect::new(area.x, area.y, area.w, area.h - if anchors[index].is_some() { META_H } else { 0.0 });
                p.bordered(boxed, fill, theme::RADIUS, 1.0, border);
                let origin = (area.x + BOX_PAD.0, area.y + BOX_PAD.1);
                if let Some(doc) = entry.doc.as_mut().filter(|_| !entry.display.is_empty()) {
                    if let Some((a, b)) = sel(1, doc) {
                        doc.draw_selection(p, origin, a, b);
                    }
                    doc.draw(p, ui, origin, color, in_view, None);
                    targets.push(Target { entry: index, doc: 1, origin, rect: Rect::new(origin.0, origin.1, width, doc.height) });
                }
                if let Some(buttons) = &actions_for {
                    effects.turn =
                        effects.turn.take().or(turn_buttons(p, ui, area.right(), boxed.bottom() + 6.0, buttons).map(|c| (c, index, entry.id)));
                }
                continue;
            }

            let mut top = area.y;
            if entry.message.compaction {
                // A summary replaced the messages above for the model; it
                // opens like a reasoning block.
                let open = !live && entry.reasoning_open == Some(true);
                let label = if live { "Summarising the conversation to free up context" } else { "Summarised the messages above to free up context" };
                let row = Toggle { label, expandable: !live, open, pulse: live, key: id(("summary", entry.id)) };
                if toggle_row(p, ui, (x, top), &row, in_view) {
                    entry.reasoning_open = Some(!open);
                    ui.animating = true;
                }
                if let Some(doc) = entry.doc.as_mut().filter(|_| open) {
                    let origin = (x + 14.0, top + REASONING_ROW);
                    p.rect(Rect::new(x, origin.1, 2.0, doc.height), t.border_strong, 1.0);
                    if let Some((a, b)) = sel(1, doc) {
                        doc.draw_selection(p, origin, a, b);
                    }
                    let event = doc.draw(p, ui, origin, t.text_muted, in_view, None);
                    effects.link = effects.link.take().or(event.open_link);
                    targets.push(Target { entry: index, doc: 1, origin, rect: Rect::new(origin.0, origin.1, width - 14.0, doc.height) });
                }
                continue;
            }
            let thinking = entry.thinking(live);
            if entry.reasoning_row(live, reasoning_view) {
                // "Thinking for 4s" while it thinks, "Thought for 12s" after.
                let open = entry.reasoning_shown(reasoning_view);
                let ms = if thinking {
                    started.map_or(0, |s| u64::try_from(s.elapsed().as_millis()).unwrap_or(u64::MAX))
                } else {
                    entry.message.reasoning_ms
                };
                let label = reasoning_label(thinking, ms);
                let row = Toggle {
                    label: &label,
                    expandable: !entry.message.reasoning.is_empty(),
                    open,
                    pulse: thinking,
                    key: id(("reasoning", entry.id)),
                };
                if toggle_row(p, ui, (x, top), &row, in_view) {
                    entry.reasoning_open = Some(!open);
                    ui.animating = true;
                }
                top += REASONING_ROW;
                if let Some(doc) = entry.reasoning_doc.as_mut().filter(|_| open) {
                    let origin = (x + 14.0, top);
                    p.rect(Rect::new(x, top, 2.0, doc.height), t.border_strong, 1.0);
                    if let Some((a, b)) = sel(0, doc) {
                        doc.draw_selection(p, origin, a, b);
                    }
                    let event = doc.draw(p, ui, origin, t.text_muted, in_view, None);
                    effects.link = effects.link.take().or(event.open_link);
                    targets.push(Target { entry: index, doc: 0, origin, rect: Rect::new(origin.0, origin.1, width - 14.0, doc.height) });
                    top += doc.height + 12.0;
                }
            }

            if thinking {
                if reasoning_view != ReasoningView::Hidden {
                    continue;
                }
                // Bare "thinking" dots when reasoning is hidden.
                for dot in 0..3 {
                    let phase = (ui.time * 5.0 - dot as f32 * 0.7).sin() * 0.5 + 0.5;
                    let dot_rect = Rect::new(x + dot as f32 * 11.0, top + 9.0 - phase * 3.0, 6.0, 6.0);
                    p.rect(dot_rect, fade(t.text_muted, 0.3 + 0.7 * phase), 3.0);
                }
                ui.animating = true;
                continue;
            }
            if entry.bubbles() {
                let column = x + AVATAR + AVATAR_GAP;
                for (k, (part, doc)) in entry.part_docs.iter_mut().enumerate() {
                    let Ok(doc_id) = u8::try_from(k + 2) else { break };
                    if k > 0 {
                        top += PART_GAP;
                    }
                    let height = part_height(part, doc);
                    if top > view.bottom() || top + height < view.y {
                        top += height;
                        continue;
                    }
                    let (origin, color, rect) = match part {
                        Part::Note(_) => {
                            // A change to the story: a dot in the portraits' column.
                            let mid = doc.texts.first().map_or(8.0, |t| t.layout.height() * 0.5);
                            p.rect(Rect::new(x + AVATAR * 0.5 - 2.5, top + mid - 2.5, 5.0, 5.0), t.border_strong, 2.5);
                            ((column, top), t.text_faint, Rect::new(column, top, width - AVATAR - AVATAR_GAP, doc.height))
                        }
                        Part::Said { character, .. } => {
                            speaker(p, (&conversation.cast, library), character, (x, top), width);
                            // Every bubble spans the column, whatever its text.
                            let bubble = Rect::new(column, top + NAME_H, width - AVATAR - AVATAR_GAP, doc.height + 2.0 * BOX_PAD.1);
                            p.bordered(bubble, t.surface, theme::RADIUS, 1.0, t.border);
                            let origin = (bubble.x + BOX_PAD.0, bubble.y + BOX_PAD.1);
                            (origin, t.text, Rect::new(origin.0, origin.1, bubble.w - 2.0 * BOX_PAD.0, doc.height))
                        }
                    };
                    if let Some((a, b)) = sel(doc_id, doc) {
                        doc.draw_selection(p, origin, a, b);
                    }
                    if in_view && ui.hovered(rect) {
                        ui.cursor = CursorIcon::Text;
                    }
                    let event = doc.draw(p, ui, origin, color, in_view, None);
                    effects.link = effects.link.take().or(event.open_link);
                    targets.push(Target { entry: index, doc: doc_id, origin, rect });
                    top += height;
                }
            } else if let Some(doc) = &mut entry.doc {
                let origin = (x, top);
                if let Some((a, b)) = sel(1, doc) {
                    doc.draw_selection(p, origin, a, b);
                }
                if in_view && ui.hovered(Rect::new(x, top, width, doc.height)) {
                    ui.cursor = CursorIcon::Text;
                }
                let code_copied = copied.and_then(|(id, code, _)| (id == entry.id).then_some(code).flatten());
                let event = doc.draw(p, ui, origin, t.text, in_view, code_copied);
                if let Some(code) = event.copy_code.and_then(|i| doc.code(i).map(|c| (i, c.to_owned()))) {
                    effects.copy = Some((entry.id, Some(code.0), code.1));
                }
                effects.link = effects.link.take().or(event.open_link);
                targets.push(Target { entry: index, doc: 1, origin, rect: Rect::new(x, top, width, doc.height) });
                top += doc.height;
            }

            if live {
                continue;
            }
            // Caption and hover actions under a finished reply.
            let meta_y = top + 6.0;
            if let Some(model) = &entry.message.model {
                let text = usage_caption(model_name(models, model), entry.message.usage, entry.message.cost);
                let caption = p.layout(&text, theme::TINY, None);
                p.text(&caption, x, meta_y + (24.0 - caption.height()) * 0.5, t.text_faint);
            }
            let is_copied = copied.is_some_and(|(id, code, _)| id == entry.id && code.is_none());
            let has_copy = !entry.display.is_empty();
            if has_copy && ((in_view && ui.hovered(area)) || is_copied) {
                let label = if is_copied { "✓ Copied" } else { "Copy" };
                if button(p, ui, Rect::new(area.right() - 72.0, meta_y, 72.0, 24.0), label, ButtonStyle::Ghost, true) {
                    effects.copy = Some((entry.id, None, entry.display.clone()));
                }
            }
            // The turn's actions, left of Copy (which keeps its place).
            if let Some(buttons) = &actions_for {
                let right = area.right() - if has_copy { 76.0 } else { 0.0 };
                effects.turn = effects.turn.take().or(turn_buttons(p, ui, right, meta_y, buttons).map(|c| (c, index, entry.id)));
            }
        }
        if let Some(status) = &retry {
            // A slow pulse: the run is waiting, not stuck.
            let pulse = (ui.time * 2.6).sin() * 0.5 + 0.5;
            p.rect(Rect::new(x, y + 12.0, 6.0, 6.0), fade(t.accent, 0.4 + 0.6 * pulse), 3.0);
            let mut text = p.layout(status, theme::SMALL, None);
            text.truncate(p.fonts, width - 16.0);
            p.text(&text, x + 16.0, y + 15.0 - text.height() * 0.5, t.text_muted);
            ui.animating = true;
        } else if resumable {
            let enabled = in_view || !ui.hovered(Rect::new(x, y, 96.0, 30.0));
            if button(p, ui, Rect::new(x, y, 96.0, 30.0), "Continue", ButtonStyle::Primary, enabled) {
                effects.resume = true;
            }
            let mut hint = p.layout("The reply stopped before it finished.", theme::SMALL, None);
            hint.truncate(p.fonts, width - 110.0);
            p.text(&hint, x + 110.0, y + (30.0 - hint.height()) * 0.5, t.text_faint);
        }
        p.set_clip(clip);

        // Selection: press to start, drag to extend, double/triple click for
        // a word/paragraph. Shift+click extends an existing selection.
        let hit = |mouse: (f32, f32)| -> Option<SelPos> {
            let target = targets.iter().min_by(|a, b| {
                let gap = |r: Rect| (r.y - mouse.1).max(mouse.1 - r.bottom()).max(0.0);
                gap(a.rect).total_cmp(&gap(b.rect))
            })?;
            let entry = &conversation.entries[target.entry];
            let doc = entry.doc_by_id(target.doc)?;
            let (piece, byte) = doc.hit(mouse.0 - target.origin.0, mouse.1 - target.origin.1)?;
            Some((target.entry, target.doc, piece, byte))
        };
        let on_text = targets.iter().any(|t| t.rect.contains(ui.mouse));
        if ui.pressed && in_view && ui.cursor != CursorIcon::Pointer && self.bar_drag.is_none() {
            match hit(ui.mouse) {
                Some(pos) if on_text || ui.mods.shift_key() => {
                    let entry = &conversation.entries[pos.0];
                    let doc = entry.doc_by_id(pos.1);
                    self.selection = match (ui.clicks, doc) {
                        (2, Some(doc)) => {
                            let word = doc.word((pos.2, pos.3));
                            Some(((pos.0, pos.1, pos.2, word.start), (pos.0, pos.1, pos.2, word.end)))
                        }
                        (clicks, Some(doc)) if clicks >= 3 => {
                            let len = doc.texts.get(pos.2).map_or(0, |p| p.text.len());
                            Some(((pos.0, pos.1, pos.2, 0), (pos.0, pos.1, pos.2, len)))
                        }
                        _ if ui.mods.shift_key() => self.selection.map(|(anchor, _)| (anchor, pos)).or(Some((pos, pos))),
                        _ => Some((pos, pos)),
                    };
                    self.dragging = ui.clicks == 1;
                }
                _ => self.selection = None,
            }
        }
        if self.dragging {
            if ui.down {
                if let (Some(pos), Some((anchor, _))) = (hit(ui.mouse), self.selection) {
                    self.selection = Some((anchor, pos));
                }
            } else {
                self.dragging = false;
            }
        }

        if let Some((id, code, text)) = effects.copy {
            self.copied = Some((id, code, ui.time));
            actions.push(Action::Copy(text));
        }
        if let Some(url) = effects.link {
            actions.push(Action::OpenLink(url));
        }
        if copied.is_some() {
            ui.animating = true;
        }
        if effects.resume {
            self.resume(current, actions);
        }
        // A Delete awaiting confirmation is dropped once its turn is left.
        if confirming.is_some() && !effects.confirm_shown {
            self.confirm_turn = None;
        }
        match effects.turn {
            Some((TurnClick::Edit, index, _)) => self.edit_turn(index),
            Some((TurnClick::Regen, ..)) => self.regenerate(actions),
            Some((TurnClick::Delete, index, id)) if confirming == Some(id) => {
                self.confirm_turn = None;
                self.delete_turn(index, actions);
            }
            Some((TurnClick::Delete, _, id)) => self.confirm_turn = Some(id),
            None => {}
        }
        if effects.save_edit {
            self.save_turn_edit(actions);
        } else if effects.cancel_edit {
            self.turn_edit = None;
        }

        if max_scroll > 0.0 {
            let thumb_y = view.y + travel * (self.scroll / max_scroll);
            let active = self.bar_drag.is_some() || ui.hovered(track);
            if self.bar_drag.is_some() {
                ui.cursor = CursorIcon::Default;
            }
            let hover = ui.anim(id("scrollbar"), f32::from(u8::from(active)));
            p.rect(Rect::new(view.right() - 9.0, thumb_y, 6.0, thumb_h), fade(t.text, 0.1 + 0.1 * hover), 3.0);
        }
    }
}

/// The reasoning header: `Thinking` (then `Thinking for 4s`) while a reply
/// thinks, `Thought for 12s` once it answered, or `Reasoning` when the time
/// is unknown (replies saved by older versions).
fn reasoning_label(thinking: bool, ms: u64) -> String {
    let secs = (ms + 500) / 1000;
    let time = if secs < 60 { format!("{}s", secs.max(1)) } else { format!("{}m {}s", secs / 60, secs % 60) };
    match (thinking, ms) {
        (true, 0..1000) => "Thinking".to_owned(),
        (true, _) => format!("Thinking for {time}"),
        (false, 0) => "Reasoning".to_owned(),
        (false, _) => format!("Thought for {time}"),
    }
}

/// Things the message loop asks for once drawing is done.
#[derive(Default)]
struct Effects {
    /// Entry id, code block (or whole message) and the text to copy.
    copy: Option<(u64, Option<usize>, String)>,
    link: Option<String>,
    /// The Continue button was clicked.
    resume: bool,
    /// A turn's button: what, an entry of the turn and the anchor's id.
    turn: Option<(TurnClick, usize, u64)>,
    /// The Delete awaiting confirmation is still shown.
    confirm_shown: bool,
    /// The edited replies' Save or Cancel was clicked.
    save_edit: bool,
    cancel_edit: bool,
}

/// What a turn offers under its last reply.
struct TurnButtons {
    /// It has replies to edit.
    edit: bool,
    /// It is the last turn: its prompt can be sent again.
    regen: bool,
    /// Delete was clicked once and awaits confirmation.
    confirming: bool,
}

/// A turn button that was clicked.
#[derive(Clone, Copy)]
enum TurnClick {
    Edit,
    Regen,
    Delete,
}

/// Draws a turn's Edit, Regen and Delete buttons, ending at `right`, on
/// the row at `y`. Returns the one clicked.
fn turn_buttons(p: &mut Painter, ui: &mut Ui, right: f32, y: f32, buttons: &TurnButtons) -> Option<TurnClick> {
    let mut right = right;
    let mut clicked = None;
    let delete = if buttons.confirming { ("Delete?", ButtonStyle::Danger, 72.0) } else { ("Delete", ButtonStyle::Ghost, 64.0) };
    let row = [
        (true, TurnClick::Delete, delete),
        (buttons.regen, TurnClick::Regen, ("Regen", ButtonStyle::Ghost, 60.0)),
        (buttons.edit, TurnClick::Edit, ("Edit", ButtonStyle::Ghost, 52.0)),
    ];
    for (shown, click, (label, style, width)) in row {
        if !shown {
            continue;
        }
        right -= width;
        if button(p, ui, Rect::new(right, y, width, 24.0), label, style, true) {
            clicked = Some(click);
        }
        right -= 4.0;
    }
    clicked
}

/// Draws `name`'s portrait (from the story's cast, with the library that
/// holds the images) at `(x, top)` and their name beside it, in a column
/// `width` wide. Returns where their bubble starts.
fn speaker(p: &mut Painter, (cast, library): (&[CastMember], &LibraryView), name: &str, (x, top): (f32, f32), width: f32) -> f32 {
    let member = cast.iter().find(|m| tools::same(&m.name, name));
    let path = member.and_then(|m| library.portrait_path(&m.portrait));
    portrait(p, path.as_deref(), name, Rect::new(x, top, AVATAR, AVATAR), AVATAR * 0.5);
    let column = x + AVATAR + AVATAR_GAP;
    let mut label = p.layout(name, theme::LABEL, None);
    label.truncate(p.fonts, width - AVATAR - AVATAR_GAP);
    p.text(&label, column, top + (NAME_H - 4.0 - label.height()) * 0.5, p.theme.text);
    column
}

/// Height of a story reply's `part`, laid out as `doc`: a note's text, or
/// a character's name and bubble beside their portrait.
fn part_height(part: &Part, doc: &Doc) -> f32 {
    match part {
        Part::Note(_) => doc.height,
        Part::Said { .. } => (NAME_H + doc.height + 2.0 * BOX_PAD.1).max(AVATAR),
    }
}

/// Height of `entry`: `edit_h` while it is being edited, otherwise its
/// laid-out height, plus the actions row when it is a boxed turn anchor.
#[allow(clippy::too_many_arguments, reason = "layout inputs; a struct would only rename them")]
fn entry_height(entry: &mut Entry, edit_h: Option<f32>, anchor: bool, p: &Painter, width: f32, live: bool, story: bool, view: ReasoningView) -> f32 {
    if let Some(height) = edit_h {
        return height;
    }
    let height = entry.measure(p, width, live, story, view);
    if anchor && entry.boxed() { height + META_H } else { height }
}

/// A row that opens and closes a block, like "Thought for 12s".
struct Toggle<'a> {
    label: &'a str,
    /// Has something to open; otherwise a dot replaces the chevron.
    expandable: bool,
    open: bool,
    /// Breathes while waiting for the model.
    pulse: bool,
    /// Animation key.
    key: u64,
}

/// Draws `row` at `origin`; returns whether it was clicked.
fn toggle_row(p: &mut Painter, ui: &mut Ui, origin: (f32, f32), row: &Toggle<'_>, interactive: bool) -> bool {
    let t = p.theme;
    let label = p.layout(row.label, theme::SMALL, None);
    let toggle = Rect::new(origin.0 - 6.0, origin.1, label.width() + 34.0, 26.0);
    let hovered = interactive && row.expandable && ui.hovered(toggle);
    let hover = ui.anim(row.key, f32::from(u8::from(hovered)));
    p.rect(toggle, fade(t.hover, hover), theme::RADIUS_SM);
    let color = if row.pulse {
        ui.animating = true;
        mix(t.text_faint, t.text, ((ui.time * 2.6).sin() * 0.5 + 0.5) * 0.75)
    } else {
        mix(t.text_muted, t.text, hover)
    };
    if row.expandable {
        let (cx, cy) = if row.open { (toggle.x + 8.0, toggle.y + 11.0) } else { (toggle.x + 10.0, toggle.y + 9.5) };
        chevron(p, cx, cy, row.open, color);
    } else {
        p.rect(Rect::new(toggle.x + 9.0, toggle.y + 10.0, 6.0, 6.0), color, 3.0);
    }
    p.text(&label, toggle.x + 24.0, toggle.y + (26.0 - label.height()) * 0.5, color);
    if hovered {
        ui.cursor = CursorIcon::Pointer;
        return ui.clicked(toggle);
    }
    false
}

/// The part of `doc` (entry `entry`, document `doc_id`) inside the selection.
fn selected_range(selection: Option<(SelPos, SelPos)>, entry: usize, doc_id: u8, doc: &Doc) -> Option<((usize, usize), (usize, usize))> {
    let (a, b) = selection?;
    if a == b || (entry, doc_id) < (a.0, a.1) || (entry, doc_id) > (b.0, b.1) {
        return None;
    }
    let from = if (entry, doc_id) == (a.0, a.1) { (a.2, a.3) } else { (0, 0) };
    let to = if (entry, doc_id) == (b.0, b.1) { (b.2, b.3) } else { doc.end() };
    (from != to).then_some((from, to))
}

/// The welcome state of an empty conversation.
/// The welcome state of an empty conversation: the opening of a story in
/// `story` (the world's name and portrait), or, outside any world, a way to
/// pick one. Returns whether "Choose a world" was clicked.
fn draw_empty(p: &mut Painter, ui: &mut Ui, view: Rect, story: Option<(&str, Option<&str>)>) -> bool {
    let t = p.theme;
    let cx = view.x + view.w * 0.5;
    let top = view.y + (view.h * 0.5 - 190.0).max(24.0);
    let wrap = Some((view.w - 64.0).min(520.0));
    let (title, subtitle, mut y) = if let Some((name, path)) = story {
        portrait(p, path, name, Rect::new(cx - 36.0, top, 72.0, 72.0), theme::RADIUS);
        (name, "Your story begins here. Say where you are and who you meet: the characters answer, and anyone not cast yet is created.", top + 92.0)
    } else {
        logo(p, Rect::new(cx - 18.0, top, 36.0, 36.0));
        ("Pick a world to play", "Stories happen in worlds: create one, or choose one and press Play.", top + 56.0)
    };
    let title = p.layout(title, theme::TITLE, wrap);
    p.text_aligned(&title, cx - title.width() * 0.5, y, Align::Center, title.width(), t.text);
    y += title.height() + 8.0;
    let subtitle = p.layout(subtitle, theme::SMALL, wrap);
    p.text_aligned(&subtitle, cx - subtitle.width() * 0.5, y, Align::Center, subtitle.width(), t.text_muted);
    y += subtitle.height() + 20.0;
    let mut choose = false;
    if story.is_none() {
        choose = button(p, ui, Rect::new(cx - 80.0, y, 160.0, 34.0), "Choose a world", ButtonStyle::Primary, true);
        y += 34.0 + 24.0;
    }

    // Keyboard shortcuts, the way Zed's welcome page lists them.
    let key = |k: &str| format!("{PRIMARY_KEY}+{k}");
    let shortcuts = [
        ("Search everything", key("K")),
        ("Play a world", key("N")),
        ("New line", "Shift+Enter".to_owned()),
        ("Stop reply", "Esc".to_owned()),
        ("Settings", key(",")),
    ];
    let width = 280.0;
    for (label, keys) in shortcuts {
        let text = p.layout(label, theme::SMALL, None);
        p.text(&text, cx - width * 0.5, y + (26.0 - text.height()) * 0.5, t.text_muted);
        keycap(p, &keys, cx + width * 0.5, y + 3.0);
        y += 30.0;
    }
    choose
}

#[cfg(test)]
mod tests {
    use super::reasoning_label;

    #[test]
    fn reasoning_labels() {
        assert_eq!(reasoning_label(true, 400), "Thinking");
        assert_eq!(reasoning_label(true, 4_200), "Thinking for 4s");
        assert_eq!(reasoning_label(false, 0), "Reasoning");
        assert_eq!(reasoning_label(false, 300), "Thought for 1s");
        assert_eq!(reasoning_label(false, 125_000), "Thought for 2m 5s");
    }
}
