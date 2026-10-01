//! The character dialog. It asks who the user plays (a new story asks
//! before it begins; the "You" chip edits them later), and edits the
//! story's cast: changing a member, creating one, or having the AI
//! generate one, which then opens as a new member to review before it
//! joins. It also sets the scene by hand (click it in the cast strip).
//! Everything it changes belongs to the story only.

use arboard::Clipboard;
use serechat::{CastMember, Error, Player, ToolCall, new_id};
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::{Chat, Load, Reasoning, tools};
use crate::app::Action;
use crate::form::{FIELD_PAD, Fields};
use crate::library::{Kind, name_editor, text_editor};
use crate::paint::{Painter, Rect, hexa};
use crate::theme;
use crate::ui::{ButtonStyle, Ui, button};

/// The form's fields. Generating uses only the first, for the idea.
const NAME: usize = 0;
const DESCRIPTION: usize = 1;
/// Widest the form gets.
const WIDTH: f32 = 560.0;
/// Smallest height of the description field.
const DESCRIPTION_MIN_H: f32 = 140.0;
/// Smallest height of the idea field.
const IDEA_MIN_H: f32 = 96.0;

/// Who the form describes.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Subject {
    /// The user's character.
    Player,
    /// The cast member with this id.
    Member(String),
    /// Someone new for the cast.
    NewMember,
    /// An idea for the AI to make someone new from.
    Generate {
        /// The request on its way, if one is.
        request: Option<u64>,
        /// Why the last one failed.
        error: Option<String>,
    },
    /// Where the story is now and what it is like there.
    Scene,
}

impl Subject {
    /// Shows one free-text field (the first) rather than name and description.
    fn single(&self) -> bool {
        matches!(self, Self::Generate { .. } | Self::Scene)
    }
}

/// The character form of one conversation.
pub(super) struct CharacterForm {
    /// The conversation it edits.
    pub conversation: u64,
    /// Name and description (or the idea or scene alone).
    pub fields: Fields,
    /// Who it describes.
    pub subject: Subject,
    /// A dialog that can be cancelled, not the first question of a story.
    editing: bool,
}

/// Everything a worker thread needs to generate one character.
pub struct GenerateJob {
    /// Conversation it is for.
    pub conversation: u64,
    /// Identifies the request, so a stale result is dropped.
    pub request: u64,
    /// Model identifier.
    pub model: String,
    /// Reasoning effort, or `None` for the model default.
    pub reasoning: Option<&'static str>,
    /// The generator's system prompt.
    pub instructions: String,
    /// What the user asked for.
    pub idea: String,
}

impl Chat {
    /// The open conversation's character form, if it is shown.
    pub(super) fn character_form(&mut self) -> Option<&mut CharacterForm> {
        let current = self.current;
        self.character_form.as_mut().filter(|f| f.conversation == current)
    }

    /// Opens the player form for a new story that needs one, and drops a
    /// stale one: from another conversation, or for a player now known.
    pub(super) fn sync_character_form(&mut self) {
        let current = self.current;
        let needs = self.current().needs_player();
        match &self.character_form {
            Some(form) if form.conversation == current && (form.editing || needs) => {}
            _ if needs => {
                let fields = Fields::new([name_editor(""), text_editor("")], [false, true]);
                self.character_form = Some(CharacterForm { conversation: current, fields, subject: Subject::Player, editing: false });
            }
            _ => self.character_form = None,
        }
    }

    /// Opens the form as a dialog over the story.
    fn open_form(&mut self, subject: Subject, name: &str, description: &str) {
        let fields = if subject.single() {
            Fields::new([text_editor(name), text_editor("")], [true, true])
        } else {
            Fields::new([name_editor(name), text_editor(description)], [false, true])
        };
        self.menu = None;
        self.character_form = Some(CharacterForm { conversation: self.current, fields, subject, editing: true });
    }

    /// Opens the form to change who the user plays.
    pub(super) fn edit_player(&mut self) {
        let player = self.current().player.clone().unwrap_or_default();
        self.open_form(Subject::Player, &player.name, &player.description);
    }

    /// Opens the form to change cast member `index`.
    pub(super) fn edit_member(&mut self, index: usize) {
        if let Some(member) = self.current().cast.get(index).cloned() {
            self.open_form(Subject::Member(member.id), &member.name, &member.description);
        }
    }

    /// Opens the form for someone new in the cast.
    pub(super) fn create_member(&mut self) {
        self.open_form(Subject::NewMember, "", "");
    }

    /// Opens the form asking the AI for someone new in the cast.
    pub(super) fn generate_member(&mut self) {
        self.open_form(Subject::Generate { request: None, error: None }, "", "");
    }

    /// Opens the form to set the scene by hand.
    pub(super) fn edit_scene(&mut self) {
        let scene = self.current().scene.clone();
        self.open_form(Subject::Scene, &scene, "");
    }

    /// Whether someone in the story other than `subject` is called `name`:
    /// the tools tell characters apart by name.
    fn name_taken(&mut self, subject: &Subject, name: &str) -> bool {
        let conversation = self.current();
        let player = *subject != Subject::Player && conversation.player.as_ref().is_some_and(|p| tools::same(&p.name, name));
        player || conversation.cast.iter().any(|m| tools::same(&m.name, name) && !matches!(subject, Subject::Member(id) if *id == m.id))
    }

    /// Applies the form, if it has a free name, and closes it; when
    /// generating, sends the request instead.
    fn submit_form(&mut self, actions: &mut Vec<Action>) {
        let Some(form) = self.character_form() else {
            return;
        };
        let subject = form.subject.clone();
        let name = form.fields.text(NAME).trim().to_owned();
        let description = form.fields.text(DESCRIPTION).trim().to_owned();
        if let Subject::Generate { request, .. } = subject {
            if request.is_none() {
                self.generate(name, actions);
            }
            return;
        }
        // The scene may be emptied: the model then sets it afresh.
        if subject != Subject::Scene && (name.is_empty() || self.name_taken(&subject, &name)) {
            return;
        }
        self.character_form = None;
        let conversation = self.current();
        match subject {
            Subject::Player => conversation.player = Some(Player { name, description }),
            Subject::Member(id) => {
                if let Some(member) = conversation.cast.iter_mut().find(|m| m.id == id) {
                    (member.name, member.description) = (name, description);
                }
            }
            Subject::NewMember => conversation.cast.push(CastMember { id: new_id(), name, description, portrait: String::new(), present: true }),
            Subject::Scene => conversation.scene = name,
            Subject::Generate { .. } => {}
        }
        if !conversation.is_fresh() && conversation.load == Load::Loaded {
            actions.push(Action::SaveSession(conversation.to_session()));
        }
    }

    /// Asks the generator for someone matching `idea`.
    fn generate(&mut self, idea: String, actions: &mut Vec<Action>) {
        let request = self.next_id();
        let reasoning = Some(self.reasoning_in_use()).filter(|r| *r != Reasoning::Auto).map(Reasoning::key);
        let conversation = self.conversations.iter().find(|c| c.id == self.current).expect("the current conversation always exists");
        let world = conversation.world.as_deref().and_then(|w| self.library.get(Kind::World, w)).map(|w| (w.name.as_str(), w.description.as_str()));
        let cast: Vec<&str> = conversation.cast.iter().map(|m| m.name.as_str()).collect();
        let instructions = tools::generator_prompt(world, conversation.player.as_ref().map(|p| p.name.as_str()), &cast);
        let idea = if idea.is_empty() { tools::SURPRISE.to_owned() } else { idea };
        actions.push(Action::GenerateCharacter(GenerateJob {
            conversation: self.current,
            request,
            model: self.model.clone(),
            reasoning,
            instructions,
            idea,
        }));
        if let Some(form) = self.character_form() {
            form.subject = Subject::Generate { request: Some(request), error: None };
        }
    }

    /// The generator answered `request` with its call: the form becomes
    /// the new member to review, or shows why it failed. Results for a
    /// form that was closed or changed since are dropped.
    pub fn character_generated(&mut self, conversation: u64, request: u64, result: Result<Option<ToolCall>, Error>) {
        let Some(form) = self.character_form.as_mut().filter(|f| f.conversation == conversation) else {
            return;
        };
        if !matches!(form.subject, Subject::Generate { request: Some(r), .. } if r == request) {
            return;
        }
        let made = result
            .map_err(|e| e.to_string())
            .and_then(|call| call.as_ref().and_then(tools::generated).ok_or_else(|| "The model did not describe anyone. Try again.".to_owned()));
        match made {
            Ok((name, description)) => {
                form.fields = Fields::new([name_editor(&name), text_editor(&description)], [false, true]);
                form.subject = Subject::NewMember;
            }
            Err(error) => form.subject = Subject::Generate { request: None, error: Some(error) },
        }
    }

    /// Keyboard input for the character form. Returns `false` when it is
    /// not open, so the composer gets the key.
    pub(super) fn dialog_key(&mut self, event: &KeyEvent, mods: ModifiersState, cb: &mut Option<Clipboard>, actions: &mut Vec<Action>) -> bool {
        let primary = if cfg!(target_os = "macos") { mods.super_key() } else { mods.control_key() };
        let Some(form) = self.character_form() else {
            return false;
        };
        let single = form.subject.single();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) if form.editing => self.character_form = None,
            Key::Named(NamedKey::Enter) if primary => self.submit_form(actions),
            Key::Character(c) if primary && c.eq_ignore_ascii_case("s") => self.submit_form(actions),
            // The first field is the only one shown.
            Key::Named(NamedKey::Tab) if single => {}
            _ => {
                form.fields.key(event, mods, cb);
            }
        }
        true
    }

    /// Draws the character form: in `area` for a new story, or as a dialog
    /// over it otherwise.
    pub(super) fn draw_character_form(&mut self, p: &mut Painter, ui: &mut Ui, area: Rect, actions: &mut Vec<Action>) {
        let t = p.theme;
        let world = self.current().world.clone().and_then(|id| self.library.get(Kind::World, &id).map(|w| w.name.clone()));
        let Some(form) = self.character_form() else {
            return;
        };
        let (subject, editing) = (form.subject.clone(), form.editing);
        let name = form.fields.text(NAME).trim().to_owned();
        let generate = match &subject {
            Subject::Generate { request, error } => Some((request.is_some(), error.clone())),
            _ => None,
        };
        let single = subject.single();
        let taken = !single && !name.is_empty() && self.name_taken(&subject, &name);
        let Some(form) = self.character_form() else {
            return;
        };

        let (title, note, submit, labels, hints) = match &subject {
            Subject::Player => (
                world.as_ref().map_or_else(|| "Who are you?".to_owned(), |w| format!("Who are you in {w}?")),
                "Your character in this story. The AI plays everyone else, and never speaks or acts for you.",
                if editing { "Save" } else { "Begin" },
                ["Name", "Description"],
                ["Your character's name", "Who you are: background, appearance, what you want…"],
            ),
            Subject::Member(_) | Subject::NewMember => (
                if subject == Subject::NewMember { "New character" } else { "Edit character" }.to_owned(),
                "For this story only: the character library keeps its own copies.",
                if subject == Subject::NewMember { "Add" } else { "Save" },
                ["Name", "Description"],
                ["Their name", "Who they are: role, personality, appearance, how they speak…"],
            ),
            Subject::Generate { request, .. } => (
                "Generate a character".to_owned(),
                "Say who you need, or leave it empty for a surprise. The AI writes them to fit this story; you review them before they join.",
                if request.is_some() { "Generating…" } else { "Generate" },
                ["Idea", ""],
                ["e.g. a grumpy innkeeper who knows more than she lets on", ""],
            ),
            Subject::Scene => (
                "The scene".to_owned(),
                "Where the story is now and what it is like there. The AI reads it before every reply, and changes it when the story moves on.",
                "Save",
                ["Scene", ""],
                ["e.g. Peeta's kitchen, before dawn; warm bread, rain on the windows", ""],
            ),
        };
        let error = match &generate {
            Some((_, error)) => error.clone(),
            None => taken.then(|| format!("Someone in this story is already called {name}.")),
        };

        let width = WIDTH.min(area.w - 48.0);
        let inner = width - 48.0;
        let note = p.layout(note, theme::SMALL, Some(inner));
        let note_h = (note.height() + 14.0).max(44.0);
        // Generating and the scene show one field alone: the first.
        let (body_field, body_min) = if single { (NAME, IDEA_MIN_H) } else { (DESCRIPTION, DESCRIPTION_MIN_H) };
        let body = form.fields.layout(p, body_field, inner);
        let body_h = (body.height() + 2.0 * FIELD_PAD.1).max(body_min);
        let name_layout = (!single).then(|| form.fields.layout(p, NAME, inner));
        let name_h = name_layout.as_ref().map_or(0.0, |l| l.line_height() + 2.0 * FIELD_PAD.1);
        let error = error.map(|e| p.layout(&e, theme::SMALL, Some(inner)));
        let error_h = error.as_ref().map_or(0.0, |e| e.height() + 12.0);
        let name_block = if name_layout.is_some() { 22.0 + name_h + 20.0 } else { 0.0 };
        let height = 24.0 + 30.0 + note_h + name_block + 22.0 + body_h + error_h + 24.0 + 34.0 + 24.0;
        let card = Rect::new(area.x + ((area.w - width) * 0.5).round(), area.y + ((area.h - height) * 0.42).max(16.0).round(), width, height);
        if editing {
            p.rect(area, hexa(0x000000, 0.35), 0.0);
        }
        p.shadow(Rect::new(card.x, card.y + 8.0, card.w, card.h), t.shadow, theme::RADIUS, 24.0);
        p.bordered(card, t.panel, theme::RADIUS, 1.0, t.border_strong);

        let x = card.x + 24.0;
        let mut y = card.y + 24.0;
        let mut title = p.layout(&title, theme::TITLE, None);
        title.truncate(p.fonts, inner);
        p.text(&title, x, y, t.text);
        y += 30.0;
        p.text(&note, x, y + 6.0, t.text_muted);
        y += note_h;

        if let Some(layout) = name_layout {
            p.label(labels[0], theme::LABEL, x, y, t.text);
            y += 22.0;
            form.fields.draw(NAME, p, ui, Rect::new(x, y, inner, name_h), layout, hints[0], true);
            y += name_h + 20.0;
        }
        let (label, hint) = if single { (labels[0], hints[0]) } else { (labels[1], hints[1]) };
        p.label(label, theme::LABEL, x, y, t.text);
        y += 22.0;
        form.fields.draw(body_field, p, ui, Rect::new(x, y, inner, body_h), body, hint, true);
        y += body_h;
        if let Some(error) = &error {
            p.text(error, x, y + 8.0, t.danger);
            y += error_h;
        }
        y += 24.0;

        let ready = match generate {
            Some((busy, _)) => !busy,
            None => subject == Subject::Scene || (!name.is_empty() && !taken),
        };
        let shortcut = if cfg!(target_os = "macos") { "Cmd+Enter" } else { "Ctrl+Enter" };
        let tip = p.layout(&format!("{shortcut} to {}", submit.trim_end_matches('…').to_lowercase()), theme::TINY, None);
        if ready {
            p.text(&tip, x, y + (34.0 - tip.height()) * 0.5, t.text_faint);
        }
        let submit_rect = Rect::new(x + inner - 120.0, y, 120.0, 34.0);
        let mut cancelled = false;
        if editing {
            cancelled = button(p, ui, Rect::new(submit_rect.x - 98.0, y, 90.0, 34.0), "Cancel", ButtonStyle::Ghost, true);
        }
        if button(p, ui, submit_rect, submit, ButtonStyle::Primary, ready) {
            self.submit_form(actions);
        } else if cancelled {
            self.character_form = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_story_asks_who_you_are_before_it_begins() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        chat.play("w".into());
        assert!(chat.current().needs_player() && !chat.current().playable());
        chat.sync_character_form();
        let mut actions = Vec::new();
        chat.submit_form(&mut actions);
        assert!(chat.character_form.is_some(), "a name is required");

        chat.character_form().unwrap().fields.insert("  Gale ");
        chat.submit_form(&mut actions);
        // Begin saves the story at once, so it is listed before anyone writes.
        assert!(matches!(&actions[..], [Action::SaveSession(s)] if s.messages.is_empty() && s.player.is_some()));
        assert!(!chat.current().is_fresh());
        assert_eq!(chat.current().player.as_ref().map(|p| p.name.as_str()), Some("Gale"));
        assert!(chat.current().playable());
        chat.sync_character_form();
        assert!(chat.character_form.is_none());

        // Editing later fills in the player and saves a story with messages.
        chat.composer.insert("I wake up.");
        chat.send(&mut actions);
        chat.edit_player();
        let form = chat.character_form().unwrap();
        assert_eq!(form.fields.text(NAME), "Gale");
        form.fields.editor(DESCRIPTION).insert("A hunter.");
        let mut actions = Vec::new();
        chat.submit_form(&mut actions);
        assert!(matches!(&actions[..], [Action::SaveSession(s)] if s.player.as_ref().is_some_and(|p| p.description == "A hunter.")));
    }

    #[test]
    fn the_cast_is_created_edited_and_generated() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        chat.play("w".into());
        chat.current().player = Some(Player { name: "Gale".into(), description: String::new() });
        let mut actions = Vec::new();

        chat.create_member();
        chat.character_form().unwrap().fields.insert("gale");
        chat.submit_form(&mut actions);
        assert!(chat.character_form.is_some(), "the player's name is taken");
        chat.create_member();
        chat.character_form().unwrap().fields.insert("Rue");
        chat.submit_form(&mut actions);
        assert!(chat.current().cast.first().is_some_and(|m| m.name == "Rue" && m.present && !m.id.is_empty()));

        chat.edit_member(0);
        chat.character_form().unwrap().fields.editor(DESCRIPTION).insert("From District 11.");
        chat.submit_form(&mut actions);
        assert_eq!(chat.current().cast[0].description, "From District 11.");
        assert_eq!(actions.len(), 2, "a begun story is saved with every change");
        actions.clear();

        // Generating sends the idea, and its answer becomes a new member to review.
        chat.generate_member();
        chat.submit_form(&mut actions);
        let Some(Action::GenerateCharacter(job)) = actions.pop() else { panic!("not generated") };
        assert!(job.idea == tools::SURPRISE && job.instructions.ends_with("Rue"));
        chat.submit_form(&mut actions);
        assert!(actions.is_empty(), "one request at a time");
        chat.character_generated(job.conversation, job.request + 1, Ok(None));
        assert!(matches!(chat.character_form().unwrap().subject, Subject::Generate { request: Some(_), .. }), "a stale answer is dropped");
        let call =
            ToolCall { call_id: "c".into(), name: tools::CREATE_CHARACTER.into(), arguments: r#"{"name":"Cato","description":"A career."}"#.into() };
        chat.character_generated(job.conversation, job.request, Ok(Some(call)));
        let form = chat.character_form().unwrap();
        assert!(form.subject == Subject::NewMember && form.fields.text(DESCRIPTION) == "A career.");
        chat.submit_form(&mut actions);
        assert_eq!(chat.current().cast.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Rue", "Cato"]);

        // A failed request can be sent again.
        chat.generate_member();
        chat.submit_form(&mut actions);
        let Some(Action::GenerateCharacter(job)) = actions.pop() else { panic!("not generated") };
        chat.character_generated(job.conversation, job.request, Ok(None));
        assert!(matches!(&chat.character_form().unwrap().subject, Subject::Generate { request: None, error: Some(_) }));
    }

    #[test]
    fn the_scene_is_set_by_hand() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        chat.play("w".into());
        chat.current().player = Some(Player { name: "Gale".into(), description: String::new() });
        chat.current().scene = "The Hob.".into();
        let mut actions = Vec::new();

        chat.edit_scene();
        let form = chat.character_form().unwrap();
        assert_eq!(form.fields.text(NAME), "The Hob.");
        form.fields.insert(" Smoky.");
        chat.submit_form(&mut actions);
        assert!(chat.character_form.is_none());
        assert!(matches!(&actions[..], [Action::SaveSession(s)] if s.scene == "The Hob. Smoky."));
        let id = chat.current().id;
        assert!(chat.instructions(id).contains("# The scene\n\nThe Hob. Smoky."), "the next reply reads it");

        // Emptied, the model sets it afresh.
        chat.edit_scene();
        chat.character_form().unwrap().fields = Fields::new([text_editor(""), text_editor("")], [true, true]);
        chat.submit_form(&mut actions);
        assert!(chat.current().scene.is_empty());
    }
}
