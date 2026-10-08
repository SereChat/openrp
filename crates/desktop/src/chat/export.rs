//! Stories as files to keep or take elsewhere: a readable transcript, or a
//! SillyTavern chat (JSONL: a header line, then a line per message, one per
//! character's bubble), which SillyTavern imports. Out-of-character
//! instructions, errors, summaries and the replies swiped away stay out.

use std::fmt::Write as _;

use serde_json::json;
use serechat::{Role, Session};

use super::tools::{self, Part};

/// A line of a story as exported.
enum Line {
    /// Someone's message: who, whether it is the user, and the text.
    Said(String, bool, String),
    /// A change to the story (the scene, an arrival).
    Note(String),
}

/// What `session` shows, in order; `player` speaks for the user.
fn lines(session: &Session, player: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for message in session.messages.iter().filter(|m| !m.failed && !m.compaction) {
        if message.role == Role::User {
            let said = tools::split_ooc(&message.content).0.trim();
            if !said.is_empty() {
                lines.push(Line::Said(player.to_owned(), true, said.to_owned()));
            }
            continue;
        }
        let mut parts = tools::reply_parts(message);
        if parts.is_empty() {
            parts = tools::parse(&message.content);
        }
        // A plain chat's reply (from before stories) has no characters.
        if parts.is_empty() && !message.content.trim().is_empty() {
            lines.push(Line::Said("Assistant".to_owned(), false, message.content.trim().to_owned()));
        }
        for part in parts {
            lines.push(match part {
                Part::Said { character, text } => Line::Said(character, false, text),
                Part::Note(note) => Line::Note(note),
            });
        }
    }
    lines
}

/// Who the user plays in `session`.
fn player(session: &Session) -> &str {
    session.player.as_ref().map_or("You", |p| p.name.as_str())
}

/// `session`, played in `world`, as a transcript: its title, then who said
/// what, with changes to the story in brackets.
#[must_use]
pub fn text(session: &Session, world: &str) -> String {
    let player = player(session);
    let mut out = if session.title.is_empty() { "Untitled story".to_owned() } else { session.title.clone() };
    if !world.is_empty() {
        let _ = write!(out, "\nIn {world}, played as {player}.");
    }
    for line in lines(session, player) {
        let _ = match line {
            Line::Said(name, _, text) => write!(out, "\n\n{name}: {text}"),
            Line::Note(note) => write!(out, "\n\n[{note}]"),
        };
    }
    out.push('\n');
    out
}

/// `session`, played in `world`, as a SillyTavern chat file.
#[must_use]
pub fn jsonl(session: &Session, world: &str) -> String {
    let player = player(session);
    // Messages carry no time of their own: the story's start stands in.
    let date = session.created.saturating_mul(1000);
    let character = session.cast.first().map_or(world, |m| m.name.as_str());
    let mut out = json!({ "user_name": player, "character_name": character, "create_date": date, "chat_metadata": {} }).to_string();
    for line in lines(session, player) {
        if let Line::Said(name, is_user, text) = line {
            let message = json!({ "name": name, "is_user": is_user, "is_system": false, "send_date": date, "mes": text, "extra": {} });
            let _ = write!(out, "\n{message}");
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use serechat::{Player, StoredMessage};

    use super::*;

    #[test]
    fn stories_export_as_text_and_sillytavern_chats() {
        let reply = tools::greeting("Mira", "*wipes a glass* What'll it be?");
        let mut failed = StoredMessage::new(Role::Assistant, "boom".into());
        failed.failed = true;
        let session = Session {
            title: "The Lantern".into(),
            created: 2,
            player: Some(Player { name: "Gale".into(), ..Player::default() }),
            messages: vec![reply, StoredMessage::new(Role::User, "Ale.\n/ooc be brief".into()), failed],
            ..Session::default()
        };
        let text = text(&session, "Ard");
        assert_eq!(text, "The Lantern\nIn Ard, played as Gale.\n\nMira: *wipes a glass* What'll it be?\n\nGale: Ale.\n");

        let chat = jsonl(&session, "Ard");
        let lines: Vec<serde_json::Value> = chat.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 3, "a header, then a line per message");
        assert_eq!((lines[0]["user_name"].as_str(), lines[0]["create_date"].as_u64()), (Some("Gale"), Some(2000)));
        assert_eq!((lines[1]["name"].as_str(), lines[1]["is_user"].as_bool()), (Some("Mira"), Some(false)));
        assert_eq!((lines[2]["mes"].as_str(), lines[2]["is_user"].as_bool()), (Some("Ale."), Some(true)));
    }
}
