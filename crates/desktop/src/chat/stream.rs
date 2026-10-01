//! Reply streaming: sending a conversation, running the narrator's tool
//! calls, retrying failed requests and compacting conversations that
//! outgrow the context window.
//!
//! A story has no narrator: its replies must call tools (see `tools.rs`),
//! which run when the reply completes; their results go back to the model
//! with the next request. A reply that only called tools and showed
//! nothing (say, it just created a character) is continued at once, up to
//! [`AUTO_ROUNDS`] times in a row.
//!
//! Failures: dropped or silent connections, rate limits and server errors are
//! retried after [`RETRY_DELAYS`]. A conversation whose last request used more
//! than [`compact_limit`] tokens, or that the server rejects as too long, is
//! first replaced (for the model only) by a summary the model writes.
//!
//! Every change is saved, so a reply interrupted by a crash, a restart or the
//! user shows a Continue button (see [`Conversation::resumable`]).

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serechat::{Completion, Error, InputItem, Role, StoredMessage, StoryChange, StreamEvent, ToolChoice, ToolResult, Usage, unix_now};

use super::tools;
use super::{Chat, Conversation, Entry, Load, Reasoning, StreamingCall};
use crate::app::Action;

/// Replies in a row that may continue on their own after only calling tools.
const AUTO_ROUNDS: u32 = 3;

/// Wait before each retry of a failed request; one retry per entry.
const RETRY_DELAYS: [Duration; 5] =
    [Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(30), Duration::from_secs(60)];
/// Retries a request gets before its error is shown.
pub(super) const MAX_RETRIES: usize = RETRY_DELAYS.len();
/// A stream this quiet is dead: the server pings every 15 seconds.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Context window assumed when the server reports none.
const DEFAULT_WINDOW: u64 = 128_000;
/// Most context a request may use before the conversation is compacted,
/// whatever the window: long prompts cost more and models lose focus.
// ponytail: fixed cap; make it a setting if people want to spend more per reply.
const MAX_CONTEXT: u64 = 250_000;

/// System instructions of a chat outside any world (from before worlds).
const PLAIN_CHAT: &str = "You are OpenRP, a helpful AI assistant in a desktop app. Answer in Markdown and keep answers focused.";
/// How the model runs a story; the world and cast follow it.
const ROLEPLAY: &str = "You play every character in an interactive roleplay story except the user's own. There is no narrator: \
    the story is told only through the characters, by what they say and do. Stay true to the world's premise, rules and tone, and \
    keep each character consistent with their description.\n\n\
    How to reply:\n\
    - Every reply is one speak call holding every line of the turn, in the order things happen. Write nothing outside tool calls.\n\
    - In each line, text is exactly what the character says, and action what they do as they say it (movement, expression, what \
    they notice). A character who only acts gets a line with an action and empty text.\n\
    - Only characters in the scene respond. Decide which of them do: whoever the user addresses, plus anyone else present who \
    would naturally react.\n\
    - Do not invent characters. Bring in someone new only when the user refers to a character who is not in the cast yet (\"I \
    walk up to the bartender\"), or when no one is in the scene to respond. Then list them in the speak call's introduce, each \
    with a description, in the same call as their first lines. Never add bystanders, newcomers or interruptions on your own.\n\
    - A cast member elsewhere acts only when the user brings them in; they are then moved into the scene. Use create_character \
    only for someone the user refers to who does not act yet.\n\
    - Keep track of the scene: where the characters are and what it is like there. When it is not set yet, or it changes (the \
    user goes somewhere, time passes, the mood or weather turns), set speak's scene to the place and its ambiance; otherwise \
    leave it empty. Cast members who stay behind or walk off go in speak's leave.\n\
    - Never speak, act or decide for the user's character. End where the user can respond.";
/// Stands in for the description of a character who has none.
const UNDESCRIBED: &str = "(Not described yet: keep them consistent with what they have said and done so far.)";
/// Added when nobody is in the scene: the first thing to do is cast someone.
const NOBODY_HERE: &str = "No one is in the scene yet. Introduce who the user meets in your speak call's introduce, each with a \
    description, and have them act in its lines: the characters the user's message names or refers to, or, if it names no one, \
    the one character the moment most needs.";

/// The system prompt of a story in `world`, played by `player`, with the
/// characters `present` in the scene and those `absent` from it (each a
/// name and description), in `scene` (empty until the model sets it).
/// `None` for a plain chat. It changes only when the story's setup or
/// scene does, so the provider can cache it.
pub(super) fn story_prompt(
    world: Option<(&str, &str)>,
    player: Option<(&str, &str)>,
    present: &[(&str, &str)],
    absent: &[(&str, &str)],
    scene: &str,
) -> String {
    let Some((name, description)) = world else {
        return PLAIN_CHAT.to_owned();
    };
    let mut prompt = format!("{ROLEPLAY}\n\n# World: {name}\n\n{description}");
    if let Some((name, description)) = player {
        let _ = write!(prompt, "\n\n# The user's character: {name}\n\n{description}");
    }
    let mut section = |title: &str, note: &str, characters: &[(&str, &str)]| {
        if !characters.is_empty() {
            let _ = write!(prompt, "\n\n# {title}\n\n{note}");
            for (name, description) in characters {
                // Someone who joined by speaking has no description yet.
                let description = if description.trim().is_empty() { UNDESCRIBED } else { description };
                let _ = write!(prompt, "\n\n## {name}\n\n{description}");
            }
        }
    };
    section("Characters in the scene", "These characters are here now; you play them.", present);
    section(
        "Characters elsewhere",
        "These characters belong to the story but are not in the current scene. They may be mentioned, but do not have them act here unless the story brings them in.",
        absent,
    );
    let scene = scene.trim();
    if !scene.is_empty() || present.is_empty() {
        prompt.push_str("\n\n# The scene");
        if !scene.is_empty() {
            let _ = write!(prompt, "\n\n{scene}");
        }
        if present.is_empty() {
            let _ = write!(prompt, "\n\n{NOBODY_HERE}");
        }
    }
    prompt
}
/// Asks for the summary that replaces a conversation's history.
const COMPACT_PROMPT: &str = "The conversation is about to exceed the context window, so everything above will be replaced by a \
    summary that you write now; the next turn sees only the summary. Write it as a handoff to yourself: the user's requests and \
    constraints (quote them where the wording matters); key facts, events and decisions so far; open threads; and where things \
    stand now. Be specific: names, places, details. Reply with the summary only.";
/// Introduces a summary when it is sent in place of the history.
const SUMMARY_INTRO: &str = "Earlier messages were replaced by this summary to fit the context window. Continue from where it leaves off.";

/// A request being streamed.
pub(super) struct ActiveStream {
    pub id: u64,
    pub cancel: Arc<AtomicBool>,
    /// Model answering, for pricing the reply.
    pub model: String,
    /// When the request went out, for timing the model's thinking.
    pub started: Instant,
    /// Entry the reply streams into.
    pub entry: u64,
    /// Retries already spent on this request.
    pub attempt: usize,
    /// Last sign of life from the server.
    pub last_event: Instant,
    /// Why the response stopped early, once it has.
    pub incomplete: Option<String>,
    /// What the server billed for it if it failed.
    pub charged: Usage,
}

/// A failed request waiting to be sent again.
pub(super) struct Retry {
    /// When to send it.
    pub at: Instant,
    /// Retries spent, including this one.
    pub attempt: usize,
    /// It asks for a summary rather than a reply.
    pub compaction: bool,
    /// What went wrong, shown while waiting.
    pub error: String,
}

/// Everything a worker thread needs to stream one reply.
pub struct SendJob {
    /// Conversation the reply belongs to.
    pub conversation: u64,
    /// Identifies this stream so late events from a stopped one are dropped.
    pub stream: u64,
    /// Model identifier.
    pub model: String,
    /// Reasoning effort, or `None` for the model default.
    pub reasoning: Option<&'static str>,
    /// System instructions: the world and cast, as they are now.
    pub instructions: String,
    /// Conversation so far.
    pub history: Vec<StoredMessage>,
    /// Offer the narrator's tools (stories do; plain chats don't).
    pub tools: bool,
    /// `tool_choice`, or `None` to let the model decide.
    pub tool_choice: Option<ToolChoice<'static>>,
    /// Raised to abort the stream.
    pub cancel: Arc<AtomicBool>,
}

/// Converts saved messages into API input, starting at the latest summary.
/// A reply's tool calls follow its text, each with its result.
#[must_use]
pub fn input_items(history: &[StoredMessage]) -> Vec<InputItem> {
    let start = history.iter().rposition(is_summary).unwrap_or(0);
    let mut items = Vec::with_capacity(history.len() - start);
    for message in history[start..].iter().filter(|m| !m.failed) {
        if message.compaction {
            items.push(InputItem::text(Role::User, format!("{SUMMARY_INTRO}\n\n{}", message.content)));
            continue;
        }
        if !message.content.is_empty() {
            items.push(InputItem::text(message.role, message.content.clone()));
        }
        for result in &message.tool_calls {
            items.push(InputItem::ToolCall(result.call.clone()));
            items.push(InputItem::ToolOutput { call_id: result.call.call_id.clone(), output: result.output.clone() });
        }
    }
    items
}

/// A finished summary, which the model's view of the conversation starts from.
fn is_summary(message: &StoredMessage) -> bool {
    message.compaction && !message.failed && !message.content.is_empty()
}

/// Tokens the conversation's next request will use at least: what the
/// latest reply after the latest summary was billed for.
fn context_used(entries: &[Entry]) -> u64 {
    let start = entries.iter().rposition(|e| is_summary(&e.message)).map_or(0, |i| i + 1);
    entries[start..]
        .iter()
        .rev()
        .find(|e| e.message.role == Role::Assistant && !e.message.failed && e.message.usage.input_tokens > 0)
        .map_or(0, |e| e.message.usage.input_tokens + e.message.usage.output_tokens)
}

/// Context use that triggers compaction for a model with `window` tokens.
fn compact_limit(window: u64) -> u64 {
    let window = if window == 0 { DEFAULT_WINDOW } else { window };
    (window / 4 * 3).min(MAX_CONTEXT)
}

impl Entry {
    /// Folds speech whose call never completed (the reply was stopped or
    /// failed) into the narration, so what the user saw stays.
    fn keep_streamed_speech(&mut self) {
        if self.streaming_calls.is_empty() {
            return;
        }
        let calls = std::mem::take(&mut self.streaming_calls);
        self.message.content = tools::display(&self.message.content, calls.iter().map(|c| (c.name.as_str(), c.arguments.as_str())), true);
    }
}

impl Conversation {
    /// The model owes a reply and nothing is on its way: the last prompt or
    /// summary was never answered, because the request failed, was stopped
    /// or the app closed. Continue sends it.
    pub(super) fn resumable(&self) -> bool {
        self.load == Load::Loaded
            && !self.busy()
            && self.entries.iter().rev().find(|e| !e.message.failed).is_some_and(|e| e.message.role == Role::User || e.message.compaction)
    }
}

impl Chat {
    /// The selected model's context window (`0` when unknown).
    fn context_window(&self) -> u64 {
        self.selected_model().map_or(0, |m| m.context_window)
    }

    /// Sends conversation `id`'s next request: a reply, or first a summary
    /// when the conversation outgrew its context budget.
    pub(super) fn request_reply(&mut self, id: u64, actions: &mut Vec<Action>) {
        let limit = compact_limit(self.context_window());
        let Some(conversation) = self.find(id) else {
            return;
        };
        let compaction = context_used(&conversation.entries) > limit;
        self.start_request(id, compaction, 0, actions);
    }

    /// Adds the entry a request streams into and starts it. `attempt`
    /// counts the retries already spent on it.
    fn start_request(&mut self, id: u64, compaction: bool, attempt: usize, actions: &mut Vec<Action>) {
        let (entry_id, stream_id) = (self.next_id(), self.next_id());
        let model = self.model.clone();
        let reasoning = Some(self.reasoning_in_use()).filter(|r| *r != Reasoning::Auto).map(Reasoning::key);
        let instructions = self.instructions(id);
        let Some(conversation) = self.find(id) else {
            return;
        };
        conversation.retry = None;
        conversation.updated = unix_now();
        let mut history: Vec<StoredMessage> = conversation.entries.iter().map(|e| e.message.clone()).collect();
        let mut message = StoredMessage::new(Role::Assistant, String::new());
        let mut at = history.len();
        if compaction {
            message.compaction = true;
            // A prompt sent just now stays word for word after the summary.
            if history.last().is_some_and(|m| m.role == Role::User && !m.failed) {
                at -= 1;
                history.truncate(at);
            }
            history.push(StoredMessage::new(Role::User, COMPACT_PROMPT.to_owned()));
        }
        conversation.entries.insert(at, Entry::new(entry_id, message));
        let cancel = Arc::new(AtomicBool::new(false));
        let now = Instant::now();
        conversation.stream = Some(ActiveStream {
            id: stream_id,
            cancel: Arc::clone(&cancel),
            model: model.clone(),
            started: now,
            entry: entry_id,
            attempt,
            last_event: now,
            incomplete: None,
            charged: Usage::default(),
        });
        actions.push(Action::SaveSession(conversation.to_session()));
        let job = SendJob {
            conversation: id,
            stream: stream_id,
            model,
            reasoning,
            instructions,
            history,
            tools: conversation.world.is_some(),
            // A story is told only through its characters, so a reply must
            // call a tool, and a repair must describe who acted undescribed;
            // a summary calls none, though tools stay declared (providers
            // reject tool history without them).
            tool_choice: if compaction {
                Some(ToolChoice::None)
            } else if conversation.repairing {
                Some(ToolChoice::Function(tools::CREATE_CHARACTER))
            } else {
                conversation.world.is_some().then_some(ToolChoice::Required)
            },
            cancel,
        };
        if id == self.current {
            self.stick_to_bottom = true;
        }
        actions.push(Action::Send(job));
    }

    fn stream_target(conversations: &mut [Conversation], conversation: u64, stream: u64) -> Option<&mut Conversation> {
        conversations.iter_mut().find(|c| c.id == conversation && c.stream.as_ref().is_some_and(|s| s.id == stream))
    }

    /// Applies one streamed update.
    pub fn stream_event(&mut self, conversation: u64, stream: u64, event: StreamEvent) {
        let Some(conversation) = Self::stream_target(&mut self.conversations, conversation, stream) else {
            return;
        };
        let Conversation { stream: Some(stream), entries, carried_cost, .. } = conversation else {
            return;
        };
        stream.last_event = Instant::now();
        let Some(entry) = entries.iter_mut().find(|e| e.id == stream.entry) else {
            return;
        };
        let message = &mut entry.message;
        let elapsed = u64::try_from(stream.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match event {
            StreamEvent::Ping => {}
            StreamEvent::ToolCallStarted { index, name } => {
                // Thinking ends where the first call starts.
                if message.content.is_empty() && message.reasoning_ms == 0 {
                    message.reasoning_ms = elapsed;
                }
                entry.streaming_calls.push(StreamingCall { index, name, arguments: String::new() });
            }
            StreamEvent::ToolCallDelta { index, delta } => {
                if let Some(call) = entry.streaming_calls.iter_mut().find(|c| c.index == index) {
                    call.arguments.push_str(&delta);
                }
            }
            StreamEvent::Charged(usage) => stream.charged = usage,
            StreamEvent::Text(delta) => {
                // Models often open with blank lines; don't render them.
                let delta = if message.content.is_empty() { delta.trim_start() } else { &delta };
                // Thinking ends where the answer starts.
                if message.content.is_empty() && !delta.is_empty() {
                    message.reasoning_ms = elapsed;
                }
                message.content.push_str(delta);
            }
            StreamEvent::Reasoning(delta) => message.reasoning.push_str(&delta),
            StreamEvent::Completed(Completion { usage, reasoning, tool_calls, incomplete }) => {
                if message.reasoning.is_empty() {
                    message.reasoning = reasoning;
                }
                // A reply of only tool calls thought until its first one;
                // one that never answered, until it completed.
                message.reasoning_ms = match (message.reasoning.is_empty(), message.content.is_empty() && message.reasoning_ms == 0) {
                    (true, _) => 0,
                    (false, true) => elapsed,
                    (false, false) => message.reasoning_ms,
                };
                // The finished calls replace their previews; they run when the stream ends.
                message.tool_calls = tool_calls.into_iter().map(|call| ToolResult { call, output: String::new() }).collect();
                entry.streaming_calls.clear();
                // Failed attempts before this reply are billed with it.
                message.cost = self.models.iter().find(|m| m.id == stream.model).map_or(0.0, |m| m.cost(usage)) + std::mem::take(carried_cost);
                message.model = Some(stream.model.clone());
                message.usage = usage;
                stream.incomplete = incomplete;
            }
        }
    }

    /// Finishes a stream: saves the reply, retries, or shows why it failed.
    /// Returns `true` if the server rejected our token.
    pub fn stream_end(&mut self, conversation: u64, stream: u64, result: Result<bool, Error>, actions: &mut Vec<Action>) -> bool {
        let unauthorized = result.as_ref().is_err_and(Error::is_unauthorized);
        let id = conversation;
        let note_id = self.next_id();
        // A stream the user stopped is already detached and ends up here.
        let Some(target) = Self::stream_target(&mut self.conversations, id, stream) else {
            return false;
        };
        let Some(active) = target.stream.take() else {
            return false;
        };
        target.updated = unix_now();
        let Some(index) = target.entries.iter().position(|e| e.id == active.entry) else {
            return false;
        };
        let message = &target.entries[index].message;
        // `server_error` and a missing code are retried; `incomplete` is not.
        let failure = |code: &str, message: &str| Err(Error::Response { code: Some(code.to_owned()), message: message.to_owned() });
        let result = match (result, active.incomplete.as_deref()) {
            (Ok(true), incomplete) if message.content.is_empty() && message.tool_calls.is_empty() => match incomplete {
                None => failure("server_error", "The model returned an empty response."),
                Some("max_output_tokens") => failure("incomplete", "The reply reached the model's output limit before it said anything."),
                Some(reason) => failure("incomplete", &format!("The provider stopped the reply early ({reason}).")),
            },
            (Ok(false), _) => Err(Error::Response { code: None, message: "The connection closed before the reply finished.".into() }),
            (other, _) => other,
        };
        match result {
            Err(error) => self.request_failed(id, &active, index, &error, actions),
            Ok(_) if target.entries[index].message.compaction => self.compacted(id, index, active.incomplete.is_some(), actions),
            Ok(_) => {
                let player = target.player.as_ref().map(|p| p.name.clone());
                let Conversation { entries, cast, scene, auto_rounds, repairing, .. } = target;
                let was_repair = std::mem::take(repairing);
                let calls = &mut entries[index].message.tool_calls;
                if let Some(reason) = &active.incomplete {
                    // Arguments cut off mid-way may be wrong: nothing runs.
                    for result in calls.iter_mut() {
                        "Not run: your reply was cut off before this call was complete.".clone_into(&mut result.output);
                    }
                    let note = if reason == "max_output_tokens" {
                        "The reply reached the model's output limit and was cut off.".to_owned()
                    } else {
                        format!("The provider stopped the reply early ({reason}).")
                    };
                    let mut message = StoredMessage::new(Role::Assistant, note);
                    message.failed = true;
                    entries.push(Entry::new(note_id, message));
                } else {
                    let (cast_before, scene_before) = (cast.clone(), scene.clone());
                    // New characters first, so they can speak in the reply that made them.
                    for creating in [true, false] {
                        for result in calls.iter_mut().filter(|r| (r.call.name == tools::CREATE_CHARACTER) == creating) {
                            result.output = tools::run(&result.call, cast, scene, player.as_deref());
                        }
                    }
                    // Kept so deleting or regenerating the reply can undo it.
                    entries[index].message.change = StoryChange::between(&cast_before, &scene_before, cast, scene);
                }
                let complete = active.incomplete.is_none() && *auto_rounds < AUTO_ROUNDS;
                // Everyone who acted this turn (since the user's message) must
                // be cast with a description: if not, the model describes them
                // next, made to call create_character.
                let turn = entries.iter().rposition(|e| e.message.role == Role::User).map_or(0, |i| i + 1);
                let calls = entries[turn..].iter().flat_map(|e| e.message.tool_calls.iter().map(|r| &r.call));
                let repair = complete && !tools::undescribed(calls, cast, player.as_deref()).is_empty();
                // A reply that only called tools (say, created a character)
                // showed nothing yet: the model carries on. A repair showed
                // its turn already.
                let message = &entries[index].message;
                let silent = message.content.trim().is_empty() && !message.tool_calls.iter().any(|r| tools::shows_speech(&r.call));
                let carry_on = complete && !repair && !was_repair && !message.tool_calls.is_empty() && silent;
                if repair || carry_on {
                    *auto_rounds += 1;
                }
                *repairing = repair;
                actions.push(Action::SaveSession(target.to_session()));
                if repair || carry_on {
                    self.request_reply(id, actions);
                }
            }
        }
        self.attention_if_waiting(id, actions);
        unauthorized
    }

    /// A request failed with `error`: compact and resend, retry later, or
    /// give up and show the error.
    fn request_failed(&mut self, id: u64, active: &ActiveStream, index: usize, error: &Error, actions: &mut Vec<Action>) {
        let note_id = self.next_id();
        let billed = self.models.iter().find(|m| m.id == active.model).map_or(0.0, |m| m.cost(active.charged));
        let Some(conversation) = self.find(id) else {
            return;
        };
        // Billed with the next reply, or with the error if none follows.
        conversation.carried_cost += billed;
        let compaction = conversation.entries[index].message.compaction;
        let can_compact = !compaction && context_used(&conversation.entries) > 0;
        // Compacting or retrying redoes the request; what streamed is dropped.
        if error.is_context_overflow() && can_compact {
            conversation.entries.remove(index);
            self.start_request(id, true, 0, actions);
            return;
        }
        if error.is_retryable() && active.attempt < MAX_RETRIES {
            conversation.entries.remove(index);
            conversation.retry =
                Some(Retry { at: Instant::now() + RETRY_DELAYS[active.attempt], attempt: active.attempt + 1, compaction, error: error.to_string() });
            return;
        }
        let text = if error.is_context_overflow() {
            "This conversation no longer fits the model's context window, even summarised. Start a new chat, or pick a model with a larger window."
                .to_owned()
        } else if compaction {
            format!("Summarising the conversation failed: {error}")
        } else {
            error.to_string()
        };
        let cost = std::mem::take(&mut conversation.carried_cost);
        let entry = &mut conversation.entries[index];
        entry.keep_streamed_speech();
        if entry.message.content.is_empty() {
            entry.message.content = text;
            entry.message.failed = true;
            entry.message.cost += cost;
            entry.doc = None;
        } else {
            let mut message = StoredMessage::new(Role::Assistant, text);
            message.failed = true;
            message.cost = cost;
            conversation.entries.insert(index + 1, Entry::new(note_id, message));
        }
        actions.push(Action::SaveSession(conversation.to_session()));
    }

    /// A summary finished streaming: send the reply it was made for. A
    /// summary that was cut off is not trusted.
    fn compacted(&mut self, id: u64, index: usize, cut_off: bool, actions: &mut Vec<Action>) {
        let Some(conversation) = self.find(id) else {
            return;
        };
        if cut_off {
            let entry = &mut conversation.entries[index];
            "Summarising the conversation failed: the summary reached the output limit.".clone_into(&mut entry.message.content);
            entry.message.failed = true;
            entry.doc = None;
            actions.push(Action::SaveSession(conversation.to_session()));
            return;
        }
        actions.push(Action::SaveSession(conversation.to_session()));
        self.start_request(id, false, 0, actions);
    }

    /// Continues a reply that failed or was interrupted.
    pub(super) fn resume(&mut self, id: u64, actions: &mut Vec<Action>) {
        if self.find(id).is_some_and(|c| c.resumable()) {
            self.request_reply(id, actions);
        }
    }

    /// Stops the current conversation's stream and retry, keeping partial output.
    pub(super) fn stop(&mut self, actions: &mut Vec<Action>) {
        let conversation = self.current();
        let mut stopped = conversation.retry.take().is_some();
        if let Some(stream) = conversation.stream.take() {
            stream.cancel.store(true, Ordering::Relaxed);
            if let Some(entry) = conversation.entries.iter_mut().find(|e| e.id == stream.entry) {
                entry.keep_streamed_speech();
            }
            conversation.entries.retain(|e| e.id != stream.entry || !e.message.content.is_empty() || !e.message.tool_calls.is_empty());
            stopped = true;
        }
        if stopped {
            actions.push(Action::SaveSession(conversation.to_session()));
        }
    }

    /// Sends due retries and gives up on streams that went silent.
    pub fn tick(&mut self, now: Instant, actions: &mut Vec<Action>) {
        let mut due = Vec::new();
        let mut silent = Vec::new();
        for c in &self.conversations {
            if let Some(retry) = c.retry.as_ref().filter(|r| r.at <= now) {
                due.push((c.id, retry.compaction, retry.attempt));
            }
            if let Some(stream) = c.stream.as_ref().filter(|s| now.saturating_duration_since(s.last_event) > IDLE_TIMEOUT) {
                stream.cancel.store(true, Ordering::Relaxed);
                silent.push((c.id, stream.id));
            }
        }
        for (id, compaction, attempt) in due {
            self.start_request(id, compaction, attempt, actions);
        }
        for (id, stream) in silent {
            let quiet = Error::Response { code: None, message: "The connection went quiet.".into() };
            self.stream_end(id, stream, Err(quiet), actions);
        }
    }

    /// When [`Chat::tick`] next has something to do.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        let deadline = |c: &Conversation| c.retry.as_ref().map(|r| r.at).or_else(|| c.stream.as_ref().map(|s| s.last_event + IDLE_TIMEOUT));
        self.conversations.iter().filter_map(deadline).min()
    }

    /// Asks for the user's attention when conversation `id` finished or failed.
    fn attention_if_waiting(&mut self, id: u64, actions: &mut Vec<Action>) {
        if self.find(id).is_some_and(|c| !c.busy()) {
            actions.push(Action::Attention);
        }
    }

    /// Whether anything streams, which animates the screen.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.conversations.iter().any(|c| c.stream.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn story_prompts() {
        assert_eq!(story_prompt(None, None, &[], &[], ""), PLAIN_CHAT);
        // Nobody in the scene: the model is told to cast someone first.
        let alone = story_prompt(Some(("Panem", "Twelve districts.")), None, &[], &[], "");
        assert!(alone.starts_with(ROLEPLAY) && alone.ends_with(&format!("# World: Panem\n\nTwelve districts.\n\n# The scene\n\n{NOBODY_HERE}")));
        let away = story_prompt(Some(("Panem", "")), Some(("Gale", "A hunter.")), &[], &[("Peeta", "A baker.")], "");
        assert!(!away.contains("# Characters in the scene") && away.contains("# The user's character: Gale\n\nA hunter."));
        assert!(away.contains("## Peeta\n\nA baker.") && away.ends_with(NOBODY_HERE), "someone elsewhere is no one here");
        let here = story_prompt(Some(("Panem", "")), None, &[("Katniss", "A hunter.")], &[], "");
        assert!(here.ends_with("## Katniss\n\nA hunter.") && !here.contains(NOBODY_HERE));
        let joined = story_prompt(Some(("Panem", "")), None, &[("Rue", " ")], &[], "");
        assert!(joined.ends_with(&format!("## Rue\n\n{UNDESCRIBED}")), "someone who joined by speaking");
        let kitchen = story_prompt(Some(("Panem", "")), None, &[("Katniss", "A hunter.")], &[], " The bakery, before dawn. ");
        assert!(kitchen.ends_with("## Katniss\n\nA hunter.\n\n# The scene\n\nThe bakery, before dawn."));
    }

    #[test]
    fn compaction_budget() {
        assert_eq!(compact_limit(0), 96_000);
        assert_eq!(compact_limit(200_000), 150_000);
        assert_eq!(compact_limit(1_000_000), MAX_CONTEXT);
    }

    #[test]
    fn history_starts_at_the_latest_summary() {
        let mut summary = StoredMessage::new(Role::Assistant, "did A".into());
        summary.compaction = true;
        let mut failed = summary.clone();
        failed.failed = true;
        let history = [StoredMessage::new(Role::User, "old".into()), summary, StoredMessage::new(Role::User, "next".into()), failed];
        let items = input_items(&history);
        assert_eq!(items.len(), 2, "old messages and the failed summary are left out");
        assert!(matches!(&items[0], InputItem::Message { role: Role::User, text } if text.ends_with("did A")));
    }

    #[test]
    fn tool_calls_go_back_with_their_results() {
        let mut reply = StoredMessage::new(Role::Assistant, String::new());
        let call = serechat::ToolCall { call_id: "c".into(), name: tools::SPEAK.into(), arguments: "{}".into() };
        reply.tool_calls.push(ToolResult { call, output: "Spoken.".into() });
        let items = input_items(&[StoredMessage::new(Role::User, "Hi".into()), reply]);
        assert_eq!(items.len(), 3, "a reply of only calls sends no empty text");
        assert!(matches!(&items[1], InputItem::ToolCall(call) if call.call_id == "c"));
        assert!(matches!(&items[2], InputItem::ToolOutput { call_id, output } if call_id == "c" && output == "Spoken."));
    }
}
