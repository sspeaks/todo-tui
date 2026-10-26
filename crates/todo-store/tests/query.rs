//! The filter compiler, sorting and keyset paging, seeded views and counts
//! (design 4.8, 2.4), against the core reference semantics.

use std::path::PathBuf;

use jiff::civil::{Date, date};
use jiff::{SignedDuration, Timestamp};
use rusqlite::Connection;
use rusqlite::types::Value;
use todo_core::filter::{
    Anchor, Clause, FilterErrorKind, Group, MAX_PREDICATE_OPERANDS, Operand, Operator, Predicate,
    RelativeDate, SortKey, ValueClass, compare, matches, target,
};
use todo_core::view::{View, defaults};
use todo_core::{
    ArchiveReason, CaptureDraft, CreatedVia, FieldValue, Filter, Item, ItemField, ItemId,
    LinkState, PersonRef, Priority, ResolutionKind, Revision, Status, StatusCategory,
    TodoRefinement, Topic, new_ulid,
};
use todo_store::{CaptureInput, ListInput, Resource, SCHEMA_VERSION, Store, StoreError};

use ItemField as F;
use Operator as O;

struct Db {
    _dir: tempfile::TempDir,
    path: PathBuf,
    store: Store,
}

fn open() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("todo.db");
    let store = Store::open(&path).unwrap();
    Db {
        _dir: dir,
        path,
        store,
    }
}

fn reopen(db: Db) -> Db {
    let Db { _dir, path, store } = db;
    drop(store);
    let store = Store::open(&path).unwrap();
    Db { _dir, path, store }
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// The fixture's today: a Sunday, in ISO week 2026-W43 (Monday 10-19 to
/// here).
const SUN: Date = date(2026, 10, 25);
const MON: Date = date(2026, 10, 26);

async fn capture_at(db: &Db, title: &str, now: Timestamp) -> Item {
    let input = CaptureInput {
        request_id: new_ulid().to_string(),
        draft: CaptureDraft::from_title(title),
        created_via: CreatedVia::Cli,
        parent_id: None,
        spool_id: None,
        now,
        today: SUN,
    };
    db.store.capture(input).await.unwrap().0
}

/// Columns to write, each with its value.
type Columns = Vec<(&'static str, Value)>;

/// Writes `sets` (`column = ?`, with its value) on the Item `id`, through
/// raw SQL: most of them are read-only to the API.
async fn update(db: &Db, id: ItemId, sets: Columns) {
    if sets.is_empty() {
        return;
    }
    db.store
        .write(move |tx| {
            let columns: Vec<String> = sets.iter().map(|(c, _)| format!("{c} = ?")).collect();
            let sql = format!("UPDATE items SET {} WHERE id = ?", columns.join(", "));
            let mut args: Vec<Value> = sets.into_iter().map(|(_, v)| v).collect();
            args.push(Value::Text(id.to_string()));
            tx.conn().execute(&sql, rusqlite::params_from_iter(args))?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn sql(db: &Db, sql: &'static str) {
    db.store
        .write(move |tx| {
            tx.conn().execute_batch(sql)?;
            Ok(())
        })
        .await
        .unwrap();
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

fn int(n: i64) -> Value {
    Value::Integer(n)
}

fn ns(t: &str) -> Value {
    Value::Integer(ts(t).as_nanosecond() as i64)
}

/// The status triple's columns for `status`.
fn status_sets(status: Status) -> Columns {
    let (category, resolution, refinement) = match status {
        Status::Inbox => ("todo", None, "inbox"),
        Status::Next => ("todo", None, "next"),
        Status::Someday => ("todo", None, "someday"),
        Status::InProgress => ("in_progress", None, "inbox"),
        Status::Done => ("done", Some("done"), "inbox"),
        Status::Cancelled => ("done", Some("cancelled"), "inbox"),
    };
    vec![
        ("status_category", text(category)),
        ("resolution_kind", resolution.map_or(Value::Null, text)),
        ("todo_refinement", text(refinement)),
    ]
}

// ---- filters --------------------------------------------------------------

fn p(target: ItemField, op: Operator, operands: Vec<Operand>) -> Clause {
    Predicate::new(target, op, operands).into()
}

fn val(v: FieldValue) -> Operand {
    Operand::Value(v)
}

fn s(v: &str) -> Operand {
    val(FieldValue::String(v.into()))
}

fn st(status: Status) -> Operand {
    val(FieldValue::Status(status))
}

fn prio(p: Priority) -> Operand {
    val(FieldValue::Priority(p))
}

fn rel(anchor: Anchor, offset_days: i32) -> Operand {
    RelativeDate::new(anchor, offset_days).into()
}

fn words(t: &str) -> Operand {
    Operand::Text(t.into())
}

fn group(all_of: Vec<Clause>, any_of: Vec<Clause>) -> Clause {
    Group { all_of, any_of }.into()
}

fn all_of(clauses: Vec<Clause>) -> Filter {
    Filter {
        all_of: clauses,
        ..Filter::default()
    }
}

fn any_of(clauses: Vec<Clause>) -> Filter {
    Filter {
        any_of: clauses,
        ..Filter::default()
    }
}

fn everything() -> Filter {
    Filter {
        include_archived: true,
        include_done: true,
        include_deferred: true,
        include_deleted: true,
        ..Filter::default()
    }
}

fn input(filter: Filter, sort: Vec<SortKey>, today: Date) -> ListInput {
    ListInput {
        filter,
        sort,
        page_size: 1000,
        today: Some(today),
        ..ListInput::default()
    }
}

/// Every page of `input` at `page_size`, concatenated.
async fn paged(db: &Db, input: &ListInput, page_size: u32) -> Vec<Item> {
    let mut input = ListInput {
        page_size,
        ..input.clone()
    };
    let mut items = Vec::new();
    loop {
        let page = db.store.list_items(input.clone()).await.unwrap();
        assert!(page.items.len() <= page_size as usize);
        items.extend(page.items);
        match page.next_page_token {
            Some(token) => input.page_token = Some(token),
            None => return items,
        }
    }
}

// ---- the M3 filter fixture: a Sunday, and Items 1 to 30 ----------------------

const P1: &str = "01K8Z0000000000000000000P1";

/// The fixture Items in order: Item `n` is `ids[n - 1]`.
struct Fixture {
    ids: Vec<ItemId>,
}

impl Fixture {
    fn id(&self, n: u32) -> ItemId {
        self.ids[n as usize - 1]
    }

    /// The fixture numbers of `items`, in order.
    fn nums(&self, items: &[Item]) -> Vec<u32> {
        items
            .iter()
            .map(|i| self.ids.iter().position(|&id| id == i.id).unwrap() as u32 + 1)
            .collect()
    }

    async fn list(&self, db: &Db, input: ListInput) -> Vec<u32> {
        self.nums(&db.store.list_items(input).await.unwrap().items)
    }

    async fn ids(&self, db: &Db, filter: Filter, today: Date) -> Vec<u32> {
        self.list(db, input(filter, vec![], today)).await
    }

    async fn sorted(&self, db: &Db, filter: Filter, sort: Vec<SortKey>, today: Date) -> Vec<u32> {
        self.list(db, input(filter, sort, today)).await
    }
}

/// Everything the implicit rules leave on Sunday, by `created_at`.
const C01: [u32; 23] = [
    18, 1, 2, 3, 4, 5, 9, 10, 11, 12, 13, 19, 20, 21, 22, 25, 26, 27, 28, 29, 30, 23, 24,
];

fn c01_minus(gone: &[u32]) -> Vec<u32> {
    C01.into_iter().filter(|n| !gone.contains(n)).collect()
}

async fn fixture(db: &Db) -> Fixture {
    use Status as S;
    let date = |d: &str| text(d);
    let rows: Vec<(&str, Status, Columns)> = vec![
        (
            "Fix flaky login test",
            S::Inbox,
            vec![
                ("priority", text("high")),
                ("due_date", date("2026-10-24")),
                ("labels", text(r#"["auth","oncall"]"#)),
            ],
        ),
        (
            "Renew passport",
            S::Next,
            vec![
                ("priority", text("medium")),
                ("due_date", date("2026-10-25")),
            ],
        ),
        (
            "Ship auth migration",
            S::InProgress,
            vec![
                ("priority", text("highest")),
                ("local_priority_override", text("low")),
            ],
        ),
        (
            "Call dentist",
            S::Inbox,
            vec![("labels", text(r#"["Auth"]"#))],
        ),
        (
            "Café visit",
            S::Someday,
            vec![
                ("priority", text("lowest")),
                ("due_date", date("2026-10-26")),
                ("description", text("plat 530 notes")),
            ],
        ),
        (
            "Done thing",
            S::Done,
            vec![("priority", text("high")), ("due_date", date("2026-10-01"))],
        ),
        ("Cancelled thing", S::Cancelled, vec![]),
        (
            "Deferred tomorrow",
            S::Next,
            vec![
                ("priority", text("high")),
                ("defer_until", date("2026-10-26")),
            ],
        ),
        (
            "Deferred today",
            S::Next,
            vec![("defer_until", date("2026-10-25"))],
        ),
        (
            "Waiting on vendor",
            S::Next,
            vec![
                ("priority", text("low")),
                ("waiting_on_text", text("the vendor")),
                ("waiting_reason", text("")),
                ("waiting_since", date("2026-10-20")),
                ("waiting_follow_up_on", date("2026-10-25")),
            ],
        ),
        (
            "Waiting later",
            S::InProgress,
            vec![
                ("waiting_on_person_id", text(P1)),
                ("waiting_reason", text("spec")),
                ("waiting_since", date("2026-10-22")),
                ("waiting_follow_up_on", date("2026-10-30")),
            ],
        ),
        (
            "Focus item",
            S::Next,
            vec![("focus_week", text("2026-W43"))],
        ),
        (
            "Next week focus",
            S::Next,
            vec![("focus_week", text("2026-W44"))],
        ),
        (
            "Archived open",
            S::Next,
            vec![
                ("archived_at", ns("2026-10-20T12:00:00Z")),
                ("archived_reason", text("user")),
                ("due_date", date("2026-10-20")),
            ],
        ),
        (
            "Archived done",
            S::Done,
            vec![
                ("archived_at", ns("2026-10-22T12:00:00Z")),
                ("archived_reason", text("auto_done")),
            ],
        ),
        (
            "Deleted",
            S::Inbox,
            vec![("deleted_at", ns("2026-10-23T00:00:00Z"))],
        ),
        (
            "Archived deferred",
            S::Someday,
            vec![
                ("archived_at", ns("2026-10-21T12:00:00Z")),
                ("archived_reason", text("user")),
                ("defer_until", date("2026-11-30")),
            ],
        ),
        ("Spool import", S::Inbox, vec![]),
        (
            "日本語のテキスト",
            S::Inbox,
            vec![("labels", text(r#"["c++"]"#))],
        ),
        ("Ti\u{1EBF}ng Vi\u{1EC7}t NFC", S::Inbox, vec![]),
        (
            "Tie\u{0302}\u{0301}ng Vie\u{0323}\u{0302}t NFD",
            S::Inbox,
            vec![],
        ),
        (
            "PLAT-530 follow up",
            S::Next,
            vec![
                ("story_points", Value::Real(3.0)),
                ("assignee_person_id", text(P1)),
            ],
        ),
        ("ns tie later id", S::Someday, vec![]),
        ("ns tie earlier id", S::Someday, vec![]),
        (
            "\u{FF21} fullwidth",
            S::Someday,
            vec![("story_points", Value::Real(0.5))],
        ),
        ("\u{1F600} grin", S::Someday, vec![]),
        (
            "Linked 99",
            S::Next,
            vec![
                ("remote_tracker_kind", text("jira")),
                ("remote_instance_id", text("work-jira")),
                ("remote_id", text("99")),
                ("remote_key", text("PLAT-99")),
                ("remote_pinned", int(0)),
            ],
        ),
        (
            "Linked 100",
            S::Next,
            vec![
                ("remote_tracker_kind", text("jira")),
                ("remote_instance_id", text("work-jira")),
                ("remote_id", text("100")),
                ("remote_key", text("PLAT-100")),
                ("remote_pinned", int(1)),
            ],
        ),
        ("say \"hi\" -minus a*b title:x", S::Inbox, vec![]),
        (
            "Year-end focus",
            S::Inbox,
            vec![("focus_week", text("2026-W53"))],
        ),
    ];
    let base = ts("2026-10-01T00:00:00Z");
    let mut ids = Vec::new();
    for (n, (title, status, mut sets)) in (1..).zip(rows) {
        let created = match n {
            18 => ts("2026-09-30T00:00:00Z"),
            23 => ts("2026-10-24T00:00:00Z"),
            24 => ts("2026-10-24T00:00:00.000000001Z"),
            n => base + SignedDuration::from_mins(n),
        };
        let item = capture_at(db, title, created).await;
        sets.extend(status_sets(status));
        update(db, item.id, sets).await;
        ids.push(item.id);
    }
    Fixture { ids }
}

/// The seeded view with `seed`.
async fn view(db: &Db, seed: &str) -> View {
    let (views, _) = db.store.list_views().await.unwrap();
    views
        .into_iter()
        .find(|v| v.seed.as_deref() == Some(seed))
        .unwrap()
}

fn filter_error(result: Result<todo_store::ItemPage, StoreError>) -> (FilterErrorKind, String) {
    match result {
        Err(StoreError::Filter(e)) => (e.kind, e.path),
        other => panic!("expected a filter error, got {other:?}"),
    }
}

#[tokio::test]
async fn the_semantics_matrix() {
    let db = open();
    let fx = fixture(&db).await;
    let one = |c: Clause| all_of(vec![c]);

    // C01: the implicit rules; created_at, id (18 first though minted last).
    assert_eq!(fx.ids(&db, Filter::default(), SUN).await, C01);
    // C02, C03: clauses never lift a rule; the flag does.
    let done = || one(p(F::Status, O::In, vec![st(Status::Done)]));
    assert_eq!(fx.ids(&db, done(), SUN).await, [] as [u32; 0]);
    let with_done = Filter {
        include_done: true,
        ..done()
    };
    assert_eq!(fx.ids(&db, with_done, SUN).await, [6]);
    // C04: the Archive view as first designed misses archived deferred
    // Items; include_deferred keeps them.
    let archive = |include_deferred| Filter {
        include_archived: true,
        include_done: true,
        include_deferred,
        ..one(p(F::Archived, O::IsSet, vec![]))
    };
    let by_archived_at = || vec![SortKey::desc(F::ArchivedAt)];
    assert_eq!(
        fx.sorted(&db, archive(false), by_archived_at(), SUN).await,
        [15, 14]
    );
    assert_eq!(
        fx.sorted(&db, archive(true), by_archived_at(), SUN).await,
        [15, 17, 14]
    );
    // C05, C06: NE matches unset; CURRENT_WEEK in the ISO week-year.
    let week = |op| one(p(F::FocusWeek, op, vec![rel(Anchor::CurrentWeek, 0)]));
    assert_eq!(fx.ids(&db, week(O::Ne), SUN).await, c01_minus(&[12]));
    assert_eq!(fx.ids(&db, week(O::Eq), SUN).await, [12]);
    assert_eq!(fx.ids(&db, week(O::Eq), MON).await, [13]);
    assert_eq!(fx.ids(&db, week(O::Eq), date(2027, 1, 1)).await, [30]);
    assert_eq!(
        fx.ids(&db, week(O::Eq), date(2027, 1, 4)).await,
        [] as [u32; 0]
    );
    // C11: done is never overdue; archived only with its flag.
    let overdue = |include_archived| Filter {
        include_done: true,
        include_archived,
        ..one(p(F::IsOverdue, O::Eq, vec![val(FieldValue::Bool(true))]))
    };
    assert_eq!(fx.ids(&db, overdue(false), SUN).await, [1]);
    assert_eq!(fx.ids(&db, overdue(true), SUN).await, [1, 14]);
    // C12: PRIORITY_NONE last both ways.
    let next = || one(p(F::Status, O::In, vec![st(Status::Next)]));
    assert_eq!(
        fx.sorted(&db, next(), vec![SortKey::asc(F::EffectivePriority)], SUN)
            .await,
        [10, 2, 9, 12, 13, 22, 27, 28]
    );
    assert_eq!(
        fx.sorted(&db, next(), vec![SortKey::desc(F::EffectivePriority)], SUN)
            .await,
        [2, 10, 9, 12, 13, 22, 27, 28]
    );
    // C13, C14: ordering by rank; the override wins; NONE never matches.
    let ge_high = |field| one(p(field, O::Ge, vec![prio(Priority::High)]));
    assert_eq!(fx.ids(&db, ge_high(F::EffectivePriority), SUN).await, [1]);
    assert_eq!(fx.ids(&db, ge_high(F::Priority), SUN).await, [1, 3]);
    let below_medium = one(p(F::EffectivePriority, O::Lt, vec![prio(Priority::Medium)]));
    assert_eq!(fx.ids(&db, below_medium, SUN).await, [3, 5, 10]);
    // C16 to C19: sets, exact and case-sensitive.
    let labels =
        |op, members: &[&str]| one(p(F::Labels, op, members.iter().map(|m| s(m)).collect()));
    assert_eq!(
        fx.ids(&db, labels(O::ContainsAny, &["auth"]), SUN).await,
        [1]
    );
    assert_eq!(
        fx.ids(
            &db,
            labels(O::ContainsAll, &["auth", "auth", "oncall"]),
            SUN
        )
        .await,
        [1]
    );
    assert_eq!(
        fx.ids(&db, labels(O::ContainsAll, &["auth", "nope"]), SUN)
            .await,
        [] as [u32; 0]
    );
    assert_eq!(
        fx.ids(&db, labels(O::IsUnset, &[]), SUN).await,
        c01_minus(&[1, 4, 19])
    );
    assert_eq!(fx.ids(&db, labels(O::IsSet, &[]), SUN).await, [1, 4, 19]);
    // C21: a nested "" is a value; NE matches the unset.
    let reason = |op, r: &str| one(p(F::WaitingReason, op, vec![s(r)]));
    assert_eq!(fx.ids(&db, reason(O::Eq, ""), SUN).await, [10]);
    assert_eq!(
        fx.ids(&db, reason(O::Ne, "spec"), SUN).await,
        c01_minus(&[11])
    );
    // C22: people by id, free text exact.
    let p1 = || val(FieldValue::Person(PersonRef::Person(P1.parse().unwrap())));
    let on = |op, who| one(p(F::WaitingOn, op, vec![who]));
    assert_eq!(fx.ids(&db, on(O::Eq, p1()), SUN).await, [11]);
    let vendor = |t: &str| val(FieldValue::Person(PersonRef::FreeText(t.into())));
    assert_eq!(
        fx.ids(&db, on(O::Eq, vendor("the vendor")), SUN).await,
        [10]
    );
    assert_eq!(
        fx.ids(&db, on(O::Eq, vendor("The Vendor")), SUN).await,
        [] as [u32; 0]
    );
    assert_eq!(fx.ids(&db, on(O::Ne, p1()), SUN).await, c01_minus(&[11]));
    assert_eq!(
        fx.ids(&db, one(p(F::Assignee, O::Eq, vec![p1()])), SUN)
            .await,
        [22]
    );
    assert_eq!(
        fx.ids(&db, one(p(F::Assignee, O::IsSet, vec![])), SUN)
            .await,
        [22]
    );
    // C23: unlinked is unset, not false.
    let pinned = |op, b| one(p(F::RemoteLinkPinned, op, vec![val(FieldValue::Bool(b))]));
    assert_eq!(fx.ids(&db, pinned(O::Eq, false), SUN).await, [27]);
    assert_eq!(
        fx.ids(&db, pinned(O::Ne, true), SUN).await,
        c01_minus(&[28])
    );
    // C35, C36: code-point order; instants 1 ns apart.
    let someday = || one(p(F::Status, O::In, vec![st(Status::Someday)]));
    assert_eq!(
        fx.sorted(&db, someday(), vec![SortKey::asc(F::Title)], SUN)
            .await,
        [5, 24, 23, 25, 26]
    );
    assert_eq!(
        fx.sorted(&db, someday(), vec![SortKey::desc(F::CreatedAt)], SUN)
            .await,
        [24, 23, 26, 25, 5]
    );
    // C41: defer_until today is visible.
    let deferred = || {
        one(p(
            F::Title,
            O::In,
            vec![s("Deferred today"), s("Deferred tomorrow")],
        ))
    };
    assert_eq!(fx.ids(&db, deferred(), SUN).await, [9]);
    assert_eq!(fx.ids(&db, deferred(), MON).await, [8, 9]);
    // C42: the delete rule is not lifted by a clause.
    let deleted = |include_deleted| Filter {
        include_deleted,
        ..one(p(F::DeletedAt, O::IsSet, vec![]))
    };
    assert_eq!(fx.ids(&db, deleted(false), SUN).await, [] as [u32; 0]);
    assert_eq!(fx.ids(&db, deleted(true), SUN).await, [16]);
    // C43, C44: weeks end on Sunday; offsets.
    let this_week = || {
        all_of(vec![
            p(F::DueDate, O::Ge, vec![rel(Anchor::StartOfWeek, 0)]),
            p(F::DueDate, O::Le, vec![rel(Anchor::EndOfWeek, 0)]),
        ])
    };
    assert_eq!(fx.ids(&db, this_week(), SUN).await, [1, 2]);
    assert_eq!(fx.ids(&db, this_week(), MON).await, [5]);
    let yesterday = one(p(F::DueDate, O::Eq, vec![rel(Anchor::Today, -1)]));
    assert_eq!(fx.ids(&db, yesterday, SUN).await, [1]);
    // C47, C48: nested and empty groups.
    let nested = one(group(
        vec![],
        vec![
            p(F::Status, O::In, vec![st(Status::Inbox)]),
            group(
                vec![
                    p(F::Status, O::In, vec![st(Status::Next)]),
                    p(F::EffectivePriority, O::Ge, vec![prio(Priority::Medium)]),
                ],
                vec![],
            ),
        ],
    ));
    assert_eq!(
        fx.ids(&db, nested, SUN).await,
        [18, 1, 2, 4, 19, 20, 21, 29, 30]
    );
    assert_eq!(fx.ids(&db, one(group(vec![], vec![])), SUN).await, C01);
    // C53: REAL against its operand.
    let points = |op, n| one(p(F::StoryPoints, op, vec![val(FieldValue::Number(n))]));
    assert_eq!(fx.ids(&db, points(O::Ge, 1.0), SUN).await, [22]);
    assert_eq!(fx.ids(&db, points(O::Lt, 1.0), SUN).await, [25]);
    assert_eq!(fx.ids(&db, points(O::Ne, 3.0), SUN).await, c01_minus(&[22]));
    // C55: ids are values.
    let first = one(p(F::Id, O::In, vec![val(FieldValue::ItemId(fx.id(1)))]));
    assert_eq!(fx.ids(&db, first, SUN).await, [1]);
    // C56: keys sort by code point, unlinked last.
    assert_eq!(
        fx.sorted(&db, next(), vec![SortKey::asc(F::RemoteLinkKey)], SUN)
            .await,
        [28, 27, 2, 9, 10, 12, 13, 22]
    );
    // Enums sort by proto number; booleans false first.
    assert_eq!(
        fx.sorted(
            &db,
            Filter::default(),
            vec![SortKey::desc(F::Status), SortKey::asc(F::IsOverdue)],
            SUN
        )
        .await,
        [
            3, 11, 5, 25, 26, 23, 24, 2, 9, 10, 12, 13, 22, 27, 28, 18, 4, 19, 20, 21, 29, 30, 1
        ]
    );
}

#[tokio::test]
async fn invalid_filters_name_their_path() {
    let db = open();
    let one = |c: Clause| all_of(vec![c]);
    let list = |filter, sort| db.store.list_items(input(filter, sort, SUN));
    // C15: NONE is outside the priority order.
    let none = one(p(F::Priority, O::Lt, vec![prio(Priority::None)]));
    assert_eq!(
        filter_error(list(none, vec![]).await),
        (
            FilterErrorKind::Invalid,
            "all_of[0].predicate.operands[0]".to_owned()
        )
    );
    // C20: sets take no EQ.
    let eq = any_of(vec![p(F::Labels, O::Eq, vec![s("auth")])]);
    assert_eq!(
        filter_error(list(eq, vec![]).await),
        (
            FilterErrorKind::Invalid,
            "any_of[0].predicate.op".to_owned()
        )
    );
    // C32b: blank text.
    let blank = one(p(F::Title, O::Matches, vec![words(" \t\0")]));
    assert_eq!(
        filter_error(list(blank, vec![]).await),
        (
            FilterErrorKind::Invalid,
            "all_of[0].predicate.operands[0]".to_owned()
        )
    );
    // C45: a relative date out of range for today.
    let far = one(p(F::DueDate, O::Eq, vec![rel(Anchor::Today, i32::MAX)]));
    assert_eq!(
        filter_error(list(far, vec![]).await),
        (
            FilterErrorKind::Invalid,
            "all_of[0].predicate.operands[0].relative_date.offset_days".to_owned()
        )
    );
    // Sorts: not sortable, and repeated.
    assert_eq!(
        filter_error(list(Filter::default(), vec![SortKey::asc(F::Labels)]).await),
        (FilterErrorKind::Invalid, "sort[0].key".to_owned())
    );
    assert_eq!(
        filter_error(
            list(
                Filter::default(),
                vec![SortKey::asc(F::DueDate), SortKey::desc(F::DueDate)]
            )
            .await
        ),
        (FilterErrorKind::Invalid, "sort[1].key".to_owned())
    );
    // Valid, but not built yet.
    let instant = one(p(F::CreatedAt, O::Ge, vec![rel(Anchor::Today, 0)]));
    assert_eq!(
        filter_error(list(instant, vec![]).await),
        (
            FilterErrorKind::Unimplemented,
            "all_of[0].predicate.operands[0]".to_owned()
        )
    );
}

/// C46, C51, C57 and C58, and which error a request with several gets: the
/// filter's first INVALID in document order, the sort's, the filter's
/// UNIMPLEMENTED, then the page token and the view.
#[tokio::test]
async fn errors_come_in_order() {
    use FieldValue as V;
    use FilterErrorKind::{Invalid as I, Unimplemented as U};
    let db = open();
    let one = |c: Clause| all_of(vec![c]);
    let error = |filter: Filter, sort: Vec<SortKey>| {
        let listed = db.store.list_items(input(filter, sort, SUN));
        async move { filter_error(listed.await) }
    };
    let operand = "all_of[0].predicate.operands[0]";
    let offset = format!("{operand}.relative_date.offset_days");
    let cases = [
        // C46: weeks and dates do not mix, and a week takes no offset.
        (
            p(F::DueDate, O::Eq, vec![rel(Anchor::CurrentWeek, 0)]),
            operand,
        ),
        (p(F::FocusWeek, O::Eq, vec![rel(Anchor::Today, 0)]), operand),
        (
            p(F::FocusWeek, O::Eq, vec![rel(Anchor::CurrentWeek, 1)]),
            &offset,
        ),
        // C57: values out of range or of the wrong kind.
        (
            p(F::StoryPoints, O::Ge, vec![val(V::Number(f64::NAN))]),
            operand,
        ),
        (
            p(
                F::CreatedAt,
                O::Lt,
                vec![val(V::Timestamp(ts("2300-01-01T00:00:00Z")))],
            ),
            operand,
        ),
        (
            p(F::DueDate, O::Eq, vec![val(V::Date(date(0, 1, 1)))]),
            operand,
        ),
        (
            p(
                F::Assignee,
                O::Eq,
                vec![val(V::Person(PersonRef::FreeText("bob".into())))],
            ),
            operand,
        ),
        (p(F::DueDate, O::Eq, vec![val(V::Cleared)]), operand),
        (p(F::Status, O::Eq, vec![s("inbox")]), operand),
        // C58: MATCHES searches only the indexed text.
        (
            p(F::PrivateNotes, O::Matches, vec![words("x")]),
            "all_of[0].predicate.target",
        ),
    ];
    for (clause, path) in cases {
        let got = error(one(clause.clone()), vec![]).await;
        assert_eq!(got, (I, path.to_owned()), "{clause:?}");
    }
    // C58: a group too deep, and too many predicates in all.
    let x = || p(F::Title, O::Eq, vec![s("x")]);
    let deep = (0..5).fold(x(), |c, _| group(vec![c], vec![]));
    let deepest = ["all_of[0].group"; 5].join(".");
    assert_eq!(error(one(deep), vec![]).await, (I, deepest));
    let many = any_of((0..129).map(|_| x()).collect());
    assert_eq!(error(many, vec![]).await, (I, String::new()));

    // C51: all_of before any_of, each in order.
    let c51 = Filter {
        all_of: vec![x(), p(F::Title, O::Lt, vec![s("x")])],
        any_of: vec![p(F::PrivateNotes, O::Matches, vec![words("x")])],
        ..Filter::default()
    };
    let op = |at: &str| (I, format!("{at}.predicate.op"));
    assert_eq!(error(c51, vec![]).await, op("all_of[1]"));
    // A relative date out of range for today is named in its place.
    let far = all_of(vec![
        p(F::DueDate, O::Eq, vec![rel(Anchor::Today, i32::MAX)]),
        p(F::Title, O::Lt, vec![s("x")]),
    ]);
    assert_eq!(error(far, vec![]).await, (I, offset.clone()));
    // The filter's INVALID, then the sort's, then the filter's UNIMPLEMENTED.
    let invalid = || one(p(F::Labels, O::Eq, vec![s("x")]));
    let unbuilt = || one(p(F::CreatedAt, O::Ge, vec![rel(Anchor::Today, 0)]));
    let bad_sort = || vec![SortKey::asc(F::Labels)];
    assert_eq!(error(invalid(), bad_sort()).await, op("all_of[0]"));
    assert_eq!(
        error(unbuilt(), bad_sort()).await,
        (I, "sort[0].key".to_owned())
    );
    assert_eq!(error(unbuilt(), vec![]).await, (U, operand.to_owned()));
    // Then the page token, then the view.
    let with_both = |filter| ListInput {
        page_token: Some("p2.0000000000000000.1.01JB8X4Z5Q6R7S8T9V0W1X2Y3Z".into()),
        view_id: Some(new_ulid()),
        ..input(filter, vec![], SUN)
    };
    let listed = db.store.list_items(with_both(unbuilt())).await;
    assert_eq!(filter_error(listed), (U, operand.to_owned()));
    let listed = db.store.list_items(with_both(Filter::default())).await;
    assert!(
        matches!(listed, Err(StoreError::InvalidPageToken)),
        "{listed:?}"
    );
}

/// A filter within the limits lists, however wide: groups cost nothing
/// against them, and an IN takes up to 1000 people. SQLite stops at an
/// expression 1000 deep, which the SQL must never reach.
#[tokio::test]
async fn wide_filters_within_the_limits_list() {
    let db = open();
    let fx = fixture(&db).await;
    let empty = || group(vec![], vec![]);
    let empties = |n| (0..n).map(|_| empty()).collect::<Vec<_>>();
    let inbox = || p(F::Status, O::In, vec![st(Status::Inbox)]);
    let inbox_ids = fx.ids(&db, all_of(vec![inbox()]), SUN).await;
    // Empty groups match everything, alone or among others, at any depth.
    for filter in [
        all_of(empties(2000)),
        any_of(empties(2000)),
        all_of(vec![group(empties(2000), empties(2000))]),
        Filter {
            any_of: [empties(1000), vec![inbox()], empties(1000)].concat(),
            ..all_of(empties(1000))
        },
    ] {
        filter.validate().unwrap();
        assert_eq!(fx.ids(&db, filter, SUN).await, C01);
    }
    let filter = || Filter {
        all_of: [empties(1000), vec![inbox()], empties(1000)].concat(),
        any_of: [empties(10), vec![group(vec![], empties(1000))]].concat(),
        ..Filter::default()
    };
    filter().validate().unwrap();
    assert_eq!(fx.ids(&db, filter(), SUN).await, inbox_ids);
    // As one more group of a view, on a later page.
    let today = view(&db, "today").await;
    let narrowed = ListInput {
        view_id: today.id,
        ..input(filter(), vec![], SUN)
    };
    let unnarrowed = ListInput {
        view_id: today.id,
        ..input(all_of(vec![inbox()]), vec![], SUN)
    };
    assert_eq!(
        fx.nums(&paged(&db, &narrowed, 1).await),
        fx.nums(&paged(&db, &unnarrowed, 1).await)
    );
    // A group that only holds empty groups is empty too; one any_of that
    // holds only a false clause is not.
    let never = || p(F::Status, O::In, vec![st(Status::Done)]);
    let filter = all_of(vec![
        group(vec![group(empties(1000), vec![])], empties(1000)),
        group(vec![], vec![never(), group(empties(1000), vec![never()])]),
    ]);
    filter.validate().unwrap();
    assert_eq!(fx.ids(&db, filter, SUN).await, [] as [u32; 0]);
    // A person IN with the most operands, in every predicate the filter
    // may hold operands for.
    let vendors = |n: usize| {
        let mut who: Vec<Operand> = (1..n)
            .map(|i| val(FieldValue::Person(PersonRef::FreeText(format!("v{i}")))))
            .collect();
        who.push(val(FieldValue::Person(PersonRef::FreeText(
            "the vendor".into(),
        ))));
        who
    };
    let on = || p(F::WaitingOn, O::In, vendors(MAX_PREDICATE_OPERANDS));
    for filter in [
        all_of(vec![on(), on(), on(), on()]),
        any_of(vec![on(), on(), on(), on()]),
    ] {
        filter.validate().unwrap();
        assert_eq!(fx.ids(&db, filter, SUN).await, [10]);
    }
}

#[tokio::test]
async fn matches_searches_one_column_as_plain_words() {
    let db = open();
    let fx = fixture(&db).await;
    let search = |field, t: &str| all_of(vec![p(field, O::Matches, vec![words(t)])]);
    let text_columns = |t: &str| {
        any_of(vec![
            p(F::Title, O::Matches, vec![words(t)]),
            p(F::Description, O::Matches, vec![words(t)]),
        ])
    };
    // C24 to C26: each term a phrase whose last word is a prefix, terms
    // ANDed in any order.
    for (t, want) in [
        ("plat-530", &[5, 22][..]),
        ("530 plat", &[5, 22]),
        ("pla 53", &[5, 22]),
        ("pla-53", &[]),
    ] {
        assert_eq!(fx.ids(&db, text_columns(t), SUN).await, want, "{t}");
    }
    // C27, C28: case and accents fold, composed or decomposed.
    assert_eq!(fx.ids(&db, search(F::Title, "CAFÉ"), SUN).await, [5]);
    assert_eq!(fx.ids(&db, search(F::Title, "tieng"), SUN).await, [20, 21]);
    assert_eq!(
        fx.ids(&db, search(F::Title, "Vie\u{0323}\u{0302}t"), SUN)
            .await,
        [20, 21]
    );
    // C29: CJK is one token.
    assert_eq!(fx.ids(&db, search(F::Title, "日本"), SUN).await, [19]);
    assert_eq!(
        fx.ids(&db, search(F::Title, "テキスト"), SUN).await,
        [] as [u32; 0]
    );
    // C30: "+" separates; labels are their JSON text.
    assert_eq!(fx.ids(&db, search(F::Title, "c++"), SUN).await, [4, 5]);
    assert_eq!(fx.ids(&db, search(F::Labels, "c++"), SUN).await, [19]);
    // C31: FTS syntax is plain text.
    for t in ["\"hi\"", "title:x", "a*b", "-minus", "say\0hi", "SAY \"HI"] {
        assert_eq!(fx.ids(&db, search(F::Title, t), SUN).await, [29], "{t}");
    }
    for t in [
        "AND",
        "NOT",
        "a AND",
        "OR x",
        "NEAR(a b)",
        ":",
        "*",
        "\"",
        "-",
        "{zz}",
    ] {
        assert_eq!(
            fx.ids(&db, search(F::Title, t), SUN).await,
            [] as [u32; 0],
            "{t}"
        );
    }
    // C32c: a term without words is ignored beside one with them.
    assert_eq!(fx.ids(&db, search(F::Title, "fix -"), SUN).await, [1]);
    // C33, C34: one column at a time.
    assert_eq!(fx.ids(&db, search(F::Title, "auth"), SUN).await, [3]);
    assert_eq!(fx.ids(&db, search(F::Labels, "auth"), SUN).await, [1, 4]);
    assert_eq!(
        fx.ids(&db, search(F::Labels, "auth,oncall"), SUN).await,
        [1]
    );
    assert_eq!(
        fx.ids(&db, search(F::RemoteLinkKey, "plat-1"), SUN).await,
        [28]
    );
    let or = any_of(vec![
        p(F::Labels, O::Matches, vec![words("oncall")]),
        p(F::Status, O::In, vec![st(Status::InProgress)]),
    ]);
    assert_eq!(fx.ids(&db, or, SUN).await, [1, 3, 11]);
    // The implicit rules still apply.
    assert_eq!(
        fx.ids(&db, search(F::Title, "deleted"), SUN).await,
        [] as [u32; 0]
    );
}

#[tokio::test]
async fn the_seeded_views_list_and_count_the_fixture() {
    let db = open();
    let fx = fixture(&db).await;
    let listed = |seed: &'static str, today| {
        let db = &db;
        let fx = &fx;
        async move {
            let view = view(db, seed).await;
            let input = ListInput {
                view_id: view.id,
                today: Some(today),
                page_size: 1000,
                ..ListInput::default()
            };
            fx.list(db, input).await
        }
    };
    assert_eq!(listed("inbox", SUN).await, [18, 1, 4, 19, 20, 21, 29, 30]);
    // C07, C08: the Today view on Sunday and Monday.
    assert_eq!(listed("today", SUN).await, [1, 2, 3, 10, 11, 12]);
    assert_eq!(listed("today", MON).await, [1, 2, 3, 10, 5, 11, 13]);
    assert_eq!(
        listed("next", SUN).await,
        [2, 3, 10, 9, 11, 12, 13, 22, 27, 28]
    );
    assert_eq!(listed("waiting", SUN).await, [10, 11]);
    assert_eq!(listed("someday", SUN).await.len(), 5);
    assert_eq!(listed("archive", SUN).await.len(), 5);

    // Counts are the same queries: set show_count everywhere to count each.
    let counts = db.store.status_counts(SUN, false).await.unwrap();
    assert_eq!((counts.inbox, counts.due, counts.overdue), (8, 2, 1));
    assert_eq!(counts.views, []);
    assert_eq!(counts.revision, db.store.current_revision());
    let counted = db.store.status_counts(SUN, true).await.unwrap();
    let shown: Vec<_> = counted.views.iter().map(|(id, _)| *id).collect();
    let (views, _) = db.store.list_views().await.unwrap();
    let with_count: Vec<_> = views
        .iter()
        .filter(|v| v.show_count)
        .map(|v| v.id.unwrap())
        .collect();
    assert_eq!(shown, with_count);
    assert_eq!(shown.len(), 4);
    sql(&db, "UPDATE views SET show_count = 1").await;
    for today in [SUN, MON] {
        let counted = db.store.status_counts(today, true).await.unwrap();
        assert_eq!(counted.views.len(), defaults().len());
        for (id, n) in counted.views {
            let input = ListInput {
                view_id: Some(id),
                today: Some(today),
                page_size: 1000,
                ..ListInput::default()
            };
            let page = db.store.list_items(input).await.unwrap();
            assert_eq!(n as usize, page.items.len(), "{id} on {today}");
        }
    }
    // Monday: 2 is overdue too, and 5 is due.
    let counts = db.store.status_counts(MON, false).await.unwrap();
    assert_eq!((counts.inbox, counts.due, counts.overdue), (8, 3, 2));
}

#[tokio::test]
async fn a_request_narrows_its_view() {
    let db = open();
    let fx = fixture(&db).await;
    let today = view(&db, "today").await;
    let narrowed = |filter, sort| ListInput {
        filter,
        sort,
        view_id: today.id,
        today: Some(SUN),
        page_size: 1000,
        ..ListInput::default()
    };
    // Its clauses only narrow, as one more group.
    let at_least_low = any_of(vec![
        p(F::EffectivePriority, O::Ge, vec![prio(Priority::Medium)]),
        p(F::Title, O::Eq, vec![s("Focus item")]),
    ]);
    assert_eq!(
        fx.list(&db, narrowed(at_least_low.clone(), vec![])).await,
        [1, 2, 12]
    );
    // A flag widens: done Items due by today join.
    let with_done = Filter {
        include_done: true,
        ..Filter::default()
    };
    assert_eq!(
        fx.list(&db, narrowed(with_done, vec![])).await,
        [1, 6, 2, 3, 10, 11, 12]
    );
    // A sort replaces the view's.
    assert_eq!(
        fx.list(&db, narrowed(at_least_low, vec![SortKey::desc(F::Title)]))
            .await,
        [2, 12, 1]
    );
    // A view that does not exist.
    let missing = new_ulid();
    let input = ListInput {
        view_id: Some(missing),
        ..ListInput::default()
    };
    match db.store.list_items(input).await {
        Err(StoreError::ViewNotFound(id)) => assert_eq!(id, missing),
        other => panic!("expected ViewNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn pages_concatenate_to_the_list() {
    let db = open();
    let fx = fixture(&db).await;
    let sorts = [
        vec![],
        vec![
            SortKey::desc(F::IsOverdue),
            SortKey::desc(F::EffectivePriority),
            SortKey::asc(F::DueDate),
        ],
        vec![SortKey::asc(F::DueDate), SortKey::desc(F::Priority)],
        vec![SortKey::desc(F::FocusWeek), SortKey::asc(F::StoryPoints)],
        vec![
            SortKey::asc(F::RemoteLinkKey),
            SortKey::desc(F::RemoteLinkPinned),
        ],
        vec![SortKey::desc(F::Status), SortKey::asc(F::WaitingSince)],
        vec![SortKey::desc(F::Id)],
        vec![SortKey::desc(F::CreatedAt), SortKey::asc(F::Title)],
    ];
    for sort in sorts {
        for filter in [Filter::default(), everything()] {
            let all = input(filter, sort.clone(), SUN);
            let whole = db.store.list_items(all.clone()).await.unwrap().items;
            assert!(whole.len() >= 23);
            for size in [1, 2, 7] {
                let pages = paged(&db, &all, size).await;
                assert_eq!(fx.nums(&pages), fx.nums(&whole), "{sort:?} at {size}");
            }
        }
    }
    // C37a: the Today view one at a time.
    let today = view(&db, "today").await;
    let by_view = ListInput {
        view_id: today.id,
        today: Some(SUN),
        ..ListInput::default()
    };
    assert_eq!(
        fx.nums(&paged(&db, &by_view, 1).await),
        [1, 2, 3, 10, 11, 12]
    );
}

#[tokio::test]
async fn later_pages_read_live_data_after_the_cursor() {
    let db = open();
    let fx = fixture(&db).await;
    let next_desc = input(
        all_of(vec![p(F::Status, O::In, vec![st(Status::Next)])]),
        vec![SortKey::desc(F::EffectivePriority)],
        SUN,
    );
    let first_page = |db| {
        let input = ListInput {
            page_size: 3,
            ..next_desc.clone()
        };
        async move { Store::list_items(db, input).await.unwrap() }
    };
    // C38: completing a row before the cursor skips nothing.
    let page = first_page(&db.store).await;
    assert_eq!(fx.nums(&page.items), [2, 10, 9]);
    update(&db, fx.id(2), status_sets(Status::Done)).await;
    let after = ListInput {
        page_size: 3,
        page_token: page.next_page_token,
        ..next_desc.clone()
    };
    assert_eq!(fx.list(&db, after).await, [12, 13, 22]);
    // C39: a row whose key moves behind the cursor shows again.
    update(&db, fx.id(2), status_sets(Status::Next)).await;
    let page = first_page(&db.store).await;
    assert_eq!(fx.nums(&page.items), [2, 10, 9]);
    update(&db, fx.id(10), vec![("priority", text("none"))]).await;
    let after = ListInput {
        page_size: 3,
        page_token: page.next_page_token,
        ..next_desc.clone()
    };
    assert_eq!(fx.list(&db, after).await, [10, 12, 13]);
}

#[tokio::test]
async fn a_token_keeps_the_first_pages_today() {
    let db = open();
    let fx = fixture(&db).await;
    let today = view(&db, "today").await;
    let sunday = ListInput {
        view_id: today.id,
        today: Some(SUN),
        page_size: 2,
        ..ListInput::default()
    };
    let first = db.store.list_items(sunday.clone()).await.unwrap();
    assert_eq!(fx.nums(&first.items), [1, 2]);
    assert!(first.items[0].derived.is_overdue && !first.items[1].derived.is_overdue);
    // C40: later pages asked for on Monday still list Sunday's view: 12
    // stays and 5 and 13 do not join.
    let monday = ListInput {
        today: Some(MON),
        page_size: 10,
        page_token: first.next_page_token,
        ..sunday.clone()
    };
    let rest = db.store.list_items(monday).await.unwrap();
    assert_eq!(fx.nums(&rest.items), [3, 10, 11, 12]);
    assert_eq!(rest.next_page_token, None);
    // Each row's is_overdue is Sunday's too: 2 is not overdue until Monday.
    let one_by_one = ListInput {
        page_size: 1,
        ..sunday
    };
    let first = db.store.list_items(one_by_one.clone()).await.unwrap();
    let second = ListInput {
        today: Some(MON),
        page_token: first.next_page_token,
        ..one_by_one
    };
    let second = db.store.list_items(second).await.unwrap();
    assert_eq!(fx.nums(&second.items), [2]);
    assert!(!second.items[0].derived.is_overdue);
    // Without the token, Monday is Monday.
    let fresh = ListInput {
        view_id: today.id,
        today: Some(MON),
        page_size: 1000,
        ..ListInput::default()
    };
    let monday = db.store.list_items(fresh).await.unwrap();
    assert_eq!(fx.nums(&monday.items), [1, 2, 3, 10, 5, 11, 13]);
    assert!(monday.items[1].derived.is_overdue);
}

#[tokio::test]
async fn a_token_is_bound_to_its_filter_sort_and_view() {
    let db = open();
    fixture(&db).await;
    let (today, next) = (view(&db, "today").await, view(&db, "next").await);
    let base = ListInput {
        filter: all_of(vec![p(F::Status, O::In, vec![st(Status::Next)])]),
        sort: vec![SortKey::asc(F::DueDate)],
        page_size: 2,
        today: Some(SUN),
        ..ListInput::default()
    };
    let token = db
        .store
        .list_items(base.clone())
        .await
        .unwrap()
        .next_page_token
        .unwrap();
    assert!(token.starts_with("p3."), "{token}");
    let with = |input: ListInput| ListInput {
        page_token: Some(token.clone()),
        ..input
    };
    let invalid = |r: Result<todo_store::ItemPage, StoreError>| {
        assert!(matches!(r, Err(StoreError::InvalidPageToken)), "{r:?}");
    };
    // C54: another filter, sort or view is refused.
    let other_filter = ListInput {
        filter: Filter {
            include_done: true,
            ..base.filter.clone()
        },
        ..base.clone()
    };
    invalid(db.store.list_items(with(other_filter)).await);
    let other_sort = ListInput {
        sort: vec![SortKey::desc(F::DueDate)],
        ..base.clone()
    };
    invalid(db.store.list_items(with(other_sort)).await);
    for view_id in [today.id, next.id] {
        let in_view = ListInput {
            view_id,
            ..base.clone()
        };
        invalid(db.store.list_items(with(in_view)).await);
    }
    // Through a view, the token is bound to what narrows it too.
    let narrowed = ListInput {
        view_id: next.id,
        sort: vec![],
        ..base.clone()
    };
    let token = db
        .store
        .list_items(narrowed.clone())
        .await
        .unwrap()
        .next_page_token;
    let in_next = |input: ListInput| ListInput {
        page_token: token.clone(),
        ..input
    };
    assert!(db.store.list_items(in_next(narrowed.clone())).await.is_ok());
    let unnarrowed = ListInput {
        filter: Filter::default(),
        ..narrowed.clone()
    };
    let widened = ListInput {
        filter: Filter {
            include_done: true,
            ..narrowed.filter.clone()
        },
        ..narrowed.clone()
    };
    for other in [unnarrowed, widened, base.clone()] {
        invalid(db.store.list_items(in_next(other)).await);
    }
    // ... while another page size or today is not.
    let other_size = ListInput {
        page_size: 7,
        today: Some(MON),
        ..base.clone()
    };
    assert!(db.store.list_items(with(other_size)).await.is_ok());
    // A view's token is bound to the view, so editing it voids the token.
    let in_next = ListInput {
        view_id: next.id,
        page_size: 1,
        ..ListInput::default()
    };
    let token = db
        .store
        .list_items(in_next.clone())
        .await
        .unwrap()
        .next_page_token;
    let again = ListInput {
        page_token: token.clone(),
        ..in_next.clone()
    };
    assert!(db.store.list_items(again.clone()).await.is_ok());
    sql(&db, "UPDATE views SET sort = '[]' WHERE seed = 'next'").await;
    invalid(db.store.list_items(again).await);
    // P0 tokens and garbage.
    for bad in [
        "p2.0000000000000000.1.01JB8X4Z5Q6R7S8T9V0W1X2Y3Z",
        "p3.",
        "p3.!!",
        "p3.AAAA",
        "garbage",
    ] {
        let bad = ListInput {
            page_token: Some(bad.to_owned()),
            ..base.clone()
        };
        invalid(db.store.list_items(bad).await);
    }
}

// ---- views: seeding, the change stream, the migration ----------------------

async fn view_seeds(db: &Db) -> Vec<String> {
    db.store
        .read(|c| {
            let mut stmt = c.prepare("SELECT seed FROM view_seeds ORDER BY seed")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_new_database_seeds_the_six_views_once() {
    let db = open();
    let (views, revision) = db.store.list_views().await.unwrap();
    let want = defaults();
    assert_eq!(views.len(), 6);
    assert_eq!(revision, 6);
    for (i, (got, default)) in views.iter().zip(&want).enumerate() {
        assert_eq!(got.seed, default.seed);
        assert_eq!(
            (
                &got.name,
                &got.filter,
                &got.sort,
                got.show_count,
                got.position
            ),
            (
                &default.name,
                &default.filter,
                &default.sort,
                default.show_count,
                i as u32
            )
        );
        assert_eq!(got.revision, i as Revision + 1);
        assert!(got.id.is_some() && got.created_at.is_some());
        assert_eq!(got.created_at, got.updated_at);
    }
    let (ops, events): (i64, i64) = db
        .store
        .read(|c| {
            Ok((
                c.query_row("SELECT count(*) FROM ops", [], |r| r.get(0))?,
                c.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((ops, events), (0, 0));

    // Reopening seeds nothing.
    let db = reopen(db);
    assert_eq!(db.store.current_revision(), 6);
    assert_eq!(db.store.list_views().await.unwrap(), (views.clone(), 6));

    // A deleted default stays deleted.
    sql(&db, "DELETE FROM views WHERE seed = 'someday'").await;
    let db = reopen(db);
    assert_eq!(db.store.current_revision(), 6);
    let (left, _) = db.store.list_views().await.unwrap();
    assert_eq!(left.len(), 5);
    assert!(left.iter().all(|v| v.seed.as_deref() != Some("someday")));
    assert_eq!(view_seeds(&db).await.len(), 6);

    // A default this database has not seen yet (here: one whose seed record
    // is gone with its view) is seeded once, after the existing views.
    sql(
        &db,
        "DELETE FROM views WHERE seed = 'waiting'; DELETE FROM view_seeds WHERE seed = 'waiting'",
    )
    .await;
    let db = reopen(db);
    assert_eq!(db.store.current_revision(), 7);
    let (views, _) = db.store.list_views().await.unwrap();
    let waiting = views.last().unwrap();
    assert_eq!(
        (waiting.seed.as_deref(), waiting.position, waiting.revision),
        (Some("waiting"), 6, 7)
    );
    assert_eq!(waiting.filter, want[3].filter);
    let db = reopen(db);
    assert_eq!(db.store.current_revision(), 7);
    assert_eq!(db.store.list_views().await.unwrap().0.len(), 5);
}

#[tokio::test]
async fn a_seeded_name_that_is_taken_gets_a_number() {
    let db = open();
    sql(
        &db,
        "DELETE FROM view_seeds WHERE seed = 'inbox'; UPDATE views SET seed = NULL WHERE seed = 'inbox'",
    )
    .await;
    let db = reopen(db);
    let (views, _) = db.store.list_views().await.unwrap();
    let names: Vec<_> = views.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Inbox", "Today", "Next", "Waiting", "Someday", "Archive", "Inbox 2"
        ]
    );
}

#[tokio::test]
async fn the_change_stream_carries_the_seeded_views() {
    let db = open();
    let replay = db.store.changes_since(0).await.unwrap();
    let (views, _) = db.store.list_views().await.unwrap();
    assert_eq!(replay.len(), 6);
    for (change, view) in replay.iter().zip(&views) {
        assert_eq!(change.topic, Topic::View);
        assert_eq!(change.id, view.id.unwrap().to_string());
        assert!(!change.deleted);
        assert_eq!(change.resource, Some(Resource::View(view.clone().into())));
    }
    // A view deleted since replays as deleted: views leave no tombstone.
    sql(&db, "DELETE FROM views WHERE seed = 'today'").await;
    let replay = db.store.changes_since(0).await.unwrap();
    assert_eq!(
        replay
            .iter()
            .map(|c| (c.revision, c.deleted, c.resource.is_some()))
            .collect::<Vec<_>>(),
        [
            (1, false, true),
            (2, true, false),
            (3, false, true),
            (4, false, true),
            (5, false, true),
            (6, false, true)
        ]
    );
    // What a later open seeds replays too.
    sql(&db, "DELETE FROM view_seeds WHERE seed = 'today'").await;
    let db = reopen(db);
    let replay = db.store.changes_since(6).await.unwrap();
    let [change] = replay.as_slice() else {
        panic!("expected one change, got {replay:?}");
    };
    let Some(Resource::View(view)) = &change.resource else {
        panic!("expected a view");
    };
    assert_eq!((change.revision, view.seed.as_deref()), (7, Some("today")));
}

#[tokio::test]
async fn migrates_an_m2_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("todo.db");
    let (a, b) = ("01JB8X4Z5Q6R7S8T9V0W1X2Y3Z", "01JB8X4Z5Q6R7S8T9V0W1X2Y40");
    {
        // The M2 schema with M2 rows: two Items (one title composed, one
        // decomposed) and a relation.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../migrations/0002_undo_relations.sql"))
            .unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO items (id, created_via, created_at, updated_at, revision, title,
                                status_category, labels)
             VALUES ('{a}', 'cli', 1000, 1000, 1, 'Ti\u{1EBF}ng Vi\u{1EC7}t', 'todo', '[\"x\"]'),
                    ('{b}', 'cli', 2000, 2000, 3, 'Tie\u{0302}\u{0301}ng NFD', 'todo', '[]');
             INSERT INTO relations (from_id, to_id, kind, created_at)
             VALUES ('{a}', '{b}', 'blocks', 3000);
             INSERT INTO changes (revision, topic, resource_id, deleted, at)
             VALUES (1, 'item', '{a}', 0, 1000), (2, 'item', '{b}', 0, 2000),
                    (3, 'item', '{b}', 0, 3000);
             UPDATE meta SET revision = 3;"
        ))
        .unwrap();
        // The M2 tokenizer keeps the accents of the composed title.
        let old: i64 = conn
            .query_row(
                "SELECT count(*) FROM items_fts WHERE items_fts MATCH 'tieng'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 1);
    }
    let db = Db {
        store: Store::open(&path).unwrap(),
        path,
        _dir: dir,
    };
    let version: u32 = Connection::open(&db.path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    // The views are seeded after the M2 revisions.
    let (views, revision) = db.store.list_views().await.unwrap();
    assert_eq!(views.len(), 6);
    assert_eq!(revision, 9);
    assert_eq!(views[0].revision, 4);

    // Both Items are found, composed or not, by every FTS column.
    let search = |field, t: &str| {
        input(
            all_of(vec![p(field, O::Matches, vec![words(t)])]),
            vec![],
            SUN,
        )
    };
    let found = db
        .store
        .list_items(search(F::Title, "tieng"))
        .await
        .unwrap();
    let ids: Vec<String> = found.items.iter().map(|i| i.id.to_string()).collect();
    assert_eq!(ids, [a, b]);
    assert_eq!(found.items[0].relations.len(), 1);
    let found = db.store.list_items(search(F::Labels, "x")).await.unwrap();
    assert_eq!(found.items.len(), 1);
    // The triggers keep the new index current, for new Items and edits to
    // the old ones.
    let item = capture_at(&db, "Tiếng mới", ts("2026-10-25T00:00:00Z")).await;
    let found = db.store.list_items(search(F::Title, "moi")).await.unwrap();
    assert_eq!(found.items, [item]);
    sql(
        &db,
        "UPDATE items SET title = 'Renamed' WHERE title = 'Tie\u{0302}\u{0301}ng NFD'",
    )
    .await;
    let found = db
        .store
        .list_items(search(F::Title, "renamed"))
        .await
        .unwrap();
    let ids: Vec<String> = found.items.iter().map(|i| i.id.to_string()).collect();
    assert_eq!(ids, [b]);
    let found = db.store.list_items(search(F::Title, "nfd")).await.unwrap();
    assert!(found.items.is_empty());
    sql(
        &db,
        "INSERT INTO items_fts (items_fts) VALUES ('integrity-check')",
    )
    .await;
    let triggers: i64 = db
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'trigger'
                 AND name IN ('items_fts_ai', 'items_fts_ad', 'items_fts_au')",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(triggers, 3);
}

// ---- the randomized oracle ----------------------------------------------------

/// xorshift64*: small, seeded, and reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// True `percent` times in a hundred.
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

const TITLES: &[&str] = &[
    "alpha",
    "Alpha",
    "beta",
    "Café",
    "cafe",
    "\u{FF21}",
    "\u{1F600}",
    "",
];
const STRINGS: &[&str] = &[
    "", "S1", "S2", "spec", "vendor", "Vendor", "jira", "PLAT-99", "PLAT-100", "work", "1", "2",
];
const MEMBERS: &[&str] = &["a", "b", "A", "ü", "mine"];
const DATES: &[&str] = &[
    "2026-10-19",
    "2026-10-24",
    "2026-10-25",
    "2026-10-26",
    "2026-11-01",
];
const WEEKS: &[&str] = &["2026-W42", "2026-W43", "2026-W44", "2026-W53"];
const PRIORITIES: &[&str] = &["none", "lowest", "low", "medium", "high", "highest"];
const INSTANTS: &[i64] = &[0, 1, 1_000, 1_000_000_000, 1_792_800_000_000_000_000];
const POINTS: &[f64] = &[0.0, 0.5, 1.0, 3.0];
const DURATIONS: &[i64] = &[0, 3_600_000_000_000, 7_200_000_000_000];
const PEOPLE: &[&str] = &["01K8Z0000000000000000000P1", "01K8Z0000000000000000000P2"];

fn opt<T>(rng: &mut Rng, percent: u64, value: impl FnOnce(&mut Rng) -> T) -> Option<T> {
    rng.chance(percent).then(|| value(rng))
}

fn opt_text(v: Option<&str>) -> Value {
    v.map_or(Value::Null, text)
}

/// Random columns for the Item `n` of a round, valid as the codec reads
/// them; `earlier` are the Items made before it.
fn random_columns(rng: &mut Rng, n: usize, earlier: &[ItemId]) -> Columns {
    let status = *rng.pick(&[
        Status::Inbox,
        Status::Next,
        Status::Someday,
        Status::InProgress,
        Status::Done,
        Status::Cancelled,
    ]);
    let mut sets = status_sets(status);
    let pick_text = |rng: &mut Rng, xs: &[&str]| text(rng.pick(xs));
    let instant = |rng: &mut Rng| int(*rng.pick(INSTANTS));
    sets.extend([
        ("title", pick_text(rng, TITLES)),
        ("description", pick_text(rng, &["", "notes", "Notes"])),
        ("item_type", pick_text(rng, &["task", "bug"])),
        ("created_via", pick_text(rng, &["tui", "cli", "mcp"])),
        ("created_at", instant(rng)),
        ("updated_at", instant(rng)),
        ("priority", pick_text(rng, PRIORITIES)),
        ("is_mine", int(rng.below(2) as i64)),
        ("has_conflict", int(rng.below(2) as i64)),
        ("description_dirty", int(rng.below(2) as i64)),
    ]);
    let o = opt(rng, 40, |rng| *rng.pick(PRIORITIES));
    sets.push(("local_priority_override", opt_text(o)));
    let members: Vec<&str> = MEMBERS.iter().copied().filter(|_| rng.chance(30)).collect();
    sets.push(("labels", text(&serde_json::to_string(&members).unwrap())));
    let scopes: Vec<&str> = MEMBERS.iter().copied().filter(|_| rng.chance(10)).collect();
    sets.push(("scopes", text(&serde_json::to_string(&scopes).unwrap())));
    if rng.chance(10) {
        sets.push(("pending_fields", text("[10]")));
    }
    for column in ["due_date", "defer_until"] {
        let d = opt(rng, 50, |rng| *rng.pick(DATES));
        sets.push((column, opt_text(d)));
    }
    for column in ["sprint", "resolution", "remote_status_name"] {
        let v = opt(rng, 30, |rng| *rng.pick(STRINGS));
        sets.push((column, opt_text(v)));
    }
    let w = opt(rng, 30, |rng| *rng.pick(WEEKS));
    sets.push(("focus_week", opt_text(w)));
    let points = opt(rng, 40, |rng| *rng.pick(POINTS));
    sets.push(("story_points", points.map_or(Value::Null, Value::Real)));
    let estimate = opt(rng, 30, |rng| *rng.pick(DURATIONS));
    sets.push(("original_estimate", estimate.map_or(Value::Null, int)));
    for column in [
        "last_user_touch_at",
        "last_remote_change_at",
        "status_entered_at",
        "staleness_waiting_since",
        "remote_created_at",
    ] {
        let at = opt(rng, 30, |rng| *rng.pick(INSTANTS));
        sets.push((column, at.map_or(Value::Null, int)));
    }
    if rng.chance(15) {
        sets.push(("deleted_at", instant(rng)));
    }
    if rng.chance(20) {
        sets.push(("archived_at", instant(rng)));
        sets.push(("archived_reason", pick_text(rng, &["user", "auto_done"])));
    }
    if rng.chance(40) {
        // Free text, or a person (which wins over text, as stored).
        let person = rng.chance(40).then(|| *rng.pick(PEOPLE));
        let free = (person.is_none() || rng.chance(20)).then(|| *rng.pick(&["vendor", "Vendor"]));
        sets.extend([
            ("waiting_on_person_id", opt_text(person)),
            ("waiting_on_text", opt_text(free)),
            ("waiting_reason", pick_text(rng, &["", "spec"])),
            ("waiting_since", pick_text(rng, DATES)),
        ]);
        let follow = opt(rng, 60, |rng| *rng.pick(DATES));
        sets.push(("waiting_follow_up_on", opt_text(follow)));
    } else if rng.chance(10) {
        // Since alone, which the codec does not read as waiting.
        sets.push(("waiting_since", pick_text(rng, DATES)));
    }
    if rng.chance(30) {
        let key = opt(rng, 80, |rng| *rng.pick(&["PLAT-99", "PLAT-100", ""]));
        let kind = opt(rng, 80, |_| "jira");
        let state = opt(rng, 60, |rng| *rng.pick(&["pending", "ok", "broken"]));
        sets.extend([
            ("remote_instance_id", text("work")),
            ("remote_id", text(&n.to_string())),
            ("remote_key", opt_text(key)),
            ("remote_tracker_kind", opt_text(kind)),
            ("remote_link_state", opt_text(state)),
            ("remote_pinned", int(rng.below(2) as i64)),
        ]);
        let pulled = opt(rng, 50, |rng| *rng.pick(INSTANTS));
        sets.push((
            "remote_last_pulled_version",
            pulled.map_or(Value::Null, int),
        ));
    }
    if rng.chance(20) {
        let id = opt(rng, 70, |rng| *rng.pick(PEOPLE));
        let free = opt(rng, 50, |_| "bob");
        sets.push(("assignee_person_id", opt_text(id)));
        sets.push(("assignee_text", opt_text(free)));
    }
    if rng.chance(15) {
        sets.push(("recurrence_series_id", text(PEOPLE[0])));
        let d = opt(rng, 50, |rng| *rng.pick(DATES));
        sets.push(("recurrence_occurrence_date", opt_text(d)));
    }
    if !earlier.is_empty() && rng.chance(20) {
        sets.push(("parent_id", text(&rng.pick(earlier).to_string())));
    }
    sets
}

/// Random operands for `field`, drawn from the values the Items hold.
fn random_operand(rng: &mut Rng, field: ItemField, op: Operator, ids: &[ItemId]) -> Operand {
    use FieldValue as V;
    let class = target(field).class;
    let instant = |rng: &mut Rng| {
        V::Timestamp(Timestamp::from_nanosecond((*rng.pick(INSTANTS)).into()).unwrap())
    };
    let date = |rng: &mut Rng| V::Date(rng.pick(DATES).parse().unwrap());
    let string = |rng: &mut Rng, xs: &[&str]| V::String((*rng.pick(xs)).to_owned());
    let priority = |rng: &mut Rng| {
        let all = [
            Priority::None,
            Priority::Lowest,
            Priority::Low,
            Priority::Medium,
            Priority::High,
            Priority::Highest,
        ];
        let from = usize::from(op.is_ordering());
        V::Priority(all[from + rng.below(all.len() - from)])
    };
    let v = match (class, field) {
        (ValueClass::Date, _) if rng.chance(30) => {
            let anchor = *rng.pick(&[Anchor::Today, Anchor::StartOfWeek, Anchor::EndOfWeek]);
            return rel(anchor, rng.below(3) as i32 - 1);
        }
        (ValueClass::FocusWeek, _) if rng.chance(30) => return rel(Anchor::CurrentWeek, 0),
        (ValueClass::Id, _) => {
            if rng.chance(80) {
                V::ItemId(*rng.pick(ids))
            } else {
                V::ItemId(new_ulid())
            }
        }
        (ValueClass::Enum, F::Status) => V::Status(*rng.pick(&[
            Status::Inbox,
            Status::Next,
            Status::Someday,
            Status::InProgress,
            Status::Done,
            Status::Cancelled,
        ])),
        (ValueClass::Enum, F::StatusCategory) => V::StatusCategory(*rng.pick(&[
            StatusCategory::Todo,
            StatusCategory::InProgress,
            StatusCategory::Done,
        ])),
        (ValueClass::Enum, F::ResolutionKind) => {
            V::ResolutionKind(*rng.pick(&[ResolutionKind::Done, ResolutionKind::Cancelled]))
        }
        (ValueClass::Enum, F::TodoRefinement) => V::TodoRefinement(*rng.pick(&[
            TodoRefinement::Inbox,
            TodoRefinement::Next,
            TodoRefinement::Someday,
        ])),
        (ValueClass::Enum, F::CreatedVia) => {
            V::CreatedVia(*rng.pick(&[CreatedVia::Tui, CreatedVia::Cli, CreatedVia::Mcp]))
        }
        (ValueClass::Enum, F::ArchivedReason) => {
            V::ArchiveReason(*rng.pick(&[ArchiveReason::User, ArchiveReason::AutoDone]))
        }
        (ValueClass::Enum, _) => {
            V::LinkState(*rng.pick(&[LinkState::Pending, LinkState::Ok, LinkState::Broken]))
        }
        (ValueClass::Priority, _) => priority(rng),
        (ValueClass::String, F::Title) => string(rng, TITLES),
        (ValueClass::String, F::ItemType) => string(rng, &["task", "bug"]),
        (ValueClass::String, F::RecurrenceSeriesId) => string(rng, PEOPLE),
        (ValueClass::String, _) => string(rng, STRINGS),
        (ValueClass::Markdown, _) => V::Markdown((*rng.pick(&["", "notes", "Notes"])).to_owned()),
        (ValueClass::Date, _) => date(rng),
        (ValueClass::FocusWeek, _) => V::IsoWeek((*rng.pick(WEEKS)).to_owned()),
        (ValueClass::Instant, _) => instant(rng),
        (ValueClass::Number, _) => V::Number(*rng.pick(POINTS)),
        (ValueClass::Duration, _) => V::Duration(SignedDuration::from_nanos(*rng.pick(DURATIONS))),
        (ValueClass::Uint, _) => V::Uint(*rng.pick(&[0, 10, 20, 40])),
        (ValueClass::Bool, _) => V::Bool(rng.chance(50)),
        (ValueClass::Set, _) => string(rng, MEMBERS),
        (ValueClass::Person, _) => {
            if field == F::WaitingOn && rng.chance(50) {
                V::Person(PersonRef::FreeText(
                    (*rng.pick(&["vendor", "Vendor"])).to_owned(),
                ))
            } else {
                V::Person(PersonRef::Person(rng.pick(PEOPLE).parse().unwrap()))
            }
        }
        (ValueClass::Presence | ValueClass::PendingFields | ValueClass::RemoteExtras, _) => {
            V::Bool(true)
        }
    };
    val(v)
}

/// A random predicate that validation accepts (MATCHES aside, which needs
/// the full-text index).
fn random_predicate(rng: &mut Rng, ids: &[ItemId]) -> Clause {
    let fields: Vec<ItemField> = ItemField::ALL
        .iter()
        .copied()
        .filter(|&f| f != F::RemoteExtras)
        .collect();
    let field = *rng.pick(&fields);
    let t = target(field);
    let mut ops = Vec::new();
    if t.can_unset || matches!(t.class, ValueClass::Set | ValueClass::PendingFields) {
        ops.extend([O::IsSet, O::IsUnset]);
    }
    match t.class {
        ValueClass::Set => ops.extend([O::ContainsAny, O::ContainsAll]),
        ValueClass::Presence | ValueClass::PendingFields => {}
        class => {
            ops.extend([O::In, O::Eq, O::Ne]);
            if matches!(
                class,
                ValueClass::Priority
                    | ValueClass::Date
                    | ValueClass::FocusWeek
                    | ValueClass::Instant
                    | ValueClass::Number
                    | ValueClass::Duration
                    | ValueClass::Uint
            ) {
                ops.extend([O::Lt, O::Le, O::Gt, O::Ge]);
            }
        }
    }
    let op = *rng.pick(&ops);
    let count = match op {
        O::IsSet | O::IsUnset => 0,
        O::In | O::ContainsAny | O::ContainsAll => 1 + rng.below(3),
        _ => 1,
    };
    let operands = (0..count)
        .map(|_| random_operand(rng, field, op, ids))
        .collect();
    p(field, op, operands)
}

fn random_clauses(rng: &mut Rng, ids: &[ItemId], depth: usize) -> (Vec<Clause>, Vec<Clause>) {
    let list = |rng: &mut Rng| {
        (0..rng.below(3))
            .map(|_| {
                if depth < 2 && rng.chance(20) {
                    let (all_of, any_of) = random_clauses(rng, ids, depth + 1);
                    group(all_of, any_of)
                } else {
                    random_predicate(rng, ids)
                }
            })
            .collect()
    };
    (list(rng), list(rng))
}

fn random_filter(rng: &mut Rng, ids: &[ItemId]) -> Filter {
    let (all_of, any_of) = random_clauses(rng, ids, 0);
    Filter {
        all_of,
        any_of,
        include_archived: rng.chance(30),
        include_done: rng.chance(30),
        include_deferred: rng.chance(30),
        include_deleted: rng.chance(20),
    }
}

fn random_sort(rng: &mut Rng) -> Vec<SortKey> {
    let sortable: Vec<ItemField> = ItemField::ALL
        .iter()
        .copied()
        .filter(|&f| target(f).sortable)
        .collect();
    let mut keys: Vec<SortKey> = Vec::new();
    for _ in 0..rng.below(4) {
        let key = *rng.pick(&sortable);
        if keys.iter().all(|k| k.key != key) {
            keys.push(SortKey {
                key,
                descending: rng.chance(50),
            });
        }
    }
    keys
}

/// Random Items × random filters and sorts: the SQL lists exactly the Items
/// the core evaluator passes, in the core comparator's order, and its pages
/// concatenate to that list.
#[tokio::test]
async fn sql_agrees_with_the_core_semantics() {
    const ROUNDS: u64 = 4;
    const ITEMS: usize = 40;
    const QUERIES: usize = 120;
    let seed = std::env::var("TODO_ORACLE_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0x05ee_d0ff_11e5);
    for round in 0..ROUNDS {
        let seed = seed.wrapping_add(round).max(1);
        let mut rng = Rng(seed);
        let db = open();
        let mut ids = Vec::new();
        for n in 0..ITEMS {
            let item = capture_at(&db, "x", ts("2026-10-01T00:00:00Z")).await;
            let sets = random_columns(&mut rng, n, &ids);
            update(&db, item.id, sets).await;
            ids.push(item.id);
        }
        let all = db
            .store
            .list_items(input(everything(), vec![], SUN))
            .await
            .unwrap()
            .items;
        assert_eq!(all.len(), ITEMS);
        let (mut skipped, mut narrowed) = (0, 0);
        for query in 0..QUERIES {
            let filter = random_filter(&mut rng, &ids);
            let sort = random_sort(&mut rng);
            let today = *rng.pick(&[SUN, MON, date(2026, 10, 19)]);
            if filter.validate().is_err() {
                skipped += 1;
                continue;
            }
            let context =
                format!("seed {seed} query {query}: {filter:?} sorted by {sort:?} on {today}");
            let resolved = filter.resolve(today).unwrap();
            let mut want: Vec<&Item> = all.iter().filter(|i| matches(i, &resolved)).collect();
            want.sort_by(|a, b| compare(a, b, &sort, today));
            let want: Vec<ItemId> = want.iter().map(|i| i.id).collect();
            let listing = input(filter, sort, today);
            let got = db.store.list_items(listing.clone()).await.unwrap().items;
            let got_ids: Vec<ItemId> = got.iter().map(|i| i.id).collect();
            assert_eq!(got_ids, want, "{context}");
            if !want.is_empty() && want.len() < ITEMS {
                narrowed += 1;
            }
            for item in &got {
                assert_eq!(item.derived.is_overdue, item.is_overdue(today), "{context}");
            }
            if query % 8 == 0 {
                for size in [1, 2, 7] {
                    let pages: Vec<ItemId> = paged(&db, &listing, size)
                        .await
                        .iter()
                        .map(|i| i.id)
                        .collect();
                    assert_eq!(pages, want, "{context} at page size {size}");
                }
            }
        }
        assert!(
            skipped < QUERIES / 10,
            "seed {seed}: {skipped} invalid filters"
        );
        // Most queries select some Items but not all.
        assert!(narrowed > QUERIES / 2, "seed {seed}: {narrowed} narrowed");
        eprintln!("seed {seed}: {narrowed} of {QUERIES} queries narrowed, {skipped} skipped");
    }
}
