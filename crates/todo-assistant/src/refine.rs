//! `refine` (design 6.2): Claude proposes cleaner fields for an Item from
//! what its text says. Its answer is only a proposal: [`patches`] keeps what
//! is valid and would change something, each patch holding the value Claude
//! was shown, so an edit made meanwhile makes it stale (design 3.11).

use jiff::civil::Date;
use serde_json::{Value, json};
use todo_core::{FieldValue, Item, ItemField, PersonRef, Priority, Waiting};

use crate::provider::Task;

/// The prompt's version, recorded nowhere yet but bumped with any change to
/// [`SYSTEM`] (design 6.4).
pub const PROMPT_VERSION: &str = "refine.v1";

/// What Claude is told it is doing (design 6.4: the stable part first).
pub const SYSTEM: &str = include_str!("../prompts/refine.v1.md");

/// The answer's shape: every field optional, `null` or absent for no
/// change.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["rationale"],
        "properties": {
            "rationale": {"type": "string", "description": "One or two sentences: why, and any likely duplicate"},
            "title": {"type": ["string", "null"]},
            "description": {"type": ["string", "null"]},
            "priority": {"type": ["string", "null"], "enum": ["none", "lowest", "low", "medium", "high", "highest", null]},
            "due_date": {"type": ["string", "null"], "description": "YYYY-MM-DD"},
            "labels": {"type": ["array", "null"], "items": {"type": "string"}},
            "waiting_on": {"type": ["string", "null"], "description": "Who the item waits on, as a name"},
            "waiting_reason": {"type": ["string", "null"]}
        }
    })
}

/// What Claude sees besides the Item: today, the labels already in use, the
/// person's style notes, and Items that may be the same thing.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub today: Option<Date>,
    pub labels_in_use: Vec<String>,
    pub style_notes: String,
    /// `(key or short id, title)` of Items like this one (design 6.3).
    pub candidates: Vec<(String, String)>,
    pub include_private_notes: bool,
}

/// The task for `item`: the system prompt and the context as one prompt
/// (design 6.4: most stable first).
pub fn task(item: &Item, ctx: &Context) -> Task {
    let mut p = String::new();
    if let Some(today) = ctx.today {
        p.push_str(&format!("Today is {} ({}).\n", today, today.strftime("%A")));
    }
    p.push_str("Priorities: none, lowest, low, medium, high, highest.\n");
    if !ctx.labels_in_use.is_empty() {
        p.push_str(&format!(
            "Labels in use: {}.\n",
            ctx.labels_in_use.join(", ")
        ));
    }
    if !ctx.style_notes.trim().is_empty() {
        p.push_str(&format!(
            "The person's style notes: {}\n",
            ctx.style_notes.trim()
        ));
    }
    if !ctx.candidates.is_empty() {
        p.push_str("\nOther items that may be the same thing:\n");
        for (id, title) in &ctx.candidates {
            p.push_str(&format!("- {id}: {title}\n"));
        }
    }
    p.push_str("\nThe item to refine, as JSON:\n");
    p.push_str(
        &serde_json::to_string_pretty(&item_json(item, ctx.include_private_notes))
            .unwrap_or_default(),
    );
    p.push('\n');
    Task {
        system: SYSTEM.to_owned(),
        prompt: p,
        schema: schema(),
    }
}

/// The Item as Claude is shown it (design 6.10: no private notes unless the
/// person allows them).
fn item_json(item: &Item, private: bool) -> Value {
    let mut v = json!({
        "title": item.core.title,
        "description": item.core.description,
        "priority": priority_word(item.core.priority),
        "due_date": item.core.due_date.map(|d| d.to_string()),
        "labels": item.core.labels,
        "waiting_on": item.overlay.waiting.as_ref().map(|w| person(&w.on)),
        "waiting_reason": item.overlay.waiting.as_ref().map(|w| w.reason.clone()),
    });
    if let Some(link) = &item.remote_link {
        v["jira_key"] = json!(link.key);
    }
    if private && !item.overlay.private_notes.is_empty() {
        v["private_notes"] = json!(item.overlay.private_notes);
    }
    v
}

fn person(p: &PersonRef) -> String {
    match p {
        PersonRef::FreeText(t) => t.clone(),
        PersonRef::Person(id) => id.to_string(),
    }
}

fn priority_word(p: Priority) -> &'static str {
    match p {
        Priority::None => "none",
        Priority::Lowest => "lowest",
        Priority::Low => "low",
        Priority::Medium => "medium",
        Priority::High => "high",
        Priority::Highest => "highest",
    }
}

fn priority_of(s: &str) -> Option<Priority> {
    Some(match s {
        "none" => Priority::None,
        "lowest" => Priority::Lowest,
        "low" => Priority::Low,
        "medium" => Priority::Medium,
        "high" => Priority::High,
        "highest" => Priority::Highest,
        _ => return None,
    })
}

/// The patches Claude's `answer` makes for `item`, as `(field, the value
/// Claude saw, the value proposed)`: only what is valid, and only what would
/// change. `editable` says which fields the person may change on this Item
/// (a linked Item's tracker-only fields are not), so none is proposed that
/// applying would refuse.
pub fn patches(
    item: &Item,
    answer: &Value,
    today: Date,
    editable: impl Fn(ItemField) -> bool,
) -> (String, Vec<(ItemField, FieldValue, FieldValue)>) {
    let rationale = answer["rationale"].as_str().unwrap_or_default().trim();
    let rationale: String = rationale.chars().take(500).collect();
    let mut out = Vec::new();
    let mut add = |field: ItemField, new: FieldValue| {
        if !editable(field) {
            return;
        }
        let Some(old) = FieldValue::of(item, field) else {
            return;
        };
        if old != new {
            out.push((field, old, new));
        }
    };
    let text = |k: &str| answer[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    if let Some(t) = text("title")
        && !t.contains(['\n', '\r'])
        && t.len() <= 500
    {
        add(ItemField::Title, FieldValue::String(t.to_owned()));
    }
    if let Some(d) = answer["description"].as_str()
        && d.trim() != item.core.description.trim()
    {
        add(
            ItemField::Description,
            FieldValue::Markdown(d.trim_end().to_owned()),
        );
    }
    if let Some(p) = text("priority").and_then(priority_of) {
        add(ItemField::Priority, FieldValue::Priority(p));
    }
    if let Some(d) = text("due_date").and_then(|d| d.parse::<Date>().ok()) {
        add(ItemField::DueDate, FieldValue::Date(d));
    }
    if let Some(labels) = answer["labels"].as_array() {
        let mut set: Vec<String> = labels
            .iter()
            .filter_map(Value::as_str)
            .map(|l| l.trim().trim_start_matches('#').to_owned())
            .filter(|l| !l.is_empty() && !l.contains(|c: char| c.is_whitespace() || c.is_control()))
            .collect();
        set.sort();
        set.dedup();
        let mut now = item.core.labels.clone();
        now.sort();
        if set != now {
            add(ItemField::Labels, FieldValue::StringSet(set));
        }
    }
    if let Some(on) = text("waiting_on")
        && !on.contains(['\n', '\r'])
    {
        let reason = text("waiting_reason").unwrap_or_default().to_owned();
        let same = item
            .overlay
            .waiting
            .as_ref()
            .is_some_and(|w| person(&w.on) == on && w.reason == reason);
        if !same {
            let since = item.overlay.waiting.as_ref().map_or(today, |w| w.since);
            let follow_up_on = item.overlay.waiting.as_ref().and_then(|w| w.follow_up_on);
            add(
                ItemField::Waiting,
                FieldValue::Waiting(Waiting {
                    on: PersonRef::FreeText(on.to_owned()),
                    reason,
                    since,
                    follow_up_on,
                }),
            );
        }
    }
    (rationale, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn item(title: &str) -> Item {
        Item::new_capture(
            todo_core::new_ulid(),
            title.to_owned(),
            todo_core::CreatedVia::Cli,
            jiff::Timestamp::UNIX_EPOCH,
        )
    }

    const TODAY: Date = date(2026, 11, 12);

    #[test]
    fn a_valid_answer_becomes_patches_of_what_changes() {
        let mut it = item("ping dana fri re flaky login test");
        it.core.labels = vec!["work".into()];
        let answer = json!({
            "rationale": "A follow-up with Dana, due Friday.",
            "title": "Follow up with Dana on the flaky login test",
            "priority": "medium",
            "due_date": "2026-11-13",
            "labels": ["work", "#testing"],
            "waiting_on": "Dana",
            "waiting_reason": "the login test fix"
        });
        let (why, p) = patches(&it, &answer, TODAY, |_| true);
        assert_eq!(why, "A follow-up with Dana, due Friday.");
        let fields: Vec<ItemField> = p.iter().map(|x| x.0).collect();
        assert_eq!(
            fields,
            [
                ItemField::Title,
                ItemField::Priority,
                ItemField::DueDate,
                ItemField::Labels,
                ItemField::Waiting
            ]
        );
        // Each patch holds what Claude saw, so an edit meanwhile makes it stale.
        assert_eq!(
            p[0].1,
            FieldValue::String("ping dana fri re flaky login test".into())
        );
        assert_eq!(
            p[3].2,
            FieldValue::StringSet(vec!["testing".into(), "work".into()])
        );
        let FieldValue::Waiting(w) = &p[4].2 else {
            panic!()
        };
        assert_eq!(
            (w.on.clone(), w.since),
            (PersonRef::FreeText("Dana".into()), TODAY)
        );
    }

    #[test]
    fn what_is_invalid_unchanged_or_not_editable_is_left_out() {
        let mut it = item("Buy milk");
        it.core.priority = Priority::High;
        let answer = json!({
            "rationale": "",
            "title": "Buy milk",
            "priority": "urgent",
            "due_date": "friday",
            "labels": ["two words", ""],
            "waiting_on": "a\nb",
            "description": "longer text"
        });
        let (_, p) = patches(&it, &answer, TODAY, |f| f != ItemField::Description);
        assert!(p.is_empty(), "{p:?}");
    }

    #[test]
    fn the_prompt_holds_the_context_and_no_private_notes_unless_allowed() {
        let mut it = item("x");
        it.overlay.private_notes = "secret".into();
        let ctx = Context {
            today: Some(TODAY),
            labels_in_use: vec!["work".into()],
            style_notes: "short titles".into(),
            candidates: vec![("PLAT-412".into(), "Auth test timeouts".into())],
            include_private_notes: false,
        };
        let t = task(&it, &ctx);
        for want in [
            "Today is 2026-11-12 (Thursday)",
            "Labels in use: work.",
            "short titles",
            "PLAT-412: Auth test timeouts",
        ] {
            assert!(t.prompt.contains(want), "{want:?} not in {}", t.prompt);
        }
        assert!(!t.prompt.contains("secret"));
        let t = task(
            &it,
            &Context {
                include_private_notes: true,
                ..ctx
            },
        );
        assert!(t.prompt.contains("secret"));
        assert!(t.system.contains("refine"));
    }
}
