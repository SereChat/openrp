//! The custom provider form: an OpenAI-compatible API's address and key,
//! with presets for well-known ones. Sign-in and settings both show it.

use arboard::Clipboard;
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::editor::Editor;
use crate::form::{FIELD_PAD, Fields};
use crate::paint::{Painter, Rect};
use crate::text::TextLayout;
use crate::theme;
use crate::ui::{ButtonStyle, Ui, button};

/// Well-known OpenAI-compatible APIs: name and API root.
const PRESETS: [(&str, &str); 4] = [
    ("OpenRouter", "https://openrouter.ai/api/v1"),
    ("Inworld", "https://api.inworld.ai/v1"),
    ("OpenAI", "https://api.openai.com/v1"),
    ("Ollama", "http://localhost:11434/v1"),
];
/// The address field.
const URL: usize = 0;
/// The key field.
const KEY: usize = 1;
/// Height of a preset button.
const PRESET_H: f32 = 26.0;
/// Space a field's label takes above it.
const LABEL_H: f32 = 22.0;
/// Space between the parts of the form.
const GAP: f32 = 14.0;

/// Address and key of a custom provider being entered.
pub struct ProviderForm {
    fields: Fields,
    /// Shown in the empty key field.
    key_hint: &'static str,
    /// Why the last attempt to connect was refused.
    pub error: Option<String>,
}

impl ProviderForm {
    /// A form holding `base_url`; `key_saved` says a key is kept for it, so
    /// the key field may stay empty.
    #[must_use]
    pub fn new(base_url: &str, key_saved: bool) -> Self {
        let mut url = Editor::default();
        url.insert(base_url);
        let key_hint = if key_saved { "Saved. Type a new key to replace it." } else { "Leave empty if the server needs none" };
        Self { fields: Fields::new([url, Editor::default()], [false, false]), key_hint, error: None }
    }

    /// Text committed by an input method.
    pub fn insert(&mut self, text: &str) {
        self.fields.insert(text);
    }

    /// The focused field's caret, for the input method's window.
    #[must_use]
    pub fn caret(&self) -> Option<Rect> {
        self.fields.caret()
    }

    /// Handles a key. Returns `true` when Enter in the last field asks to
    /// connect.
    pub fn key(&mut self, event: &KeyEvent, mods: ModifiersState, cb: &mut Option<Clipboard>) -> bool {
        if self.fields.key(event, mods, cb) {
            self.error = None;
            return false;
        }
        event.logical_key == Key::Named(NamedKey::Enter)
    }

    /// The address and key to connect with (`None`: the field was empty),
    /// or `None` with [`ProviderForm::error`] set when the address is not one.
    pub fn submit(&mut self) -> Option<(String, Option<String>)> {
        let url = self.fields.text(URL).trim().trim_end_matches('/');
        // People paste the endpoint as often as the root.
        let url = url.strip_suffix("/chat/completions").unwrap_or(url);
        let host = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or_default();
        if host.is_empty() || url.contains(char::is_whitespace) {
            self.error = Some("Enter the API's address, starting with https:// (or http:// for a server on this computer).".into());
            return None;
        }
        // Keys pasted with a line break would make an invalid header.
        let key: String = self.fields.text(KEY).split_whitespace().collect();
        Some((url.to_owned(), (!key.is_empty()).then_some(key)))
    }

    /// Height of the form drawn `width` wide.
    #[must_use]
    pub fn height(&self, p: &Painter, width: f32) -> f32 {
        let fields: f32 = self.layouts(p, width).iter().map(|(_, h)| LABEL_H + h + GAP).sum();
        let error = self.error.as_deref().map_or(0.0, |e| p.layout(e, theme::SMALL, Some(width)).height() + GAP);
        PRESET_H + GAP + fields + error - GAP
    }

    /// Draws the form at `(x, y)`, `width` wide.
    pub fn draw(&mut self, p: &mut Painter, ui: &mut Ui, x: f32, mut y: f32, width: f32) {
        let t = p.theme;
        let mut preset_x = x;
        for (name, url) in PRESETS {
            let preset_w = (p.layout(name, theme::LABEL, None).width() + 20.0).round();
            if button(p, ui, Rect::new(preset_x, y, preset_w, PRESET_H), name, ButtonStyle::Secondary, true) {
                self.fields.replace(URL, url);
                // The key is what comes next.
                self.fields.focus(KEY);
                self.error = None;
            }
            preset_x += preset_w + 6.0;
        }
        y += PRESET_H + GAP;
        let [url, key] = self.layouts(p, width);
        for (index, label, hint, (layout, h)) in [(URL, "Base URL", "https://…/v1", url), (KEY, "API key", self.key_hint, key)] {
            p.label(label, theme::LABEL, x, y, t.text);
            y += LABEL_H;
            self.fields.draw(index, p, ui, Rect::new(x, y, width, h), layout, hint, true);
            y += h + GAP;
        }
        if let Some(error) = &self.error {
            let text = p.layout(error, theme::SMALL, Some(width));
            p.text(&text, x, y, t.danger);
        }
    }

    /// Each field's layout and box height for `width`. Both wrap, so a long
    /// key stays inside its box, yet Enter moves on as in one-line fields.
    // ponytail: the key shows as typed; mask it once `Fields` can draw a
    // stand-in layout with mapped offsets.
    fn layouts(&self, p: &Painter, width: f32) -> [(TextLayout, f32); 2] {
        [URL, KEY].map(|index| {
            let layout = p.layout(self.fields.text(index), theme::BODY, Some(width - 2.0 * FIELD_PAD.0));
            let h = layout.height().max(layout.line_height()) + 2.0 * FIELD_PAD.1;
            (layout, h)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_checked_and_tidied() {
        let mut form = ProviderForm::new(" https://openrouter.ai/api/v1/chat/completions/ ", false);
        form.fields.focus(KEY);
        form.insert(" sk-or\n-v1 ");
        assert_eq!(form.submit(), Some(("https://openrouter.ai/api/v1".into(), Some("sk-or-v1".into()))));
        assert_eq!(ProviderForm::new("http://localhost:11434/v1", false).submit(), Some(("http://localhost:11434/v1".into(), None)));
        for bad in ["", "openrouter.ai", "https://", "ftp://x", "https://a b"] {
            let mut form = ProviderForm::new(bad, false);
            assert!(form.submit().is_none() && form.error.is_some(), "{bad}");
        }
    }
}
