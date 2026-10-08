//! Impersonation: the AI writes the user's next message for them, straight
//! into the composer, to edit and send (or not). It reads the story as a
//! reply would, with the same system prompt (so the provider's cache serves
//! it) and an instruction after the story state; it calls no tools. A draft
//! in the composer is its starting point, and is replaced as the text comes
//! in. The request is billed with the story's next reply, like memory reviews.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serechat::{Error, StreamEvent, ToolChoice, Usage};

use super::stream::SendJob;
use super::{Chat, Load};
use crate::app::Action;

/// Asks for the user's next message instead of a reply. `{player}` is
/// their character's name.
const IMPERSONATE: &str = "[OOC: Do not continue the story and call no tools. Write the user's next message instead, as {player}: \
    what {player} says and does next, in first person, the way the user writes. Actions go in *asterisks*, speech as plain text. \
    Only {player}: never another character's words or actions, and no narration of what others do. Keep it short, one to three \
    sentences unless the moment needs more. Reply with the message itself and nothing else.]";
/// Added when the user had started writing: `{draft}` is what they typed.
const DRAFT: &str = "The user started writing it like this; keep what they meant and make it whole:\n\n{draft}";

/// The AI writing the user's next message.
pub(super) struct Impersonation {
    /// The conversation it is for.
    pub conversation: u64,
    /// Its stream's id.
    pub stream: u64,
    /// Raised to stop it.
    pub cancel: Arc<AtomicBool>,
    /// The model writing, for pricing it.
    pub model: String,
    /// Who the user plays.
    pub player: String,
    /// Text arrived: the draft it replaces is gone.
    pub writing: bool,
    /// What the server billed, if it failed.
    pub charged: Usage,
}

impl Chat {
    /// Whether the open story's next message could be written for the user
    /// now: a story begun, loaded, with nothing on its way.
    pub(super) fn can_impersonate(&self) -> bool {
        self.conversations.iter().find(|c| c.id == self.current).is_some_and(|c| {
            c.world.is_some() && c.player.is_some() && c.load == Load::Loaded && !c.busy() && c.playable() && self.impersonation.is_none()
        })
    }

    /// Whether the AI is writing the open story's next message.
    pub(super) fn impersonating(&self) -> bool {
        self.impersonation.as_ref().is_some_and(|i| i.conversation == self.current)
    }

    /// Has the AI write the user's next message into the composer, from
    /// what is typed there (if anything); stops it when it is already writing.
    pub(super) fn impersonate(&mut self, actions: &mut Vec<Action>) {
        if self.impersonating() {
            self.stop_impersonating();
            return;
        }
        if !self.can_impersonate() {
            return;
        }
        let current = self.current;
        let draft = self.composer.text().trim().to_owned();
        let (instructions, state) = (self.instructions(current), self.state(current).unwrap_or_default());
        let (stream, model, reasoning) = (self.next_id(), self.model.clone(), self.reasoning_key());
        let Some(conversation) = self.conversations.iter().find(|c| c.id == current) else { return };
        let Some(player) = conversation.player.as_ref().map(|p| p.name.clone()) else { return };
        let mut ask = IMPERSONATE.replace("{player}", &player);
        if !draft.is_empty() {
            let _ = write!(ask, "\n\n{}", DRAFT.replace("{draft}", &draft));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let job = SendJob {
            conversation: current,
            stream,
            model: model.clone(),
            reasoning,
            instructions,
            history: conversation.entries.iter().map(|e| e.message.clone()).collect(),
            state: Some(format!("{state}\n\n{ask}")),
            // Declared, as the history calls them; none is called.
            tools: true,
            tool_choice: Some(ToolChoice::None),
            cancel: Arc::clone(&cancel),
        };
        self.impersonation = Some(Impersonation { conversation: current, stream, cancel, model, player, writing: false, charged: Usage::default() });
        actions.push(Action::Send(job));
    }

    /// Stops the AI writing the user's message; what it wrote stays.
    pub(super) fn stop_impersonating(&mut self) {
        if let Some(impersonation) = self.impersonation.take() {
            impersonation.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Whether `stream` of `conversation` is the impersonation's.
    pub(super) fn is_impersonation(&self, conversation: u64, stream: u64) -> bool {
        self.impersonation.as_ref().is_some_and(|i| i.conversation == conversation && i.stream == stream)
    }

    /// Applies one streamed update of the impersonation: its text goes into
    /// the composer, replacing the draft it started from.
    pub(super) fn impersonated(&mut self, event: StreamEvent) {
        let Some(impersonation) = &mut self.impersonation else { return };
        match event {
            StreamEvent::Text(delta) => {
                let delta = if impersonation.writing { delta.as_str() } else { delta.trim_start() };
                if delta.is_empty() {
                    return;
                }
                if !impersonation.writing {
                    impersonation.writing = true;
                    self.composer.take();
                }
                self.composer.set_cursor(self.composer.text().len(), false);
                self.composer.insert(delta);
                self.selection = None;
            }
            StreamEvent::Charged(usage) => impersonation.charged = usage,
            StreamEvent::Completed(completion) => impersonation.charged = completion.usage,
            _ => {}
        }
    }

    /// The impersonation's stream ended: what it cost goes with the next
    /// reply, a name it led with is dropped, and a failure is told. Returns
    /// whether the server rejected our token.
    pub(super) fn impersonation_ended(&mut self, result: &Result<bool, Error>) -> bool {
        let Some(impersonation) = self.impersonation.take() else { return false };
        let cost = self.models.iter().find(|m| m.id == impersonation.model).map_or(0.0, |m| m.cost(impersonation.charged));
        if let Some(conversation) = self.find(impersonation.conversation) {
            conversation.carried_cost += cost;
        }
        if let Err(e) = result {
            if e.is_unauthorized() {
                return true;
            }
            self.notify(format!("Could not write your message: {e}"), false);
            return false;
        }
        if !impersonation.writing {
            self.notify("The model wrote nothing. Try again, or pick another model.".to_owned(), false);
            return false;
        }
        // Models sometimes lead with the speaker's name, as in a script.
        let text = self.composer.text().trim();
        let named = text.get(..impersonation.player.len() + 1).is_some_and(|head| head.eq_ignore_ascii_case(&format!("{}:", impersonation.player)));
        let cleaned = if named { text[impersonation.player.len() + 1..].trim() } else { text };
        let cleaned = cleaned.to_owned();
        self.composer.take();
        self.composer.insert(&cleaned);
        false
    }
}

#[cfg(test)]
mod tests {
    use serechat::{Completion, Player, StreamEvent, ToolChoice};

    use super::*;
    use crate::chat::Reasoning;

    #[test]
    fn the_ai_writes_the_users_message_into_the_composer() {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        chat.play("w".into());
        chat.current().player = Some(Player { name: "Gale".into(), ..Player::default() });
        chat.composer.insert("I ask about");
        let mut actions = Vec::new();
        chat.impersonate(&mut actions);
        let Some(Action::Send(job)) = actions.pop() else { panic!("not sent") };
        assert!(job.tool_choice == Some(ToolChoice::None) && job.tools);
        assert!(job.state.as_deref().is_some_and(|s| s.contains("as Gale") && s.ends_with("I ask about")), "the draft is its starting point");
        assert!(chat.impersonating() && !chat.can_impersonate());

        // The draft stays until text comes, then the text replaces it.
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text("  ".into()));
        assert_eq!(chat.composer.text(), "I ask about");
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text(" Gale: I ask about the".into()));
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text(" mines.".into()));
        assert_eq!(chat.composer.text(), "Gale: I ask about the mines.");
        let usage = serechat::Usage::new(100, 10);
        chat.stream_event(job.conversation, job.stream, StreamEvent::Completed(Completion { usage, ..Completion::default() }));
        assert!(!chat.stream_end(job.conversation, job.stream, Ok(true), &mut actions));
        assert_eq!(chat.composer.text(), "I ask about the mines.", "the name it led with is dropped");
        assert!(!chat.impersonating() && chat.current().entries.is_empty(), "nothing joined the story");

        // Stopped, it writes no more; switching stories stops it too.
        chat.impersonate(&mut actions);
        let Some(Action::Send(job)) = actions.pop() else { panic!("not sent") };
        chat.impersonate(&mut actions);
        assert!(job.cancel.load(Ordering::Relaxed) && !chat.impersonating());
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text("Late.".into()));
        assert_eq!(chat.composer.text(), "I ask about the mines.");
    }
}
