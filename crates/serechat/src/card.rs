//! Character cards, the format SillyTavern, Character Hub and most roleplay
//! apps share characters in: JSON (`chara_card_v3`, `chara_card_v2` or the
//! flat V1 fields), or a PNG whose image is the portrait and whose `tEXt`
//! chunks hold that JSON in base64 (`ccv3`, preferred, or `chara`).
//!
//! Reading maps a card onto a [`Character`]: personality and scenario join
//! the description, the first message and alternate greetings become its
//! greetings, and the example dialogue, tags and embedded lorebook
//! (`character_book`) carry over. A card's own system prompt, post-history
//! instructions and creator notes are dropped: OpenRP writes its own prompt.
//! Writing makes a V3 card whose fields V2 readers understand too, and a PNG
//! card holds both (`chara` and `ccv3`), as SillyTavern writes them.
//!
//! Lorebooks are read from SillyTavern's world-info files (`entries` keyed
//! by number, with `key` and `disable`), a card's `character_book`
//! (`entries` listed, with `keys` and `enabled`), or a whole card.
//!
//! Everything here reads hostile input: chunks are bounds-checked, nothing
//! recurses, and text is never sliced off a char boundary.
//!
//! ponytail: compressed text chunks (`zTXt`, compressed `iTXt`) and CHARX
//! (zip) cards are not read; card writers in use don't make them. Add zlib
//! and zip reading if one turns up.

use std::borrow::Cow;

use serde_json::{Value, json};

use crate::error::Result;
use crate::library::{Character, LoreEntry, invalid};

/// The PNG signature.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
/// Largest card read, in bytes, as for portraits.
const MAX_CARD: usize = 20 << 20;

/// Reads the character card in `bytes`, a PNG card or card JSON. The
/// character has no id, portrait or times yet.
///
/// # Errors
/// Not a PNG or JSON card, a PNG holding no card, or a card without a name.
pub fn read_card(bytes: &[u8]) -> Result<Character> {
    character(&card_value(bytes)?).ok_or_else(|| invalid("This is not a character card: it has no name."))
}

/// Reads the lore entries of a lorebook in `bytes`: SillyTavern world info,
/// a card's `character_book`, or a whole card (PNG or JSON). Disabled and
/// empty entries are left out.
///
/// # Errors
/// The file is not JSON or a PNG card, or holds no lore entries.
pub fn read_lorebook(bytes: &[u8]) -> Result<Vec<LoreEntry>> {
    let value = card_value(bytes)?;
    let book = value.get("data").and_then(|d| d.get("character_book")).or_else(|| value.get("character_book")).unwrap_or(&value);
    let entries = book_entries(book);
    if entries.is_empty() { Err(invalid("No lore entries were found in this file.")) } else { Ok(entries) }
}

/// `character` as card JSON (V3), for a `.json` file.
#[must_use]
pub fn card_json(character: &Character) -> String {
    format!("{:#}", json!({ "spec": "chara_card_v3", "spec_version": "3.0", "data": card_data(character) }))
}

/// The PNG `png` with `character`'s card in it, replacing any card it held.
///
/// # Errors
/// `png` is not a whole PNG file.
pub fn embed_card(png: &[u8], character: &Character) -> Result<Vec<u8>> {
    let chunks = chunks(png);
    if chunks.last().is_none_or(|c| &c.kind != b"IEND") {
        return Err(invalid("The portrait is not a valid PNG image."));
    }
    let data = card_data(character);
    // V2 readers also find the V1 fields at the top.
    let mut v2 = json!({ "spec": "chara_card_v2", "spec_version": "2.0", "data": data });
    for key in ["name", "description", "personality", "scenario", "first_mes", "mes_example"] {
        v2[key] = data[key].clone();
    }
    let v3 = json!({ "spec": "chara_card_v3", "spec_version": "3.0", "data": data });
    let (v2, v3) = (base64(v2.to_string().as_bytes()), base64(v3.to_string().as_bytes()));
    let mut out = Vec::with_capacity(png.len() + v2.len() + v3.len() + 64);
    out.extend_from_slice(PNG);
    for chunk in &chunks {
        if matches!(text_chunk(chunk), Some((keyword, _)) if is_card_keyword(keyword)) {
            continue;
        }
        if &chunk.kind == b"IEND" {
            write_chunk(&mut out, *b"tEXt", &[b"chara\0", v2.as_bytes()].concat());
            write_chunk(&mut out, *b"tEXt", &[b"ccv3\0", v3.as_bytes()].concat());
        }
        out.extend_from_slice(chunk.raw);
    }
    Ok(out)
}

/// `text` with the placeholders cards use replaced: `{{char}}` and `<BOT>`
/// by `character` (left as they are when there is none), `{{user}}` and
/// `<USER>` by `user`, in any case.
#[must_use]
pub fn macros<'a>(text: &'a str, character: Option<&str>, user: &str) -> Cow<'a, str> {
    if !text.contains("{{") && !text.contains('<') {
        return Cow::Borrowed(text);
    }
    let tokens = [("{{char}}", character), ("<bot>", character), ("{{user}}", Some(user)), ("<user>", Some(user))];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['{', '<']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let found = tokens.iter().find_map(|(token, value)| Some((*token, (*value)?)).filter(|(token, _)| rest.get(..token.len()).is_some_and(|s| s.eq_ignore_ascii_case(token))));
        // `{` and `<` are one byte each.
        let (taken, value) = found.map_or((1, &rest[..1]), |(token, value)| (token.len(), value));
        out.push_str(value);
        rest = &rest[taken..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// The card JSON in `bytes`, from a PNG's text chunk or the bytes themselves.
fn card_value(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > MAX_CARD {
        return Err(invalid("The file is larger than 20 MB."));
    }
    if bytes.starts_with(PNG) {
        let text = png_card(bytes).ok_or_else(|| invalid("This image holds no character card."))?;
        let json = base64_decode(text).ok_or_else(|| invalid("The card in this image is damaged."))?;
        return Ok(serde_json::from_slice(&json)?);
    }
    // JSON, perhaps after a byte-order mark.
    let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if json.trim_ascii_start().first() != Some(&b'{') {
        return Err(invalid("Only PNG and JSON files can hold a card."));
    }
    Ok(serde_json::from_slice(json)?)
}

/// The character a card's JSON describes; `None` without a name.
fn character(card: &Value) -> Option<Character> {
    // V2 and V3 keep their fields under `data`; V1 and older exports at the top.
    let data = card.get("data").filter(|d| d.is_object()).unwrap_or(card);
    let text = |keys: &[&str]| keys.iter().find_map(|k| data.get(*k).and_then(Value::as_str)).map(clean).unwrap_or_default();
    let name = text(&["name", "char_name"]);
    if name.is_empty() {
        return None;
    }
    let mut description = text(&["description", "char_persona"]);
    for (label, value) in [("Personality", text(&["personality"])), ("Scenario", text(&["scenario", "world_scenario"]))] {
        if !value.is_empty() && !description.contains(&value) {
            let gap = if description.is_empty() { "" } else { "\n\n" };
            description = format!("{description}{gap}{label}: {value}");
        }
    }
    let first = text(&["first_mes", "char_greeting"]);
    let greetings = std::iter::once(first).chain(strings(data.get("alternate_greetings"))).filter(|g| !g.is_empty()).collect();
    let mut tags: Vec<String> = Vec::new();
    for tag in strings(data.get("tags")) {
        if !tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            tags.push(tag);
        }
    }
    Some(Character {
        name,
        description,
        greetings,
        examples: text(&["mes_example", "example_dialogue"]),
        tags,
        lore: data.get("character_book").map(book_entries).unwrap_or_default(),
        ..Character::default()
    })
}

/// The JSON fields of `character`'s card (its `data`).
fn card_data(character: &Character) -> Value {
    let mut data = json!({
        "name": character.name,
        "description": character.description,
        "personality": "",
        "scenario": "",
        "first_mes": character.greetings.first().map_or("", String::as_str),
        "mes_example": character.examples,
        "creator_notes": "",
        "system_prompt": "",
        "post_history_instructions": "",
        "alternate_greetings": character.greetings.get(1..).unwrap_or_default(),
        "group_only_greetings": [],
        "tags": character.tags,
        "creator": "",
        "character_version": "",
        "extensions": {},
    });
    if !character.lore.is_empty() {
        let entries: Vec<Value> = character
            .lore
            .iter()
            .enumerate()
            .map(|(id, entry)| {
                json!({
                    "keys": entry.keys,
                    "content": entry.content,
                    "constant": entry.constant,
                    "enabled": true,
                    "insertion_order": id,
                    "case_sensitive": false,
                    "use_regex": false,
                    "selective": false,
                    "secondary_keys": [],
                    "position": "before_char",
                    "priority": 10,
                    "id": id,
                    "name": "",
                    "comment": "",
                    "extensions": {},
                })
            })
            .collect();
        data["character_book"] = json!({ "name": character.name, "entries": entries, "extensions": {} });
    }
    data
}

/// The usable entries of a lorebook's JSON, in its order.
fn book_entries(book: &Value) -> Vec<LoreEntry> {
    let mut entries: Vec<&Value> = match book.get("entries") {
        Some(Value::Array(list)) => list.iter().collect(),
        Some(Value::Object(map)) => map.values().collect(),
        _ => Vec::new(),
    };
    // World info is keyed by number, which JSON objects do not keep in order.
    let order = |e: &Value| ["insertion_order", "order", "uid", "id"].iter().find_map(|k| e.get(*k).and_then(Value::as_i64)).unwrap_or(0);
    entries.sort_by_key(|e| order(e));
    entries
        .into_iter()
        .filter_map(|e| {
            let off = e.get("disable").and_then(Value::as_bool) == Some(true) || e.get("enabled").and_then(Value::as_bool) == Some(false);
            let content = e.get("content").and_then(Value::as_str).map(clean).unwrap_or_default();
            let keys = strings(e.get("keys").or_else(|| e.get("key")));
            let constant = e.get("constant").and_then(Value::as_bool) == Some(true);
            // An entry without keys is read only when constant.
            (!off && !content.is_empty() && (constant || !keys.is_empty())).then_some(LoreEntry { keys, content, constant })
        })
        .collect()
}

/// The non-empty strings of a JSON list, or of a comma-separated string.
fn strings(value: Option<&Value>) -> Vec<String> {
    let items: Vec<String> = match value {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(clean).collect(),
        Some(Value::String(text)) => text.split(',').map(clean).collect(),
        _ => Vec::new(),
    };
    items.into_iter().filter(|s| !s.is_empty()).collect()
}

/// `text` trimmed, with Windows line breaks made plain.
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n").trim().to_owned()
}

/// One chunk of a PNG file.
struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
    /// The whole chunk: length, type, data and checksum.
    raw: &'a [u8],
}

/// The chunks of `png` in order, up to the first that does not fit (all of
/// them, ending with `IEND`, in a whole file).
fn chunks(png: &[u8]) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let Some(mut rest) = png.strip_prefix(PNG) else { return out };
    while let Some(head) = rest.get(..8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let Some(raw) = len.checked_add(12).and_then(|total| rest.get(..total)) else { break };
        let kind = [head[4], head[5], head[6], head[7]];
        out.push(Chunk { kind, data: &raw[8..8 + len], raw });
        rest = &rest[raw.len()..];
        if &kind == b"IEND" {
            break;
        }
    }
    out
}

/// The keyword and text of an uncompressed text chunk.
fn text_chunk<'a>(chunk: &Chunk<'a>) -> Option<(&'a [u8], &'a [u8])> {
    let nul = chunk.data.iter().position(|&b| b == 0)?;
    let (keyword, rest) = (&chunk.data[..nul], &chunk.data[nul + 1..]);
    match &chunk.kind {
        b"tEXt" => Some((keyword, rest)),
        b"iTXt" => {
            // Compression flag and method, language, translated keyword, text.
            let (&compressed, rest) = rest.split_first()?;
            let rest = rest.get(1..).filter(|_| compressed == 0)?;
            let language = rest.iter().position(|&b| b == 0)?;
            let rest = &rest[language + 1..];
            let translated = rest.iter().position(|&b| b == 0)?;
            Some((keyword, &rest[translated + 1..]))
        }
        _ => None,
    }
}

/// Whether a text chunk's keyword names a card.
fn is_card_keyword(keyword: &[u8]) -> bool {
    keyword.eq_ignore_ascii_case(b"chara") || keyword.eq_ignore_ascii_case(b"ccv3")
}

/// The base64 card text of a PNG card: its `ccv3` chunk, else its `chara`.
fn png_card(png: &[u8]) -> Option<&[u8]> {
    let mut v2 = None;
    for chunk in chunks(png) {
        match text_chunk(&chunk) {
            Some((keyword, text)) if keyword.eq_ignore_ascii_case(b"ccv3") => return Some(text),
            Some((keyword, text)) if keyword.eq_ignore_ascii_case(b"chara") => v2 = v2.or(Some(text)),
            _ => {}
        }
    }
    v2
}

/// Appends a chunk of `kind` holding `data`, with its checksum.
fn write_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    // Card text is far below 4 GB; the file's size limit sees to that.
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[&kind, data]).to_be_bytes());
}

/// The CRC-32 a PNG chunk ends with, over `parts` in order.
fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for &byte in parts.iter().flat_map(|p| p.iter()) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Standard base64 of `bytes`, padded.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let n = group.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= group.len() { char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]) } else { '=' });
        }
    }
    out
}

/// Decodes base64 (standard or URL-safe, padding and line breaks allowed);
/// `None` on any other character.
fn base64_decode(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest PNG: a 1×1 grey pixel.
    fn tiny_png() -> Vec<u8> {
        let mut png = PNG.to_vec();
        write_chunk(&mut png, *b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]);
        write_chunk(&mut png, *b"IDAT", &[0x78, 0x01, 0x63, 0x68, 0x00, 0x00, 0x00, 0x82, 0x00, 0x81]);
        write_chunk(&mut png, *b"IEND", &[]);
        png
    }

    #[test]
    fn checksums_and_base64() {
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082, "every PNG ends with this");
        for text in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "héllo ✓"] {
            assert_eq!(base64_decode(base64(text.as_bytes()).as_bytes()).unwrap(), text.as_bytes());
        }
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode(b"Zm9v\nYg==").unwrap(), b"foob");
        assert!(base64_decode(b"Zm9v!").is_none());
    }

    #[test]
    fn cards_round_trip_through_png() {
        let lore = vec![LoreEntry { keys: vec!["Lantern".into()], content: "The inn.".into(), constant: false }];
        let mira = Character {
            name: "Mira".into(),
            description: "A barkeep.".into(),
            greetings: vec!["What'll it be, {{user}}?".into(), "Back again?".into()],
            examples: "<START>\n{{char}}: Hm.".into(),
            tags: vec!["fantasy".into()],
            lore,
            ..Character::default()
        };
        let card = embed_card(&tiny_png(), &mira).unwrap();
        assert_eq!(read_card(&card).unwrap(), mira);
        // Embedding again replaces the card rather than adding one.
        let renamed = Character { name: "Mara".into(), ..mira.clone() };
        let again = embed_card(&card, &renamed).unwrap();
        assert_eq!(read_card(&again).unwrap().name, "Mara");
        assert_eq!(chunks(&again).iter().filter(|c| text_chunk(c).is_some()).count(), 2);
        assert!(chunks(&again).iter().all(|c| crc32(&[&c.kind, c.data]).to_be_bytes() == c.raw[c.raw.len() - 4..]));
        // And the JSON file reads the same.
        assert_eq!(read_card(card_json(&mira).as_bytes()).unwrap(), mira);
    }

    #[test]
    fn older_and_odd_cards_read() {
        let v1 = r#"{"name":"Rex","description":"A clone.","personality":"Loyal.","scenario":"Kamino.","first_mes":"Sir.","mes_example":"","tags":"army, Army ,clone"}"#;
        let rex = read_card(v1.as_bytes()).unwrap();
        assert_eq!(rex.description, "A clone.\n\nPersonality: Loyal.\n\nScenario: Kamino.");
        assert!(rex.greetings == ["Sir."] && rex.tags == ["army", "clone"]);
        let pyg = "\u{feff}{\"char_name\":\"Old\",\"char_persona\":\"Very.\",\"char_greeting\":\"Hi\\r\\nthere\"}";
        assert_eq!(read_card(pyg.as_bytes()).unwrap().greetings, ["Hi\nthere"]);

        // Hostile input fails cleanly.
        assert!(read_card(b"{\"data\":{}}").is_err(), "no name");
        assert!(read_card(b"GIF89a").is_err());
        assert!(read_card(&tiny_png()).is_err(), "an image without a card");
        let mut cut = tiny_png();
        cut.truncate(20);
        assert!(read_card(&cut).is_err() && embed_card(&cut, &rex).is_err());
        let mut huge = PNG.to_vec();
        huge.extend_from_slice(&[0xFF; 8]);
        assert!(chunks(&huge).is_empty(), "a length past the end");
    }

    #[test]
    fn lorebooks_read_from_every_shape() {
        let world_info = r#"{"entries":{"1":{"uid":1,"key":["Dragon"],"content":"Dragons fly.","disable":false},
            "0":{"uid":0,"key":[],"content":"Always.","constant":true},"2":{"uid":2,"key":["x"],"content":"Off.","disable":true},
            "3":{"uid":3,"key":[],"content":"Never found."}}}"#;
        let entries = read_lorebook(world_info.as_bytes()).unwrap();
        assert_eq!(entries.iter().map(|e| e.content.as_str()).collect::<Vec<_>>(), ["Always.", "Dragons fly."]);
        let book = r#"{"spec":"chara_card_v2","data":{"name":"A","character_book":{"entries":[{"keys":["inn"],"content":"Warm.","enabled":true}]}}}"#;
        assert_eq!(read_lorebook(book.as_bytes()).unwrap()[0].keys, ["inn"]);
        assert!(read_lorebook(b"{}").is_err());
    }

    #[test]
    fn placeholders_are_filled() {
        assert_eq!(macros("{{char}} greets {{User}} <BOT><user>", Some("Mira"), "Gale"), "Mira greets Gale MiraGale");
        assert_eq!(macros("{{char}} and {{user}}", None, "Gale"), "{{char}} and Gale", "no character: left as is");
        assert!(matches!(macros("plain", Some("M"), "G"), Cow::Borrowed(_)));
        assert_eq!(macros("a < b {{ é", Some("M"), "G"), "a < b {{ é");
    }
}
