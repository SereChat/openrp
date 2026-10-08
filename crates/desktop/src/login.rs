//! Sign-in screen for SereChat's browser sign-in: the consent page opens in
//! the system browser, which comes back to the app by itself (see
//! `serechat::SignIn`). A link below it swaps in the form for a custom
//! provider instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arboard::Clipboard;
use serechat::{Client, Error};
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::app::Action;
use crate::paint::{Painter, Rect};
use crate::provider::ProviderForm;
use crate::text::{Align, Style};
use crate::theme;
use crate::ui::{ButtonStyle, Ui, button, logo};

/// A sign-in waiting for the browser. Dropping it stops its listener.
struct Waiting {
    attempt: u64,
    /// The consent page, once the listener is up.
    url: Option<String>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// State of the sign-in screen.
pub struct Login {
    /// The sign-in under way, if any.
    waiting: Option<Waiting>,
    /// Sign-ins started, so the answer to an abandoned one is ignored.
    attempts: u64,
    error: Option<String>,
    /// Informational note, e.g. "link copied".
    notice: Option<String>,
    /// Something that went wrong out of sight (keeping the sign-in in the
    /// keychain), told once the chat opens.
    pub problem: Option<String>,
    /// The custom provider form, shown instead of SereChat sign-in.
    custom: Option<Box<ProviderForm>>,
}

impl Login {
    /// A fresh screen, optionally explaining why the user landed here.
    #[must_use]
    pub fn new(error: Option<String>) -> Self {
        Self { waiting: None, attempts: 0, error, notice: None, problem: None, custom: None }
    }

    /// The screen showing the custom provider form, holding `base_url` and
    /// explaining `error`.
    #[must_use]
    pub fn custom(error: Option<String>, base_url: &str) -> Self {
        let mut form = ProviderForm::new(base_url, false);
        form.error = error;
        Self { custom: Some(Box::new(form)), ..Self::new(None) }
    }

    /// Where the input method's candidate window should appear.
    #[must_use]
    pub fn ime_area(&self) -> Option<Rect> {
        self.custom.as_ref().and_then(|form| form.caret())
    }

    /// Asks to use the custom provider entered, if the form accepts it.
    fn connect(&mut self, actions: &mut Vec<Action>) {
        if let Some((base_url, api_key)) = self.custom.as_mut().and_then(|form| form.submit()) {
            actions.push(Action::ConnectCustom { base_url, api_key });
        }
    }

    /// The listener of sign-in `attempt` is up at `url`. Returns whether it
    /// is the one under way, whose page should now open.
    pub fn opened(&mut self, attempt: u64, url: &str) -> bool {
        let Some(waiting) = self.waiting.as_mut().filter(|w| w.attempt == attempt) else {
            return false;
        };
        waiting.url = Some(url.to_owned());
        true
    }

    /// Sign-in `attempt` finished. Returns the signed-in client, or shows
    /// why there is none.
    pub fn signed_in(&mut self, attempt: u64, result: Result<Client, Error>) -> Option<Client> {
        if self.waiting.as_ref().is_none_or(|w| w.attempt != attempt) {
            return None;
        }
        self.waiting = None;
        result.map_err(|e| self.error = Some(e.to_string())).ok()
    }

    /// Records that the browser could not be opened and the link was copied instead.
    pub fn browser_failed(&mut self) {
        self.notice = Some("Couldn't open your browser. The sign-in link was copied; paste it into a browser.".into());
    }

    fn start(&mut self, actions: &mut Vec<Action>) {
        self.attempts += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.waiting = Some(Waiting { attempt: self.attempts, url: None, cancel: Arc::clone(&cancel) });
        self.error = None;
        self.notice = None;
        actions.push(Action::StartLogin { attempt: self.attempts, cancel });
    }

    /// Text committed by an input method.
    pub fn commit(&mut self, text: &str) {
        if let Some(form) = &mut self.custom {
            form.insert(text);
        }
    }

    /// Keyboard input.
    pub fn key(&mut self, event: &KeyEvent, mods: ModifiersState, cb: &mut Option<Clipboard>, actions: &mut Vec<Action>) {
        if let Some(form) = &mut self.custom {
            if event.logical_key == Key::Named(NamedKey::Escape) {
                self.custom = None;
            } else if form.key(event, mods, cb) {
                self.connect(actions);
            }
            return;
        }
        match (&event.logical_key, &self.waiting) {
            (Key::Named(NamedKey::Enter), None) => self.start(actions),
            (Key::Named(NamedKey::Escape), Some(_)) => self.waiting = None,
            _ => {}
        }
    }

    /// Draws the screen.
    pub fn draw(&mut self, p: &mut Painter, ui: &mut Ui, view: Rect, actions: &mut Vec<Action>) {
        let t = p.theme;
        let width = 400.0;
        let inner = width - 64.0;
        let (title, subtitle) = match (&self.custom, &self.waiting) {
            (Some(_), _) => ("Use a custom provider", "Any OpenAI-compatible API, such as OpenRouter or Inworld. Your key stays on this device."),
            (None, Some(_)) => ("Sign in to OpenRP", "Approve OpenRP in your browser. This window carries on by itself once you have."),
            (None, None) => ("Sign in to OpenRP", "Sign in with your SereChat account to start chatting."),
        };
        let subtitle = p.layout(subtitle, Style { line_height: 1.5, ..theme::SMALL }, Some(inner));
        let message = self.error.as_deref().map(|e| (e, t.danger)).or(self.notice.as_deref().map(|n| (n, t.text_muted)));
        let message = message.map(|(text, color)| (p.layout(text, theme::SMALL, Some(inner)), color));
        let form_h = self.custom.as_ref().map(|form| form.height(p, inner));

        // Logo, title, subtitle, the main button, and the links row under it.
        let mut height = 32.0 + 40.0 + 20.0 + 30.0 + 6.0 + subtitle.height() + 24.0 + 36.0 + 8.0 + 28.0 + 32.0;
        if let Some(form_h) = form_h {
            height += form_h + 24.0;
        } else if let Some((layout, _)) = &message {
            height += layout.height() + 16.0;
        }

        let card = Rect::new(((view.w - width) * 0.5).round(), ((view.h - height) * 0.5).round(), width, height);
        p.shadow(Rect::new(card.x, card.y + 8.0, card.w, card.h), t.shadow, theme::RADIUS, 24.0);
        p.bordered(card, t.panel, theme::RADIUS, 1.0, t.border_strong);

        let x = card.x + 32.0;
        let mut y = card.y + 32.0;
        logo(p, Rect::new(card.x + (card.w - 40.0) * 0.5, y, 40.0, 40.0));
        y += 40.0 + 20.0;
        let title = p.layout(title, theme::TITLE, None);
        p.text_aligned(&title, x, y, Align::Center, inner, t.text);
        y += 30.0 + 6.0;
        p.text_aligned(&subtitle, x, y, Align::Center, inner, t.text_muted);
        y += subtitle.height() + 24.0;

        if let (Some(form), Some(form_h)) = (&mut self.custom, form_h) {
            form.draw(p, ui, x, y, inner);
            y += form_h + 24.0;
            if button(p, ui, Rect::new(x, y, inner, 36.0), "Connect", ButtonStyle::Primary, true) {
                self.connect(actions);
            }
            y += 36.0 + 8.0;
            if button(p, ui, Rect::new(x, y, inner, 28.0), "Sign in with SereChat instead", ButtonStyle::Ghost, true) {
                self.custom = None;
            }
            return;
        }

        let (label, enabled) = if self.waiting.is_some() { ("Waiting for your browser…", false) } else { ("Continue in browser  →", true) };
        if button(p, ui, Rect::new(x, y, inner, 36.0), label, ButtonStyle::Primary, enabled) {
            self.start(actions);
        }
        y += 36.0 + 8.0;

        if let Some(waiting) = &self.waiting {
            let half = (inner - 8.0) * 0.5;
            if button(p, ui, Rect::new(x, y, half, 28.0), "Reopen browser", ButtonStyle::Ghost, waiting.url.is_some())
                && let Some(url) = waiting.url.clone()
            {
                actions.push(Action::OpenAuthPage(url));
            }
            if button(p, ui, Rect::new(x + half + 8.0, y, half, 28.0), "Cancel", ButtonStyle::Ghost, true) {
                self.waiting = None;
            }
        } else if button(p, ui, Rect::new(x, y, inner, 28.0), "Use a custom provider", ButtonStyle::Ghost, true) {
            self.custom = Some(Box::new(ProviderForm::new("", false)));
        }
        y += 28.0;

        if let Some((layout, color)) = &message {
            y += 16.0;
            p.text_aligned(layout, x, y, Align::Center, inner, *color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_sign_in_under_way_counts() {
        let mut login = Login::new(None);
        let mut actions = Vec::new();
        login.start(&mut actions);
        let Some(Action::StartLogin { attempt: first, cancel }) = actions.pop() else { panic!("no sign-in started") };
        login.start(&mut actions);
        assert!(cancel.load(Ordering::Relaxed), "starting over stops the old listener");
        assert!(!login.opened(first, "https://old") && login.signed_in(first, Ok(Client::new())).is_none(), "the old one is ignored");
        assert!(login.opened(first + 1, "https://serechat.com/oauth/authorize"));
        assert!(login.signed_in(first + 1, Err(Error::SignIn("Declined.".into()))).is_none());
        assert_eq!((login.error.as_deref(), login.waiting.is_none()), (Some("Declined."), true));
    }
}
