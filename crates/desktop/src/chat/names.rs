//! Telling characters apart by name, as the model writes them.
//!
//! Models are loose with names: they change case and accents' spacing, use
//! a first name or a title ("Rex" for "Captain Rex"), or keep using a name
//! the user has since changed. A name therefore finds its character in three
//! steps, the first that matches winning:
//!
//! 1. the same name, ignoring case, punctuation and spacing;
//! 2. a name the character went by before (their aliases);
//! 3. a name whose words are all part of exactly one character's name (or
//!    one of theirs all part of it), ignoring "the", "a" and "an". Two
//!    candidates mean no match: guessing would merge different people.

use serechat::{CastMember, Player};

/// Words that never tell names apart.
const FILLER: [&str; 3] = ["the", "a", "an"];

/// `name` folded for comparing: lower case, letters and digits only, one
/// space between words.
fn fold(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut space = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        } else if c.is_whitespace() || c == '-' || c == '_' {
            space = true;
        }
        // Other marks (apostrophes, dots) join what they separate: O'Brien.
    }
    out
}

/// The words of a folded name that tell it apart.
fn words(folded: &str) -> Vec<&str> {
    folded.split(' ').filter(|w| !w.is_empty() && !FILLER.contains(w)).collect()
}

/// Whether two names are the same name (step 1).
#[must_use]
pub fn same(a: &str, b: &str) -> bool {
    let a = fold(a);
    !a.is_empty() && a == fold(b)
}

/// Whether one name's words all appear in the other (step 3).
fn overlaps(a: &str, b: &str) -> bool {
    let (a, b) = (fold(a), fold(b));
    let (a, b) = (words(&a), words(&b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let within = |small: &[&str], big: &[&str]| small.iter().all(|w| big.contains(w));
    within(&a, &b) || within(&b, &a)
}

/// Who a name in the story means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    /// The cast member at this index.
    Member(usize),
    /// The user's character.
    Player,
}

/// Whom `name` means in a story with this `cast` and `player`, if anyone
/// (see the module docs for how).
#[must_use]
pub fn resolve(cast: &[CastMember], player: Option<&Player>, name: &str) -> Option<Who> {
    if fold(name).is_empty() {
        return None;
    }
    let people = || {
        let members = cast.iter().enumerate().map(|(i, m)| (Who::Member(i), m.name.as_str(), m.aliases.as_slice()));
        members.chain(player.map(|p| (Who::Player, p.name.as_str(), p.aliases.as_slice())))
    };
    if let Some((who, ..)) = people().find(|(_, own, _)| same(own, name)) {
        return Some(who);
    }
    if let Some((who, ..)) = people().find(|(_, _, aliases)| aliases.iter().any(|a| same(a, name))) {
        return Some(who);
    }
    let mut matches = people().filter(|(_, own, aliases)| overlaps(own, name) || aliases.iter().any(|a| overlaps(a, name)));
    match (matches.next(), matches.next()) {
        (Some((who, ..)), None) => Some(who),
        _ => None,
    }
}

/// The cast member `name` means, if it means one.
#[must_use]
pub fn member(cast: &[CastMember], player: Option<&Player>, name: &str) -> Option<usize> {
    match resolve(cast, player, name)? {
        Who::Member(index) => Some(index),
        Who::Player => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cast(names: &[(&str, &[&str])]) -> Vec<CastMember> {
        names
            .iter()
            .map(|(name, aliases)| CastMember {
                id: name.to_lowercase(),
                name: (*name).to_owned(),
                aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
                ..CastMember::default()
            })
            .collect()
    }

    #[test]
    fn names_find_their_character() {
        let cast = cast(&[("Captain Rex", &[]), ("Zoë", &[]), ("Alice", &["Bob"]), ("Shaak Ti", &[]), ("Mira O'Hara", &[])]);
        let player = Player { name: "Gale Hawthorne".into(), ..Player::default() };
        let who = |name: &str| resolve(&cast, Some(&player), name);
        assert_eq!(who("captain  REX"), Some(Who::Member(0)), "case and spacing");
        assert_eq!(who("Rex"), Some(Who::Member(0)), "a short name");
        assert_eq!(who("ZOË"), Some(Who::Member(1)));
        assert_eq!(who("Bob"), Some(Who::Member(2)), "a former name");
        assert_eq!(who("Mira OHara"), Some(Who::Member(4)), "punctuation");
        assert_eq!(who("Gale"), Some(Who::Player), "the user's character, by first name");
        assert_eq!(who("The Captain"), Some(Who::Member(0)));
        assert_eq!(who("Ti"), Some(Who::Member(3)));
        assert_eq!(who("Cato"), None);
        assert_eq!(who("the"), None, "filler alone means no one");
        assert_eq!(who(" *** "), None);
        assert_eq!(member(&cast, Some(&player), "Gale"), None);
    }

    #[test]
    fn exact_names_win_and_ties_match_no_one() {
        let cast = cast(&[("Rex", &[]), ("Captain Rex", &[]), ("John Smith", &[]), ("John Doe", &[])]);
        assert_eq!(resolve(&cast, None, "rex"), Some(Who::Member(0)), "the exact name first");
        assert_eq!(resolve(&cast, None, "Captain Rex"), Some(Who::Member(1)));
        assert_eq!(resolve(&cast, None, "John"), None, "two Johns: no guess");
        assert!(same("Zoë", "zoë") && !same("", "") && !same("Ann", "Anne"));
    }
}
