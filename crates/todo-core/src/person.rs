//! People (design 3.5): who an Item is assigned to, reported by or waiting
//! on, made from the tracker's users. Matching `~name` to one is here, pure,
//! so the daemon's capture and preview agree.

use crate::capture::CaptureDraft;
use crate::ids::PersonId;
use crate::item::PersonRef;

/// A known person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: PersonId,
    pub display_name: String,
    /// The tracker's login name (Data Center `name`), which can be renamed.
    pub username: String,
    pub emails: Vec<String>,
    pub identities: Vec<Identity>,
}

/// One tracker account of a person.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub instance_id: String,
    /// Stable (a Data Center user key).
    pub account_id: String,
}

/// What `~text` names (design 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    One(PersonId),
    /// Two or more people: the text stays free text.
    Many,
    None,
}

/// The person `text` names, case-insensitively: exactly one whose username,
/// an email or display name is the text; else, when none is, exactly one
/// whose display name's first word is. Never a guess between two.
pub fn match_person(text: &str, people: &[Person]) -> Match {
    match named(text, people).as_slice() {
        [] => Match::None,
        [one] => Match::One(one.id),
        _ => Match::Many,
    }
}

/// How many people `text` names, as [`match_person`] counts them: what "2
/// people match" says.
pub fn count_matches(text: &str, people: &[Person]) -> usize {
    named(text, people).len()
}

/// Everyone `text` names in the first tier that names anyone: the whole
/// username, email or display name, then the display name's first word.
fn named<'a>(text: &str, people: &'a [Person]) -> Vec<&'a Person> {
    let t = fold(text);
    if t.is_empty() {
        return Vec::new();
    }
    let whole = |p: &Person| {
        fold(&p.username) == t
            || fold(&p.display_name) == t
            || p.emails.iter().any(|e| fold(e) == t)
    };
    // "Speaks, Seth" is a common Data Center form: the comma is no part of
    // the word.
    let first = |p: &Person| {
        p.display_name
            .split_whitespace()
            .next()
            .is_some_and(|w| fold(w.trim_end_matches([',', ';'])) == t)
    };
    for tier in [&whole as &dyn Fn(&Person) -> bool, &first] {
        let found: Vec<&Person> = people.iter().filter(|p| tier(p)).collect();
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// The capture's `~` token against `people` (design 2.3): free text naming
/// exactly one person becomes that person. Returns what it named.
pub fn resolve_waiting(draft: &mut CaptureDraft, people: &[Person]) -> Match {
    let Some(waiting) = &mut draft.waiting else {
        return Match::None;
    };
    let PersonRef::FreeText(text) = &waiting.on else {
        return Match::None;
    };
    let m = match_person(text, people);
    if let Match::One(id) = m {
        waiting.on = PersonRef::Person(id);
    }
    m
}

/// Up to `limit` people for `query`, best first, for completion: a whole
/// name, username or email first, then a word of one starting with it, then
/// one containing it; by display name within each. An empty query lists
/// everyone by name.
pub fn search<'a>(query: &str, people: &'a [Person], limit: usize) -> Vec<&'a Person> {
    let q = fold(query);
    let rank = |p: &Person| -> Option<u8> {
        if q.is_empty() {
            return Some(0);
        }
        let mut keys = vec![fold(&p.display_name), fold(&p.username)];
        keys.extend(p.emails.iter().map(|e| fold(e)));
        // The words of a name, a username, and an address's local part: a
        // domain everyone shares ("example.com") is nobody's word.
        let mut words = vec![fold(&p.display_name), fold(&p.username)];
        words.extend(
            p.emails
                .iter()
                .map(|e| fold(e.split('@').next().unwrap_or(""))),
        );
        if keys.contains(&q) {
            Some(0)
        } else if words.iter().any(|k| {
            k.split(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | '_' | '-'))
                .any(|w| w.starts_with(&q))
        }) {
            Some(1)
        } else if keys.iter().any(|k| k.contains(&q)) {
            Some(2)
        } else {
            None
        }
    };
    let mut ranked: Vec<(u8, String, &Person)> = people
        .iter()
        .filter_map(|p| rank(p).map(|r| (r, fold(&p.display_name), p)))
        .collect();
    ranked.sort_by(|a, b| (a.0, &a.1, a.2.id).cmp(&(b.0, &b.1, b.2.id)));
    ranked.into_iter().take(limit).map(|(.., p)| p).collect()
}

/// Case and surrounding space folded away.
fn fold(s: &str) -> String {
    s.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::parse_capture;
    use jiff::civil::date;

    fn person(name: &str, username: &str, email: &str) -> Person {
        Person {
            id: crate::new_ulid(),
            display_name: name.into(),
            username: username.into(),
            emails: if email.is_empty() {
                vec![]
            } else {
                vec![email.into()]
            },
            identities: vec![],
        }
    }

    #[test]
    fn one_match_links_and_two_never_guess() {
        let dana = person("Dana Example", "dana", "dana@example.com");
        let dan = person("Dan Smith", "dsmith", "dan.smith@example.com");
        let sam = person("Sam Example", "sam", "");
        let sammy = person("Sam Other", "sother", "");
        let people = vec![dana.clone(), dan.clone(), sam.clone(), sammy.clone()];
        for (text, want) in [
            ("dana", Match::One(dana.id)),
            ("DANA", Match::One(dana.id)),
            ("dana@example.com", Match::One(dana.id)),
            (" Dana Example ", Match::One(dana.id)),
            ("dan", Match::One(dan.id)),
            ("dsmith", Match::One(dan.id)),
            // A username wins over another's first name: it is whole.
            ("sam", Match::One(sam.id)),
            ("sother", Match::One(sammy.id)),
            ("Example", Match::None),
            ("the vendor", Match::None),
            ("", Match::None),
        ] {
            assert_eq!(match_person(text, &people), want, "{text}");
        }
        let two = vec![
            person("Alex One", "aone", ""),
            person("Alex Two", "atwo", ""),
        ];
        assert_eq!(match_person("alex", &two), Match::Many);
        assert_eq!(count_matches("alex", &two), 2);
        assert_eq!(count_matches("dana", &people), 1);
        assert_eq!(count_matches("nobody", &people), 0);
        // "Last, First": the first word is the last name, without its comma.
        let seth = person("Speaks, Seth", "sspeaks", "");
        assert_eq!(
            match_person("speaks", std::slice::from_ref(&seth)),
            Match::One(seth.id)
        );
        let same = vec![person("Kim", "k1", ""), person("Kim", "k2", "")];
        assert_eq!(match_person("kim", &same), Match::Many);
    }

    #[test]
    fn capture_links_its_waiting_on_one_match_only() {
        let dana = person("Dana Example", "dana", "dana@example.com");
        let people = vec![
            dana.clone(),
            person("Alex One", "a1", ""),
            person("Alex Two", "a2", ""),
        ];
        let today = date(2026, 11, 13);
        let mut d = parse_capture("ping ~dana", today);
        assert_eq!(resolve_waiting(&mut d, &people), Match::One(dana.id));
        assert_eq!(d.waiting.unwrap().on, PersonRef::Person(dana.id));
        let mut d = parse_capture("ping ~alex", today);
        assert_eq!(resolve_waiting(&mut d, &people), Match::Many);
        assert_eq!(d.waiting.unwrap().on, PersonRef::FreeText("alex".into()));
        let mut d = parse_capture(r#"ping ~"the vendor""#, today);
        assert_eq!(resolve_waiting(&mut d, &people), Match::None);
        let mut d = parse_capture("ping", today);
        assert_eq!(resolve_waiting(&mut d, &people), Match::None);
    }

    #[test]
    fn search_ranks_whole_then_word_then_inside() {
        let dana = person("Dana Example", "dana", "dana@example.com");
        let jordan = person("Jordan Lee", "jlee", "jordan.lee@example.com");
        let danielle = person("Danielle Ray", "dray", "");
        let people = vec![jordan.clone(), danielle.clone(), dana.clone()];
        let names = |q: &str| -> Vec<String> {
            search(q, &people, 10)
                .into_iter()
                .map(|p| p.display_name.clone())
                .collect()
        };
        assert_eq!(names("dana"), ["Dana Example"]);
        assert_eq!(names("dan"), ["Dana Example", "Danielle Ray", "Jordan Lee"]);
        assert_eq!(names("lee"), ["Jordan Lee"]);
        assert_eq!(names("zzz"), Vec::<String>::new());
        // A shared domain is no one's word: "e" finds Erin by name first.
        let erin = person("Erin Ng", "eng", "eng@example.com");
        let aaron = person("Aaron Abel", "aabel", "aabel@example.com");
        let both = vec![aaron, erin];
        assert_eq!(
            search("e", &both, 10)
                .iter()
                .map(|p| p.display_name.as_str())
                .collect::<Vec<_>>(),
            ["Erin Ng", "Aaron Abel"]
        );
        assert_eq!(names(""), ["Dana Example", "Danielle Ray", "Jordan Lee"]);
        assert_eq!(search("", &people, 1).len(), 1);
    }
}
