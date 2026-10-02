//! The tools a story is told with; it has no narrator, so every reply
//! must call them:
//!
//! * `speak`: everything characters say and do in a reply, as `messages`
//!   of (character, action, text), one per character, after an `introduce`
//!   list that casts newcomers with a description in the same call (a
//!   required field: models skip a separate tool far more often). One call
//!   can hold several speakers, so a scene where three characters react
//!   plays out in order. A cast member who acts moves into the scene; those
//!   in `leave` move out. A non-empty `scene` (required, so the model weighs
//!   it every turn) sets where the story is now and what it is like there.
//!   Stories saved before `messages` call the field `lines`; both are read.
//! * `create_character`: someone joins the story's cast (only the story's:
//!   the character library is the user's) without acting yet, or a member
//!   without a description gets one.
//!
//! No one is cast without a description. Someone who acts but was never
//! introduced is reported instead, and the next request makes the model
//! call `create_character` for them (see `stream.rs`).
//!
//! The cast strip's Generate asks a separate generator for one character;
//! its only tool is `create_character` (see [`generator_prompt`]).
//!
//! Both run instantly on the UI thread: they only read or change the story.
//! A reply shows as [`Part`]s: one bubble per character (whatever the model
//! sent, a character's messages merge into one) and short notes for scene
//! changes, arrivals and departures. They update while the model writes
//! them, from arguments that are still incomplete JSON. There is no
//! narration: text a story reply writes outside its calls is never shown.

use std::fmt::Write as _;

use serde_json::{Value, json};
use serechat::{CastMember, ToolCall, ToolResult, new_id};

/// The speech tool's name.
pub const SPEAK: &str = "speak";
/// The character tool's name.
pub const CREATE_CHARACTER: &str = "create_character";
/// The memory tool's name.
pub const REMEMBER: &str = "remember";
/// Longest a memory may be, in characters; longer ones are cut.
const MEMORY_LIMIT: usize = 400;

/// Every tool as (name, description, JSON Schema of its arguments).
#[must_use]
pub fn tool_definitions() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            SPEAK,
            "Tell the story through the characters: everything they say and do this turn, in one call, with one message per \
             responding character. Only characters in the scene take part, never the user's character: whoever the user \
             addresses, and others present who would react. There is no narrator and no narration: what happens is shown only \
             through the characters' words and actions. Bring in someone new only when the user refers to them or no one is in \
             the scene; they must then be in introduce, with a description.",
            json!({
                "type": "object",
                "properties": {
                    "scene": {
                        "type": "string",
                        "description": "Only when the scene is not set yet or changes this turn: where the characters are now and what it is like there (place, time of day, mood, weather), e.g. \"Peeta's kitchen, before dawn; warm bread, rain on the windows\". Empty otherwise."
                    },
                    "introduce": {
                        "type": "array",
                        "description": "Every character in these lines who is not in the cast yet, described: only someone the user refers to, or the first character when no one is in the scene. Usually empty.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "description": "Their name, exactly as used in the lines." },
                                "description": { "type": "string", "description": "Who they are: role, personality, appearance, how they speak." }
                            },
                            "required": ["name", "description"]
                        }
                    },
                    "messages": {
                        "type": "array",
                        "description": "One message per responding character, in the order they respond. Each character appears at most once: everything they say and do this turn goes in their one message.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "character": { "type": "string", "description": "Who responds: a name from the cast in the scene, or one introduced above." },
                                "action": { "type": "string", "description": "What they do as they respond, e.g. \"lowers her bow, eyes on the treeline\". May be empty." },
                                "text": { "type": "string", "description": "Exactly what they say, without quotation marks. A short action between their words goes in *asterisks*, e.g. \"Stay back. *She steps closer.* I mean it.\" Empty when they only act." }
                            },
                            "required": ["character", "text"]
                        }
                    },
                    "leave": {
                        "type": "array",
                        "description": "Cast members who leave the scene this turn: they walk off, or stay behind when the scene moves on. Usually empty.",
                        "items": { "type": "string" }
                    }
                },
                "required": ["scene", "introduce", "messages"]
            }),
        ),
        (
            CREATE_CHARACTER,
            "Add a character to this story's cast, with a description: someone the user refers to who is not acting this turn \
             (for a newcomer who acts now, use speak's introduce), or to describe a cast member who has no description yet. Never \
             for characters the user has not referred to, nor for the user's character.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Their name, as they will be addressed." },
                    "description": { "type": "string", "description": "Who they are: role, personality, appearance, how they speak." },
                    "present": { "type": "boolean", "description": "Whether they are in the current scene. Defaults to true." }
                },
                "required": ["name", "description"]
            }),
        ),
        (
            REMEMBER,
            "Keep facts the story must not forget, in its memories (shown to you in every request, even after the conversation \
             is summarised): promises, secrets revealed, decisions, injuries, how relationships changed, what someone now owns or \
             knows. Call it in the same reply as speak, only for what matters later, never for what the memories already hold.",
            json!({
                "type": "object",
                "properties": {
                    "memories": {
                        "type": "array",
                        "description": "Each a short, self-contained fact naming who it is about, e.g. \"Katniss promised Prim she would come home.\"",
                        "items": { "type": "string" }
                    }
                },
                "required": ["memories"]
            }),
        ),
    ]
}

/// A user's message in a story as shown (Markdown): what they wrote, with
/// an out-of-character instruction as a muted line under it.
#[must_use]
pub fn user_display(text: &str) -> String {
    match split_ooc(text) {
        ("", Some(ooc)) => format!("*OOC: {}*", plain(ooc)),
        (said, Some(ooc)) => format!("{said}\n\n*OOC: {}*", plain(ooc)),
        (said, None) => said.trim_end().to_owned(),
    }
}

/// The text a user message says in character, and the out-of-character
/// instruction it ends with: everything from a line starting with `/ooc`.
#[must_use]
pub fn split_ooc(text: &str) -> (&str, Option<&str>) {
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let indent = line.len() - line.trim_start().len();
        let rest = &line[indent..];
        if rest.strip_prefix("/ooc").is_some_and(|after| after.is_empty() || after.starts_with(char::is_whitespace)) {
            let instruction = text[start + indent + "/ooc".len()..].trim();
            return (text[..start].trim_end(), (!instruction.is_empty()).then_some(instruction));
        }
        start += line.len();
    }
    (text, None)
}

/// System instructions of the character generator, which can do one thing.
const GENERATOR: &str = "You create characters for an interactive roleplay story. Your only job is to invent exactly one \
    character from the user's request and return them with one create_character call; write nothing else, and never continue \
    the story. The description is for the AI that will play them: who they are, their role, personality, appearance, how they \
    speak and what they want, in a few short paragraphs. Fit the world below. Never create the user's character or anyone \
    already in the cast.";
/// The request sent when the user asked for nobody in particular.
pub const SURPRISE: &str = "Surprise me: someone this story would benefit from.";

/// The generator's only tool, as (name, description, JSON Schema).
#[must_use]
pub fn generator_tool() -> (&'static str, &'static str, Value) {
    (
        CREATE_CHARACTER,
        "Create the requested character.",
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Their name, as they will be addressed." },
                "description": { "type": "string", "description": "Who they are: role, personality, appearance, how they speak, what they want." }
            },
            "required": ["name", "description"]
        }),
    )
}

/// The generator's system prompt for a story in `world` (name and
/// description), played by `player`, whose `cast` holds these names.
#[must_use]
pub fn generator_prompt(world: Option<(&str, &str)>, player: Option<&str>, cast: &[&str]) -> String {
    let mut prompt = GENERATOR.to_owned();
    if let Some((name, description)) = world {
        let _ = write!(prompt, "\n\n# World: {name}\n\n{description}");
    }
    if let Some(player) = player {
        let _ = write!(prompt, "\n\n# The user's character\n\n{player}");
    }
    if !cast.is_empty() {
        let _ = write!(prompt, "\n\n# Already in the cast\n\n{}", cast.join(", "));
    }
    prompt
}

/// The (name, description) a generator call made, if it made someone.
#[must_use]
pub fn generated(call: &ToolCall) -> Option<(String, String)> {
    let args = serde_json::from_str::<Value>(&call.arguments).ok()?;
    let field = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    Some((field("name")?, field("description")?))
}

/// Whether two names are the same character's.
#[must_use]
pub fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Adds `name` to the cast with `description`, or describes a member who
/// has no description yet. Returns what the model is told.
fn cast_in(cast: &mut Vec<CastMember>, player: Option<&str>, name: &str, description: &str, present: bool) -> String {
    let (name, description) = (name.trim(), description.trim());
    if name.is_empty() {
        return "Error: a character needs a name.".to_owned();
    }
    if player.is_some_and(|p| same(p, name)) {
        return format!("{name} is the user's character; nothing was created.");
    }
    if description.is_empty() {
        return format!("Error: {name} needs a description; nothing was created.");
    }
    match cast.iter_mut().find(|m| same(&m.name, name)) {
        Some(member) if member.description.trim().is_empty() => {
            description.clone_into(&mut member.description);
            member.present |= present;
            format!("{name} is now described.")
        }
        Some(_) => format!("{name} is already in the cast."),
        None => {
            cast.push(CastMember { id: new_id(), name: name.to_owned(), description: description.to_owned(), portrait: String::new(), present });
            format!("{name} joined the cast{}.", if present { " and is in the scene" } else { ", away from the scene for now" })
        }
    }
}

/// A `speak` call's messages (`lines` in stories saved before `messages`).
fn messages(args: &Value) -> impl Iterator<Item = &Value> {
    args.get("messages").or_else(|| args.get("lines")).and_then(Value::as_array).into_iter().flatten()
}

/// The names a `speak` call's messages are spoken by.
fn speakers(args: &Value) -> impl Iterator<Item = &str> {
    messages(args).filter_map(|l| l.get("character").and_then(Value::as_str)).map(str::trim).filter(|s| !s.is_empty())
}

/// Runs `call` in a story with this `cast` (which it may grow or change),
/// `scene` (which it may set), `memories` (which it may add to) and
/// `player` (the user's character) and returns what the model is told.
pub fn run(call: &ToolCall, cast: &mut Vec<CastMember>, scene: &mut String, memories: &mut Vec<String>, player: Option<&str>) -> String {
    let Ok(args) = serde_json::from_str::<Value>(&call.arguments) else {
        return "Error: the arguments were not valid JSON. Nothing happened; call it again.".to_owned();
    };
    let field = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    match call.name.as_str() {
        SPEAK => {
            if speakers(&args).next().is_none() {
                return "Error: speak needs at least one message with a character.".to_owned();
            }
            let mut notes: Vec<String> = args
                .get("introduce")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|new| cast_in(cast, player, &field(new, "name"), &field(new, "description"), true))
                .filter(|note| !note.ends_with("already in the cast."))
                .collect();
            // Whoever acts is in the scene; whoever is not cast and
            // described yet is not added half-finished, but reported.
            let (mut arrived, mut missing, mut for_player) = (Vec::new(), Vec::new(), false);
            let (mut seen, mut twice): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
            for speaker in speakers(&args) {
                if seen.iter().any(|s| same(s, speaker)) {
                    if !twice.iter().any(|s| same(s, speaker)) {
                        twice.push(speaker);
                    }
                    continue;
                }
                seen.push(speaker);
                if player.is_some_and(|p| same(p, speaker)) {
                    for_player = true;
                    continue;
                }
                match cast.iter_mut().find(|m| same(&m.name, speaker)) {
                    Some(member) if !member.description.trim().is_empty() => {
                        if !member.present {
                            member.present = true;
                            arrived.push(speaker.to_owned());
                        }
                    }
                    _ if missing.iter().any(|m: &String| same(m, speaker)) => {}
                    _ => missing.push(speaker.to_owned()),
                }
            }
            if !arrived.is_empty() {
                notes.push(format!("Moved into the scene: {}.", arrived.join(", ")));
            }
            // Leaving comes last: someone may speak, then walk off.
            let mut left = Vec::new();
            for name in args.get("leave").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                if let Some(member) = cast.iter_mut().find(|m| m.present && same(&m.name, name)) {
                    member.present = false;
                    left.push(member.name.clone());
                }
            }
            if !left.is_empty() {
                notes.push(format!("Left the scene: {}.", left.join(", ")));
            }
            let new_scene = field(&args, "scene");
            if !new_scene.trim().is_empty() {
                new_scene.trim().clone_into(scene);
                notes.push("Scene set.".to_owned());
            }
            if for_player {
                notes.push("Never speak or act for the user's character.".to_owned());
            }
            if !twice.is_empty() {
                let names = twice.join(", ");
                notes.push(format!("{names} had more than one message; they were shown as one. Give each character a single message."));
            }
            if !missing.is_empty() {
                notes.push(format!("Not in the cast with a description: {}. Add them with create_character now.", missing.join(", ")));
            }
            if notes.is_empty() { "Spoken.".to_owned() } else { format!("Spoken. {}", notes.join(" ")) }
        }
        CREATE_CHARACTER => {
            let present = args.get("present").and_then(Value::as_bool).unwrap_or(true);
            cast_in(cast, player, &field(&args, "name"), &field(&args, "description"), present)
        }
        REMEMBER => {
            let mut kept = 0;
            for memory in args.get("memories").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                let memory: String = memory.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MEMORY_LIMIT).collect();
                if !memory.is_empty() && !memories.iter().any(|m| m.eq_ignore_ascii_case(&memory)) {
                    memories.push(memory);
                    kept += 1;
                }
            }
            if kept == 0 { "Nothing new to remember.".to_owned() } else { "Remembered.".to_owned() }
        }
        other => format!("Error: there is no tool named {other}."),
    }
}

/// Who acted in a reply's `speak` calls but is not in the cast with a
/// description: they still need one.
#[must_use]
pub fn undescribed<'a>(calls: impl Iterator<Item = &'a ToolCall>, cast: &[CastMember], player: Option<&str>) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    for call in calls.filter(|c| c.name == SPEAK) {
        let Ok(args) = serde_json::from_str::<Value>(&call.arguments) else {
            continue;
        };
        for speaker in speakers(&args) {
            let described = cast.iter().any(|m| same(&m.name, speaker) && !m.description.trim().is_empty());
            let known = described || player.is_some_and(|p| same(p, speaker)) || missing.iter().any(|m| same(m, speaker));
            if !known {
                missing.push(speaker.to_owned());
            }
        }
    }
    missing
}

/// Whether a reply's tool call shows the user something: a message where
/// a character says or does anything.
#[must_use]
pub fn shows_speech(call: &ToolCall) -> bool {
    let filled = |line: &Value, key: &str| line.get(key).and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty());
    call.name == SPEAK
        && serde_json::from_str::<Value>(&call.arguments).is_ok_and(|args| messages(&args).any(|l| filled(l, "text") || filled(l, "action")))
}

/// One piece of what a story reply shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// Everything one character does and says in the reply: one bubble.
    Said {
        /// Who.
        character: String,
        /// Markdown: their actions in italics, and their words.
        text: String,
    },
    /// A change to the story (scene, arrival, departure), shown small.
    Note(String),
}

/// What a story reply's calls (name, arguments) show, in order: a new
/// scene and arrivals, one [`Part::Said`] per character (their messages
/// merged, whatever the model sent), then departures. `streaming` calls
/// may be incomplete; arrivals and departures show once they are not.
#[must_use]
pub fn parts<'a>(calls: impl Iterator<Item = (&'a str, &'a str)>, streaming: bool) -> Vec<Part> {
    let (mut parts, mut departures, mut remembered) = (Vec::new(), Vec::new(), Vec::new());
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
    for (name, arguments) in calls {
        let Some(args) = parse_partial(arguments) else {
            continue;
        };
        match name {
            SPEAK => {
                // A new scene first, as it is written: it sets the stage.
                let scene = plain(&text(&args, "scene"));
                if !scene.is_empty() {
                    parts.push(Part::Note(scene));
                }
                // Newcomers arrive before they act; announced once complete.
                if !streaming {
                    for new in args.get("introduce").and_then(Value::as_array).into_iter().flatten() {
                        let who = plain(&text(new, "name"));
                        if !who.is_empty() && !text(new, "description").is_empty() {
                            parts.push(Part::Note(format!("{who} joins the story")));
                        }
                    }
                }
                for message in messages(&args) {
                    let (speaker, action, said) = (plain(&text(message, "character")), plain(&text(message, "action")), text(message, "text"));
                    if speaker.is_empty() || (action.is_empty() && said.is_empty()) {
                        continue;
                    }
                    let mut block = if action.is_empty() { String::new() } else { format!("*{action}*") };
                    if !said.is_empty() {
                        if !block.is_empty() {
                            block.push(' ');
                        }
                        block.push_str(&said);
                    }
                    add_said(&mut parts, &speaker, &block);
                }
                if !streaming {
                    let leaving = args.get("leave").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(plain);
                    departures.extend(leaving.filter(|who| !who.is_empty()).map(|who| Part::Note(format!("{who} leaves the scene"))));
                }
            }
            CREATE_CHARACTER if !streaming => {
                let who = plain(&text(&args, "name"));
                if !who.is_empty() {
                    parts.push(Part::Note(format!("{who} joins the story")));
                }
            }
            REMEMBER if !streaming => {
                let memories = args.get("memories").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
                let memories = memories.map(|m| plain(m).split_whitespace().collect::<Vec<_>>().join(" "));
                remembered.extend(memories.filter(|m| !m.is_empty()).map(|m| Part::Note(format!("Remembered: {m}"))));
            }
            _ => {}
        }
    }
    parts.extend(departures);
    parts.extend(remembered);
    parts
}

/// Adds `text` to `character`'s bubble, or starts one; returns its index.
fn add_said(parts: &mut Vec<Part>, character: &str, text: &str) -> usize {
    let earlier = parts.iter().position(|part| matches!(part, Part::Said { character: c, .. } if same(c, character)));
    if let Some(index) = earlier {
        if let Part::Said { text: said, .. } = &mut parts[index] {
            said.push_str("\n\n");
            said.push_str(text);
        }
        return index;
    }
    parts.push(Part::Said { character: character.to_owned(), text: text.to_owned() });
    parts.len() - 1
}

/// The speaker a Markdown block opens with (`**Name**`) and what follows
/// the name, if the block is one character's speech.
fn speaker(block: &str) -> Option<(&str, &str)> {
    let rest = block.strip_prefix("**")?;
    let end = rest.find("**")?;
    let (name, after) = (rest[..end].trim(), &rest[end + 2..]);
    let valid = !name.is_empty() && !name.contains('\n') && name.chars().count() <= 80;
    (valid && (after.is_empty() || after.starts_with([':', ' ', '\n']))).then_some((name, after))
}

/// Parts read back from a reply's Markdown: [`display`]'s format, or the
/// `**Name** · *action*` line above a quote of earlier versions. For
/// replies edited when edits still became plain text; empty when no one
/// speaks in it.
#[must_use]
pub fn parse(markdown: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut current = None;
    for block in markdown.split("\n\n").map(str::trim).filter(|b| !b.is_empty()) {
        if let Some((name, after)) = speaker(block) {
            let text = if let Some(said) = after.strip_prefix(':') {
                said.trim().to_owned()
            } else {
                // `**Name** · *action*`, then `> ` lines.
                let (header, quote) = after.split_once('\n').unwrap_or((after, ""));
                let action = plain(header.trim().trim_start_matches('·'));
                let said: Vec<&str> = quote.lines().map(|l| l.trim_start().trim_start_matches('>').trim()).collect();
                let said = said.join("\n");
                match (action.is_empty(), said.is_empty()) {
                    (true, _) => said,
                    (false, true) => format!("*{action}*"),
                    (false, false) => format!("*{action}* {said}"),
                }
            };
            current = Some(add_said(&mut parts, name, &text));
            continue;
        }
        let italic = block.len() > 2 && block.starts_with('*') && block.ends_with('*') && !block.starts_with("**");
        let event = italic && [" joins the story", " leaves the scene"].iter().any(|e| block.trim_end_matches(['*', '.']).ends_with(e));
        match current {
            // A paragraph after someone's speech is more of it.
            Some(index) if !event => {
                if let Some(Part::Said { text, .. }) = parts.get_mut(index) {
                    text.push_str("\n\n");
                    text.push_str(block);
                }
            }
            // Arrivals and departures once ended with a full stop.
            _ if event => parts.push(Part::Note(plain(block).trim_end_matches('.').to_owned())),
            _ => parts.push(Part::Note(plain(block))),
        }
    }
    if parts.iter().any(|p| matches!(p, Part::Said { .. })) { parts } else { Vec::new() }
}

/// Replaces the speech of a reply's `calls` with `said` (character and
/// Markdown, empty ones dropped) in one speak call where the first was,
/// keeping its scene, arrivals, departures and results; other calls stay.
/// With no speak call yet, one is added.
pub fn respeak(calls: &mut Vec<ToolResult>, said: &[(&str, &str)]) {
    let (mut scene, mut introduce, mut leave, mut outputs) = (String::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut at, mut call_id) = (None, None);
    let mut kept = Vec::with_capacity(calls.len());
    for result in calls.drain(..) {
        if result.call.name != SPEAK {
            kept.push(result);
            continue;
        }
        at.get_or_insert(kept.len());
        call_id.get_or_insert(result.call.call_id);
        if let Ok(args) = serde_json::from_str::<Value>(&result.call.arguments) {
            if scene.is_empty() {
                args.get("scene").and_then(Value::as_str).unwrap_or_default().clone_into(&mut scene);
            }
            introduce.extend(args.get("introduce").and_then(Value::as_array).into_iter().flatten().cloned());
            leave.extend(args.get("leave").and_then(Value::as_array).into_iter().flatten().cloned());
        }
        if !result.output.is_empty() {
            outputs.push(result.output);
        }
    }
    let messages: Vec<Value> =
        said.iter().filter(|(_, text)| !text.trim().is_empty()).map(|(character, text)| json!({ "character": character, "text": text.trim() })).collect();
    if !messages.is_empty() {
        let arguments = json!({ "scene": scene, "introduce": introduce, "messages": messages, "leave": leave }).to_string();
        let call = ToolCall { call_id: call_id.unwrap_or_else(|| format!("call_{}", new_id())), name: SPEAK.into(), arguments };
        let output = if outputs.is_empty() { "Spoken.".to_owned() } else { outputs.join(" ") };
        kept.insert(at.unwrap_or(kept.len()), ToolResult { call, output });
    }
    *calls = kept;
}

/// `parts` as Markdown, for copying and editing.
#[must_use]
pub fn display(parts: &[Part]) -> String {
    let blocks: Vec<String> = parts
        .iter()
        .map(|part| match part {
            Part::Said { character, text } => format!("**{character}**: {text}"),
            Part::Note(note) => format!("*{note}*"),
        })
        .collect();
    blocks.join("\n\n")
}

/// `text` on one line without Markdown emphasis or code marks, so a name
/// or action never breaks the formatting around it.
fn plain(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).filter(|c| !matches!(c, '*' | '_' | '`')).collect::<String>().trim().to_owned()
}

/// Parses JSON that may be cut off part-way, as tool arguments are while
/// they stream: open strings, arrays and objects are closed, and a
/// trailing key or value too incomplete for that is dropped.
#[must_use]
pub fn parse_partial(text: &str) -> Option<Value> {
    let mut end = text.len();
    // Each attempt cuts back to an earlier boundary, so this ends.
    for _ in 0..32 {
        let (closed, boundaries) = close(&text[..end]);
        if let Ok(value) = serde_json::from_str(&closed) {
            return Some(value);
        }
        end = boundaries.into_iter().rfind(|&b| b < end)?;
    }
    None
}

/// `text` with its open string, arrays and objects closed, and the places
/// it can be cut back to (before a comma, after an opening bracket).
fn close(text: &str) -> (String, Vec<usize>) {
    let (mut stack, mut boundaries) = (Vec::new(), Vec::new());
    let (mut in_string, mut escaped) = (false, false);
    for (i, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => {
                stack.push(if c == '{' { '}' } else { ']' });
                boundaries.push(i + 1);
            }
            '}' | ']' => {
                stack.pop();
            }
            ',' => boundaries.push(i),
            _ => {}
        }
    }
    let mut closed = text.to_owned();
    if escaped {
        // A lone backslash would escape the closing quote.
        closed.pop();
    }
    if in_string {
        closed.push('"');
    }
    while let Some(closer) = stack.pop() {
        closed.push(closer);
    }
    (closed, boundaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: &Value) -> ToolCall {
        ToolCall { call_id: "c".into(), name: name.into(), arguments: arguments.to_string() }
    }

    fn member(name: &str, present: bool) -> CastMember {
        CastMember { id: name.to_lowercase(), name: name.into(), description: format!("{name}, described."), present, ..CastMember::default() }
    }

    #[test]
    fn partial_json_streams_line_by_line() {
        let full = r#"{"lines":[{"character":"Katniss","action":"draws","text":"Stay back."},{"character":"Peeta","text":"Easy."}]}"#;
        for end in 0..=full.len() {
            // Every prefix parses (or is too short to say anything) without panicking.
            let _ = parse_partial(&full[..end]);
        }
        let cut = |n: &str| parse_partial(&full[..full.find(n).unwrap() + n.len()]).unwrap();
        assert_eq!(cut("Stay ba")["lines"][0]["text"], "Stay ba", "an open string is shown so far");
        assert_eq!(cut(r#""Peeta","te"#)["lines"][1]["character"], "Peeta", "a half key is dropped");
        assert_eq!(cut(r#""text":"#)["lines"][0]["action"], "draws");
        assert_eq!(parse_partial(r#"{"lines":[{"text":"a\"#).unwrap()["lines"][0]["text"], "a");
        assert_eq!(parse_partial(full).unwrap()["lines"][1]["text"], "Easy.");
        assert!(parse_partial("").is_none());
    }

    fn said(character: &str, text: &str) -> Part {
        Part::Said { character: character.into(), text: text.into() }
    }

    #[test]
    fn each_character_gets_one_bubble() {
        let args = json!({ "messages": [
            { "character": "Katniss", "action": "draws her *bow*", "text": "Stay back." },
            { "character": "Peeta", "text": "Easy." },
            { "character": "katniss", "text": "I mean it." },
            { "character": "Haymitch", "action": "takes a long drink", "text": "" },
            { "character": "", "text": "nobody" },
            { "character": "Effie", "text": " " },
        ]});
        let shown = parts([(SPEAK, args.to_string().as_str())].into_iter(), false);
        assert_eq!(
            shown,
            [said("Katniss", "*draws her bow* Stay back.\n\nI mean it."), said("Peeta", "Easy."), said("Haymitch", "*takes a long drink*")]
        );
        assert_eq!(display(&shown[1..]), "**Peeta**: Easy.\n\n**Haymitch**: *takes a long drink*");
        // Stories saved before `messages` still show.
        let old = json!({ "lines": [{ "character": "Peeta", "text": "Easy." }] }).to_string();
        assert_eq!(parts([(SPEAK, old.as_str())].into_iter(), false), [said("Peeta", "Easy.")]);
        let created = json!({ "name": "Cato", "description": "A career." }).to_string();
        assert_eq!(parts([(CREATE_CHARACTER, created.as_str())].into_iter(), false), [Part::Note("Cato joins the story".into())]);
        assert!(parts([(CREATE_CHARACTER, created.as_str())].into_iter(), true).is_empty(), "only finished arrivals show");
    }

    #[test]
    fn edited_text_reads_back_into_bubbles() {
        // What display writes reads back as it was.
        let shown = [Part::Note("The woods.".into()), said("Katniss", "*draws* Stay back.\n\nI mean it."), said("Peeta", "Easy.")];
        assert_eq!(parse(&display(&shown)), shown);
        // Earlier versions: a name over a quote, after narration.
        let old = "Leaves rustle.\n\n**Katniss** · *draws her bow*\n> Stay back.\n> I mean it.\n\n**Peeta**\n> Easy.\n\n*Rue joins the story.*";
        assert_eq!(
            parse(old),
            [
                Part::Note("Leaves rustle.".into()),
                said("Katniss", "*draws her bow* Stay back.\nI mean it."),
                said("Peeta", "Easy."),
                Part::Note("Rue joins the story".into())
            ]
        );
        assert!(parse("Just an answer, **bold** in it.").is_empty(), "no one speaks: plain Markdown");
        assert!(parse("**Note**:").len() == 1 && parse("**").is_empty() && parse("****: x").is_empty());
        for text in ["**é", "**a**", "*", "**\n**: x", "**Ünï**· x\n>"] {
            let _ = parse(text);
        }
    }

    #[test]
    fn remembering_adds_new_facts_once() {
        let (mut cast, mut scene, mut memories) = (Vec::new(), String::new(), vec!["Rue is hurt.".to_owned()]);
        let facts = call(REMEMBER, &json!({ "memories": [" Katniss  promised\nPrim. ", "rue is hurt.", "", 7] }));
        assert_eq!(run(&facts, &mut cast, &mut scene, &mut memories, None), "Remembered.");
        assert_eq!(memories, ["Rue is hurt.", "Katniss promised Prim."]);
        assert_eq!(run(&facts, &mut cast, &mut scene, &mut memories, None), "Nothing new to remember.");
        let long = call(REMEMBER, &json!({ "memories": ["é".repeat(1000)] }));
        run(&long, &mut cast, &mut scene, &mut memories, None);
        assert_eq!(memories[2].chars().count(), MEMORY_LIMIT);
        let shown = parts([(REMEMBER, facts.arguments.as_str())].into_iter(), false);
        assert_eq!(shown[0], Part::Note("Remembered: Katniss promised Prim.".into()));
        assert!(parts([(REMEMBER, facts.arguments.as_str())].into_iter(), true).is_empty(), "shown once complete");
    }

    #[test]
    fn edits_rewrite_the_speak_call() {
        let speak = |id: &str, args: Value| ToolResult { call: ToolCall { call_id: id.into(), name: SPEAK.into(), arguments: args.to_string() }, output: "Spoken.".into() };
        let created = ToolResult { call: call(CREATE_CHARACTER, &json!({ "name": "Rue", "description": "A girl." })), output: "Rue joined.".into() };
        let mut calls = vec![
            created.clone(),
            speak("a", json!({ "scene": "The woods.", "messages": [{ "character": "Katniss", "text": "Hi." }, { "character": "Peeta", "text": "Yo." }] })),
            speak("b", json!({ "messages": [{ "character": "Rue", "text": "Hey." }], "leave": ["Peeta"] })),
        ];
        respeak(&mut calls, &[("Katniss", "Hello there."), ("Peeta", " "), ("Rue", "Hey.")]);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], created, "other calls stay");
        assert_eq!((calls[1].call.call_id.as_str(), calls[1].output.as_str()), ("a", "Spoken. Spoken."));
        let shown = parts(calls.iter().map(|r| (r.call.name.as_str(), r.call.arguments.as_str())), false);
        assert_eq!(
            shown,
            [
                Part::Note("Rue joins the story".into()),
                Part::Note("The woods.".into()),
                said("Katniss", "Hello there."),
                said("Rue", "Hey."),
                Part::Note("Peeta leaves the scene".into())
            ]
        );
        // A reply read back from text gets a call.
        let mut none = Vec::new();
        respeak(&mut none, &[("Katniss", "Hi.")]);
        assert!(none[0].call.name == SPEAK && none[0].call.call_id.starts_with("call_") && none[0].output == "Spoken.");
    }

    #[test]
    fn tools_run_against_the_story() {
        let mut cast = vec![member("Katniss", true), member("Haymitch", false)];
        let (mut scene, mut memories) = (String::new(), Vec::new());
        let created = run(&call(CREATE_CHARACTER, &json!({ "name": "Cato", "description": "A career." })), &mut cast, &mut scene, &mut memories, Some("Gale"));
        assert!(created.contains("Cato joined"));
        assert!(cast.last().is_some_and(|m| m.name == "Cato" && m.present && !m.id.is_empty()));
        assert!(run(&call(CREATE_CHARACTER, &json!({ "name": "cato", "description": "Again." })), &mut cast, &mut scene, &mut memories, None).contains("already"));
        assert!(
            run(&call(CREATE_CHARACTER, &json!({ "name": "Gale", "description": "Me." })), &mut cast, &mut scene, &mut memories, Some("Gale"))
                .contains("user's character")
        );
        assert!(
            run(&call(CREATE_CHARACTER, &json!({ "name": "Rue", "description": " " })), &mut cast, &mut scene, &mut memories, None).starts_with("Error"),
            "never undescribed"
        );
        assert_eq!(cast.len(), 3);

        let lines =
            |who: &[&str]| json!({ "introduce": [], "messages": who.iter().map(|w| json!({ "character": w, "text": "Hi." })).collect::<Vec<_>>() });
        assert_eq!(run(&call(SPEAK, &lines(&["Katniss", "Cato"])), &mut cast, &mut scene, &mut memories, Some("Gale")), "Spoken.");
        // Acting moves someone into the scene; a stranger is reported, not added.
        let noted = run(&call(SPEAK, &lines(&["Haymitch", "Rue", "Gale", "Rue"])), &mut cast, &mut scene, &mut memories, Some("Gale"));
        assert!(noted.contains("Moved into the scene: Haymitch.") && noted.contains("Not in the cast with a description: Rue."));
        assert!(noted.contains("Never speak or act for the user's character.") && noted.contains("Rue had more than one message"));
        assert!(cast.iter().all(|m| m.present) && !cast.iter().any(|m| m.name == "Rue" || m.name == "Gale"));
        assert_eq!(undescribed([&call(SPEAK, &lines(&["Rue", "Katniss", "rue", "Gale"]))].into_iter(), &cast, Some("Gale")), ["Rue"]);

        // Introduced in the same call, a newcomer is cast before they act.
        let introduced =
            json!({ "introduce": [{ "name": "Rue", "description": "A girl from District 11." }], "lines": [{ "character": "Rue", "text": "Hi." }] });
        let spoken = run(&call(SPEAK, &introduced), &mut cast, &mut scene, &mut memories, None);
        assert!(spoken.starts_with("Spoken. Rue joined the cast") && !spoken.contains("Not in the cast"), "{spoken}");
        assert!(undescribed([&call(SPEAK, &introduced)].into_iter(), &cast, None).is_empty());

        // A member without a description (older stories) can be described.
        cast.push(CastMember { id: "x".into(), name: "Rex".into(), present: true, ..CastMember::default() });
        assert_eq!(undescribed([&call(SPEAK, &lines(&["Rex"]))].into_iter(), &cast, None), ["Rex"]);
        assert_eq!(
            run(&call(CREATE_CHARACTER, &json!({ "name": "Rex", "description": "A clone captain." })), &mut cast, &mut scene, &mut memories, None),
            "Rex is now described."
        );
        assert!(undescribed([&call(SPEAK, &lines(&["Rex"]))].into_iter(), &cast, None).is_empty());

        assert!(run(&call(SPEAK, &json!({ "lines": [] })), &mut cast, &mut scene, &mut memories, None).starts_with("Error"));
        assert!(run(&ToolCall { call_id: "c".into(), name: SPEAK.into(), arguments: "{".into() }, &mut cast, &mut scene, &mut memories, None).starts_with("Error"));
        assert!(run(&call("fly", &json!({})), &mut cast, &mut scene, &mut memories, None).starts_with("Error"));
        assert!(scene.is_empty(), "no call set a scene yet");

        // Moving on: a new scene, and Haymitch stays behind after speaking.
        let moved =
            json!({ "scene": " The train, at dusk. ", "lines": [{ "character": "Haymitch", "text": "Go." }], "leave": ["haymitch", "Nobody"] });
        let noted = run(&call(SPEAK, &moved), &mut cast, &mut scene, &mut memories, None);
        assert!(noted.contains("Left the scene: Haymitch.") && noted.ends_with("Scene set."), "{noted}");
        assert!(scene == "The train, at dusk." && cast.iter().any(|m| m.name == "Haymitch" && !m.present));
        let shown = display(&parts([(SPEAK, moved.to_string().as_str())].into_iter(), false));
        assert_eq!(shown, "*The train, at dusk.*\n\n**Haymitch**: Go.\n\n*haymitch leaves the scene*\n\n*Nobody leaves the scene*");
        let kept = json!({ "scene": "", "lines": [{ "character": "Katniss", "text": "Hi." }] });
        run(&call(SPEAK, &kept), &mut cast, &mut scene, &mut memories, None);
        assert_eq!(scene, "The train, at dusk.", "an empty scene keeps the last one");

        assert!(shows_speech(&call(SPEAK, &lines(&["Katniss"]))));
        assert!(!shows_speech(&call(SPEAK, &json!({ "lines": [{ "character": "K", "text": " " }] }))));
        assert!(shows_speech(&call(SPEAK, &json!({ "lines": [{ "character": "K", "action": "nods", "text": "" }] }))), "acting counts");
        assert!(!shows_speech(&call(CREATE_CHARACTER, &json!({ "name": "X" }))));
    }

    #[test]
    fn the_generator_makes_one_described_character() {
        let prompt = generator_prompt(Some(("Panem", "Twelve districts.")), Some("Gale"), &["Katniss", "Peeta"]);
        assert!(prompt.starts_with(GENERATOR) && prompt.contains("# World: Panem\n\nTwelve districts."));
        assert!(prompt.contains("# The user's character\n\nGale") && prompt.ends_with("Katniss, Peeta"));
        assert_eq!(generator_prompt(None, None, &[]), GENERATOR);

        let made = generated(&call(CREATE_CHARACTER, &json!({ "name": " Rue ", "description": "A girl from District 11." })));
        assert_eq!(made, Some(("Rue".into(), "A girl from District 11.".into())));
        assert!(generated(&call(CREATE_CHARACTER, &json!({ "name": "Rue", "description": " " }))).is_none(), "never undescribed");
        assert!(generated(&ToolCall { call_id: "c".into(), name: CREATE_CHARACTER.into(), arguments: "{".into() }).is_none());
    }
}
