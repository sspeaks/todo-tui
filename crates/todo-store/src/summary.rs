//! `ops.summary`: the toast text of an op, e.g. `mark done (3 items)`.
//! Clients add the verb ("Undid: mark done (3 items)"). Deterministic
//! English, computed when the op is recorded.

use jiff::civil::Date;
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
    /// `SetRecurrence`; `false` clears the rule.
    SetRecurrence(bool),
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
            Action::SetRecurrence(true) => "set repeat",
            Action::SetRecurrence(false) => "stop repeating",
        }
    }

    /// Whether several Items read `<verb> (3 items)` rather than
    /// `<verb> 3 items`: the verbs that already end in a word the count
    /// would read as its object ("mark done 3 items", "set repeat 3 items").
    fn counted_in_parens(self) -> bool {
        matches!(self, Action::SetStatus(_) | Action::SetRecurrence(_))
    }
}

/// `Mon Nov 2`, with the year when it is not `today`'s.
fn day(date: Date, today: Date) -> String {
    let format = if date.year() == today.year() {
        "%a %b %-d"
    } else {
        "%a %b %-d %Y"
    };
    date.strftime(format).to_string()
}

/// `"<title>"`, cut to [`MAX_TITLE_CHARS`] with `…`.
pub(crate) fn quoted(title: &str) -> String {
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
/// request order, how many descendants a delete or undelete cascaded
/// to that no mutation names, and the dates of the instances the op
/// generated (design 3.8), with `today` deciding when a date needs its
/// year. One Item reads `mark done "x"`; several with one shared action read
/// `mark done (3 items)` for a status and `edit 3 items` otherwise; mixed
/// actions read `change 3 items`. A generated instance adds
/// `· next Mon Nov 2` for one Item, `· 2 next` for several.
pub(crate) fn mutation(
    actions: &[(Action, ItemId, &str)],
    subtasks: usize,
    generated: &[Date],
    today: Date,
) -> String {
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
    let one = items.len() == 1;
    let text = match (shared, items.len()) {
        (Some(action), 1) => format!("{} {}", action.verb(), quoted(title)),
        (Some(action), n) if action.counted_in_parens() => {
            format!("{} ({n} items)", action.verb())
        }
        (Some(action), n) => format!("{} {n} items", action.verb()),
        (None, 1) => format!("change {}", quoted(title)),
        (None, n) => format!("change {n} items"),
    };
    let text = match subtasks {
        0 => text,
        1 => format!("{text} and 1 subtask"),
        n => format!("{text} and {n} subtasks"),
    };
    match (generated, one) {
        ([], _) => text,
        ([next], true) => format!("{text} · next {}", day(*next, today)),
        (next, _) => format!("{text} · {} next", next.len()),
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

/// `3 done items`, or `1 done item`.
fn items(n: usize, what: &str) -> String {
    match n {
        1 => format!("1 {what} item"),
        n => format!("{n} {what} items"),
    }
}

/// The scheduler's sweep: `auto-archive 3 done items`, `unarchive 1
/// reopened item`, or both joined by `and`. Cancelled Items count as done,
/// as in the Archive view.
pub(crate) fn auto_archive(archived: usize, healed: usize) -> String {
    let archive = format!("auto-archive {}", items(archived, "done"));
    let heal = format!("unarchive {}", items(healed, "reopened"));
    match (archived, healed) {
        (_, 0) => archive,
        (0, _) => heal,
        _ => format!("{archive} and {heal}"),
    }
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
    use jiff::civil::date;
    use ulid::Ulid;

    use super::*;

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    /// A Monday.
    const TODAY: Date = date(2026, 10, 26);

    /// [`mutation`] with nothing generated, which most cases are.
    fn sum(actions: &[(Action, ItemId, &str)], subtasks: usize) -> String {
        mutation(actions, subtasks, &[], TODAY)
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
            (Action::SetRecurrence(true), "set repeat \"x\""),
            (Action::SetRecurrence(false), "stop repeating \"x\""),
        ];
        for (action, want) in rows {
            assert_eq!(sum(&[(action, id(1), "x")], 0), want);
        }
        // The same action twice on one Item is still one Item.
        assert_eq!(
            sum(&[(Action::Edit, id(1), "x"), (Action::Edit, id(1), "x")], 0),
            "edit \"x\""
        );
        assert_eq!(
            sum(
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
        assert_eq!(sum(&three(done), 0), "mark done (3 items)");
        assert_eq!(sum(&three(Action::Edit), 0), "edit 3 items");
        assert_eq!(sum(&three(Action::Archive), 0), "archive 3 items");
        assert_eq!(
            sum(&three(Action::SetRecurrence(true)), 0),
            "set repeat (3 items)"
        );
        assert_eq!(
            sum(&three(Action::SetRecurrence(false)), 0),
            "stop repeating (3 items)"
        );
        // Different targets, or different actions: a plain count.
        let mixed = [
            (done, id(1), "a"),
            (Action::SetStatus(Status::Next), id(2), "b"),
        ];
        assert_eq!(sum(&mixed, 0), "change 2 items");
        let mixed = [(Action::Edit, id(1), "a"), (Action::Delete, id(2), "b")];
        assert_eq!(sum(&mixed, 0), "change 2 items");
        assert_eq!(sum(&[], 0), "change nothing");
    }

    #[test]
    fn a_cascade_counts_its_subtasks() {
        assert_eq!(
            sum(&[(Action::Delete, id(1), "x")], 1),
            "delete \"x\" and 1 subtask"
        );
        assert_eq!(
            sum(&[(Action::Undelete, id(1), "x")], 3),
            "undelete \"x\" and 3 subtasks"
        );
        let two = [(Action::Delete, id(1), "a"), (Action::Delete, id(2), "b")];
        assert_eq!(sum(&two, 2), "delete 2 items and 2 subtasks");
    }

    #[test]
    fn a_generated_instance_names_its_date() {
        let done = Action::SetStatus(Status::Done);
        let one = [(done, id(1), "water plants")];
        assert_eq!(
            mutation(&one, 0, &[date(2026, 11, 2)], TODAY),
            "mark done \"water plants\" · next Mon Nov 2"
        );
        assert_eq!(
            mutation(
                &[(Action::SetStatus(Status::Cancelled), id(1), "x")],
                0,
                &[date(2026, 10, 26)],
                TODAY
            ),
            "cancel \"x\" · next Mon Oct 26"
        );
        // Another year is spelled out.
        assert_eq!(
            mutation(&one, 0, &[date(2027, 1, 4)], TODAY),
            "mark done \"water plants\" · next Mon Jan 4 2027"
        );
        // Several Items are counted, however many instances they made, and
        // so is one Item that somehow made two.
        let three = [(done, id(1), "a"), (done, id(2), "b"), (done, id(3), "c")];
        let dates = [date(2026, 11, 2), date(2026, 11, 3)];
        assert_eq!(
            mutation(&three, 0, &dates, TODAY),
            "mark done (3 items) · 2 next"
        );
        assert_eq!(
            mutation(&three, 0, &dates[..1], TODAY),
            "mark done (3 items) · 1 next"
        );
        // After the subtask count, which belongs to the action.
        assert_eq!(
            mutation(&one, 1, &dates[..1], TODAY),
            "mark done \"water plants\" and 1 subtask · next Mon Nov 2"
        );
    }

    #[test]
    fn the_sweep_counts_what_it_archived_and_healed() {
        assert_eq!(auto_archive(1, 0), "auto-archive 1 done item");
        assert_eq!(auto_archive(3, 0), "auto-archive 3 done items");
        assert_eq!(auto_archive(0, 1), "unarchive 1 reopened item");
        assert_eq!(
            auto_archive(2, 1),
            "auto-archive 2 done items and unarchive 1 reopened item"
        );
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
