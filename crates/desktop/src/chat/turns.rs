//! Acting on a whole response, the turn a prompt started: the user's
//! message and everything up to the next one (replies, rounds that went on
//! by themselves, notes about failures, summaries).
//!
//! * Edit makes the turn's replies editable at once, a field per character
//!   bubble. Edited speech is rewritten in the reply's speak call, so it
//!   stays bubbles and the model sees the call as edited. A reply without
//!   characters (a plain chat's) keeps its edited text only.
//! * Regenerate (last turn only) sends its prompt again, as if just sent.
//!   The replies it had are kept as a swipe: the last turn's ‹ › switch
//!   between every reply its prompt got.
//! * Delete removes the prompt and everything that answered it.
//!
//! Regenerating, swiping, or deleting the last turn rewinds the story too:
//! each reply recorded what its tools changed (cast, scene, memories), and
//! that is undone in reverse, except what was changed again since; swiping
//! to a reply makes its changes again. Deleting an earlier turn only
//! removes messages, as later turns build on what it did. What removed
//! replies cost is not forgotten: swiped-away replies keep theirs, a reply
//! that failed is billed with the one replacing it, and a deleted turn's
//! cost moves to the reply before it.

use std::ops::Range;

use serechat::{Role, StoredMessage};

use super::stream::is_summary;
use super::tools::{self, Part};
use super::{Chat, Conversation, Entry, Load};
use crate::app::Action;
use crate::form::Fields;

/// Replies being edited, in one conversation.
pub(super) struct TurnEdit {
    /// The conversation they belong to.
    pub conversation: u64,
    /// What each field edits, in order.
    pub slots: Vec<Slot>,
    /// One field per slot.
    pub fields: Fields,
}

/// What one field of a turn edit holds.
pub(super) struct Slot {
    /// Id of the reply.
    pub entry: u64,
    /// The character whose bubble it is, or `None` for a whole reply
    /// without characters.
    pub character: Option<String>,
    /// The text before editing.
    pub text: String,
}

/// The turn holding entry `index`: from the user message that started it
/// up to the next one.
pub(super) fn turn(entries: &[Entry], index: usize) -> Range<usize> {
    if entries.is_empty() {
        return 0..0;
    }
    let index = index.min(entries.len() - 1);
    let start = entries[..=index].iter().rposition(|e| e.message.role == Role::User).unwrap_or(0);
    let end = entries.iter().skip(start + 1).position(|e| e.message.role == Role::User).map_or(entries.len(), |i| start + 1 + i);
    start..end
}

/// Where a turn's actions are drawn: under its last entry that is not a
/// summary (its prompt, when nothing answered it).
pub(super) fn anchor(entries: &[Entry], turn: &Range<usize>) -> usize {
    turn.clone().rev().find(|&i| !entries[i].message.compaction).unwrap_or(turn.start)
}

/// Whether a reply can be edited: model output, shown, not a summary.
fn editable(entry: &Entry) -> bool {
    let message = &entry.message;
    message.role == Role::Assistant && !message.failed && !message.compaction && (!message.content.is_empty() || !message.tool_calls.is_empty())
}

/// Whether the turn has replies to edit.
pub(super) fn has_editable(entries: &[Entry], turn: &Range<usize>) -> bool {
    entries[turn.clone()].iter().any(editable)
}

impl Conversation {
    /// Whether turns can be changed now: loaded and nothing on its way.
    fn settled(&self) -> bool {
        self.load == Load::Loaded && !self.busy()
    }

    /// Undoes, in reverse, what the replies in `range` changed in the story.
    fn rewind(&mut self, range: Range<usize>) {
        let Self { entries, cast, scene, memories, .. } = self;
        for entry in entries[range].iter().rev() {
            if let Some(change) = &entry.message.change {
                change.undo(cast, scene, memories);
            }
        }
    }

    /// Entries in `range` are about to go: a summary after them that keeps
    /// some of them word for word keeps fewer, and a prompt among them the
    /// memory reviews read counts no more.
    fn forget(&mut self, range: &Range<usize>) {
        let prompts_before = self.entries[..range.start].iter().filter(|e| e.message.is_prompt()).count();
        let removed = self.entries[range.clone()].iter().filter(|e| e.message.is_prompt()).count();
        let read = self.reviewed.saturating_sub(prompts_before).min(removed);
        self.reviewed -= read;
        if let Some(at) = self.entries.iter().rposition(|e| is_summary(&e.message)).filter(|&at| at >= range.end) {
            let kept = &mut self.entries[at].message.kept;
            let window = at - (*kept).min(at)..at;
            *kept -= range.end.min(window.end).saturating_sub(range.start.max(window.start));
        }
    }

    /// The last prompt's index and how its replies can be swiped: the one
    /// shown (from 0) and how many there are. `None` without a prompt.
    pub(super) fn swipes(&self) -> Option<(usize, usize, usize)> {
        let prompt = self.entries.iter().rposition(|e| e.message.role == Role::User)?;
        let message = &self.entries[prompt].message;
        Some((prompt, message.swipe.min(message.swipes.len()), message.swipes.len() + 1))
    }

    /// The greeting opening the story and the others it can be swiped to:
    /// (the one shown, from 0, and how many), while nobody wrote yet.
    pub(super) fn greetings(&self) -> Option<(usize, usize)> {
        if self.entries.iter().any(|e| e.message.role == Role::User) {
            return None;
        }
        let first = &self.entries.first()?.message;
        let whole = first.role == Role::Assistant && !first.swipes.is_empty() && first.swipes.iter().all(|s| s.len() == 1);
        whole.then(|| (first.swipe.min(first.swipes.len()), first.swipes.len() + 1))
    }
}

/// Whether `replies` showed the user anything worth swiping back to.
fn worth_keeping(replies: &[StoredMessage]) -> bool {
    replies.iter().any(|m| m.role == Role::Assistant && !m.failed && !m.compaction && (!m.content.is_empty() || !m.tool_calls.is_empty()))
}

impl Chat {
    /// Makes the replies of the turn holding entry `index` editable.
    pub(super) fn edit_turn(&mut self, index: usize) {
        let current = self.current;
        let conversation = self.current();
        if !conversation.settled() || index >= conversation.entries.len() {
            return;
        }
        let story = conversation.world.is_some();
        let turn = turn(&conversation.entries, index);
        let mut slots = Vec::new();
        for entry in conversation.entries[turn].iter_mut().filter(|e| editable(e)) {
            entry.refresh_display(false, story);
            if entry.parts.is_empty() {
                if !entry.display.is_empty() {
                    slots.push(Slot { entry: entry.id, character: None, text: entry.display.clone() });
                }
                continue;
            }
            // A field per character; notes stay as they are.
            for part in &entry.parts {
                if let Part::Said { character, text } = part {
                    slots.push(Slot { entry: entry.id, character: Some(character.clone()), text: text.clone() });
                }
            }
        }
        if !slots.is_empty() {
            self.selection = None;
            let texts: Vec<String> = slots.iter().map(|s| s.text.clone()).collect();
            self.turn_edit = Some(TurnEdit { conversation: current, slots, fields: Fields::multiline(&texts) });
        }
    }

    /// The open conversation's turn edit, if one is open.
    pub(super) fn turn_edit(&mut self) -> Option<&mut TurnEdit> {
        let current = self.current;
        self.turn_edit.as_mut().filter(|e| e.conversation == current)
    }

    /// Applies the edit to each changed reply: a character's speech is
    /// rewritten in its speak call (so it stays a bubble, and the model
    /// sees the call as edited), an emptied character leaves the reply, and
    /// a reply emptied entirely is removed. A reply without characters
    /// keeps its new text only. Saves the story.
    pub(super) fn save_turn_edit(&mut self, actions: &mut Vec<Action>) {
        let Some(edit) = self.turn_edit.take() else { return };
        let Some(conversation) = self.find(edit.conversation) else { return };
        if conversation.load != Load::Loaded {
            return;
        }
        let mut changed = false;
        let mut done = 0;
        while done < edit.slots.len() {
            // The slots of one reply are next to each other.
            let id = edit.slots[done].entry;
            let end = edit.slots[done..].iter().position(|s| s.entry != id).map_or(edit.slots.len(), |n| done + n);
            let fields: Vec<(&Slot, &str)> = (done..end).map(|k| (&edit.slots[k], edit.fields.text(k).trim())).collect();
            done = end;
            let Some(index) = conversation.entries.iter().position(|e| e.id == id) else { continue };
            if fields.iter().all(|(slot, text)| slot.text == *text) {
                continue;
            }
            changed = true;
            if fields.iter().all(|(_, text)| text.is_empty()) {
                // Its cost stays in the story, on the reply before it.
                let cost = conversation.entries[index].message.cost;
                conversation.forget(&(index..index + 1));
                conversation.entries.remove(index);
                carry_cost(conversation, index, cost);
                continue;
            }
            let message = &mut conversation.entries[index].message;
            if fields[0].0.character.is_some() {
                let said: Vec<(&str, &str)> = fields.iter().filter_map(|(slot, text)| Some((slot.character.as_deref()?, *text))).collect();
                tools::respeak(&mut message.tool_calls, &said);
                message.content.clear();
            } else {
                fields[0].1.clone_into(&mut message.content);
                // The model sees the edited text, not the calls it replaced.
                message.tool_calls.clear();
            }
            let entry = &mut conversation.entries[index];
            entry.streaming_calls.clear();
            entry.display_key = (usize::MAX, 0, false);
        }
        if changed {
            actions.push(Action::SaveSession(conversation.to_session()));
        }
    }

    /// Sends the last prompt again: its replies become a swipe (if they
    /// showed anything) and what they changed in the story is undone.
    pub(super) fn regenerate(&mut self, actions: &mut Vec<Action>) {
        let id = self.current;
        let conversation = self.current();
        let Some((prompt, shown, _)) = conversation.swipes() else { return };
        if !conversation.settled() {
            return;
        }
        let replies = prompt + 1..conversation.entries.len();
        conversation.rewind(replies.clone());
        conversation.forget(&replies);
        // The new reply is read by the next memory review.
        let prompts = conversation.entries.iter().filter(|e| e.message.is_prompt()).count();
        if conversation.reviewed >= prompts {
            conversation.reviewed = prompts.saturating_sub(1);
        }
        let old: Vec<StoredMessage> = conversation.entries.drain(replies).map(|e| e.message).collect();
        let keep = worth_keeping(&old);
        if !keep {
            // A failure is billed with the reply that replaces it.
            conversation.carried_cost += old.iter().map(StoredMessage::total_cost).sum::<f64>();
        }
        let message = &mut conversation.entries[prompt].message;
        if keep {
            message.swipes.insert(shown, old);
        }
        message.swipe = message.swipes.len();
        conversation.auto_rounds = 0;
        conversation.repairing = false;
        self.selection = None;
        self.turn_edit = None;
        self.request_reply(id, actions);
    }

    /// Shows reply `target` (from 0) of the last prompt's swipes in place
    /// of the one shown: the story rewinds what the shown one changed and
    /// makes the changes of the one swiped to. Saves the story.
    pub(super) fn swipe_to(&mut self, target: usize, actions: &mut Vec<Action>) {
        let first = self.next_id;
        let conversation = self.current();
        let Some((prompt, shown, count)) = conversation.swipes() else { return };
        if !conversation.settled() || target >= count || target == shown {
            return;
        }
        let replies = prompt + 1..conversation.entries.len();
        conversation.rewind(replies.clone());
        conversation.forget(&replies);
        let old: Vec<StoredMessage> = conversation.entries.drain(replies).map(|e| e.message).collect();
        let mut all = std::mem::take(&mut conversation.entries[prompt].message.swipes);
        // The reply shown goes back among the others, unless it only failed.
        let target = if worth_keeping(&old) {
            all.insert(shown, old);
            target
        } else {
            conversation.carried_cost += old.iter().map(StoredMessage::total_cost).sum::<f64>();
            if target > shown { target - 1 } else { target }
        };
        let chosen = if target < all.len() { all.remove(target) } else { Vec::new() };
        let message = &mut conversation.entries[prompt].message;
        (message.swipes, message.swipe) = (all, target);
        let added = chosen.len() as u64;
        for (message, id) in chosen.into_iter().zip(first + 1..) {
            if let Some(change) = &message.change {
                change.redo(&mut conversation.cast, &mut conversation.scene, &mut conversation.memories);
            }
            conversation.entries.push(Entry::new(id, message));
        }
        actions.push(Action::SaveSession(conversation.to_session()));
        self.next_id += added;
        self.selection = None;
        self.turn_edit = None;
    }

    /// Shows greeting `target` (from 0) in place of the one opening the
    /// story, before anyone wrote. Saves the story.
    pub(super) fn swipe_greeting(&mut self, target: usize, actions: &mut Vec<Action>) {
        let id = self.next_id();
        let conversation = self.current();
        let Some((shown, count)) = conversation.greetings() else { return };
        if !conversation.settled() || target >= count || target == shown {
            return;
        }
        let mut current = conversation.entries.remove(0).message;
        let mut all = std::mem::take(&mut current.swipes);
        current.swipe = 0;
        all.insert(shown, vec![current]);
        // Each holds one message (see `greetings`).
        let mut chosen = all.remove(target).swap_remove(0);
        (chosen.swipes, chosen.swipe) = (all, target);
        conversation.entries.insert(0, Entry::new(id, chosen));
        actions.push(Action::SaveSession(conversation.to_session()));
        self.selection = None;
        self.turn_edit = None;
    }

    /// Deletes the turn holding entry `index`: its prompt and every reply
    /// to it. The last turn's changes to the story are undone.
    pub(super) fn delete_turn(&mut self, index: usize, actions: &mut Vec<Action>) {
        let conversation = self.current();
        if !conversation.settled() || index >= conversation.entries.len() {
            return;
        }
        let turn = turn(&conversation.entries, index);
        if turn.end == conversation.entries.len() {
            conversation.rewind(turn.clone());
        }
        conversation.forget(&turn);
        let start = turn.start;
        let cost: f64 = conversation.entries.drain(turn).map(|e| e.message.total_cost()).sum();
        carry_cost(conversation, start, cost);
        if start == 0 {
            // The title came from the first prompt.
            let first = conversation.entries.iter().find(|e| e.message.role == Role::User);
            conversation.title = first.map(|e| e.message.content.lines().next().unwrap_or_default().chars().take(80).collect()).unwrap_or_default();
        }
        actions.push(Action::SaveSession(conversation.to_session()));
        self.selection = None;
        self.turn_edit = None;
    }
}

/// Adds `cost`, billed for messages removed at `index`, to the nearest
/// reply before it that stays, or to the next reply when there is none.
fn carry_cost(conversation: &mut Conversation, index: usize, cost: f64) {
    if cost == 0.0 {
        return;
    }
    let index = index.min(conversation.entries.len());
    let before = conversation.entries[..index].iter_mut().rev().find(|e| e.message.role == Role::Assistant);
    match before {
        Some(entry) => entry.message.cost += cost,
        None => conversation.carried_cost += cost,
    }
}

#[cfg(test)]
mod tests {
    use serechat::{Completion, StreamEvent, ToolCall};

    use super::*;
    use crate::chat::{Reasoning, SendJob};

    /// A story with a player, in which `prompt` was sent and answered by a
    /// speak call of `args`.
    fn answer(chat: &mut Chat, prompt: &str, args: &str) -> SendJob {
        chat.composer.insert(prompt);
        let mut actions = Vec::new();
        chat.send(&mut actions);
        let Some(Action::Send(job)) = actions.pop() else { panic!("not sent") };
        let call = ToolCall { call_id: format!("c{}", job.stream), name: super::super::tools::SPEAK.into(), arguments: args.into() };
        let completion = Completion { tool_calls: vec![call], usage: serechat::Usage::new(10, 5), ..Completion::default() };
        chat.stream_event(job.conversation, job.stream, StreamEvent::Completed(completion));
        chat.stream_end(job.conversation, job.stream, Ok(true), &mut Vec::new());
        job
    }

    fn story() -> Chat {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        chat.play("w".into());
        chat.current().player = Some(serechat::Player { name: "Gale".into(), ..serechat::Player::default() });
        chat
    }

    const RUE: &str =
        r#"{"scene":"The woods.","introduce":[{"name":"Rue","description":"A girl from 11."}],"lines":[{"character":"Rue","text":"Hi."}]}"#;
    const THRESH: &str =
        r#"{"scene":"The lake.","introduce":[{"name":"Thresh","description":"Big."}],"lines":[{"character":"Thresh","text":"Yes?"}]}"#;
    const LEAVES: &str = r#"{"scene":"The lake.","lines":[{"character":"Rue","text":"Bye."}],"leave":["Rue"]}"#;

    #[test]
    fn turns_span_a_prompt_and_its_replies() {
        let mut chat = story();
        answer(&mut chat, "Hello.", RUE);
        answer(&mut chat, "Bye.", LEAVES);
        let entries = &chat.current().entries;
        assert_eq!(entries.len(), 4);
        assert_eq!((turn(entries, 0), turn(entries, 1), turn(entries, 3)), (0..2, 0..2, 2..4));
        assert_eq!(anchor(entries, &(2..4)), 3);
        assert!(has_editable(entries, &(0..2)));
    }

    #[test]
    fn delete_then_regenerate_rewinds_the_story() {
        let mut chat = story();
        answer(&mut chat, "Hello.", RUE);
        answer(&mut chat, "Bye.", LEAVES);
        assert_eq!(chat.current().scene, "The lake.");
        assert!(chat.current().cast.iter().all(|m| !m.present));

        // Deleting the last turn undoes it: Rue is back, and so is the woods.
        let mut actions = Vec::new();
        chat.delete_turn(3, &mut actions);
        let conversation = chat.current();
        assert_eq!(conversation.entries.len(), 2);
        assert_eq!(conversation.scene, "The woods.");
        assert!(conversation.cast.iter().any(|m| m.name == "Rue" && m.present));
        let cost_before = conversation.entries.iter().map(|e| e.message.cost).sum::<f64>();
        assert!(matches!(&actions[..], [Action::SaveSession(s)] if s.messages.len() == 2 && s.scene == "The woods."));

        // Regenerating the new last turn undoes its reply too, then asks again.
        let mut actions = Vec::new();
        chat.regenerate(&mut actions);
        let conversation = chat.current();
        assert!(conversation.cast.is_empty() && conversation.scene.is_empty(), "back to before the first reply");
        assert_eq!(conversation.entries.len(), 2, "the prompt and the reply on its way");
        assert!(conversation.busy());
        let Some(Action::Send(job)) = actions.pop() else { panic!("not sent") };
        assert_eq!(job.history.len(), 1, "only the prompt");
        assert!((conversation.carried_cost - cost_before).abs() < f64::EPSILON || cost_before == 0.0);

        // Nothing changes while a reply is on its way.
        chat.delete_turn(0, &mut actions);
        assert_eq!(chat.current().entries.len(), 2);
    }

    /// Completes `job` with a speak call of `args`.
    fn complete(chat: &mut Chat, job: &SendJob, args: &str) {
        let call = ToolCall { call_id: format!("c{}", job.stream), name: super::super::tools::SPEAK.into(), arguments: args.into() };
        let completion = Completion { tool_calls: vec![call], usage: serechat::Usage::new(10, 5), ..Completion::default() };
        chat.stream_event(job.conversation, job.stream, StreamEvent::Completed(completion));
        chat.stream_end(job.conversation, job.stream, Ok(true), &mut Vec::new());
    }

    #[test]
    fn regenerated_replies_are_swiped_between() {
        let mut chat = story();
        answer(&mut chat, "Hello.", RUE);
        chat.current().entries[1].message.cost = 0.5;
        let mut actions = Vec::new();
        chat.regenerate(&mut actions);
        let Some(Action::Send(job)) = actions.pop() else { panic!("not sent") };
        assert!(chat.current().cast.is_empty(), "the first reply's newcomer is undone");
        complete(&mut chat, &job, THRESH);
        let conversation = chat.current();
        assert_eq!(conversation.swipes(), Some((0, 1, 2)), "the new reply is the second of two");
        assert!(conversation.scene == "The lake." && conversation.cast.iter().map(|m| m.name.as_str()).eq(["Thresh"]));
        assert!(conversation.cost() >= 0.5, "the reply swiped away still counts");

        // Back to the first: its story comes back, the second's goes.
        let mut actions = Vec::new();
        chat.swipe_to(0, &mut actions);
        let conversation = chat.current();
        assert_eq!(conversation.swipes(), Some((0, 0, 2)));
        assert!(conversation.scene == "The woods." && conversation.cast.iter().map(|m| m.name.as_str()).eq(["Rue"]));
        assert!(matches!(&actions[..], [Action::SaveSession(s)] if s.messages[0].swipes.len() == 1 && s.messages.len() == 2));
        let entry = conversation.entries.last_mut().unwrap();
        entry.refresh_display(false, true);
        assert!(entry.display.ends_with("**Rue**: Hi."), "{}", entry.display);

        // And forward again; out of range does nothing.
        chat.swipe_to(1, &mut Vec::new());
        assert!(chat.current().scene == "The lake." && chat.current().swipes() == Some((0, 1, 2)));
        chat.swipe_to(5, &mut Vec::new());
        assert_eq!(chat.current().swipes(), Some((0, 1, 2)));

        // Regenerating again adds a third; a reply that failed is not kept.
        let mut actions = Vec::new();
        chat.regenerate(&mut actions);
        let Some(Action::Send(job)) = actions.pop() else { panic!("not sent") };
        let error = serechat::Error::Response { code: Some("invalid_request_error".into()), message: "bad".into() };
        chat.stream_end(job.conversation, job.stream, Err(error), &mut Vec::new());
        assert_eq!(chat.current().swipes(), Some((0, 2, 3)), "two kept, the failure shown");
        chat.swipe_to(0, &mut Vec::new());
        assert_eq!(chat.current().swipes(), Some((0, 0, 2)), "the failure is dropped when swiped away");
    }

    #[test]
    fn deleting_an_earlier_turn_keeps_the_story() {
        let mut chat = story();
        answer(&mut chat, "Hello.", RUE);
        answer(&mut chat, "Bye.", LEAVES);
        let mut actions = Vec::new();
        chat.delete_turn(0, &mut actions);
        let conversation = chat.current();
        assert_eq!(conversation.entries.len(), 2);
        assert_eq!(conversation.title, "Bye.", "the title follows the first prompt left");
        assert_eq!(conversation.scene, "The lake.", "later turns build on it: nothing is undone");
        assert!(conversation.cast.iter().any(|m| m.name == "Rue"));
    }

    #[test]
    fn edits_rewrite_each_characters_bubble() {
        let mut chat = story();
        let two = r#"{"scene":"The woods.","introduce":[{"name":"Rue","description":"A girl from 11."},{"name":"Thresh","description":"Her ally."}],"messages":[{"character":"Rue","text":"Hi."},{"character":"Thresh","action":"nods","text":""}]}"#;
        answer(&mut chat, "Hello.", two);
        chat.edit_turn(0);
        let edit = chat.turn_edit().expect("editing");
        let texts: Vec<(Option<&str>, &str)> = edit.slots.iter().map(|s| (s.character.as_deref(), s.text.as_str())).collect();
        assert_eq!(texts, [(Some("Rue"), "Hi."), (Some("Thresh"), "*nods*")], "a field per character");
        edit.fields.insert(" Changed.");
        let mut actions = Vec::new();
        chat.save_turn_edit(&mut actions);
        let prompt = chat.current().entries[0].message.clone();
        let entry = &mut chat.current().entries[1];
        entry.refresh_display(false, true);
        assert_eq!(entry.display, "*The woods.*\n\n*Rue joins the story*\n\n*Thresh joins the story*\n\n**Rue**: Hi. Changed.\n\n**Thresh**: *nods*", "still bubbles");
        let reply = &entry.message;
        assert!(reply.content.is_empty() && reply.tool_calls.len() == 1 && reply.change.is_some(), "deleting it still undoes what it did");
        assert!(matches!(&actions[..], [Action::SaveSession(_)]));
        let items = crate::chat::stream::input_items(&[prompt, reply.clone()]);
        assert!(matches!(&items[1], serechat::InputItem::ToolCall(call) if call.arguments.contains("Hi. Changed.")), "the model sees the edit");

        // An emptied character leaves the reply; emptied entirely, it is removed.
        chat.edit_turn(1);
        chat.turn_edit().unwrap().fields = Fields::multiline(&["Bye.".to_owned(), String::new()]);
        chat.save_turn_edit(&mut actions);
        let entry = &mut chat.current().entries[1];
        entry.refresh_display(false, true);
        assert!(entry.display.ends_with("**Rue**: Bye.") && !entry.display.contains("**Thresh**"), "{}", entry.display);
        chat.edit_turn(1);
        chat.turn_edit().unwrap().fields = Fields::multiline(&[String::new()]);
        chat.save_turn_edit(&mut actions);
        assert_eq!(chat.current().entries.len(), 1);
    }

    #[test]
    fn replies_edited_into_text_read_back_as_bubbles() {
        let mut chat = story();
        answer(&mut chat, "Hello.", RUE);
        let entry = &mut chat.current().entries[1];
        entry.message.tool_calls.clear();
        "**Rue** · *waves*\n> Hi.".clone_into(&mut entry.message.content);
        chat.edit_turn(0);
        let edit = chat.turn_edit().expect("editing");
        assert_eq!(edit.slots[0].character.as_deref(), Some("Rue"));
        edit.fields.insert(" Again.");
        chat.save_turn_edit(&mut Vec::new());
        let reply = &chat.current().entries[1].message;
        assert!(reply.content.is_empty() && reply.tool_calls[0].call.arguments.contains("*waves* Hi. Again."), "now a call");
    }
}
