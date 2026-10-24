use std::collections::BTreeSet;
use std::path::PathBuf;

use jiff::civil::{Date, date};
use jiff::{SignedDuration, Timestamp};
use rusqlite::Connection;
use todo_core::{
    Actor, CaptureDraft, CreatedVia, FieldChange, FieldUpdate, FieldValue, Item, ItemField, ItemId,
    Mutation, MutationError, MutationOp, PersonRef, Priority, Revision, StalenessSignals, Status,
    StatusFields, Topic, Waiting, new_ulid,
};
use todo_store::{
    CaptureInput, ChangesSinceError, Expired, ListQuery, MutateInput, OpKind, Resource,
    SCHEMA_VERSION, Store, StoreError,
};
use tokio::sync::broadcast::error::TryRecvError;

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

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

fn today() -> Date {
    date(2026, 3, 1)
}

fn input(title: &str, at: i64) -> CaptureInput {
    CaptureInput {
        request_id: new_ulid().to_string(),
        draft: CaptureDraft::from_title(title),
        created_via: CreatedVia::Cli,
        parent_id: None,
        spool_id: None,
        now: ts(at),
        today: today(),
    }
}

#[tokio::test]
async fn migrates_an_empty_database() {
    let db = open();
    let conn = Connection::open(&db.path).unwrap();
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    for table in [
        "meta",
        "items",
        "items_fts",
        "ops",
        "events",
        "changes",
        "request_ids",
        "spool_imports",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{table}");
    }
    assert_eq!(db.store.current_revision(), 0);

    // Reopening is a no-op migration.
    drop(db.store);
    let reopened = Store::open(&db.path).unwrap();
    assert_eq!(reopened.current_revision(), 0);
}

#[tokio::test]
async fn refuses_a_newer_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("todo.db");
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    match Store::open(&path) {
        Err(StoreError::SchemaTooNew { found, supported }) => {
            assert_eq!(found, SCHEMA_VERSION + 1);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        other => panic!("expected SchemaTooNew, got {:?}", other.err()),
    }
}

#[tokio::test]
async fn fts5_indexes_items() {
    let db = open();
    db.store
        .capture(input("call the dentist", 1))
        .await
        .unwrap();
    db.store.capture(input("buy milk", 2)).await.unwrap();
    let hits: Vec<String> = db
        .store
        .read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT items.title FROM items_fts JOIN items ON items.seq = items_fts.rowid
                 WHERE items_fts MATCH 'dentist'",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(hits, ["call the dentist"]);

    // The update trigger keeps the index current.
    db.store
        .write(|tx| {
            tx.conn()
                .execute("UPDATE items SET title = 'see the orthodontist'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let n: i64 = db
        .store
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM items_fts WHERE items_fts MATCH 'orthodontist'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn capture_and_list_round_trip() {
    let db = open();
    let (item, result) = db.store.capture(input("call dentist", 100)).await.unwrap();
    assert_eq!(result.revision, 1);
    assert_eq!(item.revision, 1);
    assert_eq!(item.status(), Status::Inbox);
    assert_eq!(item.created_via, CreatedVia::Cli);
    assert_eq!(db.store.current_revision(), 1);

    let page = db.store.list_items(ListQuery::default()).await.unwrap();
    assert_eq!(page.revision, 1);
    assert_eq!(page.next_page_token, None);
    assert_eq!(page.items, vec![item.clone()]);

    let (op_rows, event_rows): (i64, i64) = db
        .store
        .read(|c| {
            Ok((
                c.query_row("SELECT count(*) FROM ops", [], |r| r.get(0))?,
                c.query_row(
                    "SELECT count(*) FROM events WHERE kind = 'create'",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((op_rows, event_rows), (1, 1));
}

#[tokio::test]
async fn capture_is_idempotent_on_request_and_spool_id() {
    let db = open();
    let first = input("once", 1);
    let a = db.store.capture(first.clone()).await.unwrap();
    let b = db.store.capture(first).await.unwrap();
    assert_eq!(a, b);

    let mut spooled = input("spooled", 2);
    spooled.spool_id = Some("spool-1".into());
    let c = db.store.capture(spooled.clone()).await.unwrap();
    spooled.request_id = new_ulid().to_string();
    let d = db.store.capture(spooled).await.unwrap();
    assert_eq!(c, d);

    assert_eq!(db.store.current_revision(), 2);
    let page = db.store.list_items(ListQuery::default()).await.unwrap();
    assert_eq!(page.items.len(), 2);
}

#[tokio::test]
async fn capture_with_missing_parent_fails() {
    let db = open();
    let mut child = input("child", 1);
    let parent = new_ulid();
    child.parent_id = Some(parent);
    match db.store.capture(child).await {
        Err(StoreError::NotFound(id)) => assert_eq!(id, parent),
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert_eq!(db.store.current_revision(), 0);
}

async fn set_columns(db: &Db, id: &Item, set: &'static str) {
    let id = id.id.to_string();
    db.store
        .write(move |tx| {
            tx.conn()
                .execute(&format!("UPDATE items SET {set} WHERE id = ?1"), [id])?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn list_applies_implicit_rules_and_status_filter() {
    let db = open();
    let mut items = Vec::new();
    for (i, title) in [
        "inbox",
        "next",
        "done",
        "cancelled",
        "archived",
        "deleted",
        "deferred",
        "due",
    ]
    .into_iter()
    .enumerate()
    {
        items.push(db.store.capture(input(title, i as i64)).await.unwrap().0);
    }
    set_columns(&db, &items[1], "todo_refinement = 'next'").await;
    set_columns(
        &db,
        &items[2],
        "status_category = 'done', resolution_kind = 'done'",
    )
    .await;
    set_columns(
        &db,
        &items[3],
        "status_category = 'done', resolution_kind = 'cancelled'",
    )
    .await;
    set_columns(&db, &items[4], "archived_at = 1, archived_reason = 'user'").await;
    set_columns(&db, &items[5], "deleted_at = 1").await;
    set_columns(&db, &items[6], "defer_until = '2026-06-01'").await;
    set_columns(&db, &items[7], "due_date = '2026-01-01'").await;

    let today = Some(jiff::civil::date(2026, 3, 1));
    let titles = |page: todo_store::ItemPage| {
        page.items
            .into_iter()
            .map(|i| i.core.title)
            .collect::<Vec<_>>()
    };
    let q = ListQuery {
        today,
        ..ListQuery::default()
    };
    let page = db.store.list_items(q.clone()).await.unwrap();
    assert!(
        page.items
            .iter()
            .any(|i| i.core.title == "due" && i.derived.is_overdue)
    );
    assert_eq!(titles(page), ["inbox", "next", "due"]);

    let page = db
        .store
        .list_items(ListQuery {
            statuses: vec![Status::Next, Status::Done],
            include_done: true,
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(titles(page), ["next", "done"]);

    let everything = ListQuery {
        include_archived: true,
        include_done: true,
        include_deferred: true,
        include_deleted: true,
        ..q.clone()
    };
    assert_eq!(
        db.store.list_items(everything).await.unwrap().items.len(),
        8
    );

    let later = ListQuery {
        today: Some(jiff::civil::date(2026, 6, 1)),
        ..ListQuery::default()
    };
    assert!(titles(db.store.list_items(later).await.unwrap()).contains(&"deferred".to_owned()));

    let done = db.store.list_items(ListQuery {
        include_done: true,
        statuses: vec![Status::Cancelled],
        ..q
    });
    let page = done.await.unwrap();
    assert_eq!(
        page.items[0].status_fields(),
        StatusFields::from_status(Status::Cancelled)
    );
}

#[tokio::test]
async fn list_pages_are_ordered_and_report_their_read_revision() {
    let db = open();
    // Created out of order; listing sorts by created_at.
    for at in [5, 3, 1, 4, 2] {
        db.store
            .capture(input(&format!("t{at}"), at))
            .await
            .unwrap();
    }
    let mut query = ListQuery {
        page_size: 2,
        ..ListQuery::default()
    };
    let mut seen = Vec::new();
    let mut revisions = Vec::new();
    loop {
        let page = db.store.list_items(query.clone()).await.unwrap();
        assert!(page.items.len() <= 2);
        revisions.push(page.revision);
        seen.extend(page.items.into_iter().map(|i| i.core.title));
        if revisions.len() == 1 {
            // Pages read live: a write between pages shows up, with its revision.
            db.store.capture(input("t9", 9)).await.unwrap();
        }
        match page.next_page_token {
            Some(token) => query.page_token = Some(token),
            None => break,
        }
    }
    assert_eq!(seen, ["t1", "t2", "t3", "t4", "t5", "t9"]);
    assert_eq!(revisions, [5, 6, 6]);

    let bad = ListQuery {
        page_token: Some("garbage".into()),
        ..ListQuery::default()
    };
    assert!(matches!(
        db.store.list_items(bad).await,
        Err(StoreError::InvalidPageToken)
    ));
}

#[tokio::test]
async fn page_tokens_are_bound_to_their_query() {
    let db = open();
    for at in 1..=3 {
        db.store
            .capture(input(&format!("t{at}"), at))
            .await
            .unwrap();
    }
    let first = db
        .store
        .list_items(ListQuery {
            page_size: 1,
            ..ListQuery::default()
        })
        .await
        .unwrap();
    let token = first.next_page_token.unwrap();
    let other = ListQuery {
        page_size: 1,
        include_done: true,
        page_token: Some(token.clone()),
        ..ListQuery::default()
    };
    assert!(matches!(
        db.store.list_items(other).await,
        Err(StoreError::InvalidPageToken)
    ));
    let same = ListQuery {
        page_size: 1,
        page_token: Some(token),
        ..ListQuery::default()
    };
    let page = db.store.list_items(same).await.unwrap();
    assert_eq!(page.items[0].core.title, "t2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revisions_are_monotonic_and_contiguous_under_concurrent_writes() {
    let db = open();
    let mut rx = db.store.subscribe();
    let mut tasks = Vec::new();
    for task in 0..16 {
        let store = db.store.clone();
        tasks.push(tokio::spawn(async move {
            let mut blocks = Vec::new();
            for n in 0..10 {
                let width = (task + n) % 3 + 1;
                let block = store
                    .write(move |tx| {
                        (0..width)
                            .map(|_| tx.record_change(Topic::Config, "cfg", true, None))
                            .collect::<todo_store::Result<Vec<_>>>()
                    })
                    .await
                    .unwrap();
                blocks.push(block);
            }
            blocks
        }));
    }
    let mut all = BTreeSet::new();
    let mut total = 0;
    for task in tasks {
        for block in task.await.unwrap() {
            for pair in block.windows(2) {
                assert_eq!(pair[1], pair[0] + 1, "block {block:?} is not contiguous");
            }
            total += block.len();
            all.extend(block);
        }
    }
    let total = total as u64;
    assert_eq!(all.len() as u64, total);
    assert_eq!(all, (1..=total).collect());
    assert_eq!(db.store.current_revision(), total);

    let mut last = 0;
    for _ in 0..total {
        let change = rx.recv().await.unwrap();
        assert_eq!(change.revision, last + 1);
        last = change.revision;
    }

    let replay = db.store.changes_since(0).await.unwrap();
    assert_eq!(
        replay.iter().map(|c| c.revision).collect::<Vec<_>>(),
        (1..=total).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn changes_since_replays_expires_and_detects_ahead() {
    let db = open();
    let (a, _) = db.store.capture(input("a", 1)).await.unwrap();
    let (b, _) = db.store.capture(input("b", 2)).await.unwrap();
    let a_id = a.id.to_string();
    let a_id2 = a_id.clone();
    db.store
        .write(move |tx| {
            tx.conn()
                .execute("UPDATE items SET title = 'a2' WHERE id = ?1", [&a_id2])?;
            tx.record_change(Topic::Item, &a_id2, false, None)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(db.store.current_revision(), 3);

    let all = db.store.changes_since(0).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|c| (c.revision, c.id.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, a_id.as_str()),
            (2, b.id.to_string().as_str()),
            (3, a_id.as_str())
        ]
    );
    // Replay carries the current state.
    let Some(Resource::Item(first)) = &all[0].resource else {
        panic!("missing resource");
    };
    assert_eq!(first.core.title, "a2");

    let tail = db.store.changes_since(2).await.unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].revision, 3);
    assert!(db.store.changes_since(3).await.unwrap().is_empty());

    assert!(matches!(
        db.store.changes_since(4).await,
        Err(ChangesSinceError::Ahead {
            since: 4,
            current: 3
        })
    ));

    db.store
        .write(|tx| {
            tx.conn()
                .execute("DELETE FROM changes WHERE revision <= 2", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        db.store.changes_since(0).await,
        Err(ChangesSinceError::Expired(Expired {
            since: 0,
            oldest: 3
        }))
    ));
    assert_eq!(db.store.changes_since(2).await.unwrap().len(), 1);

    db.store
        .write(|tx| {
            tx.conn().execute("DELETE FROM changes", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        db.store.changes_since(2).await,
        Err(ChangesSinceError::Expired(Expired {
            since: 2,
            oldest: 4
        }))
    ));
    assert!(db.store.changes_since(3).await.unwrap().is_empty());
}

#[tokio::test]
async fn broadcast_happens_only_after_commit() {
    let db = open();
    let mut rx = db.store.subscribe();

    let failed = db
        .store
        .write(|tx| {
            tx.record_change(Topic::Config, "cfg", true, None)?;
            Err::<(), _>(StoreError::Corrupt("abort".into()))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(db.store.current_revision(), 0);
    assert!(db.store.changes_since(0).await.unwrap().is_empty());

    // A panicking closure rolls back too, and the writer survives.
    let panicked = db
        .store
        .write(|tx| {
            tx.record_change(Topic::Config, "cfg", true, None)?;
            panic!("boom");
            #[allow(unreachable_code)]
            Ok(())
        })
        .await;
    assert!(matches!(panicked, Err(StoreError::Closed)));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

    let (item, _) = db.store.capture(input("live", 1)).await.unwrap();
    let change = rx.recv().await.unwrap();
    assert_eq!(change.revision, 1);
    assert_eq!(change.topic, Topic::Item);
    assert_eq!(change.id, item.id.to_string());
    assert_eq!(change.resource, Some(Resource::Item(item.into())));

    // By the time the change is visible, an independent reader sees the commit.
    let conn = Connection::open(&db.path).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn capture_stores_every_draft_field() {
    let db = open();
    let mut rx = db.store.subscribe();
    let waiting = Waiting {
        on: PersonRef::FreeText("the vendor".into()),
        reason: String::new(),
        since: today(),
        follow_up_on: None,
    };
    let mut capture = input("", 100);
    capture.draft = CaptureDraft {
        priority: Priority::High,
        due_date: Some(date(2026, 2, 27)),
        labels: vec!["home".into(), "errands".into()],
        waiting: Some(waiting.clone()),
        status: Status::Someday,
        ..CaptureDraft::from_title("call the vendor")
    };
    let (item, result) = db.store.capture(capture).await.unwrap();
    assert_eq!(result.revision, 1);
    assert_eq!(item.core.title, "call the vendor");
    assert_eq!(item.core.priority, Priority::High);
    assert_eq!(item.core.due_date, Some(date(2026, 2, 27)));
    assert_eq!(item.core.labels, ["home", "errands"]);
    assert_eq!(item.overlay.waiting, Some(waiting));
    assert_eq!(item.status(), Status::Someday);
    assert_eq!(
        item.derived.staleness,
        StalenessSignals {
            last_user_touch_at: Some(ts(100)),
            last_remote_change_at: None,
            status_entered_at: Some(ts(100)),
            waiting_since: Some(ts(100)),
        }
    );
    assert!(item.derived.is_overdue);

    // Stored and broadcast as returned; overdue is relative to today.
    assert_eq!(
        db.store.get_item(item.id, today()).await.unwrap().item,
        item
    );
    let change = rx.recv().await.unwrap();
    assert_eq!(change.resource, Some(Resource::Item(item.clone().into())));
    let earlier = db.store.get_item(item.id, date(2026, 2, 1)).await.unwrap();
    assert!(!earlier.item.derived.is_overdue);
}

#[tokio::test]
async fn capture_rejects_an_empty_title_and_people() {
    let db = open();
    match db.store.capture(input(" ", 1)).await {
        Err(StoreError::Mutation {
            index: 0,
            error:
                MutationError::Invalid {
                    field: ItemField::Title,
                    ..
                },
        }) => {}
        other => panic!("expected an invalid title, got {other:?}"),
    }
    let mut person = input("ask", 1);
    person.draft.waiting = Some(Waiting {
        on: PersonRef::Person(new_ulid()),
        reason: String::new(),
        since: today(),
        follow_up_on: None,
    });
    match db.store.capture(person).await {
        Err(StoreError::Mutation {
            index: 0,
            error:
                MutationError::Unsupported {
                    field: ItemField::WaitingOn,
                    ..
                },
        }) => {}
        other => panic!("expected an unsupported person, got {other:?}"),
    }
    assert_eq!(snapshot(&db).await, (0, [0; 4], vec![]));
}

fn mutation(id: ItemId, op: MutationOp) -> Mutation {
    Mutation {
        id,
        expected_revision: None,
        op,
    }
}

fn set(id: ItemId, field: ItemField, value: FieldValue) -> Mutation {
    mutation(id, MutationOp::Update(vec![FieldUpdate { field, value }]))
}

fn title(id: ItemId, title: &str) -> Mutation {
    set(id, ItemField::Title, FieldValue::String(title.into()))
}

fn mutate(kind: OpKind, mutations: Vec<Mutation>) -> MutateInput {
    MutateInput {
        request_id: new_ulid().to_string(),
        actor: Actor::User,
        kind,
        mutations,
        now: ts(1_000),
        today: today(),
    }
}

async fn capture(db: &Db, title: &str) -> Item {
    db.store.capture(input(title, 1)).await.unwrap().0
}

/// The current revision, the `ops`, `events`, `changes` and `request_ids`
/// row counts, and every Item: what a failed write must leave untouched.
async fn snapshot(db: &Db) -> (Revision, [i64; 4], Vec<Item>) {
    let counts = db
        .store
        .read(|c| {
            let n = |table: &str| {
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            };
            Ok([n("ops")?, n("events")?, n("changes")?, n("request_ids")?])
        })
        .await
        .unwrap();
    let all = ListQuery {
        include_archived: true,
        include_done: true,
        include_deferred: true,
        include_deleted: true,
        today: Some(today()),
        ..ListQuery::default()
    };
    let items = db.store.list_items(all).await.unwrap().items;
    (db.store.current_revision(), counts, items)
}

/// `(kind, actor, request_id, revision)` of an `ops` row.
async fn op_row(db: &Db, op_id: ulid::Ulid) -> (String, String, Option<String>, i64) {
    db.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT kind, actor, request_id, revision FROM ops WHERE id = ?1",
                [op_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .await
        .unwrap()
}

#[derive(Debug, PartialEq)]
struct EventRow {
    item_id: String,
    actor: String,
    kind: String,
    change: FieldChange,
}

/// The events of an op, in insertion order, with `old`/`new` decoded.
async fn events(db: &Db, op_id: ulid::Ulid) -> Vec<EventRow> {
    db.store
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT item_id, actor, kind, field, old, new FROM events
                 WHERE op_id = ?1 ORDER BY rowid",
            )?;
            let rows = stmt.query_map([op_id.to_string()], |r| {
                let decode = |json: String| serde_json::from_str(&json).unwrap();
                Ok(EventRow {
                    item_id: r.get(0)?,
                    actor: r.get(1)?,
                    kind: r.get(2)?,
                    change: FieldChange {
                        field: ItemField::from_number(r.get(3)?).unwrap(),
                        old: decode(r.get(4)?),
                        new: decode(r.get(5)?),
                    },
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap()
}

/// `changes` rows after `since`: `(revision, resource_id, deleted)`.
async fn change_rows(db: &Db, since: i64) -> Vec<(i64, String, bool)> {
    db.store
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT revision, resource_id, deleted FROM changes
                 WHERE revision > ?1 ORDER BY revision",
            )?;
            let rows = stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn mutate_records_one_op_with_an_event_per_changed_field() {
    let db = open();
    let item = capture(&db, "call dentist").await;
    let mut rx = db.store.subscribe();
    let (_, [ops, events_n, changes, requests], _) = snapshot(&db).await;
    let update = |field, value| FieldUpdate { field, value };
    let req = mutate(
        OpKind::Update,
        vec![Mutation {
            id: item.id,
            expected_revision: Some(1),
            op: MutationOp::Update(vec![
                update(
                    ItemField::Title,
                    FieldValue::String("call the dentist".into()),
                ),
                update(ItemField::Priority, FieldValue::Priority(Priority::High)),
                // Unchanged: no event.
                update(ItemField::Labels, FieldValue::StringSet(vec![])),
                update(ItemField::DueDate, FieldValue::Date(date(2026, 2, 1))),
            ]),
        }],
    );
    let request_id = req.request_id.clone();
    let out = db.store.mutate(req).await.unwrap();
    assert_eq!(out.result.revision, 2);
    assert_eq!(db.store.current_revision(), 2);
    let [after] = out.items.as_slice() else {
        panic!("expected one item, got {:?}", out.items);
    };
    assert_eq!(after.revision, 2);
    assert_eq!(after.core.title, "call the dentist");
    assert_eq!(after.core.priority, Priority::High);
    assert_eq!(after.updated_at, ts(1_000));
    assert!(after.derived.is_overdue);
    assert_eq!(
        db.store.get_item(item.id, today()).await.unwrap().item,
        *after
    );

    assert_eq!(
        op_row(&db, out.result.op_id).await,
        ("update".into(), "user".into(), Some(request_id), 2)
    );
    let event = |field, old, new| EventRow {
        item_id: item.id.to_string(),
        actor: "user".into(),
        kind: "set".into(),
        change: FieldChange { field, old, new },
    };
    assert_eq!(
        events(&db, out.result.op_id).await,
        [
            event(
                ItemField::Title,
                FieldValue::String("call dentist".into()),
                FieldValue::String("call the dentist".into()),
            ),
            event(
                ItemField::Priority,
                FieldValue::Priority(Priority::None),
                FieldValue::Priority(Priority::High),
            ),
            event(
                ItemField::DueDate,
                FieldValue::Cleared,
                FieldValue::Date(date(2026, 2, 1)),
            ),
        ]
    );
    assert_eq!(change_rows(&db, 1).await, [(2, item.id.to_string(), false)]);
    // Nothing else: one op, three events, one change, one request id.
    assert_eq!(
        snapshot(&db).await.1,
        [ops + 1, events_n + 3, changes + 1, requests + 1]
    );

    // Broadcast after commit: an independent reader already sees the row.
    let change = rx.try_recv().unwrap();
    assert_eq!((change.revision, change.deleted), (2, false));
    assert_eq!(change.resource, Some(Resource::Item(after.clone().into())));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    let stored: String = Connection::open(&db.path)
        .unwrap()
        .query_row("SELECT title FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, "call the dentist");
}

#[tokio::test]
async fn an_update_rewrites_every_other_column_unchanged() {
    let db = open();
    let parent = capture(&db, "parent").await;
    let other = capture(&db, "other").await;
    let item = capture(&db, "x").await;
    set_columns(
        &db,
        &item,
        "remote_tracker_kind = 'jira', remote_instance_id = 'work', remote_id = '10001',
         remote_key = 'PLAT-1', remote_url = 'https://jira.example.com/browse/PLAT-1',
         remote_last_pulled_version = 5, remote_link_state = 'broken', remote_pinned = 1,
         description = '**d**', item_type = 'Bug', status_category = 'in_progress',
         remote_status_name = 'In Review', priority = 'high', assignee_text = 'sam',
         reporter_text = 'dana', labels = '[\"a\",\"b\"]', components = '[\"api\"]',
         fix_versions = '[\"1.2\"]', due_date = '2026-04-01', sprint = 'S7',
         story_points = 2.5, original_estimate = 60000000000, remaining_estimate = 1,
         resolution = 'Fixed', remote_created_at = 7, remote_updated_at = 8,
         todo_refinement = 'someday', waiting_on_text = 'the vendor', waiting_reason = 'quote',
         waiting_since = '2026-02-01', waiting_follow_up_on = '2026-03-05',
         defer_until = '2026-02-15', private_notes = 'n', focus_week = '2026-W09',
         local_priority_override = 'low', archived_at = 9, archived_reason = 'left_scope',
         recurrence_series_id = '01JB8X4Z5Q6R7S8T9V0W1X2Y3Z',
         recurrence_occurrence_date = '2026-02-02', remote_extras = '{\"cf\":[1]}',
         is_mine = 1, pending_fields = '[16,41001]', has_conflict = 1, has_push_failure = 1,
         description_lossy = 1, description_dirty = 1, last_user_touch_at = 9,
         last_remote_change_at = 10, status_entered_at = 11, staleness_waiting_since = 12,
         scopes = '[\"mine\"]'",
    )
    .await;
    let (id, parent_id, other_id) = (
        item.id.to_string(),
        parent.id.to_string(),
        other.id.to_string(),
    );
    db.store
        .write(move |tx| {
            tx.conn().execute(
                "UPDATE items SET parent_id = ?2, merged_into = ?3 WHERE id = ?1",
                [id, parent_id, other_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let find = |items: Vec<Item>| items.into_iter().find(|i| i.id == item.id).unwrap();
    let before = find(snapshot(&db).await.2);
    assert!(before.is_linked() && before.overlay.waiting.is_some());
    assert_eq!(before.derived.merged_into, Some(other.id));

    // A sync update leaves the user-touch signal alone, so only the title,
    // updated_at and revision move.
    let mut req = mutate(OpKind::Update, vec![title(item.id, "renamed")]);
    req.actor = Actor::Sync;
    let out = db.store.mutate(req).await.unwrap();
    let mut want = before;
    want.core.title = "renamed".into();
    want.updated_at = ts(1_000);
    want.revision = 4;
    assert_eq!(out.items, [want.clone()]);
    assert_eq!(find(snapshot(&db).await.2), want);

    // The FTS index follows the rewrite.
    let hits: i64 = db
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM items_fts WHERE items_fts MATCH 'renamed'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(hits, 1);
}

#[tokio::test]
async fn ops_and_events_record_their_actor() {
    let db = open();
    let item = capture(&db, "x").await;
    let suggestion = new_ulid();
    for (actor, name, expected) in [
        (Actor::User, "a", "user".to_owned()),
        (Actor::Sync, "b", "sync".to_owned()),
        (Actor::System, "c", "system".to_owned()),
        (
            Actor::Suggestion(suggestion),
            "d",
            format!("suggestion:{suggestion}"),
        ),
    ] {
        let mut req = mutate(OpKind::Update, vec![title(item.id, name)]);
        req.actor = actor;
        let out = db.store.mutate(req).await.unwrap();
        assert_eq!(op_row(&db, out.result.op_id).await.1, expected);
        let events = events(&db, out.result.op_id).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].actor, expected);
    }
}

#[tokio::test]
async fn a_failing_mutation_rolls_back_the_whole_batch() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture(&db, "b").await;
    let mut rx = db.store.subscribe();
    let before = snapshot(&db).await;

    let mut batch = vec![
        title(a.id, "a2"),
        mutation(b.id, MutationOp::SetStatus(Status::Next)),
        title(a.id, "  "),
    ];
    match db.store.mutate(mutate(OpKind::Batch, batch.clone())).await {
        Err(StoreError::Mutation {
            index: 2,
            error:
                MutationError::Invalid {
                    field: ItemField::Title,
                    ..
                },
        }) => {}
        other => panic!("expected an invalid title, got {other:?}"),
    }
    assert_eq!(snapshot(&db).await, before);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

    let missing = new_ulid();
    batch[2] = mutation(missing, MutationOp::Archive);
    match db.store.mutate(mutate(OpKind::Batch, batch)).await {
        Err(StoreError::NotFound(id)) => assert_eq!(id, missing),
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert_eq!(snapshot(&db).await, before);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn revision_mismatch_writes_nothing() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture(&db, "b").await;
    let before = snapshot(&db).await;
    let stale = Mutation {
        id: b.id,
        expected_revision: Some(1),
        op: MutationOp::Archive,
    };
    let mut req = mutate(OpKind::Batch, vec![title(a.id, "a2"), stale]);
    match db.store.mutate(req.clone()).await {
        Err(StoreError::RevisionMismatch {
            id,
            expected: 1,
            actual: 2,
        }) => assert_eq!(id, b.id),
        other => panic!("expected RevisionMismatch, got {other:?}"),
    }
    assert_eq!(snapshot(&db).await, before);

    // The failed request did not use up its request id.
    req.mutations[1].expected_revision = Some(2);
    let out = db.store.mutate(req).await.unwrap();
    assert_eq!(out.result.revision, 4);
}

#[tokio::test]
async fn an_item_mutated_twice_in_a_batch_gets_one_revision() {
    let db = open();
    let a = capture(&db, "a").await;
    let out = db
        .store
        .mutate(mutate(
            OpKind::Batch,
            vec![
                Mutation {
                    expected_revision: Some(1),
                    ..title(a.id, "a2")
                },
                Mutation {
                    expected_revision: Some(1),
                    ..mutation(a.id, MutationOp::SetStatus(Status::Next))
                },
            ],
        ))
        .await
        .unwrap();
    assert_eq!(out.result.revision, 2);
    assert_eq!(out.items.len(), 2);
    assert_eq!(out.items[0], out.items[1]);
    assert_eq!(out.items[0].core.title, "a2");
    assert_eq!(out.items[0].status(), Status::Next);
    assert_eq!(out.items[0].revision, 2);
    assert_eq!(change_rows(&db, 1).await, [(2, a.id.to_string(), false)]);
    let fields: Vec<ItemField> = events(&db, out.result.op_id)
        .await
        .into_iter()
        .map(|e| e.change.field)
        .collect();
    assert_eq!(fields, [ItemField::Title, ItemField::TodoRefinement]);
}

#[tokio::test]
async fn a_batch_allocates_contiguous_revisions_in_first_touch_order() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture(&db, "b").await;
    let c = capture(&db, "c").await;
    let mut rx = db.store.subscribe();
    let req = mutate(
        OpKind::Batch,
        vec![
            mutation(c.id, MutationOp::Archive),
            mutation(a.id, MutationOp::SetStatus(Status::Done)),
            title(c.id, "c2"),
            mutation(b.id, MutationOp::Delete),
        ],
    );
    let request_id = req.request_id.clone();
    let out = db.store.mutate(req).await.unwrap();
    assert_eq!(out.result.revision, 6);
    assert_eq!(db.store.current_revision(), 6);
    let revisions: Vec<(ItemId, Revision)> = out.items.iter().map(|i| (i.id, i.revision)).collect();
    assert_eq!(revisions, [(c.id, 4), (a.id, 5), (c.id, 4), (b.id, 6)]);
    assert_eq!(out.items[0].core.title, "c2");
    assert_eq!(
        change_rows(&db, 3).await,
        [
            (4, c.id.to_string(), false),
            (5, a.id.to_string(), false),
            (6, b.id.to_string(), false),
        ]
    );
    assert_eq!(
        op_row(&db, out.result.op_id).await,
        ("batch".into(), "user".into(), Some(request_id), 6)
    );
    let live: Vec<Revision> = (0..3).map(|_| rx.try_recv().unwrap().revision).collect();
    assert_eq!(live, [4, 5, 6]);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn a_replayed_request_returns_the_original_result_and_current_state() {
    let db = open();
    let a = capture(&db, "a").await;
    let req = mutate(
        OpKind::Update,
        vec![Mutation {
            expected_revision: Some(1),
            ..title(a.id, "a2")
        }],
    );
    let out = db.store.mutate(req.clone()).await.unwrap();
    assert_eq!(db.store.mutate(req.clone()).await.unwrap(), out);

    // After the Item changed, a replay still returns the original op and
    // revision, with the current state, unchecked against expected_revision.
    let later = db
        .store
        .mutate(mutate(OpKind::Update, vec![title(a.id, "a3")]))
        .await
        .unwrap();
    let before = snapshot(&db).await;
    let replay = db.store.mutate(req.clone()).await.unwrap();
    assert_eq!(replay.result, out.result);
    assert_eq!(replay.items, later.items);
    assert_eq!(snapshot(&db).await, before);

    let mut missing = req;
    missing.mutations[0].id = new_ulid();
    assert!(matches!(
        db.store.mutate(missing).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_request_id_replays_only_the_same_kind_of_request() {
    let db = open();
    let captured = input("a", 1);
    let a = db.store.capture(captured.clone()).await.unwrap().0;
    let update = mutate(OpKind::Update, vec![title(a.id, "a2")]);
    db.store.mutate(update.clone()).await.unwrap();
    let before = snapshot(&db).await;

    let reused = |request_id: &str, kind| MutateInput {
        request_id: request_id.to_owned(),
        ..mutate(kind, vec![mutation(a.id, MutationOp::Archive)])
    };
    for input in [
        reused(&captured.request_id, OpKind::Archive),
        reused(&update.request_id, OpKind::Archive),
        reused(&update.request_id, OpKind::Batch),
    ] {
        match db.store.mutate(input).await {
            Err(StoreError::RequestIdReused) => {}
            other => panic!("expected RequestIdReused, got {other:?}"),
        }
    }
    let recapture = CaptureInput {
        request_id: update.request_id.clone(),
        ..input("b", 2)
    };
    match db.store.capture(recapture).await {
        Err(StoreError::RequestIdReused) => {}
        other => panic!("expected RequestIdReused, got {other:?}"),
    }
    assert_eq!(snapshot(&db).await, before);

    // The same kind still replays.
    assert_eq!(db.store.capture(captured).await.unwrap().0.id, a.id);
    db.store.mutate(update).await.unwrap();
    assert_eq!(snapshot(&db).await, before);
}

#[tokio::test]
async fn a_batch_that_changes_nothing_records_an_op_without_a_revision() {
    let db = open();
    let a = capture(&db, "a").await;
    let mut rx = db.store.subscribe();
    let (_, [ops, events_n, changes, requests], items) = snapshot(&db).await;
    let req = mutate(
        OpKind::Batch,
        vec![mutation(a.id, MutationOp::Unarchive), title(a.id, "a")],
    );
    let request_id = req.request_id.clone();
    let out = db.store.mutate(req.clone()).await.unwrap();
    assert_eq!(out.result.revision, 1);
    assert_eq!(out.items, [a.clone(), a]);
    assert_eq!(db.store.current_revision(), 1);
    assert_eq!(
        op_row(&db, out.result.op_id).await,
        ("batch".into(), "user".into(), Some(request_id), 1)
    );
    assert_eq!(events(&db, out.result.op_id).await, []);
    assert_eq!(change_rows(&db, 1).await, []);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(
        snapshot(&db).await,
        (1, [ops + 1, events_n, changes, requests + 1], items)
    );

    let replay = db.store.mutate(req).await.unwrap();
    assert_eq!(replay.result, out.result);
}

async fn titles(db: &Db, include_deleted: bool) -> Vec<String> {
    let query = ListQuery {
        include_deleted,
        ..ListQuery::default()
    };
    let page = db.store.list_items(query).await.unwrap();
    page.items.into_iter().map(|i| i.core.title).collect()
}

#[tokio::test]
async fn delete_hides_an_item_until_undelete() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture(&db, "b").await;
    let mut rx = db.store.subscribe();

    let deleted = db
        .store
        .mutate(mutate(
            OpKind::Delete,
            vec![mutation(a.id, MutationOp::Delete)],
        ))
        .await
        .unwrap();
    let tombstone = &deleted.items[0];
    assert_eq!(tombstone.derived.deleted_at, Some(ts(1_000)));
    assert_eq!(titles(&db, false).await, ["b"]);
    assert_eq!(titles(&db, true).await, ["a", "b"]);
    assert_eq!(
        db.store.get_item(a.id, today()).await.unwrap().item,
        *tombstone
    );

    // Watchers see an ordinary change carrying the tombstone.
    assert_eq!(change_rows(&db, 2).await, [(3, a.id.to_string(), false)]);
    let change = rx.try_recv().unwrap();
    assert!(!change.deleted);
    assert_eq!(
        change.resource,
        Some(Resource::Item(tombstone.clone().into()))
    );

    // Other mutations of a deleted Item fail; deleting again changes nothing.
    for op in [
        MutationOp::Update(vec![FieldUpdate {
            field: ItemField::Title,
            value: FieldValue::String("a2".into()),
        }]),
        MutationOp::SetStatus(Status::Next),
        MutationOp::Archive,
        MutationOp::Unarchive,
    ] {
        let batch = vec![mutation(b.id, MutationOp::Archive), mutation(a.id, op)];
        match db.store.mutate(mutate(OpKind::Batch, batch)).await {
            Err(StoreError::Mutation {
                index: 1,
                error: MutationError::Deleted,
            }) => {}
            other => panic!("expected Deleted, got {other:?}"),
        }
    }
    let again = db
        .store
        .mutate(mutate(
            OpKind::Delete,
            vec![mutation(a.id, MutationOp::Delete)],
        ))
        .await
        .unwrap();
    assert_eq!(again.result.revision, 3);
    assert_eq!(again.items[0], *tombstone);

    let restored = db
        .store
        .mutate(mutate(
            OpKind::Undelete,
            vec![mutation(a.id, MutationOp::Undelete)],
        ))
        .await
        .unwrap();
    assert_eq!(restored.result.revision, 4);
    assert_eq!(restored.items[0].derived.deleted_at, None);
    assert_eq!(titles(&db, false).await, ["a", "b"]);
}

fn parent(id: ItemId, parent: ItemId) -> Mutation {
    set(id, ItemField::ParentId, FieldValue::ItemId(parent))
}

/// Runs a batch that must fail on a parent reference: `(index, reason)`.
async fn parent_rejection(db: &Db, mutations: Vec<Mutation>) -> (usize, String) {
    let before = snapshot(db).await;
    let result = db.store.mutate(mutate(OpKind::Batch, mutations)).await;
    assert_eq!(snapshot(db).await, before);
    match result {
        Err(StoreError::Mutation {
            index,
            error:
                MutationError::Invalid {
                    field: ItemField::ParentId,
                    reason,
                },
        }) => (index, reason),
        other => panic!("expected a parent rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn a_parent_must_exist_be_live_and_not_create_a_cycle() {
    let db = open();
    let p = capture(&db, "p").await;
    let c = capture(&db, "c").await;
    let d = capture(&db, "d").await;
    let x = capture(&db, "x").await;
    let cycle = "would create a cycle".to_owned();

    assert_eq!(
        parent_rejection(&db, vec![parent(c.id, new_ulid())]).await,
        (0, "no such Item".into())
    );
    db.store
        .mutate(mutate(
            OpKind::Delete,
            vec![mutation(d.id, MutationOp::Delete)],
        ))
        .await
        .unwrap();
    let deleted = (0, "the parent is deleted".to_owned());
    assert_eq!(
        parent_rejection(&db, vec![parent(c.id, d.id)]).await,
        deleted
    );
    // Earlier mutations of the batch count: deleted, or undeleted.
    assert_eq!(
        parent_rejection(
            &db,
            vec![mutation(p.id, MutationOp::Delete), parent(c.id, p.id)]
        )
        .await,
        (1, deleted.1)
    );
    let out = db
        .store
        .mutate(mutate(
            OpKind::Batch,
            vec![mutation(d.id, MutationOp::Undelete), parent(c.id, d.id)],
        ))
        .await
        .unwrap();
    assert_eq!(out.items[1].core.parent_id, Some(d.id));

    // c under p, x under c: neither p under c nor p under x.
    db.store
        .mutate(mutate(
            OpKind::Batch,
            vec![parent(c.id, p.id), parent(x.id, c.id)],
        ))
        .await
        .unwrap();
    assert_eq!(
        parent_rejection(&db, vec![parent(p.id, c.id)]).await,
        (0, cycle.clone())
    );
    assert_eq!(
        parent_rejection(&db, vec![parent(p.id, x.id)]).await,
        (0, cycle.clone())
    );

    // A cycle formed within one batch.
    let y = capture(&db, "y").await;
    let z = capture(&db, "z").await;
    assert_eq!(
        parent_rejection(&db, vec![parent(y.id, z.id), parent(z.id, y.id)]).await,
        (1, cycle)
    );

    // A corrupt parent loop elsewhere fails the walk instead of spinning.
    let (m, n) = (capture(&db, "m").await, capture(&db, "n").await);
    let (m_id, n_id) = (m.id.to_string(), n.id.to_string());
    db.store
        .write(move |tx| {
            let sql = "UPDATE items SET parent_id = ?2 WHERE id = ?1";
            tx.conn().execute(sql, [&m_id, &n_id])?;
            tx.conn().execute(sql, [&n_id, &m_id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .mutate(mutate(OpKind::Update, vec![parent(y.id, m.id)]))
            .await,
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn person_references_wait_for_people() {
    let db = open();
    let a = capture(&db, "a").await;
    let person = PersonRef::Person(new_ulid());
    let waiting = |on| Waiting {
        on,
        reason: "quote".into(),
        since: date(2026, 2, 20),
        follow_up_on: None,
    };
    let unsupported = |result: Result<_, StoreError>, expected: ItemField| match result {
        Err(StoreError::Mutation {
            index: 0,
            error: MutationError::Unsupported { field, .. },
        }) => assert_eq!(field, expected),
        other => panic!("expected an unsupported {expected:?}, got {other:?}"),
    };
    // The error names the person's own field, as core does for waiting.on.
    for (field, value, named) in [
        (
            ItemField::Assignee,
            FieldValue::Person(person.clone()),
            ItemField::Assignee,
        ),
        (
            ItemField::Waiting,
            FieldValue::Waiting(waiting(person.clone())),
            ItemField::WaitingOn,
        ),
    ] {
        let req = mutate(OpKind::Update, vec![set(a.id, field, value)]);
        unsupported(db.store.mutate(req).await, named);
    }

    // Free text is stored, but its `on` cannot then become a person.
    let vendor = waiting(PersonRef::FreeText("the vendor".into()));
    let out = db
        .store
        .mutate(mutate(
            OpKind::Update,
            vec![set(
                a.id,
                ItemField::Waiting,
                FieldValue::Waiting(vendor.clone()),
            )],
        ))
        .await
        .unwrap();
    assert_eq!(out.items[0].overlay.waiting, Some(vendor.clone()));
    assert_eq!(
        out.items[0].derived.staleness.waiting_since,
        Some(ts(1_000))
    );
    let req = mutate(
        OpKind::Update,
        vec![set(a.id, ItemField::WaitingOn, FieldValue::Person(person))],
    );
    unsupported(db.store.mutate(req).await, ItemField::WaitingOn);
    let stored = db.store.get_item(a.id, today()).await.unwrap().item;
    assert_eq!(stored.overlay.waiting, Some(vendor));
    assert_eq!(db.store.current_revision(), 2);
}

#[tokio::test]
async fn an_estimate_must_fit_its_column() {
    let db = open();
    let a = capture(&db, "a").await;
    let estimate = |field, d| set(a.id, field, FieldValue::Duration(d));
    // Durations are stored as i64 nanoseconds (about 292 years); the proto
    // accepts up to 10,000 years.
    let before = snapshot(&db).await;
    let too_long = SignedDuration::from_hours(3_000_000);
    let batch = vec![
        title(a.id, "a2"),
        estimate(ItemField::OriginalEstimate, too_long),
    ];
    match db.store.mutate(mutate(OpKind::Batch, batch)).await {
        Err(StoreError::Mutation {
            index: 1,
            error:
                MutationError::Invalid {
                    field: ItemField::OriginalEstimate,
                    ..
                },
        }) => {}
        other => panic!("expected an invalid estimate, got {other:?}"),
    }
    assert_eq!(snapshot(&db).await, before);

    let longest = SignedDuration::from_nanos(i64::MAX);
    let out = db
        .store
        .mutate(mutate(
            OpKind::Update,
            vec![estimate(ItemField::RemainingEstimate, longest)],
        ))
        .await
        .unwrap();
    let stored = db.store.get_item(a.id, today()).await.unwrap().item;
    assert_eq!(stored.core.remaining_estimate, Some(longest));
    assert_eq!(stored, out.items[0]);
    let [event] = events(&db, out.result.op_id).await.try_into().unwrap();
    assert_eq!(event.change.new, FieldValue::Duration(longest));
}

async fn merge(db: &Db, from: &Item, into: ItemId) {
    let (from, into) = (from.id.to_string(), into.to_string());
    db.store
        .write(move |tx| {
            tx.conn().execute(
                "UPDATE items SET merged_into = ?2, deleted_at = 1 WHERE id = ?1",
                [from, into],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn get_item_reads_missing_deleted_and_merged_items() {
    let db = open();
    let missing = new_ulid();
    match db.store.get_item(missing, today()).await {
        Err(StoreError::NotFound(id)) => assert_eq!(id, missing),
        other => panic!("expected NotFound, got {other:?}"),
    }

    let a = capture(&db, "a").await;
    let b = capture(&db, "b").await;
    let c = capture(&db, "c").await;
    let got = db.store.get_item(c.id, today()).await.unwrap();
    assert_eq!(
        (got.item, got.revision, got.resolved_from),
        (c.clone(), 3, None)
    );

    // A soft-deleted Item is returned as is.
    db.store
        .mutate(mutate(
            OpKind::Delete,
            vec![mutation(b.id, MutationOp::Delete)],
        ))
        .await
        .unwrap();
    let got = db.store.get_item(b.id, today()).await.unwrap();
    assert_eq!(got.item.derived.deleted_at, Some(ts(1_000)));
    assert_eq!((got.revision, got.resolved_from), (4, None));

    // a merged into b, b into c: a resolves to c.
    merge(&db, &a, b.id).await;
    merge(&db, &b, c.id).await;
    let got = db.store.get_item(a.id, today()).await.unwrap();
    assert_eq!(got.item, c);
    assert_eq!((got.revision, got.resolved_from), (4, Some(a.id)));

    // A merge cycle or a dangling target is corrupt.
    merge(&db, &c, a.id).await;
    assert!(matches!(
        db.store.get_item(a.id, today()).await,
        Err(StoreError::Corrupt(_))
    ));
    let raw = Connection::open(&db.path).unwrap();
    raw.pragma_update(None, "foreign_keys", false).unwrap();
    raw.execute(
        "UPDATE items SET merged_into = ?1 WHERE id = ?2",
        [new_ulid().to_string(), c.id.to_string()],
    )
    .unwrap();
    assert!(matches!(
        db.store.get_item(a.id, today()).await,
        Err(StoreError::Corrupt(_))
    ));
}
