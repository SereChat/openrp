//! Reply streaming: sending a conversation, running a story's tool
//! calls, retrying failed requests and compacting conversations that
//! outgrow the context window.
//!
//! A story has no narrator: its replies must call tools (see `tools.rs`),
//! which run when the reply completes; their results go back to the model
//! with the next request. A reply that only called tools and showed
//! nothing (say, it just created a character) is continued at once, up to
//! [`AUTO_ROUNDS`] times in a row. Every [`REVIEW_EVERY`] prompts, a separate
//! request reads the latest turns and adds what the story must not forget to
//! its memories.
//!
//! The system prompt holds what rarely changes (the rules, the world, who the
//! user plays and the whole cast, with their example dialogue and the lore
//! that always holds), so providers can cache it with the history after it;
//! the story state that changes every turn (the scene, who is in it, the lore
//! the latest turns mention, memories, the author's note) rides on the latest
//! message only and is never saved (see [`story_state`]). Cards' `{{user}}`
//! and `{{char}}` are filled in as both are written.
//!
//! Failures: dropped or silent connections, rate limits and server errors are
//! retried after [`RETRY_DELAYS`] (or as long as the server asks). A
//! conversation whose last request used more than [`compact_limit`] tokens,
//! or that the server rejects as too long, is first replaced (for the model
//! only) by a summary the model writes; the latest turns stay word for word
//! after it (see [`summary_cut`]).
//!
//! Every change is saved, so a reply interrupted by a crash, a restart or the
//! user shows a Continue button (see [`Conversation::resumable`]).

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serechat::{
    CastMember, Completion, Error, InputItem, LoreEntry, Player, Role, StoredMessage, StoryChange, StreamEvent, ToolCall, ToolChoice, ToolResult, Usage,
    macros, new_id, unix_now,
};

use super::tools;
use super::{Chat, Conversation, Entry, Load, StreamingCall};
use crate::app::Action;
use crate::library::Kind;

/// Replies in a row that may continue on their own after only calling tools.
const AUTO_ROUNDS: u32 = 3;

/// Prompts after which a story's memories are reviewed on their own.
// ponytail: fixed; make it a setting if people want it sooner or later.
const REVIEW_EVERY: usize = 8;
/// Most characters of the latest turns a review reads. A story that was
/// never reviewed (one from before reviews) is read from its end.
const REVIEW_CHARS: usize = 40_000;
/// A review this slow is given up; its turns are read again next time.
const REVIEW_TIMEOUT: Duration = Duration::from_secs(180);
/// Turns before the latest kept word for word after a summary, within
/// [`keep_budget`].
const KEEP_TURNS: usize = 3;
/// The error code of a story reply that wrote text instead of calling its
/// tools: it is retried once, then shown.
const NO_TOOL_CALL: &str = "no_tool_call";

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
    The latest message ends with the story state: the scene, who is in it, who is elsewhere, lore the latest turns call for, what \
    the story must not forget and the user's author's note. The app writes it, not the user, and it is always current: trust it \
    over older turns.\n\n\
    How to reply:\n\
    - Every reply is one speak call. Write nothing outside tool calls: there is no narration, ever. Never describe events, \
    surroundings or the passing of time in your own voice; the scene field holds the place and its ambiance, and everything \
    else is shown through what the characters say and do.\n\
    - Each responding character gets exactly one message in the speak call, holding everything they say and do this turn. \
    Never give a character two messages. In it, action is what they do as they respond (movement, expression, what they \
    notice) and text exactly what they say; a short action between their words goes in *asterisks*. A character who only \
    acts gets an action and empty text.\n\
    - Only characters in the scene respond. Decide which of them do: whoever the user addresses, plus anyone else present who \
    would naturally react.\n\
    - Do not invent characters. Bring in someone new only when the user refers to a character who is not in the cast yet (\"I \
    walk up to the bartender\"), or when no one is in the scene to respond. Then list them in the speak call's introduce, each \
    with a description, in the same call as their first lines. Never add bystanders, newcomers or interruptions on your own.\n\
    - A cast member elsewhere acts only when the user brings them in; they are then moved into the scene. Use create_character \
    only for someone the user refers to who does not act yet.\n\
    - Call every character by their name in the cast. When the story changes someone for good (they reveal their real name, \
    take a new one, or their role or appearance changes), call update_character.\n\
    - Keep track of the scene: where the characters are and what it is like there. When it is not set yet, or it changes (the \
    user goes somewhere, time passes, the mood or weather turns), set speak's scene to the place and its ambiance; otherwise \
    leave it empty. Cast members who stay behind or walk off go in speak's leave.\n\
    - When something happens that the story must not forget (a promise, a secret revealed, a decision, an injury, a changed \
    relationship), call remember in the same reply with short facts. Never repeat what the memories already hold.\n\
    - A user message may end with an out-of-character instruction, marked OOC: it is the user talking to you, not their \
    character speaking or acting. Follow it in this reply.\n\
    - Follow the author's note, if there is one, in every reply.\n\
    - Never speak, act or decide for the user's character. End where the user can respond.";
/// Opens the cast section of a story's prompt.
const CAST: &str = "The characters you play. The story state says which of them are in the scene now.";
/// Opens a character's example dialogue.
const EXAMPLES: &str = "Example dialogue, showing their voice and manner. It never happened in this story.";
/// Opens the lore that always holds, in the prompt.
const LORE_ALWAYS: &str = "Background that always holds in this story. Use it where it fits; never recite it.";
/// Opens the lore the latest turns mention, in the story state.
const LORE: &str = "Background on what the latest turns mention. Use it where it fits; never recite it.";
/// Stands first in a history that opens with a character's greeting:
/// some providers want a conversation to start with the user.
const STORY_BEGINS: &str = "(The story begins.)";
/// Prompts back from the latest whose turns are searched for lore keys.
const LORE_TURNS: usize = 2;
/// Most characters of lore the story state carries; entries past it wait.
// ponytail: fixed, in characters; make it a token budget setting if
// people load large lorebooks.
const LORE_BUDGET: usize = 8_000;
/// Opens the story state sent with the latest message.
const STATE: &str = "[Story state, kept by the app: not part of the user's message]";
/// Opens the memories section of the story state.
const MEMORIES: &str = "Facts this story must not forget, kept with remember. They hold even when the conversation above no longer shows them.";
/// Opens the author's note section of the story state.
const NOTE: &str = "The user's guidance for the whole story. Follow it in every reply.";
/// Introduces an out-of-character instruction sent with a user message.
const OOC: &str = "(OOC: the user's instruction for this reply, not something their character says or does.)";
/// Opens the list of cast members away from the scene.
const ELSEWHERE: &str = "Part of the story, but not in the current scene. They may be mentioned, but do not have them act here unless the \
    user brings them in.";
/// Stands in for the description of a character who has none.
const UNDESCRIBED: &str = "(Not described yet: keep them consistent with what they have said and done so far.)";
/// Added when nobody is in the scene: the first thing to do is cast someone.
const NOBODY_HERE: &str = "No one is in the scene yet. Introduce who the user meets in your speak call's introduce, each with a \
    description, and have them act in its lines: the characters the user's message names or refers to, or, if it names no one, \
    the one character the moment most needs.";

/// `(Formerly called …)` for someone with former `aliases`; empty without.
fn formerly(aliases: &[String]) -> String {
    if aliases.is_empty() { String::new() } else { format!("(Formerly called {}; they are the same person.)\n\n", aliases.join(", ")) }
}

/// A world as a story's prompt reads it: name, description and lore.
pub(super) type WorldText<'a> = (&'a str, &'a str, &'a [LoreEntry]);

/// The system prompt of a story in `world`, played by `player`, with this
/// `cast`; `None` for a plain chat. It holds only what rarely changes, so
/// providers can cache it: who is in the scene, the lore the latest turns
/// mention, memories and the note go in [`story_state`].
pub(super) fn story_prompt(world: Option<WorldText<'_>>, player: Option<&Player>, cast: &[CastMember]) -> String {
    let Some((name, description, lore)) = world else {
        return PLAIN_CHAT.to_owned();
    };
    let user = player.map_or("the user", |p| p.name.as_str());
    let mut prompt = format!("{ROLEPLAY}\n\n# World: {name}\n\n{}", macros(description, None, user));
    if let Some(player) = player {
        let _ = write!(prompt, "\n\n# The user's character: {}\n\n{}{}", player.name, formerly(&player.aliases), macros(&player.description, None, user));
    }
    if !cast.is_empty() {
        let _ = write!(prompt, "\n\n# Cast\n\n{CAST}");
        for member in cast {
            // Someone who joined by speaking has no description yet.
            let description = if member.description.trim().is_empty() { UNDESCRIBED } else { &member.description };
            let name = Some(member.name.as_str());
            let _ = write!(prompt, "\n\n## {}\n\n{}{}", member.name, formerly(&member.aliases), macros(description, name, user));
            let examples = examples(&member.examples);
            if !examples.is_empty() {
                let _ = write!(prompt, "\n\n### How {} talks\n\n{EXAMPLES}\n\n{}", member.name, macros(&examples, name, user));
            }
        }
    }
    // Lore that holds whatever is said: the world's, then each member's.
    let always: Vec<String> = lore_of(lore, cast).filter(|(_, e)| e.constant).map(|(who, e)| macros(&e.content, who, user).into_owned()).collect();
    if !always.is_empty() {
        let _ = write!(prompt, "\n\n# Lore\n\n{LORE_ALWAYS}\n\n{}", always.join("\n\n"));
    }
    prompt.trim_end().to_owned()
}

/// Example dialogue as the model reads it: card separators (`<START>`) and
/// blank runs dropped.
fn examples(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().eq_ignore_ascii_case("<start>")).collect();
    lines.join("\n").split("\n\n\n").collect::<Vec<_>>().join("\n\n").trim().to_owned()
}

/// Every lore entry of a story, the world's (`lore`) and then each cast
/// member's, with the name `{{char}}` means in it.
fn lore_of<'a>(lore: &'a [LoreEntry], cast: &'a [CastMember]) -> impl Iterator<Item = (Option<&'a str>, &'a LoreEntry)> {
    lore.iter().map(|e| (None, e)).chain(cast.iter().flat_map(|m| m.lore.iter().map(move |e| (Some(m.name.as_str()), e))))
}

/// The lore a story's latest turns call for: entries (not constant ones,
/// which the prompt holds) with a key in `text`, the turns as plain text
/// with the scene, filled in for `user`, within [`LORE_BUDGET`].
pub(super) fn lore_in_play(lore: &[LoreEntry], cast: &[CastMember], text: &str, user: &str) -> Vec<String> {
    let text = text.to_lowercase();
    let mut spent = 0;
    let mut found = Vec::new();
    for (who, entry) in lore_of(lore, cast).filter(|(_, e)| !e.constant && e.mentioned_in(&text)) {
        let content = macros(&entry.content, who, user).into_owned();
        if spent + content.len() > LORE_BUDGET {
            continue;
        }
        spent += content.len();
        found.push(content);
    }
    found
}

/// Where the turns searched for lore start: [`LORE_TURNS`] prompts back.
pub(super) fn lore_start(entries: &[Entry]) -> usize {
    let prompts: Vec<usize> = entries.iter().enumerate().filter(|(_, e)| e.message.is_prompt()).map(|(i, _)| i).collect();
    prompts.len().checked_sub(LORE_TURNS).map_or(0, |at| prompts[at])
}

/// What a story is like right now, sent after the latest message: the
/// `scene`, who of the `cast` is in it and who is elsewhere, the `lore` the
/// latest turns call for, the `memories` and the author's `note`, which
/// weighs most there, at the end.
pub(super) fn story_state(cast: &[CastMember], lore: &[String], memories: &[String], scene: &str, note: &str) -> String {
    let mut state = STATE.to_owned();
    let scene = scene.trim();
    let _ = write!(state, "\n\n# The scene\n\n{}", if scene.is_empty() { "Not set yet: set it in your speak call." } else { scene });
    let present: Vec<&str> = cast.iter().filter(|m| m.present).map(|m| m.name.as_str()).collect();
    if present.is_empty() {
        let _ = write!(state, "\n\n# In the scene\n\n{NOBODY_HERE}");
    } else {
        let _ = write!(state, "\n\n# In the scene\n\n{}", present.join(", "));
    }
    let absent: Vec<&str> = cast.iter().filter(|m| !m.present).map(|m| m.name.as_str()).collect();
    if !absent.is_empty() {
        let _ = write!(state, "\n\n# Elsewhere\n\n{ELSEWHERE} {}", absent.join(", "));
    }
    if !lore.is_empty() {
        let _ = write!(state, "\n\n# Lore\n\n{LORE}\n\n{}", lore.join("\n\n"));
    }
    if !memories.is_empty() {
        let _ = write!(state, "\n\n# Memories\n\n{MEMORIES}\n");
        for memory in memories {
            let _ = write!(state, "\n- {memory}");
        }
    }
    let note = note.trim();
    if !note.is_empty() {
        let _ = write!(state, "\n\n# Author's note\n\n{NOTE}\n\n{note}");
    }
    state
}

/// Adds the story `state` to the end of what the model reads last: the
/// latest prompt, or the result of the latest call when a reply carries on
/// by itself (so turns keep alternating, as some models require).
pub fn attach_state(items: &mut Vec<InputItem>, state: &str) {
    match items.last_mut() {
        Some(InputItem::Message { role: Role::User, text }) => {
            text.push_str("\n\n");
            text.push_str(state);
        }
        Some(InputItem::ToolOutput { output, .. }) => {
            output.push_str("\n\n");
            output.push_str(state);
        }
        _ => items.push(InputItem::text(Role::User, state)),
    }
}

/// A user message as the model reads it: what their character says and
/// does, then any out-of-character instruction (see [`tools::split_ooc`]).
fn user_text(text: &str) -> String {
    match tools::split_ooc(text) {
        ("", Some(ooc)) => format!("{OOC}\n\n{ooc}"),
        (said, Some(ooc)) => format!("{said}\n\n{OOC}\n\n{ooc}"),
        (said, None) => said.to_owned(),
    }
}
/// Asks for the summary that replaces a conversation's history.
const COMPACT_PROMPT: &str = "The conversation is about to exceed the context window, so everything above will be replaced by a \
    summary that you write now; the next turn sees only the summary. Write it as a handoff to yourself: the user's requests and \
    constraints (quote them where the wording matters); key facts, events and decisions so far; open threads; and where things \
    stand now. Be specific: names, places, details. Reply with the summary only.";
/// Asks for the summary that replaces a story's earlier turns.
const COMPACT_STORY: &str = "The story is about to outgrow the context window, so the turns above will be replaced by a summary \
    that you write now (the latest turns stay word for word after it). Write it for yourself, to carry on the roleplay as if you \
    remembered everything: what happened, in order, with names, places and details; where each character stands now (what they \
    know, want and feel, their relationships, injuries and belongings); how each of them speaks; promises, secrets and open \
    threads; and where the scene is. Leave out what the memories already hold. Reply with the summary only, as plain text: call \
    no tools.";
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
    /// A story reply wrote text outside its calls (dropped: no narration).
    pub wrote_text: bool,
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
    /// The story state, sent after the latest message; `None` outside a story.
    pub state: Option<String>,
    /// Offer the story's tools (stories do; plain chats don't).
    pub tools: bool,
    /// `tool_choice`, or `None` to let the model decide.
    pub tool_choice: Option<ToolChoice<'static>>,
    /// Raised to abort the stream.
    pub cancel: Arc<AtomicBool>,
}

impl SendJob {
    /// The request's input: the history, with the story state after it.
    #[must_use]
    pub fn input(&self) -> Vec<InputItem> {
        let mut items = input_items(&self.history);
        if let Some(state) = &self.state {
            attach_state(&mut items, state);
        }
        items
    }
}

/// Everything a worker thread needs to review a story's memories.
pub struct ReviewJob {
    /// Conversation it is for.
    pub conversation: u64,
    /// Identifies the request, so a stale result is dropped.
    pub request: u64,
    /// Model identifier.
    pub model: String,
    /// Reasoning effort, or `None` for the model default.
    pub reasoning: Option<&'static str>,
    /// The reviewer's system prompt.
    pub instructions: String,
    /// The latest turns, as plain text.
    pub transcript: String,
    /// Raised to give up on the review.
    pub cancel: Arc<AtomicBool>,
}

/// A memory review on its way.
pub(super) struct Review {
    /// Its request id.
    pub request: u64,
    /// The model reviewing, for pricing it.
    pub model: String,
    /// Raised to give up on it.
    pub cancel: Arc<AtomicBool>,
    /// When it is given up.
    pub deadline: Instant,
    /// The prompts reviewed before it, restored if it fails.
    pub before: usize,
}

/// Converts saved messages into API input, starting at the latest summary
/// and the turns it kept word for word. A reply's tool calls follow its
/// text, each with its result.
#[must_use]
pub fn input_items(history: &[StoredMessage]) -> Vec<InputItem> {
    let (summary, kept, after) = match history.iter().rposition(is_summary) {
        Some(at) => (Some(&history[at]), &history[at - history[at].kept.min(at)..at], &history[at + 1..]),
        None => (None, &history[..0], history),
    };
    let mut items = Vec::with_capacity(kept.len() + after.len() + 2);
    // The summary first, then the turns it kept, then what came after.
    if let Some(summary) = summary {
        items.push(InputItem::text(Role::User, format!("{SUMMARY_INTRO}\n\n{}", summary.content)));
    }
    // A story opened by a character's greeting starts with the user too.
    if summary.is_none() && kept.iter().chain(after).find(|m| !m.failed && !m.compaction).is_some_and(|m| m.role != Role::User) {
        items.push(InputItem::text(Role::User, STORY_BEGINS));
    }
    for message in kept.iter().chain(after).filter(|m| !m.failed) {
        if message.compaction {
            continue;
        }
        if message.role == Role::User {
            items.push(InputItem::text(Role::User, user_text(&message.content)));
        } else if !message.content.is_empty() {
            items.push(InputItem::text(message.role, message.content.clone()));
        }
        for result in &message.tool_calls {
            items.push(InputItem::ToolCall(result.call.clone()));
            items.push(InputItem::ToolOutput { call_id: result.call.call_id.clone(), output: result.output.clone() });
        }
    }
    items
}

/// Where a memory review starts reading: the first entry after the first
/// `read` prompts of `entries` and their replies.
pub(super) fn first_unread(entries: &[Entry], read: usize) -> usize {
    if read == 0 {
        return 0;
    }
    entries.iter().enumerate().filter(|(_, e)| e.message.is_prompt()).nth(read).map_or(entries.len(), |(i, _)| i)
}

/// A finished summary, which the model's view of the conversation starts from.
pub(super) fn is_summary(message: &StoredMessage) -> bool {
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

/// Characters of latest turns a summary may keep word for word under a
/// compaction `limit`: a quarter of it, at about four characters a token.
fn keep_budget(limit: u64) -> usize {
    usize::try_from(limit).unwrap_or(usize::MAX)
}

/// Characters a message sends the model (its text, calls and results).
fn size(messages: &[StoredMessage]) -> usize {
    let calls = |m: &StoredMessage| m.tool_calls.iter().map(|r| r.call.arguments.len() + r.output.len()).sum::<usize>();
    messages.iter().filter(|m| !m.failed).map(|m| m.content.len() + calls(m)).sum()
}

/// Where a summary written now, before `history[at]`, stops: the start of
/// the turns it leaves word for word after it. A turn still under way (no
/// new prompt at `at`) is kept whole; up to [`KEEP_TURNS`] turns before it
/// join while they fit in `budget` characters, never reaching back past
/// the previous summary. Something is always left to summarise.
fn summary_cut(history: &[StoredMessage], at: usize, budget: usize) -> usize {
    let floor = history[..at].iter().rposition(is_summary).map_or(0, |s| s + 1);
    let prompts: Vec<usize> = (floor..at).filter(|&i| history[i].is_prompt()).collect();
    let under_way = !history.get(at).is_some_and(StoredMessage::is_prompt);
    let mandatory = if under_way { prompts.last().copied().unwrap_or(at) } else { at };
    let (mut cut, mut used) = (mandatory, 0);
    for &start in prompts.iter().rev().filter(|&&p| p < mandatory).take(KEEP_TURNS) {
        used += size(&history[start..cut]);
        if used > budget {
            break;
        }
        cut = start;
    }
    // Without an earlier summary, the summary needs messages of its own:
    // give up kept turns, oldest first, until it has some.
    let summarises = |cut: usize| floor > 0 || history[..cut].iter().any(|m| !m.failed);
    if !summarises(cut) {
        cut = prompts.iter().copied().find(|&p| p > cut && p <= mandatory && summarises(p)).unwrap_or(at);
    }
    cut
}

impl Entry {
    /// Keeps calls that never completed (the reply was stopped or failed)
    /// as calls that did not run, their arguments closed into valid JSON,
    /// so the speech the user saw stays.
    fn keep_streamed_speech(&mut self) {
        for call in std::mem::take(&mut self.streaming_calls) {
            let Some(arguments) = tools::parse_partial(&call.arguments) else { continue };
            let call = ToolCall { call_id: format!("call_{}", new_id()), name: call.name, arguments: arguments.to_string() };
            let output = "Not run: the reply stopped before this call was complete.".to_owned();
            self.message.tool_calls.push(ToolResult { call, output });
        }
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

    /// Has the model add what the latest turns taught the story to its
    /// memories, when [`REVIEW_EVERY`] prompts went by since the last time
    /// or `force`d (and there is a prompt to read). The request runs apart
    /// from the story: the user can carry on. A review that fails or takes
    /// longer than [`REVIEW_TIMEOUT`] leaves its turns for the next one.
    pub(super) fn review_memories(&mut self, id: u64, force: bool, actions: &mut Vec<Action>) {
        let request = self.next_id();
        let (model, reasoning) = self.side_model();
        let Some(conversation) = self.conversations.iter().find(|c| c.id == id) else {
            return;
        };
        if conversation.world.is_none() || conversation.load != Load::Loaded || conversation.reviewing.is_some() {
            return;
        }
        let from = first_unread(&conversation.entries, conversation.reviewed);
        let unread: Vec<StoredMessage> = conversation.entries[from..].iter().map(|e| e.message.clone()).collect();
        let prompts = unread.iter().filter(|m| m.is_prompt()).count();
        if prompts == 0 || (!force && prompts < REVIEW_EVERY) {
            return;
        }
        let world = conversation.world.as_deref().and_then(|w| self.library.get(Kind::World, w)).map(|w| (w.name.as_str(), w.description.as_str()));
        let cast: Vec<&str> = conversation.cast.iter().map(|m| m.name.as_str()).collect();
        let player = conversation.player.as_ref().map_or("The user", |p| p.name.as_str());
        let transcript = tools::transcript(&unread, player, REVIEW_CHARS);
        let instructions = tools::reviewer_prompt(world, conversation.player.as_ref().map(|p| p.name.as_str()), &cast, &conversation.memories);
        let cancel = Arc::new(AtomicBool::new(false));
        let job = ReviewJob { conversation: id, request, model: model.clone(), reasoning, instructions, transcript, cancel: Arc::clone(&cancel) };
        if let Some(conversation) = self.find(id) {
            let before = conversation.reviewed;
            conversation.reviewed = before + prompts;
            conversation.reviewing = Some(Review { request, model, cancel, deadline: Instant::now() + REVIEW_TIMEOUT, before });
            actions.push(Action::ReviewMemories(job));
        }
    }

    /// The review of `request` answered: what it found joins the story's
    /// memories (once each), and the request is billed with its next reply.
    /// A failed one leaves its turns for the next review. An answer for a
    /// story deleted, or given up on since, is dropped.
    pub fn memories_reviewed(&mut self, conversation: u64, request: u64, result: Result<(Option<ToolCall>, Usage), Error>, actions: &mut Vec<Action>) {
        let waiting = |c: &Conversation| c.id == conversation && c.reviewing.as_ref().is_some_and(|r| r.request == request);
        let Some(index) = self.conversations.iter().position(waiting) else {
            return;
        };
        let Some(review) = self.conversations[index].reviewing.take() else {
            return;
        };
        let Ok((call, usage)) = result else {
            self.conversations[index].reviewed = review.before;
            return;
        };
        let cost = self.models.iter().find(|m| m.id == review.model).map_or(0.0, |m| m.cost(usage));
        let conversation = &mut self.conversations[index];
        conversation.carried_cost += cost;
        if let Some(call) = call.filter(|c| c.name == tools::REMEMBER) {
            tools::run(&call, &mut Vec::new(), &mut String::new(), &mut conversation.memories, None);
        }
        if conversation.load == Load::Loaded {
            actions.push(Action::SaveSession(conversation.to_session()));
        }
    }

    /// Adds the entry a request streams into and starts it. `attempt`
    /// counts the retries already spent on it.
    fn start_request(&mut self, id: u64, compaction: bool, attempt: usize, actions: &mut Vec<Action>) {
        let (entry_id, stream_id) = (self.next_id(), self.next_id());
        // A summary is side work, for the background model if it can hold
        // as much as the story's.
        let (model, reasoning) = if compaction { self.summary_model() } else { (self.model.clone(), self.reasoning_key()) };
        let instructions = self.instructions(id);
        let state = self.state(id);
        let budget = keep_budget(compact_limit(self.context_window()));
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
            // A prompt sent just now stays word for word after the summary,
            // and so do the latest turns before it.
            if history.last().is_some_and(StoredMessage::is_prompt) {
                at -= 1;
            }
            let cut = summary_cut(&history, at, budget);
            message.kept = at - cut;
            history.truncate(cut);
            let ask = if conversation.world.is_some() { COMPACT_STORY } else { COMPACT_PROMPT };
            history.push(StoredMessage::new(Role::User, ask.to_owned()));
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
            wrote_text: false,
        });
        actions.push(Action::SaveSession(conversation.to_session()));
        let job = SendJob {
            conversation: id,
            stream: stream_id,
            model,
            reasoning,
            instructions,
            history,
            state,
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
        if self.is_impersonation(conversation, stream) {
            self.impersonated(event);
            return;
        }
        let Some(conversation) = Self::stream_target(&mut self.conversations, conversation, stream) else {
            return;
        };
        let story = conversation.world.is_some();
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
            // A story has no narrator: what its replies write outside their
            // calls is dropped. A summary is text, though.
            StreamEvent::Text(delta) if story && !message.compaction => stream.wrote_text |= !delta.trim().is_empty(),
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
        if self.is_impersonation(conversation, stream) {
            return self.impersonation_ended(&result);
        }
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
                // It wrote, but not through the characters: tried once more.
                None if active.wrote_text => failure(
                    NO_TOOL_CALL,
                    "The model answered in plain text instead of through the characters, so there is nothing to show. Some models \
                     don't follow the story's tools reliably: try again, or pick another model.",
                ),
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
                let player = target.player.clone();
                let Conversation { entries, cast, scene, memories, auto_rounds, repairing, .. } = target;
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
                    let (cast_before, scene_before, known) = (cast.clone(), scene.clone(), memories.len());
                    // New characters first, so they can speak in the reply that made them.
                    for creating in [true, false] {
                        for result in calls.iter_mut().filter(|r| (r.call.name == tools::CREATE_CHARACTER) == creating) {
                            result.output = tools::run(&result.call, cast, scene, memories, player.as_ref());
                        }
                    }
                    // Kept so deleting or regenerating the reply can undo it.
                    entries[index].message.change = StoryChange::between(&cast_before, &scene_before, cast, scene, &memories[known..]);
                }
                let complete = active.incomplete.is_none() && *auto_rounds < AUTO_ROUNDS;
                // Everyone who acted this turn (since the user's message) must
                // be cast with a description: if not, the model describes them
                // next, made to call create_character.
                let turn = entries.iter().rposition(|e| e.message.role == Role::User).map_or(0, |i| i + 1);
                let calls = entries[turn..].iter().flat_map(|e| e.message.tool_calls.iter().map(|r| &r.call));
                let repair = complete && !tools::undescribed(calls, cast, player.as_ref()).is_empty();
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
                } else if active.incomplete.is_none() {
                    self.review_memories(id, false, actions);
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
        // A reply that skipped the tools gets one more chance.
        let skipped_tools = error.code() == Some(NO_TOOL_CALL) && active.attempt == 0;
        if (error.is_retryable() || skipped_tools) && active.attempt < MAX_RETRIES {
            conversation.entries.remove(index);
            // As long as the server asks, if it says.
            let wait = error.retry_after().unwrap_or(RETRY_DELAYS[active.attempt]);
            conversation.retry = Some(Retry { at: Instant::now() + wait, attempt: active.attempt + 1, compaction, error: error.to_string() });
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
        if entry.message.content.is_empty() && entry.message.tool_calls.is_empty() {
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

    /// Stops the current conversation's stream and retry, keeping partial
    /// output, and the AI writing the user's message.
    pub(super) fn stop(&mut self, actions: &mut Vec<Action>) {
        if self.impersonating() {
            self.stop_impersonating();
        }
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

    /// Sends due retries and gives up on streams that went silent and on
    /// memory reviews that take too long.
    pub fn tick(&mut self, now: Instant, actions: &mut Vec<Action>) {
        let mut due = Vec::new();
        let mut silent = Vec::new();
        for c in &mut self.conversations {
            if c.reviewing.as_ref().is_some_and(|r| r.deadline <= now)
                && let Some(review) = c.reviewing.take()
            {
                review.cancel.store(true, Ordering::Relaxed);
                c.reviewed = review.before;
            }
        }
        self.notices.retain(|n| n.until > now);
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
        let reviews = self.conversations.iter().filter_map(|c| c.reviewing.as_ref().map(|r| r.deadline));
        let notices = self.notices.iter().map(|n| n.until);
        self.conversations.iter().filter_map(deadline).chain(reviews).chain(notices).min()
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
        self.impersonation.is_some() || self.conversations.iter().any(|c| c.stream.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn story_prompts() {
        let member = |name: &str, description: &str, present: bool| CastMember {
            id: name.to_lowercase(),
            name: name.into(),
            description: description.into(),
            present,
            ..CastMember::default()
        };
        assert_eq!(story_prompt(None, None, &[]), PLAIN_CHAT);
        let alone = story_prompt(Some(("Panem", "Twelve districts.", &[])), None, &[]);
        assert!(alone.starts_with(ROLEPLAY) && alone.ends_with("# World: Panem\n\nTwelve districts."));
        let gale = Player { name: "Gale".into(), aliases: vec!["Hunter".into()], description: "A hunter.".into(), ..Player::default() };
        let cast = [member("Katniss", "A hunter.", true), member("Rue", " ", true), member("Peeta", "A baker.", false)];
        let full = story_prompt(Some(("Panem", "", &[])), Some(&gale), &cast);
        assert!(full.contains("# The user's character: Gale\n\n(Formerly called Hunter; they are the same person.)\n\nA hunter."));
        assert!(full.contains(&format!("# Cast\n\n{CAST}\n\n## Katniss\n\nA hunter.\n\n## Rue\n\n{UNDESCRIBED}\n\n## Peeta\n\nA baker.")));
        // Who is here, the scene and memories change every turn: never in the prompt.
        let moved = [member("Katniss", "A hunter.", false), member("Rue", " ", true), member("Peeta", "A baker.", true)];
        assert_eq!(story_prompt(Some(("Panem", "", &[])), Some(&gale), &moved), full, "the prompt stays cacheable");
    }

    #[test]
    fn cards_examples_and_lore_reach_the_model() {
        let entry = |keys: &[&str], content: &str, constant| LoreEntry { keys: keys.iter().map(|k| (*k).to_owned()).collect(), content: content.into(), constant };
        let world_lore = [entry(&[], "Magic is rare.", true), entry(&["Lantern"], "The Lantern is {{user}}'s inn.", false)];
        let mira = CastMember {
            name: "Mira".into(),
            description: "{{char}} runs a bar; she likes {{user}}.".into(),
            examples: "<START>\n{{user}}: Hi.\n{{char}}: *nods*\n<START>\n{{char}}: Again?".into(),
            lore: vec![entry(&["ale"], "{{char}} brews her own ale.", false)],
            present: true,
            ..CastMember::default()
        };
        let gale = Player { name: "Gale".into(), ..Player::default() };
        let prompt = story_prompt(Some(("Ard", "", &world_lore)), Some(&gale), std::slice::from_ref(&mira));
        assert!(prompt.contains("## Mira\n\nMira runs a bar; she likes Gale."), "{prompt}");
        assert!(prompt.contains(&format!("### How Mira talks\n\n{EXAMPLES}\n\nGale: Hi.\nMira: *nods*\nMira: Again?")));
        assert!(prompt.ends_with(&format!("# Lore\n\n{LORE_ALWAYS}\n\nMagic is rare.")), "only constant lore in the prompt");

        let found = lore_in_play(&world_lore, std::slice::from_ref(&mira), "Gale: Two ALES at the lantern, please.", "Gale");
        assert_eq!(found, ["The Lantern is Gale's inn.", "Mira brews her own ale."]);
        assert!(lore_in_play(&world_lore, &[], "Nothing here.", "Gale").is_empty());
        let state = story_state(&[], &found, &[], "", "");
        assert!(state.contains(&format!("# Lore\n\n{LORE}\n\nThe Lantern is Gale's inn.")));

        // A story opened by a greeting starts with the user, for providers that need it.
        let greeting = super::tools::greeting("Mira", "Hello.");
        let items = input_items(&[greeting, StoredMessage::new(Role::User, "Hi.".into())]);
        assert!(matches!(&items[0], InputItem::Message { role: Role::User, text } if text == STORY_BEGINS));
        assert!(matches!(&items[1], InputItem::ToolCall(_)));
    }

    #[test]
    fn the_story_state_rides_on_the_latest_message() {
        let member = |name: &str, present: bool| CastMember { name: name.into(), present, ..CastMember::default() };
        let nobody = story_state(&[member("Peeta", false)], &[], &[], "", "");
        assert!(nobody.starts_with(STATE) && nobody.contains(NOBODY_HERE) && nobody.ends_with(&format!("{ELSEWHERE} Peeta")));
        assert!(nobody.contains("Not set yet"), "the model is asked to set the scene");
        let memories = ["Peeta saved Katniss.".to_owned(), "Rue is hurt.".to_owned()];
        let full = story_state(&[member("Katniss", true), member("Rue", true)], &[], &memories, " The woods. ", " Keep it short. ");
        let expected = format!(
            "# The scene\n\nThe woods.\n\n# In the scene\n\nKatniss, Rue\n\n# Memories\n\n{MEMORIES}\n\n- Peeta saved Katniss.\n- Rue is hurt.\n\n# Author's note\n\n{NOTE}\n\nKeep it short."
        );
        assert!(full.ends_with(&expected), "the author's note last, where it weighs most: {full}");

        // After the prompt, or after the result of the last call.
        let mut items = vec![InputItem::text(Role::User, "Hi.")];
        attach_state(&mut items, "STATE");
        assert!(matches!(&items[..], [InputItem::Message { text, .. }] if text == "Hi.\n\nSTATE"));
        items.push(InputItem::ToolOutput { call_id: "c".into(), output: "Done.".into() });
        attach_state(&mut items, "STATE");
        assert!(matches!(&items[1], InputItem::ToolOutput { output, .. } if output == "Done.\n\nSTATE"));
        let mut items = vec![InputItem::text(Role::Assistant, "Hello.")];
        attach_state(&mut items, "STATE");
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn summaries_keep_the_latest_turns() {
        let prompt = |text: &str| StoredMessage::new(Role::User, text.into());
        let reply = |text: &str| StoredMessage::new(Role::Assistant, text.into());
        let history = [prompt("a"), reply("A"), prompt("b"), reply("B"), prompt("c"), reply("C"), prompt("d"), reply("D"), prompt("new")];
        // Before a new prompt: up to three earlier turns stay, while they fit.
        assert_eq!(summary_cut(&history, 8, 10_000), 2);
        assert_eq!(summary_cut(&history, 8, 4), 4, "only what fits the budget");
        assert_eq!(summary_cut(&history, 8, 0), 8, "nothing fits: everything is summarised");
        // Mid-turn (no new prompt): the turn under way stays whole.
        let under_way = &history[..8];
        assert_eq!(summary_cut(under_way, 8, 0), 6);
        // A single turn: something must be summarised.
        assert_eq!(summary_cut(&history[..2], 2, 10_000), 2);
        // Never back past the previous summary.
        let mut summary = reply("S");
        summary.compaction = true;
        let after = [prompt("a"), reply("A"), summary, prompt("b"), reply("B"), prompt("new")];
        assert_eq!(summary_cut(&after, 5, 10_000), 3);

        // The model reads the summary, the turns it kept, then the rest.
        let mut summary = reply("Earlier: a and b.");
        (summary.compaction, summary.kept) = (true, 2);
        let history = [prompt("a"), reply("A"), prompt("b"), reply("B"), summary, prompt("new")];
        let items = input_items(&history);
        let texts: Vec<&str> = items.iter().filter_map(|i| if let InputItem::Message { text, .. } = i { Some(text.as_str()) } else { None }).collect();
        assert_eq!(texts.len(), 4);
        assert!(texts[0].ends_with("Earlier: a and b.") && texts[1..] == ["b", "B", "new"]);
    }

    #[test]
    fn reviews_start_after_the_prompts_read() {
        let entry = |role, id| Entry::new(id, StoredMessage::new(role, "x".into()));
        let entries = [entry(Role::User, 1), entry(Role::Assistant, 2), entry(Role::User, 3), entry(Role::Assistant, 4)];
        assert_eq!(first_unread(&entries, 0), 0);
        assert_eq!(first_unread(&entries, 1), 2);
        assert_eq!(first_unread(&entries, 2), 4);
        assert_eq!(first_unread(&entries, 9), 4);
    }

    #[test]
    fn out_of_character_instructions_are_marked() {
        assert_eq!(user_text("I knock."), "I knock.");
        assert_eq!(user_text("I knock.\n/ooc make it tense"), format!("I knock.\n\n{OOC}\n\nmake it tense"));
        assert_eq!(user_text("  /ooc  skip ahead a day "), format!("{OOC}\n\nskip ahead a day"));
        assert_eq!(user_text("/oocx"), "/oocx", "only the command itself");
        let items = input_items(&[StoredMessage::new(Role::User, "Hi.\n/ooc be brief".into())]);
        assert!(matches!(&items[0], InputItem::Message { text, .. } if text.ends_with("be brief") && text.contains(OOC)));
        assert_eq!(tools::user_display("I *wave*.\n/ooc a *dark* turn"), "I *wave*.\n\n*OOC: a dark turn*");
        assert_eq!(tools::split_ooc("/ooc"), ("", None));
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
