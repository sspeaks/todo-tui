//! Saved views (design 2.4): a stored filter and sort, shown as a tab.

use jiff::Timestamp;
use ulid::Ulid;

use crate::field::ItemField;
use crate::filter::{
    Anchor, Clause, Filter, Group, Operand, Operator, Predicate, RelativeDate, SortKey,
};
use crate::ids::Revision;
use crate::status::Status;
use crate::value::FieldValue;

/// A view's id.
pub type ViewId = Ulid;

/// A saved view, mirroring `todo.v1.View`.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// Unset until stored.
    pub id: Option<ViewId>,
    /// The tab label.
    pub name: String,
    /// Matched as in `ListItems`, implicit rules included.
    pub filter: Filter,
    /// Applied in order, then the tie-break (see [`crate::filter::compare`]).
    pub sort: Vec<SortKey>,
    /// Whether the tab bar shows a count.
    pub show_count: bool,
    /// 0-based place in the tab order.
    pub position: u32,
    /// The default this view was seeded from; unset for a view a user made.
    pub seed: Option<String>,
    /// 0 until stored.
    pub revision: Revision,
    /// Unset until stored.
    pub created_at: Option<Timestamp>,
    /// Unset until stored.
    pub updated_at: Option<Timestamp>,
}

/// The views seeded into every database (design 2.4), in tab order: each
/// with its seed key and position, and no id, revision or timestamps. The
/// implicit rules apply on top of each filter, and the tie-break follows
/// each sort.
pub fn defaults() -> Vec<View> {
    use ItemField as F;
    use Operator as O;
    let today = || Operand::from(RelativeDate::new(Anchor::Today, 0));
    let statuses = |statuses: &[Status]| {
        let operands = statuses
            .iter()
            .map(|&s| Operand::Value(FieldValue::Status(s)))
            .collect();
        Clause::from(Predicate::new(F::Status, O::In, operands))
    };
    let is_set = |field| Clause::from(Predicate::new(field, O::IsSet, Vec::new()));
    let all_of = |clauses: Vec<Clause>| Filter {
        all_of: clauses,
        ..Filter::default()
    };
    let today_filter = Filter {
        any_of: vec![
            Predicate::new(F::DueDate, O::Le, vec![today()]).into(),
            statuses(&[Status::InProgress]),
            Group {
                all_of: vec![
                    is_set(F::Waiting),
                    Predicate::new(F::WaitingFollowUpOn, O::Le, vec![today()]).into(),
                ],
                any_of: Vec::new(),
            }
            .into(),
            Predicate::new(
                F::FocusWeek,
                O::Eq,
                vec![RelativeDate::new(Anchor::CurrentWeek, 0).into()],
            )
            .into(),
        ],
        ..Filter::default()
    };
    let archive_filter = Filter {
        any_of: vec![
            is_set(F::Archived),
            statuses(&[Status::Done, Status::Cancelled]),
        ],
        include_archived: true,
        include_done: true,
        include_deferred: true,
        ..Filter::default()
    };
    let rows = [
        (
            "inbox",
            "Inbox",
            all_of(vec![statuses(&[Status::Inbox])]),
            vec![SortKey::asc(F::CreatedAt)],
            true,
        ),
        (
            "today",
            "Today",
            today_filter,
            vec![
                SortKey::desc(F::IsOverdue),
                SortKey::desc(F::EffectivePriority),
                SortKey::asc(F::DueDate),
            ],
            true,
        ),
        (
            "next",
            "Next",
            all_of(vec![statuses(&[Status::Next, Status::InProgress])]),
            vec![
                SortKey::desc(F::EffectivePriority),
                SortKey::asc(F::DueDate),
            ],
            true,
        ),
        (
            "waiting",
            "Waiting",
            all_of(vec![is_set(F::Waiting)]),
            vec![SortKey::asc(F::WaitingSince)],
            true,
        ),
        (
            "someday",
            "Someday",
            all_of(vec![statuses(&[Status::Someday])]),
            vec![SortKey::desc(F::UpdatedAt)],
            false,
        ),
        (
            "archive",
            "Archive",
            archive_filter,
            vec![SortKey::desc(F::UpdatedAt)],
            false,
        ),
    ];
    rows.into_iter()
        .zip(0..)
        .map(|((seed, name, filter, sort, show_count), position)| View {
            id: None,
            name: name.to_owned(),
            filter,
            sort,
            show_count,
            position,
            seed: Some(seed.to_owned()),
            revision: 0,
            created_at: None,
            updated_at: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use jiff::civil::{Date, date};

    use super::*;
    use crate::filter::{compare, matches, validate_sort};
    use crate::item::{ArchiveReason, Archived, CreatedVia, Item, PersonRef, Priority, Waiting};
    use crate::status::StatusFields;

    /// A Sunday, in ISO week 2026-W43.
    const SUN: Date = date(2026, 10, 25);
    const MON: Date = date(2026, 10, 26);

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    /// Item `n` with `status`, created and updated at minute `n`.
    fn item(n: u128, status: Status, change: impl FnOnce(&mut Item)) -> Item {
        let at = ts(n as i64 * 60);
        let mut item = Item::new_capture(Ulid::from(n), format!("item {n}"), CreatedVia::Tui, at);
        item.set_status_fields(StatusFields::from_status(status));
        change(&mut item);
        item
    }

    fn waiting(since: Date, follow_up_on: Date) -> Option<Waiting> {
        Some(Waiting {
            on: PersonRef::FreeText("the vendor".into()),
            reason: String::new(),
            since,
            follow_up_on: Some(follow_up_on),
        })
    }

    fn archived(at: i64) -> Option<Archived> {
        Some(Archived {
            at: ts(at),
            reason: ArchiveReason::User,
        })
    }

    /// Part of the M3 filter fixture (Sunday 2026-10-25).
    fn fixture() -> Vec<Item> {
        vec![
            item(1, Status::Inbox, |i| {
                i.core.priority = Priority::High;
                i.core.due_date = Some(date(2026, 10, 24));
            }),
            item(2, Status::Next, |i| {
                i.core.priority = Priority::Medium;
                i.core.due_date = Some(SUN);
            }),
            item(3, Status::InProgress, |i| {
                i.core.priority = Priority::Highest;
                i.overlay.local_priority_override = Some(Priority::Low);
            }),
            item(4, Status::Inbox, |_| {}),
            item(5, Status::Someday, |i| {
                i.core.priority = Priority::Lowest;
                i.core.due_date = Some(MON);
            }),
            item(6, Status::Done, |i| {
                i.core.priority = Priority::High;
                i.core.due_date = Some(date(2026, 10, 1));
            }),
            item(7, Status::Cancelled, |_| {}),
            item(8, Status::Next, |i| {
                i.core.priority = Priority::High;
                i.overlay.defer_until = Some(MON);
            }),
            item(9, Status::Next, |i| i.overlay.defer_until = Some(SUN)),
            item(10, Status::Next, |i| {
                i.core.priority = Priority::Low;
                i.overlay.waiting = waiting(date(2026, 10, 20), SUN);
            }),
            item(11, Status::InProgress, |i| {
                i.overlay.waiting = waiting(date(2026, 10, 22), date(2026, 10, 30));
            }),
            item(12, Status::Next, |i| {
                i.overlay.focus_week = Some("2026-W43".into())
            }),
            item(13, Status::Next, |i| {
                i.overlay.focus_week = Some("2026-W44".into())
            }),
            item(14, Status::Next, |i| {
                i.overlay.archived = archived(1_000);
                i.core.due_date = Some(date(2026, 10, 20));
            }),
            item(15, Status::Done, |i| i.overlay.archived = archived(3_000)),
            item(16, Status::Inbox, |i| {
                i.derived.deleted_at = Some(ts(2_000))
            }),
            item(17, Status::Someday, |i| {
                i.overlay.archived = archived(2_000);
                i.overlay.defer_until = Some(date(2026, 11, 30));
            }),
        ]
    }

    /// The ids `view` lists from the fixture on `today`, in its order.
    fn list(view: &View, today: Date) -> Vec<u128> {
        let filter = view.filter.resolve(today).unwrap();
        let mut items: Vec<Item> = fixture()
            .into_iter()
            .filter(|i| matches(i, &filter))
            .collect();
        items.sort_by(|a, b| compare(a, b, &view.sort, today));
        items.iter().map(|i| u128::from(i.id)).collect()
    }

    fn by_seed(seed: &str) -> View {
        defaults()
            .into_iter()
            .find(|v| v.seed.as_deref() == Some(seed))
            .unwrap()
    }

    #[test]
    fn the_six_defaults_are_valid() {
        let views = defaults();
        let rows: Vec<_> = views
            .iter()
            .map(|v| (v.position, v.seed.as_deref(), v.name.as_str(), v.show_count))
            .collect();
        assert_eq!(
            rows,
            [
                (0, Some("inbox"), "Inbox", true),
                (1, Some("today"), "Today", true),
                (2, Some("next"), "Next", true),
                (3, Some("waiting"), "Waiting", true),
                (4, Some("someday"), "Someday", false),
                (5, Some("archive"), "Archive", false),
            ]
        );
        for view in &views {
            assert_eq!(view.filter.validate(), Ok(()), "{}", view.name);
            assert_eq!(validate_sort(&view.sort), Ok(()), "{}", view.name);
            assert!(view.filter.resolve(SUN).is_ok(), "{}", view.name);
            assert_eq!(
                (view.id, view.revision, view.created_at, view.updated_at),
                (None, 0, None, None)
            );
        }
    }

    #[test]
    fn the_defaults_list_the_fixture() {
        assert_eq!(list(&by_seed("inbox"), SUN), [1, 4]);
        // C07 and C08: overdue first, then by effective priority (an
        // override of low ranks 3 with 10), unset due dates last.
        let today = by_seed("today");
        assert_eq!(list(&today, SUN), [1, 2, 3, 10, 11, 12]);
        assert_eq!(list(&today, MON), [1, 2, 3, 10, 5, 11, 13]);
        assert_eq!(list(&by_seed("next"), SUN), [2, 3, 10, 9, 11, 12, 13]);
        assert_eq!(list(&by_seed("next"), MON), [8, 2, 3, 10, 9, 11, 12, 13]);
        assert_eq!(list(&by_seed("waiting"), SUN), [10, 11]);
        assert_eq!(list(&by_seed("someday"), SUN), [5]);
        // Done and cancelled Items before they are archived, and archived
        // ones deferred; never deleted ones.
        assert_eq!(list(&by_seed("archive"), SUN), [17, 15, 14, 7, 6]);
    }
}
