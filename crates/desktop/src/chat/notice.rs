//! Notices: problems that happened out of sight (a story that could not
//! be saved or read), shown as small cards under the header until closed,
//! or for [`SHOWN_FOR`]. A notice about files offers the data folder. A file
//! exported where the user chose is told the same way, without alarm, with
//! a way to its folder.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::Chat;
use crate::app::Action;
use crate::paint::{Painter, Rect, fade};
use crate::theme;
use crate::ui::{ButtonStyle, Ui, button};

/// How long a notice stays unless closed.
const SHOWN_FOR: Duration = Duration::from_secs(15);
/// Widest a notice gets.
const WIDTH: f32 = 460.0;
/// Inner padding of a notice.
const PAD: f32 = 12.0;
/// Space between notices.
const GAP: f32 = 8.0;

/// One thing to tell the user about.
pub(super) struct Notice {
    /// What happened.
    pub text: String,
    /// Offer to open the data folder.
    pub folder: bool,
    /// A folder to offer instead: where a file was exported.
    pub reveal: Option<PathBuf>,
    /// Something went wrong (rather than a file being saved).
    pub problem: bool,
    /// When it goes by itself.
    pub until: Instant,
}

impl Notice {
    /// A problem shown from now on.
    pub fn new(text: String, folder: bool) -> Self {
        Self { text, folder, reveal: None, problem: true, until: Instant::now() + SHOWN_FOR }
    }

    /// A file saved where the user chose, in `folder`.
    pub fn saved(text: String, folder: Option<PathBuf>) -> Self {
        Self { reveal: folder, problem: false, ..Self::new(text, false) }
    }
}

impl Chat {
    /// Draws the notices stacked under the header of `main`, and applies
    /// their buttons.
    pub(super) fn draw_notices(&mut self, p: &mut Painter, ui: &mut Ui, main: Rect, actions: &mut Vec<Action>) {
        self.notices_rect = None;
        if self.notices.is_empty() {
            return;
        }
        let t = p.theme;
        let width = WIDTH.min(main.w - 32.0);
        let x = main.x + ((main.w - width) * 0.5).round();
        let mut y = theme::HEADER_HEIGHT + 10.0;
        let (mut close, mut open) = (None, None);
        // The notices take the mouse: nothing beneath them reacts.
        let blocker = ui.blocker.take();
        for (index, notice) in self.notices.iter().enumerate() {
            let text_w = width - 2.0 * PAD - 28.0;
            let text = p.layout(&notice.text, theme::SMALL, Some(text_w));
            let button_label = if notice.folder {
                Some(("Open folder", 104.0))
            } else {
                notice.reveal.as_ref().map(|_| ("Show in folder", 120.0))
            };
            let buttons_h = if button_label.is_some() { 34.0 } else { 0.0 };
            let card = Rect::new(x, y, width, text.height() + 2.0 * PAD + buttons_h);
            p.shadow(Rect::new(card.x, card.y + 4.0, card.w, card.h), t.shadow, theme::RADIUS, 14.0);
            let border = if notice.problem { fade(t.danger, 0.5) } else { t.border_strong };
            p.bordered(card, t.panel, theme::RADIUS, 1.0, border);
            p.text(&text, card.x + PAD, card.y + PAD, t.text);
            if button(p, ui, Rect::new(card.right() - 32.0, card.y + 6.0, 26.0, 26.0), "×", ButtonStyle::Ghost, true) {
                close = Some(index);
            }
            if let Some((label, w)) = button_label
                && button(p, ui, Rect::new(card.x + PAD - 8.0, card.bottom() - 36.0, w, 28.0), label, ButtonStyle::Ghost, true)
            {
                open = Some(index);
            }
            self.notices_rect = Some(self.notices_rect.map_or(card, |r: Rect| Rect::new(r.x, r.y, r.w, card.bottom() - r.y)));
            y = card.bottom() + GAP;
        }
        ui.blocker = blocker;
        if let Some(notice) = open.map(|index| &self.notices[index]) {
            actions.push(notice.reveal.clone().map_or(Action::OpenDataDir, Action::OpenFolder));
        }
        if let Some(index) = close {
            self.notices.remove(index);
        }
    }
}
