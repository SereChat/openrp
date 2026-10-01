//! The tools a story is told with; it has no narrator, so every reply
//! must call them:
//!
//! * `speak`: everything characters say and do in a reply, as ordered
//!   lines of (character, action, text), after an `introduce` list that
//!   casts newcomers with a description in the same call (a required
//!   field: models skip a separate tool far more often). One call can hold
//!   several speakers, so a scene where three characters react plays out
//!   in order. A cast member who acts moves into the scene.
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
//! Their calls are shown as Markdown, speech as `**Name** · *action*`
//! followed by a quote, and update while the model writes them, from
//! arguments that are still incomplete JSON. Any text a reply writes
//! outside them shows first, as written.

use std::fmt::Write as _;

use serde_json::{Value, json};
use serechat::{CastMember, ToolCall, new_id};

/// The speech tool's name.
pub const SPEAK: &str = "speak";
/// The character tool's name.
pub const CREATE_CHARACTER: &str = "create_character";

/// Every tool as (name, description, JSON Schema of its arguments).
#[must_use]
pub fn tool_definitions() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            SPEAK,
            "Tell the story through the characters: everything they say and do this turn, in one call, every line in the order \
             it happens. Only characters in the scene take part, never the user's character: whoever the user addresses, and \
             others present who would react. There is no narrator: what happens is shown through the characters' actions. Bring \
             in someone new only when the user refers to them or no one is in the scene; they must then be in introduce, with a \
             description.",
            json!({
                "type": "object",
                "properties": {
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
                    "lines": {
                        "type": "array",
                        "description": "What happens, in order.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "character": { "type": "string", "description": "Who acts: a name from the cast in the scene, or one introduced above." },
                                "action": { "type": "string", "description": "What they do, e.g. \"lowers her bow, eyes on the treeline\". May be empty." },
                                "text": { "type": "string", "description": "Exactly what they say, without quotation marks. Empty when they only act." }
                            },
                            "required": ["character", "text"]
                        }
                    }
                },
                "required": ["introduce", "lines"]
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
    ]
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

/// The names a `speak` call's lines are spoken by.
fn speakers(args: &Value) -> impl Iterator<Item = &str> {
    let lines = args.get("lines").and_then(Value::as_array).into_iter().flatten();
    lines.filter_map(|l| l.get("character").and_then(Value::as_str)).map(str::trim).filter(|s| !s.is_empty())
}

/// Runs `call` in a story with this `cast` (which it may grow or change)
/// and `player` (the user's character) and returns what the model is told.
pub fn run(call: &ToolCall, cast: &mut Vec<CastMember>, player: Option<&str>) -> String {
    let Ok(args) = serde_json::from_str::<Value>(&call.arguments) else {
        return "Error: the arguments were not valid JSON. Nothing happened; call it again.".to_owned();
    };
    let field = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    match call.name.as_str() {
        SPEAK => {
            if speakers(&args).next().is_none() {
                return "Error: speak needs at least one line with a character.".to_owned();
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
            for speaker in speakers(&args) {
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
            if for_player {
                notes.push("Never speak or act for the user's character.".to_owned());
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

/// Whether a reply's tool call shows the user something: a line where a
/// character says or does anything.
#[must_use]
pub fn shows_speech(call: &ToolCall) -> bool {
    let filled = |line: &Value, key: &str| line.get(key).and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty());
    call.name == SPEAK
        && serde_json::from_str::<Value>(&call.arguments)
            .ok()
            .and_then(|args| args.get("lines")?.as_array().map(|lines| lines.iter().any(|l| filled(l, "text") || filled(l, "action"))))
            .unwrap_or(false)
}

/// The Markdown a reply shows: its narration, then what its calls (name,
/// arguments) show. `streaming` calls may be incomplete; only their speech
/// is shown.
#[must_use]
pub fn display<'a>(narration: &str, calls: impl Iterator<Item = (&'a str, &'a str)>, streaming: bool) -> String {
    let mut out = narration.trim_end().to_owned();
    let mut push = |block: &str| {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(block);
    };
    for (name, arguments) in calls {
        let Some(args) = parse_partial(arguments) else {
            continue;
        };
        let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
        match name {
            SPEAK => {
                // Newcomers arrive before they act; announced once complete.
                if !streaming {
                    for new in args.get("introduce").and_then(Value::as_array).into_iter().flatten() {
                        let who = plain(&text(new, "name"));
                        if !who.is_empty() && !text(new, "description").is_empty() {
                            push(&format!("*{who} joins the story.*"));
                        }
                    }
                }
                for line in args.get("lines").and_then(Value::as_array).into_iter().flatten() {
                    let (speaker, action, said) = (plain(&text(line, "character")), plain(&text(line, "action")), text(line, "text"));
                    if speaker.is_empty() {
                        continue;
                    }
                    let mut block = format!("**{speaker}**");
                    if !action.is_empty() {
                        let _ = write!(block, " · *{action}*");
                    }
                    for said_line in said.lines() {
                        block.push_str("\n> ");
                        block.push_str(said_line);
                    }
                    push(&block);
                }
            }
            CREATE_CHARACTER if !streaming => {
                let who = plain(&text(&args, "name"));
                if !who.is_empty() {
                    push(&format!("*{who} joins the story.*"));
                }
            }
            _ => {}
        }
    }
    out
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

    #[test]
    fn speech_shows_as_markdown() {
        let args = json!({ "lines": [
            { "character": "Katniss", "action": "draws her *bow*", "text": "Stay back.\nI mean it." },
            { "character": "Peeta", "text": "Easy." },
            { "character": "Haymitch", "action": "takes a long drink", "text": "" },
            { "character": "", "text": "nobody" },
        ]});
        let shown = display("The woods go quiet.", [(SPEAK, args.to_string().as_str())].into_iter(), false);
        assert_eq!(
            shown,
            "The woods go quiet.\n\n**Katniss** · *draws her bow*\n> Stay back.\n> I mean it.\n\n**Peeta**\n> Easy.\n\n**Haymitch** · *takes a long drink*"
        );
        let created = json!({ "name": "Cato", "description": "A career." }).to_string();
        assert_eq!(display("", [(CREATE_CHARACTER, created.as_str())].into_iter(), false), "*Cato joins the story.*");
        assert_eq!(display("", [(CREATE_CHARACTER, created.as_str())].into_iter(), true), "", "only finished arrivals show");
    }

    #[test]
    fn tools_run_against_the_story() {
        let mut cast = vec![member("Katniss", true), member("Haymitch", false)];
        let created = run(&call(CREATE_CHARACTER, &json!({ "name": "Cato", "description": "A career." })), &mut cast, Some("Gale"));
        assert!(created.contains("Cato joined"));
        assert!(cast.last().is_some_and(|m| m.name == "Cato" && m.present && !m.id.is_empty()));
        assert!(run(&call(CREATE_CHARACTER, &json!({ "name": "cato", "description": "Again." })), &mut cast, None).contains("already"));
        assert!(run(&call(CREATE_CHARACTER, &json!({ "name": "Gale", "description": "Me." })), &mut cast, Some("Gale")).contains("user's character"));
        assert!(
            run(&call(CREATE_CHARACTER, &json!({ "name": "Rue", "description": " " })), &mut cast, None).starts_with("Error"),
            "never undescribed"
        );
        assert_eq!(cast.len(), 3);

        let lines =
            |who: &[&str]| json!({ "introduce": [], "lines": who.iter().map(|w| json!({ "character": w, "text": "Hi." })).collect::<Vec<_>>() });
        assert_eq!(run(&call(SPEAK, &lines(&["Katniss", "Cato"])), &mut cast, Some("Gale")), "Spoken.");
        // Acting moves someone into the scene; a stranger is reported, not added.
        let noted = run(&call(SPEAK, &lines(&["Haymitch", "Rue", "Gale", "Rue"])), &mut cast, Some("Gale"));
        assert!(noted.contains("Moved into the scene: Haymitch.") && noted.contains("Not in the cast with a description: Rue."));
        assert!(noted.contains("Never speak or act for the user's character."));
        assert!(cast.iter().all(|m| m.present) && !cast.iter().any(|m| m.name == "Rue" || m.name == "Gale"));
        assert_eq!(undescribed([&call(SPEAK, &lines(&["Rue", "Katniss", "rue", "Gale"]))].into_iter(), &cast, Some("Gale")), ["Rue"]);

        // Introduced in the same call, a newcomer is cast before they act.
        let introduced =
            json!({ "introduce": [{ "name": "Rue", "description": "A girl from District 11." }], "lines": [{ "character": "Rue", "text": "Hi." }] });
        let spoken = run(&call(SPEAK, &introduced), &mut cast, None);
        assert!(spoken.starts_with("Spoken. Rue joined the cast") && !spoken.contains("Not in the cast"), "{spoken}");
        assert!(undescribed([&call(SPEAK, &introduced)].into_iter(), &cast, None).is_empty());

        // A member without a description (older stories) can be described.
        cast.push(CastMember { id: "x".into(), name: "Rex".into(), present: true, ..CastMember::default() });
        assert_eq!(undescribed([&call(SPEAK, &lines(&["Rex"]))].into_iter(), &cast, None), ["Rex"]);
        assert_eq!(
            run(&call(CREATE_CHARACTER, &json!({ "name": "Rex", "description": "A clone captain." })), &mut cast, None),
            "Rex is now described."
        );
        assert!(undescribed([&call(SPEAK, &lines(&["Rex"]))].into_iter(), &cast, None).is_empty());

        assert!(run(&call(SPEAK, &json!({ "lines": [] })), &mut cast, None).starts_with("Error"));
        assert!(run(&ToolCall { call_id: "c".into(), name: SPEAK.into(), arguments: "{".into() }, &mut cast, None).starts_with("Error"));
        assert!(run(&call("fly", &json!({})), &mut cast, None).starts_with("Error"));

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
