//! `ops.summary`: the toast text of an op, e.g. `mark done (3 items)`.
//! Clients add the verb ("Undid: mark done (3 items)"). Deterministic
//! English, computed when the op is recorded.

use todo_core::{ItemId, RelationKind, Status};

/// Titles in summaries are cut to this many characters, ellipsis included.
const MAX_TITLE_CHARS: usize = 40;

/// What one mutation of an op did, for its summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Edit,
    SetStatus(Status),
    Archive,
    Unarchive,
    Delete,
    Undelete,
}

impl Action {
    fn verb(self) -> &'static str {
        match self {
            Action::Edit => "edit",
            Action::SetStatus(Status::Inbox) => "move to inbox",
            Action::SetStatus(Status::Next) => "move to next",
            Action::SetStatus(Status::Someday) => "move to someday",
            Action::SetStatus(Status::InProgress) => "start",
            Action::SetStatus(Status::Done) => "mark done",
            Action::SetStatus(Status::Cancelled) => "cancel",
            Action::Archive => "archive",
            Action::Unarchive => "unarchive",
            Action::Delete => "delete",
            Action::Undelete => "undelete",
        }
    }
}

/// `"<title>"`, cut to [`MAX_TITLE_CHARS`] with `…`.
fn quoted(title: &str) -> String {
    if title.chars().count() <= MAX_TITLE_CHARS {
        return format!("\"{title}\"");
    }
    let cut: String = title.chars().take(MAX_TITLE_CHARS - 1).collect();
    format!("\"{cut}…\"")
}

pub(crate) fn capture(title: &str) -> String {
    format!("capture {}", quoted(title))
}

/// The summary of a mutate op: its mutations' `(action, Item, title)` in
/// request order, and how many descendants a delete or undelete cascaded
/// to that no mutation names. One Item reads `mark done "x"`; several with one shared action read
/// `mark done (3 items)` for a status and `edit 3 items` otherwise; mixed
/// actions read `change 3 items`.
pub(crate) fn mutation(actions: &[(Action, ItemId, &str)], subtasks: usize) -> String {
    let mut items: Vec<ItemId> = actions.iter().map(|(_, id, _)| *id).collect();
    items.sort_unstable();
    items.dedup();
    let Some(&(first, _, title)) = actions.first() else {
        return "change nothing".to_owned();
    };
    let shared = actions
        .iter()
        .all(|(action, _, _)| *action == first)
        .then_some(first);
    let text = match (shared, items.len()) {
        (Some(action), 1) => format!("{} {}", action.verb(), quoted(title)),
        (Some(action @ Action::SetStatus(_)), n) => format!("{} ({n} items)", action.verb()),
        (Some(action), n) => format!("{} {n} items", action.verb()),
        (None, 1) => format!("change {}", quoted(title)),
        (None, n) => format!("change {n} items"),
    };
    match subtasks {
        0 => text,
        1 => format!("{text} and 1 subtask"),
        n => format!("{text} and {n} subtasks"),
    }
}

/// `relate "a" blocks "b"`, or `unrelate …` for a removal.
pub(crate) fn relation(add: bool, kind: RelationKind, from: &str, to: &str) -> String {
    let verb = if add { "relate" } else { "unrelate" };
    let kind = match kind {
        RelationKind::Blocks => "blocks",
        RelationKind::Duplicates => "duplicates",
        RelationKind::RelatesTo => "relates to",
    };
    format!("{verb} {} {kind} {}", quoted(from), quoted(to))
}

/// `undo: <entry>` or `redo: <entry>`, or `nothing to undo` on an empty stack.
pub(crate) fn undo(redo: bool, entry: Option<&str>) -> String {
    let verb = if redo { "redo" } else { "undo" };
    match entry {
        Some(entry) => format!("{verb}: {entry}"),
        None => format!("nothing to {verb}"),
    }
}

#[cfg(test)]
mod tests {
    use ulid::Ulid;

    use super::*;

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    #[test]
    fn titles_are_quoted_and_cut() {
        assert_eq!(capture("call dentist"), "capture \"call dentist\"");
        let forty = "a".repeat(40);
        assert_eq!(capture(&forty), format!("capture \"{forty}\""));
        let long = format!("{forty}é and more");
        assert_eq!(capture(&long), format!("capture \"{}…\"", "a".repeat(39)));
        // Characters, not bytes.
        let wide = "é".repeat(41);
        assert_eq!(quoted(&wide), format!("\"{}…\"", "é".repeat(39)));
    }

    #[test]
    fn one_item_names_its_title() {
        let rows = [
            (Action::Edit, "edit \"x\""),
            (Action::SetStatus(Status::Inbox), "move to inbox \"x\""),
            (Action::SetStatus(Status::Next), "move to next \"x\""),
            (Action::SetStatus(Status::Someday), "move to someday \"x\""),
            (Action::SetStatus(Status::InProgress), "start \"x\""),
            (Action::SetStatus(Status::Done), "mark done \"x\""),
            (Action::SetStatus(Status::Cancelled), "cancel \"x\""),
            (Action::Archive, "archive \"x\""),
            (Action::Unarchive, "unarchive \"x\""),
            (Action::Delete, "delete \"x\""),
            (Action::Undelete, "undelete \"x\""),
        ];
        for (action, want) in rows {
            assert_eq!(mutation(&[(action, id(1), "x")], 0), want);
        }
        // The same action twice on one Item is still one Item.
        assert_eq!(
            mutation(&[(Action::Edit, id(1), "x"), (Action::Edit, id(1), "x")], 0),
            "edit \"x\""
        );
        assert_eq!(
            mutation(
                &[(Action::Edit, id(1), "x"), (Action::Archive, id(1), "x")],
                0
            ),
            "change \"x\""
        );
    }

    #[test]
    fn several_items_are_counted() {
        let done = Action::SetStatus(Status::Done);
        let three = |action| {
            [
                (action, id(1), "a"),
                (action, id(2), "b"),
                (action, id(3), "c"),
            ]
        };
        assert_eq!(mutation(&three(done), 0), "mark done (3 items)");
        assert_eq!(mutation(&three(Action::Edit), 0), "edit 3 items");
        assert_eq!(mutation(&three(Action::Archive), 0), "archive 3 items");
        // Different targets, or different actions: a plain count.
        let mixed = [
            (done, id(1), "a"),
            (Action::SetStatus(Status::Next), id(2), "b"),
        ];
        assert_eq!(mutation(&mixed, 0), "change 2 items");
        let mixed = [(Action::Edit, id(1), "a"), (Action::Delete, id(2), "b")];
        assert_eq!(mutation(&mixed, 0), "change 2 items");
        assert_eq!(mutation(&[], 0), "change nothing");
    }

    #[test]
    fn a_cascade_counts_its_subtasks() {
        assert_eq!(
            mutation(&[(Action::Delete, id(1), "x")], 1),
            "delete \"x\" and 1 subtask"
        );
        assert_eq!(
            mutation(&[(Action::Undelete, id(1), "x")], 3),
            "undelete \"x\" and 3 subtasks"
        );
        let two = [(Action::Delete, id(1), "a"), (Action::Delete, id(2), "b")];
        assert_eq!(mutation(&two, 2), "delete 2 items and 2 subtasks");
    }

    #[test]
    fn relations_and_undo() {
        assert_eq!(
            relation(true, RelationKind::Blocks, "a", "b"),
            "relate \"a\" blocks \"b\""
        );
        assert_eq!(
            relation(true, RelationKind::Duplicates, "a", "b"),
            "relate \"a\" duplicates \"b\""
        );
        assert_eq!(
            relation(false, RelationKind::RelatesTo, "a", "b"),
            "unrelate \"a\" relates to \"b\""
        );
        assert_eq!(undo(false, Some("edit \"x\"")), "undo: edit \"x\"");
        assert_eq!(undo(true, Some("edit \"x\"")), "redo: edit \"x\"");
        assert_eq!(undo(false, None), "nothing to undo");
        assert_eq!(undo(true, None), "nothing to redo");
    }
}
