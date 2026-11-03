//! The Teams meeting form, prefilled from Items (design 2.9): a deep link
//! that Teams opens; todo-tui never creates the meeting itself.

use crate::item::{Item, PersonRef};

/// Teams' new-meeting form.
pub const TEAMS_NEW_MEETING: &str = "https://teams.microsoft.com/l/meeting/new";

/// What the form is filled with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meeting {
    pub subject: String,
    pub content: String,
    /// Email addresses.
    pub attendees: Vec<String>,
}

/// The longest content the link carries, in bytes before encoding: links
/// over about 8 KB fail in some browsers.
const MAX_CONTENT: usize = 2000;

/// The meeting about `items` (at least one), in order: the subject is the
/// first title plus "+ N more" (unless `subject` overrides it), the content
/// each Item's first description paragraph and Jira link, the attendees
/// each `waiting.on` that is one email address.
pub fn meeting(items: &[Item], subject: Option<&str>) -> Meeting {
    let first = items
        .first()
        .map(|i| single_line(&i.core.title))
        .unwrap_or_default();
    let subject = match subject.map(single_line).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None if items.len() > 1 => format!("{first} + {} more", items.len() - 1),
        None => first,
    };
    let mut parts = Vec::new();
    for item in items {
        let mut part = Vec::new();
        if items.len() > 1 {
            part.push(single_line(&item.core.title));
        }
        let paragraph = first_paragraph(&item.core.description);
        if !paragraph.is_empty() {
            part.push(paragraph);
        }
        if let Some(link) = &item.remote_link {
            let key = link.key.trim();
            match (key.is_empty(), link.url.trim()) {
                (false, "") => part.push(key.to_owned()),
                (false, url) => part.push(format!("{key}: {url}")),
                (true, "") => {}
                (true, url) => part.push(url.to_owned()),
            }
        }
        if !part.is_empty() {
            parts.push(part.join("\n"));
        }
    }
    let mut content = parts.join("\n\n");
    if content.len() > MAX_CONTENT {
        let mut end = MAX_CONTENT;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
        content.push('…');
    }
    let mut attendees: Vec<String> = Vec::new();
    for item in items {
        if let Some(PersonRef::FreeText(on)) = item.overlay.waiting.as_ref().map(|w| &w.on) {
            let on = on.trim();
            if is_email(on) && !attendees.iter().any(|a| a.eq_ignore_ascii_case(on)) {
                attendees.push(on.to_owned());
            }
        }
    }
    Meeting {
        subject,
        content,
        attendees,
    }
}

impl Meeting {
    /// The deep link: each field percent-encoded, a space as `%20` (Teams
    /// refuses `+`), the empty ones left out. No times: Teams' own
    /// scheduling assistant finds the slot.
    pub fn teams_url(&self) -> String {
        let mut params = vec![("subject", self.subject.clone())];
        if !self.content.is_empty() {
            params.push(("content", self.content.clone()));
        }
        if !self.attendees.is_empty() {
            params.push(("attendees", self.attendees.join(",")));
        }
        let query: Vec<String> = params
            .into_iter()
            .map(|(k, v)| format!("{k}={}", encode(&v)))
            .collect();
        format!("{TEAMS_NEW_MEETING}?{}", query.join("&"))
    }
}

/// One `@`, something on each side, a dot after it, no whitespace.
fn is_email(s: &str) -> bool {
    let mut parts = s.split('@');
    let (Some(user), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !user.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The first paragraph: lines up to the first blank one, joined.
fn first_paragraph(markdown: &str) -> String {
    markdown
        .trim()
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whitespace collapsed, control characters (which a title may hold) as
/// spaces.
fn single_line(s: &str) -> String {
    let printable: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    printable.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// RFC 3986 percent-encoding of everything but the unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use jiff::civil::date;

    use super::*;
    use crate::item::{CreatedVia, LinkState, RemoteLink, Waiting};

    fn item(title: &str) -> Item {
        Item::new_capture(
            crate::new_ulid(),
            title.into(),
            CreatedVia::Tui,
            Timestamp::UNIX_EPOCH,
        )
    }

    fn waiting_on(item: &mut Item, on: &str) {
        item.overlay.waiting = Some(Waiting {
            on: PersonRef::FreeText(on.into()),
            reason: String::new(),
            since: date(2026, 11, 3),
            follow_up_on: None,
        });
    }

    #[test]
    fn one_item_fills_the_subject_content_and_attendee() {
        let mut a = item("Plan the Q4 offsite");
        a.core.description = "Agenda & budget\nfor the team.\n\nMore later.".into();
        a.remote_link = Some(RemoteLink {
            tracker_kind: "jira".into(),
            instance_id: "w".into(),
            remote_id: "1".into(),
            key: "PLAT-1".into(),
            url: "https://jira.example.com/browse/PLAT-1".into(),
            last_pulled_version: None,
            link_state: LinkState::Ok,
            pinned: false,
        });
        waiting_on(&mut a, "dana@example.com");
        let m = meeting(&[a], None);
        assert_eq!(
            m,
            Meeting {
                subject: "Plan the Q4 offsite".into(),
                content:
                    "Agenda & budget for the team.\nPLAT-1: https://jira.example.com/browse/PLAT-1"
                        .into(),
                attendees: vec!["dana@example.com".into()],
            }
        );
        assert_eq!(
            m.teams_url(),
            "https://teams.microsoft.com/l/meeting/new?subject=Plan%20the%20Q4%20offsite\
             &content=Agenda%20%26%20budget%20for%20the%20team.%0APLAT-1%3A%20https%3A%2F%2Fjira.example.com%2Fbrowse%2FPLAT-1\
             &attendees=dana%40example.com"
        );
    }

    #[test]
    fn several_items_name_the_first_and_count_the_rest() {
        let mut a = item("Café #1");
        let mut b = item("b");
        let c = item("c");
        waiting_on(&mut a, "Dana@Example.com");
        waiting_on(&mut b, "dana@example.com");
        let m = meeting(&[a.clone(), b, c], None);
        assert_eq!(m.subject, "Café #1 + 2 more");
        assert_eq!(m.content, "Café #1\n\nb\n\nc");
        assert_eq!(m.attendees, ["Dana@Example.com"]);
        assert!(m.teams_url().starts_with(&format!(
            "{TEAMS_NEW_MEETING}?subject=Caf%C3%A9%20%231%20%2B%202%20more&"
        )));
        // A subject given wins; free text that is not one address is no attendee.
        let mut d = item("d");
        waiting_on(&mut d, "the vendor");
        let m = meeting(&[d], Some("  Kickoff\n"));
        assert_eq!((m.subject.as_str(), m.attendees.len()), ("Kickoff", 0));
        assert_eq!(
            m.teams_url(),
            format!("{TEAMS_NEW_MEETING}?subject=Kickoff")
        );
    }

    #[test]
    fn addresses() {
        for ok in ["a@b.co", "first.last+tag@sub.example.org"] {
            assert!(is_email(ok), "{ok}");
        }
        for bad in [
            "dana",
            "a@b",
            "@b.co",
            "a@@b.co",
            "a b@c.co",
            "a@.co",
            "a@b.",
            "a@b.co@d.co",
        ] {
            assert!(!is_email(bad), "{bad}");
        }
    }

    #[test]
    fn long_content_is_cut() {
        let mut a = item("x");
        a.core.description = "é".repeat(MAX_CONTENT);
        let m = meeting(&[a], None);
        assert!(m.content.len() <= MAX_CONTENT + '…'.len_utf8());
        assert!(m.content.ends_with('…'));
    }
}
