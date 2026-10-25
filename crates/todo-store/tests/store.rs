use std::collections::BTreeSet;
use std::path::PathBuf;

use jiff::civil::{Date, date};
use jiff::{SignedDuration, Timestamp};
use rusqlite::Connection;
use todo_core::{
    Actor, ArchiveReason, Archived, CaptureDraft, CreatedVia, FieldChange, FieldUpdate, FieldValue,
    Item, ItemField, ItemId, Mutation, MutationError, MutationOp, PersonRef, Priority, Relation,
    RelationKind, ResolutionKind, Revision, SkipReason, StalenessSignals, Status, StatusCategory,
    StatusFields, TodoRefinement, Topic, Waiting, new_ulid,
};
use todo_store::{
    CaptureInput, ChangesSinceError, EndProblem, Expired, ListQuery, MutateInput, MutateOutput,
    OpKind, RelationInput, RelationOutput, Resource, SCHEMA_VERSION, Store, StoreError, UndoInput,
    UndoOutput, UndoSkipRow, UndoState,
};
use tokio::sync::broadcast::error::TryRecvError;
use ulid::Ulid;

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
        "relations",
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
async fn a_spool_id_dedupes_after_the_request_id_is_forgotten() {
    let db = open();
    // The CLI sends its capture id as both keys.
    let id = new_ulid().to_string();
    let capture = CaptureInput {
        request_id: id.clone(),
        spool_id: Some(id.clone()),
        ..input("once", 1)
    };
    let first = db.store.capture(capture.clone()).await.unwrap();
    let rows = |sql: &'static str| {
        let id = id.clone();
        let store = db.store.clone();
        async move {
            store
                .read(move |c| Ok(c.query_row(sql, [id], |r| r.get::<_, i64>(0))?))
                .await
                .unwrap()
        }
    };
    let spooled = "SELECT count(*) FROM spool_imports WHERE spool_id = ?1";
    let requested = "SELECT count(*) FROM request_ids WHERE request_id = ?1";
    assert_eq!((rows(spooled).await, rows(requested).await), (1, 1));

    // Age the request id out of its 24h window; the next write prunes it.
    db.store
        .write(|tx| {
            tx.conn().execute(
                "UPDATE request_ids SET at = at - ?1",
                [25 * 60 * 60 * 1_000_000_000_i64],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let again = db.store.capture(capture).await.unwrap();
    assert_eq!(again, first);
    assert_eq!((rows(spooled).await, rows(requested).await), (1, 0));
    assert_eq!(titles(&db, false).await, ["once"]);
}

#[tokio::test]
async fn capture_under_a_missing_or_deleted_parent_fails() {
    let db = open();
    // Deleted through raw SQL: the store has no other way to make a tombstone
    // before mutations are set up.
    let gone = db.store.capture(input("gone", 1)).await.unwrap().0;
    set_columns(&db, &gone, "deleted_at = 1").await;
    let before = snapshot(&db).await;
    for (parent, reason) in [
        (new_ulid(), "no such Item"),
        (gone.id, "the parent is deleted"),
    ] {
        let mut child = input("child", 2);
        child.parent_id = Some(parent);
        match db.store.capture(child).await {
            Err(StoreError::Mutation {
                index: 0,
                error: MutationError::Invalid { field, reason: got },
            }) => {
                assert_eq!(field, ItemField::ParentId);
                assert_eq!(got, reason);
            }
            other => panic!("expected an invalid parent, got {other:?}"),
        }
    }
    assert_eq!(snapshot(&db).await, before);
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

/// The field events (`set` and `derived`) of an op, in op order, with
/// `old`/`new` decoded.
async fn events(db: &Db, op_id: ulid::Ulid) -> Vec<EventRow> {
    db.store
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT item_id, actor, kind, field, old, new FROM events
                 WHERE op_id = ?1 AND kind IN ('set', 'derived') ORDER BY ord",
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
            // The staleness signals before and after, last.
            EventRow {
                kind: "derived".into(),
                ..event(
                    ItemField::Staleness,
                    FieldValue::Staleness(item.derived.staleness),
                    FieldValue::Staleness(after.derived.staleness),
                )
            },
        ]
    );
    assert_eq!(
        after.derived.staleness,
        StalenessSignals {
            last_user_touch_at: Some(ts(1_000)),
            ..item.derived.staleness
        }
    );
    assert_eq!(change_rows(&db, 1).await, [(2, item.id.to_string(), false)]);
    // Nothing else: one op, four events, one change, one request id.
    assert_eq!(
        snapshot(&db).await.1,
        [ops + 1, events_n + 4, changes + 1, requests + 1]
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
    for (at, (actor, name, expected)) in (1_000..).zip([
        (Actor::User, "a", "user".to_owned()),
        (Actor::Sync, "b", "sync".to_owned()),
        (Actor::System, "c", "system".to_owned()),
        (
            Actor::Suggestion(suggestion),
            "d",
            format!("suggestion:{suggestion}"),
        ),
    ]) {
        let mut req = mutate(OpKind::Update, vec![title(item.id, name)]);
        req.actor = actor;
        req.now = ts(at);
        let out = db.store.mutate(req).await.unwrap();
        assert_eq!(op_row(&db, out.result.op_id).await.1, expected);
        let events = events(&db, out.result.op_id).await;
        // The user and suggestions also touch the staleness signals.
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        match actor {
            Actor::User | Actor::Suggestion(_) => assert_eq!(kinds, ["set", "derived"]),
            Actor::Sync | Actor::System => assert_eq!(kinds, ["set"]),
        }
        assert!(events.iter().all(|e| e.actor == expected));
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
    assert_eq!(
        fields,
        [
            ItemField::Title,
            ItemField::TodoRefinement,
            ItemField::Staleness
        ]
    );
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

    // The request id names an op on `a`; for another Item, existing or not,
    // it is a different request.
    let b = capture(&db, "b").await;
    let before = snapshot(&db).await;
    for other in [b.id, new_ulid()] {
        let mut retargeted = req.clone();
        retargeted.mutations[0].id = other;
        match db.store.mutate(retargeted).await {
            Err(StoreError::RequestIdReused) => {}
            other => panic!("expected RequestIdReused, got {other:?}"),
        }
    }
    assert_eq!(snapshot(&db).await, before);

    // A batch is checked by kind only: its request id records the first
    // mutation's Item.
    let batch = mutate(OpKind::Batch, vec![title(a.id, "a4"), title(b.id, "b2")]);
    let out = db.store.mutate(batch.clone()).await.unwrap();
    let mut reordered = batch;
    reordered.mutations.reverse();
    assert_eq!(db.store.mutate(reordered).await.unwrap().result, out.result);
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
    let [event, _derived] = events(&db, out.result.op_id).await.try_into().unwrap();
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

// ---- M2: the undo stack, parent rules and relations ------------------------

fn undo_input(at: i64) -> UndoInput {
    UndoInput {
        request_id: new_ulid().to_string(),
        expected_op_id: None,
        now: ts(at),
        today: today(),
    }
}

async fn undo(db: &Db) -> UndoOutput {
    db.store.undo(undo_input(2_000)).await.unwrap()
}

async fn redo(db: &Db) -> UndoOutput {
    db.store.redo(undo_input(3_000)).await.unwrap()
}

async fn get(db: &Db, id: ItemId) -> Item {
    db.store.get_item(id, today()).await.unwrap().item
}

/// Mutates as `actor` at second `at`.
async fn run(
    db: &Db,
    actor: Actor,
    at: i64,
    kind: OpKind,
    mutations: Vec<Mutation>,
) -> MutateOutput {
    let mut req = mutate(kind, mutations);
    req.actor = actor;
    req.now = ts(at);
    db.store.mutate(req).await.unwrap()
}

/// A user capture and its op.
async fn capture_op(db: &Db, title: &str) -> (Item, Ulid) {
    let (item, result) = db.store.capture(input(title, 1)).await.unwrap();
    (item, result.op_id)
}

async fn capture_under(db: &Db, title: &str, parent: ItemId) -> Item {
    let mut req = input(title, 1);
    req.parent_id = Some(parent);
    db.store.capture(req).await.unwrap().0
}

/// The entries undo and redo would act on.
async fn tops(db: &Db) -> (Option<Ulid>, Option<Ulid>) {
    let UndoState { undo, redo } = db.store.undo_state().await.unwrap();
    (undo.map(|e| e.op_id), redo.map(|e| e.op_id))
}

/// `(undo_state, summary)` of an `ops` row.
async fn op_stack(db: &Db, op_id: Ulid) -> (Option<String>, Option<String>) {
    db.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT undo_state, summary FROM ops WHERE id = ?1",
                [op_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
}

async fn undo_state_of(db: &Db, op_id: Ulid) -> Option<String> {
    op_stack(db, op_id).await.0
}

/// The Items whose `deleted_at` an op set, in op order.
async fn deleted_at_events(db: &Db, op_id: Ulid) -> Vec<String> {
    events(db, op_id)
        .await
        .into_iter()
        .filter(|e| e.change.field == ItemField::DeletedAt)
        .map(|e| e.item_id)
        .collect()
}

/// The parent invariant (design 3.7): no live Item has a deleted parent.
async fn assert_no_live_orphans(db: &Db) {
    let orphans: Vec<String> = db
        .store
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT child.id FROM items child JOIN items parent ON parent.id = child.parent_id
                 WHERE child.deleted_at IS NULL AND parent.deleted_at IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(
        orphans,
        Vec::<String>::new(),
        "live Items under deleted parents"
    );
}

fn skip(item_id: ItemId, fields: &[ItemField], reason: SkipReason) -> UndoSkipRow {
    UndoSkipRow {
        item_id,
        fields: fields.to_vec(),
        reason,
        relation: None,
    }
}

/// The Item without the write metadata that undo and redo move forward.
fn content(item: &Item) -> Item {
    Item {
        updated_at: Timestamp::UNIX_EPOCH,
        revision: 0,
        ..item.clone()
    }
}

fn update(id: ItemId, updates: &[(ItemField, FieldValue)]) -> Mutation {
    let updates = updates
        .iter()
        .map(|(field, value)| FieldUpdate {
            field: *field,
            value: value.clone(),
        })
        .collect();
    mutation(id, MutationOp::Update(updates))
}

fn status(id: ItemId, status: Status) -> Mutation {
    mutation(id, MutationOp::SetStatus(status))
}

fn high() -> FieldValue {
    FieldValue::Priority(Priority::High)
}

#[tokio::test]
async fn mutate_takes_only_mutations() {
    let db = open();
    let x = capture(&db, "x").await;
    let before = snapshot(&db).await;
    for req in [
        mutate(
            OpKind::Update,
            vec![mutation(x.id, MutationOp::Restore(vec![]))],
        ),
        mutate(OpKind::Undo, vec![title(x.id, "y")]),
        mutate(OpKind::Capture, vec![title(x.id, "y")]),
        mutate(OpKind::AddRelation, vec![title(x.id, "y")]),
    ] {
        match db.store.mutate(req).await {
            Err(StoreError::NotAMutation(_)) => {}
            other => panic!("expected NotAMutation, got {other:?}"),
        }
    }
    assert_eq!(snapshot(&db).await, before);
}

#[tokio::test]
async fn events_are_written_in_mutation_order_with_staleness_last() {
    let db = open();
    let x = capture(&db, "x").await;
    let y = capture(&db, "y").await;
    let out = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Batch,
        vec![
            title(x.id, "x2"),
            title(y.id, "y2"),
            set(x.id, ItemField::Priority, high()),
        ],
    )
    .await;
    let op_id = out.result.op_id.to_string();
    let rows: Vec<(String, String, u32, i64)> = db
        .store
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT item_id, kind, field, ord FROM events WHERE op_id = ?1 ORDER BY ord",
            )?;
            let rows = stmt.query_map([op_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    let row = |item: &Item, kind: &str, field: ItemField, ord| {
        (item.id.to_string(), kind.to_owned(), field.number(), ord)
    };
    assert_eq!(
        rows,
        [
            row(&x, "set", ItemField::Title, 0),
            row(&y, "set", ItemField::Title, 1),
            row(&x, "set", ItemField::Priority, 2),
            row(&x, "derived", ItemField::Staleness, 3),
            row(&y, "derived", ItemField::Staleness, 4),
        ]
    );
}

#[tokio::test]
async fn undo_restores_what_still_matches_and_reports_the_rest() {
    let db = open();
    let (x, captured) = capture_op(&db, "a").await;
    let edit = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![update(
            x.id,
            &[
                (ItemField::Title, FieldValue::String("b".into())),
                (ItemField::Priority, high()),
            ],
        )],
    )
    .await;
    assert_eq!(tops(&db).await, (Some(edit.result.op_id), None));
    // Off the stack: the title changes since.
    run(
        &db,
        Actor::System,
        1_500,
        OpKind::Update,
        vec![title(x.id, "c")],
    )
    .await;
    let revision = db.store.current_revision();

    let out = undo(&db).await;
    let after = get(&db, x.id).await;
    assert_eq!(after.core.title, "c");
    assert_eq!(after.core.priority, Priority::None);
    // Restored, not refreshed: an undo is no user touch.
    assert_eq!(after.derived.staleness, x.derived.staleness);
    assert_eq!(after.updated_at, ts(2_000));
    assert_eq!(after.revision, revision + 1);
    assert_eq!(out.result.revision, revision + 1);
    assert_eq!(out.items, std::slice::from_ref(&after));
    assert_eq!(
        out.skipped,
        [skip(x.id, &[ItemField::Title], SkipReason::ChangedSince)]
    );
    let entry = out.entry.unwrap();
    assert_eq!(entry.op_id, edit.result.op_id);
    assert_eq!(entry.kind, OpKind::Update);
    assert_eq!(entry.item_ids, [x.id]);
    assert_eq!(entry.description, "edit \"b\"");
    assert_eq!(op_row(&db, out.result.op_id).await.0, "undo");
    assert_eq!(
        op_stack(&db, out.result.op_id).await,
        (None, Some("undo: edit \"b\"".into()))
    );
    let kinds: Vec<(String, String)> = events(&db, out.result.op_id)
        .await
        .into_iter()
        .map(|e| (e.kind, e.actor))
        .collect();
    assert_eq!(
        kinds,
        [
            ("set".into(), "user".into()),
            ("derived".into(), "user".into())
        ]
    );
    assert_eq!(tops(&db).await, (Some(captured), Some(edit.result.op_id)));

    // The redo re-applies what the undo restored, and only that.
    let out = redo(&db).await;
    assert_eq!(out.skipped, []);
    let redone = get(&db, x.id).await;
    assert_eq!(
        (redone.core.title.as_str(), redone.core.priority),
        ("c", Priority::High)
    );
    assert_eq!(tops(&db).await, (Some(edit.result.op_id), None));
}

#[tokio::test]
async fn the_status_triple_is_restored_and_skipped_as_one_unit() {
    let db = open();
    // Done with the refinement kept at inbox, then moved to next: undoing by
    // SetStatus(done) would keep `next`; the recorded triple does not.
    let x = capture(&db, "x").await;
    run(
        &db,
        Actor::System,
        500,
        OpKind::SetStatus,
        vec![status(x.id, Status::Done)],
    )
    .await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::SetStatus,
        vec![status(x.id, Status::Next)],
    )
    .await;
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    let done = StatusFields {
        status_category: StatusCategory::Done,
        resolution_kind: Some(ResolutionKind::Done),
        todo_refinement: TodoRefinement::Inbox,
    };
    assert_eq!(get(&db, x.id).await.status_fields(), done);
    redo(&db).await;
    assert_eq!(get(&db, x.id).await.status(), Status::Next);

    // The resolution changed since: the whole triple stays, the rest of the
    // op is undone.
    let y = capture(&db, "y").await;
    let entry = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Batch,
        vec![
            status(y.id, Status::Done),
            set(y.id, ItemField::Priority, high()),
        ],
    )
    .await;
    run(
        &db,
        Actor::System,
        1_500,
        OpKind::SetStatus,
        vec![status(y.id, Status::Cancelled)],
    )
    .await;
    let out = undo(&db).await;
    assert_eq!(out.entry.unwrap().op_id, entry.result.op_id);
    assert_eq!(
        out.skipped,
        [skip(
            y.id,
            &[ItemField::StatusCategory, ItemField::ResolutionKind],
            SkipReason::ChangedSince
        )]
    );
    let y = get(&db, y.id).await;
    assert_eq!(y.status(), Status::Cancelled);
    assert_eq!(y.core.priority, Priority::None);
}

#[tokio::test]
async fn matching_is_by_value_exact_for_floats_and_unordered_for_labels() {
    let db = open();
    let x = capture(&db, "a").await;
    let labels = |l: &[&str]| FieldValue::StringSet(l.iter().map(|&s| s.to_owned()).collect());
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![update(
            x.id,
            &[
                (ItemField::Title, FieldValue::String("b".into())),
                (ItemField::StoryPoints, FieldValue::Number(0.1 + 0.2)),
                (ItemField::Labels, labels(&["x", "y"])),
            ],
        )],
    )
    .await;
    // Off the stack: the title changes and changes back (ABA), the labels
    // are reordered.
    run(
        &db,
        Actor::System,
        1_100,
        OpKind::Update,
        vec![title(x.id, "c")],
    )
    .await;
    run(
        &db,
        Actor::System,
        1_200,
        OpKind::Update,
        vec![update(
            x.id,
            &[
                (ItemField::Title, FieldValue::String("b".into())),
                (ItemField::Labels, labels(&["y", "x"])),
            ],
        )],
    )
    .await;
    assert_eq!(get(&db, x.id).await.core.labels, ["y", "x"]);
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    let back = get(&db, x.id).await;
    assert_eq!(back.core.title, "a");
    assert_eq!(back.core.story_points, None);
    assert_eq!(back.core.labels, Vec::<String>::new());
}

#[tokio::test]
async fn undoing_a_capture_soft_deletes_it_and_replays_do_not_recreate_it() {
    let db = open();
    let mut req = input("call dentist", 100);
    req.spool_id = Some("spool-1".into());
    let (item, captured) = db.store.capture(req.clone()).await.unwrap();
    assert_eq!(tops(&db).await, (Some(captured.op_id), None));

    let out = undo(&db).await;
    let entry = out.entry.unwrap();
    assert_eq!(
        (entry.op_id, entry.kind, entry.description.as_str()),
        (captured.op_id, OpKind::Capture, "capture \"call dentist\"")
    );
    assert_eq!(entry.item_ids, [item.id]);
    let [deleted] = <[Item; 1]>::try_from(out.items).unwrap();
    assert_eq!(deleted.id, item.id);
    assert_eq!(deleted.derived.deleted_at, Some(ts(2_000)));
    assert_eq!(titles(&db, false).await, Vec::<String>::new());

    // A late retry of the capture, by request id or by spool id, answers
    // with the tombstone instead of capturing again.
    assert_eq!(
        db.store.capture(req.clone()).await.unwrap(),
        (deleted.clone(), captured)
    );
    req.request_id = new_ulid().to_string();
    assert_eq!(
        db.store.capture(req).await.unwrap(),
        (deleted.clone(), captured)
    );
    assert_eq!(titles(&db, true).await, ["call dentist"]);

    // Redo undeletes the same Item.
    let out = redo(&db).await;
    let [back] = <[Item; 1]>::try_from(out.items).unwrap();
    assert_eq!(content(&back), content(&item));
    assert_eq!(tops(&db).await, (Some(captured.op_id), None));
}

#[tokio::test]
async fn restores_write_the_recorded_archive_and_deletion_times() {
    let db = open();
    let x = capture(&db, "x").await;
    set_columns(
        &db,
        &x,
        "archived_at = 500000000000, archived_reason = 'auto_done'",
    )
    .await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Unarchive,
        vec![mutation(x.id, MutationOp::Unarchive)],
    )
    .await;
    undo(&db).await;
    assert_eq!(
        get(&db, x.id).await.overlay.archived,
        Some(Archived {
            at: ts(500),
            reason: ArchiveReason::AutoDone
        })
    );

    let y = capture(&db, "y").await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Delete,
        vec![mutation(y.id, MutationOp::Delete)],
    )
    .await;
    undo(&db).await;
    assert!(!get(&db, y.id).await.is_deleted());
    redo(&db).await;
    assert_eq!(get(&db, y.id).await.derived.deleted_at, Some(ts(1_000)));
}

#[tokio::test]
async fn undo_of_an_edit_then_a_delete_restores_both() {
    let db = open();
    let x = capture(&db, "a").await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Batch,
        vec![title(x.id, "b"), mutation(x.id, MutationOp::Delete)],
    )
    .await;
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    assert_eq!(content(&get(&db, x.id).await), content(&x));
}

#[tokio::test]
async fn undo_of_a_reparenting_batch_runs_in_reverse() {
    let db = open();
    let p = capture(&db, "p").await;
    let c = capture(&db, "c").await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![parent(p.id, c.id)],
    )
    .await;
    // Swap: p leaves c, then c goes under p. Undone in this order, p would
    // go back under c while c is still under p.
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Batch,
        vec![
            set(p.id, ItemField::ParentId, FieldValue::Cleared),
            parent(c.id, p.id),
        ],
    )
    .await;
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    assert_eq!(get(&db, p.id).await.core.parent_id, Some(c.id));
    assert_eq!(get(&db, c.id).await.core.parent_id, None);
    let out = redo(&db).await;
    assert_eq!(out.skipped, []);
    assert_eq!(get(&db, p.id).await.core.parent_id, None);
    assert_eq!(get(&db, c.id).await.core.parent_id, Some(p.id));
    assert_no_live_orphans(&db).await;
}

#[tokio::test]
async fn an_op_that_changes_nothing_is_no_entry_and_keeps_redo() {
    let db = open();
    let (x, captured) = capture_op(&db, "a").await;
    let edit = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![title(x.id, "b")],
    )
    .await;
    undo(&db).await;
    assert_eq!(tops(&db).await, (Some(captured), Some(edit.result.op_id)));

    let noop = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Unarchive,
        vec![mutation(x.id, MutationOp::Unarchive)],
    )
    .await;
    assert_eq!(undo_state_of(&db, noop.result.op_id).await, None);
    assert_eq!(tops(&db).await, (Some(captured), Some(edit.result.op_id)));
    redo(&db).await;
    assert_eq!(get(&db, x.id).await.core.title, "b");
}

#[tokio::test]
async fn undo_of_an_edit_to_a_since_deleted_item_drops_the_entry() {
    let db = open();
    let (x, captured) = capture_op(&db, "a").await;
    let edit = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![title(x.id, "b")],
    )
    .await;
    run(
        &db,
        Actor::System,
        1_500,
        OpKind::Delete,
        vec![mutation(x.id, MutationOp::Delete)],
    )
    .await;
    let (revision, [ops, events_n, changes, requests], items) = snapshot(&db).await;

    let out = undo(&db).await;
    assert_eq!(out.entry.map(|e| e.op_id), Some(edit.result.op_id));
    assert_eq!(out.items, []);
    assert_eq!(
        out.skipped,
        [skip(x.id, &[ItemField::Title], SkipReason::ItemDeleted)]
    );
    assert_eq!(out.result.revision, revision);
    // Only the undo op and its request id are recorded; no Item is written.
    assert_eq!(
        snapshot(&db).await,
        (revision, [ops + 1, events_n, changes, requests + 1], items)
    );
    assert_eq!(
        undo_state_of(&db, edit.result.op_id).await.as_deref(),
        Some("dropped")
    );
    // Off both stacks.
    assert_eq!(tops(&db).await, (Some(captured), None));
}

#[tokio::test]
async fn a_new_entry_clears_redo_and_off_stack_ops_do_not() {
    let db = open();
    let x = capture(&db, "a").await;
    let y = capture(&db, "y").await;
    let edit = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![title(x.id, "b")],
    )
    .await;
    undo(&db).await;

    // A system op on another field, and a system capture, leave redo alone.
    run(
        &db,
        Actor::System,
        1_500,
        OpKind::Update,
        vec![set(x.id, ItemField::Priority, high())],
    )
    .await;
    let mut restored = input("restored", 1);
    restored.created_via = CreatedVia::Restore;
    db.store.capture(restored).await.unwrap();
    assert_eq!(tops(&db).await.1, Some(edit.result.op_id));
    let out = redo(&db).await;
    assert_eq!(out.skipped, []);
    let x2 = get(&db, x.id).await;
    assert_eq!(
        (x2.core.title.as_str(), x2.core.priority),
        ("b", Priority::High)
    );

    // A new user op clears it.
    undo(&db).await;
    let other = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![title(y.id, "y2")],
    )
    .await;
    assert_eq!(tops(&db).await, (Some(other.result.op_id), None));
    assert_eq!(
        undo_state_of(&db, edit.result.op_id).await.as_deref(),
        Some("discarded")
    );
    let out = redo(&db).await;
    assert_eq!(out.entry, None);
    assert_eq!(get(&db, x.id).await.core.title, "a");
}

#[tokio::test]
async fn a_partial_redo_restores_what_still_matches() {
    let db = open();
    let x = capture(&db, "a").await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![update(
            x.id,
            &[
                (ItemField::Title, FieldValue::String("b".into())),
                (ItemField::Priority, high()),
            ],
        )],
    )
    .await;
    undo(&db).await;
    run(
        &db,
        Actor::System,
        2_500,
        OpKind::Update,
        vec![title(x.id, "c")],
    )
    .await;
    let out = redo(&db).await;
    assert_eq!(
        out.skipped,
        [skip(x.id, &[ItemField::Title], SkipReason::ChangedSince)]
    );
    let redone = get(&db, x.id).await;
    assert_eq!(
        (redone.core.title.as_str(), redone.core.priority),
        ("c", Priority::High)
    );
    assert_eq!(redone.derived.staleness.last_user_touch_at, Some(ts(1_000)));
    // The next undo reverts what the redo did, and only that.
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    let undone = get(&db, x.id).await;
    assert_eq!(
        (undone.core.title.as_str(), undone.core.priority),
        ("c", Priority::None)
    );
}

fn reopen(db: Db) -> Db {
    let Db { _dir, path, store } = db;
    drop(store);
    let store = Store::open(&path).unwrap();
    Db { _dir, path, store }
}

#[tokio::test]
async fn the_stacks_survive_reopening_the_store() {
    let mut db = open();
    let (x, captured) = capture_op(&db, "a").await;
    let first = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![title(x.id, "b")],
    )
    .await;
    let second = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![title(x.id, "c")],
    )
    .await;
    db = reopen(db);
    assert_eq!(undo(&db).await.entry.unwrap().op_id, second.result.op_id);
    db = reopen(db);
    assert_eq!(undo(&db).await.entry.unwrap().op_id, first.result.op_id);
    db = reopen(db);
    assert_eq!(get(&db, x.id).await.core.title, "a");
    assert_eq!(tops(&db).await, (Some(captured), Some(first.result.op_id)));
    redo(&db).await;
    db = reopen(db);
    redo(&db).await;
    assert_eq!(get(&db, x.id).await.core.title, "c");
    assert_eq!(tops(&db).await, (Some(second.result.op_id), None));
}

#[tokio::test]
async fn the_stack_is_lifo_and_undo_and_redo_are_not_entries() {
    let db = open();
    let (x, captured) = capture_op(&db, "a").await;
    let ops = [
        run(
            &db,
            Actor::User,
            1_000,
            OpKind::Update,
            vec![title(x.id, "b")],
        )
        .await,
        run(
            &db,
            Actor::Suggestion(new_ulid()),
            1_000,
            OpKind::Update,
            vec![set(x.id, ItemField::Priority, high())],
        )
        .await,
        run(
            &db,
            Actor::User,
            1_000,
            OpKind::Archive,
            vec![mutation(x.id, MutationOp::Archive)],
        )
        .await,
    ]
    .map(|out| out.result.op_id);
    for &op in ops.iter().rev() {
        assert_eq!(undo(&db).await.entry.unwrap().op_id, op);
    }
    assert_eq!(tops(&db).await, (Some(captured), Some(ops[0])));
    for &op in &ops[..2] {
        assert_eq!(redo(&db).await.entry.unwrap().op_id, op);
    }
    assert_eq!(tops(&db).await, (Some(ops[1]), Some(ops[2])));
    let x2 = get(&db, x.id).await;
    assert_eq!(
        (x2.core.title.as_str(), x2.core.priority),
        ("b", Priority::High)
    );
    assert_eq!(x2.overlay.archived, None);
    let entries: i64 = db
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM ops WHERE undo_state IS NOT NULL",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(entries, 4);
}

#[tokio::test]
async fn the_expected_entry_guards_undo_and_redo() {
    let db = open();
    let guarded = |expected| UndoInput {
        expected_op_id: Some(expected),
        ..undo_input(2_000)
    };
    let changed = |result: Result<UndoOutput, StoreError>| match result {
        Err(StoreError::UndoStackChanged { expected, top }) => (expected, top),
        other => panic!("expected UndoStackChanged, got {other:?}"),
    };
    // Empty stacks.
    let stray = new_ulid();
    assert_eq!(changed(db.store.undo(guarded(stray)).await), (stray, None));
    assert_eq!(changed(db.store.redo(guarded(stray)).await), (stray, None));

    let (x, captured) = capture_op(&db, "a").await;
    let edit = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![title(x.id, "b")],
    )
    .await;
    let edit = edit.result.op_id;
    let before = snapshot(&db).await;
    assert_eq!(
        changed(db.store.undo(guarded(captured)).await),
        (captured, Some(edit))
    );
    assert_eq!(snapshot(&db).await, before);
    let out = db.store.undo(guarded(edit)).await.unwrap();
    assert_eq!(out.entry.unwrap().op_id, edit);

    assert_eq!(
        changed(db.store.redo(guarded(captured)).await),
        (captured, Some(edit))
    );
    db.store.redo(guarded(edit)).await.unwrap();
    assert_eq!(get(&db, x.id).await.core.title, "b");
}

#[tokio::test]
async fn undo_and_redo_round_trip() {
    let db = open();
    let x = capture(&db, "a").await;
    let waiting = Waiting {
        on: PersonRef::FreeText("the vendor".into()),
        reason: "quote".into(),
        since: date(2026, 2, 20),
        follow_up_on: None,
    };
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Batch,
        vec![
            update(
                x.id,
                &[
                    (ItemField::Title, FieldValue::String("b".into())),
                    (
                        ItemField::Labels,
                        FieldValue::StringSet(vec!["l1".into(), "l2".into()]),
                    ),
                    (ItemField::StoryPoints, FieldValue::Number(2.5)),
                    (ItemField::Waiting, FieldValue::Waiting(waiting)),
                ],
            ),
            status(x.id, Status::Next),
            status(x.id, Status::Done),
            set(
                x.id,
                ItemField::WaitingReason,
                FieldValue::String("contract".into()),
            ),
            mutation(x.id, MutationOp::Archive),
        ],
    )
    .await;
    let done = get(&db, x.id).await;
    assert_eq!(done.status(), Status::Done);
    for round in 0..2 {
        let out = db.store.undo(undo_input(2_000 + round)).await.unwrap();
        assert_eq!(out.skipped, []);
        assert_eq!(content(&get(&db, x.id).await), content(&x), "round {round}");
        let out = db.store.redo(undo_input(3_000 + round)).await.unwrap();
        assert_eq!(out.skipped, []);
        assert_eq!(
            content(&get(&db, x.id).await),
            content(&done),
            "round {round}"
        );
    }
}

#[tokio::test]
async fn undo_and_redo_are_idempotent_on_their_request_id() {
    let db = open();
    let x = capture(&db, "a").await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![update(
            x.id,
            &[
                (ItemField::Title, FieldValue::String("b".into())),
                (ItemField::Priority, high()),
            ],
        )],
    )
    .await;
    run(
        &db,
        Actor::System,
        1_500,
        OpKind::Update,
        vec![title(x.id, "c")],
    )
    .await;
    let req = undo_input(2_000);
    let first = db.store.undo(req.clone()).await.unwrap();
    assert_eq!(first.skipped.len(), 1);
    let before = snapshot(&db).await;
    let tops_before = tops(&db).await;
    // The same response, without undoing the next entry.
    assert_eq!(db.store.undo(req.clone()).await.unwrap(), first);
    assert_eq!(snapshot(&db).await, before);
    assert_eq!(tops(&db).await, tops_before);
    // Undo and redo share the request id namespace.
    match db.store.redo(req.clone()).await {
        Err(StoreError::RequestIdReused) => {}
        other => panic!("expected RequestIdReused, got {other:?}"),
    }
    let reused = MutateInput {
        request_id: req.request_id.clone(),
        ..mutate(OpKind::Update, vec![title(x.id, "d")])
    };
    match db.store.mutate(reused).await {
        Err(StoreError::RequestIdReused) => {}
        other => panic!("expected RequestIdReused, got {other:?}"),
    }
    assert_eq!(snapshot(&db).await, before);

    // An undo of an empty stack is recorded too: retried after a new op, it
    // still undoes nothing.
    let db = open();
    let empty = undo_input(2_000);
    let out = db.store.undo(empty.clone()).await.unwrap();
    assert_eq!((out.entry.clone(), out.items.len()), (None, 0));
    assert_eq!(
        op_stack(&db, out.result.op_id).await.1.as_deref(),
        Some("nothing to undo")
    );
    let (y, captured) = capture_op(&db, "y").await;
    assert_eq!(db.store.undo(empty).await.unwrap(), out);
    assert!(!get(&db, y.id).await.is_deleted());
    assert_eq!(tops(&db).await, (Some(captured), None));
}

#[tokio::test]
async fn undo_restores_staleness_only_with_its_content() {
    let db = open();
    let x = db.store.capture(input("x", 100)).await.unwrap().0;
    run(
        &db,
        Actor::User,
        200,
        OpKind::SetStatus,
        vec![status(x.id, Status::Next)],
    )
    .await;
    let moved = get(&db, x.id).await;
    assert_eq!(moved.derived.staleness.status_entered_at, Some(ts(200)));
    db.store.undo(undo_input(300)).await.unwrap();
    let back = get(&db, x.id).await;
    assert_eq!(back.derived.staleness, x.derived.staleness);
    assert_eq!(back.derived.staleness.status_entered_at, Some(ts(100)));
    assert_eq!(back.updated_at, ts(300));
    db.store.redo(undo_input(400)).await.unwrap();
    assert_eq!(
        get(&db, x.id).await.derived.staleness,
        moved.derived.staleness
    );

    // The status unit is skipped, so status_entered_at stays, even though it
    // still holds the op's value (the system op ran at the same instant);
    // the touch follows the restored priority.
    let y = db.store.capture(input("y", 100)).await.unwrap().0;
    run(
        &db,
        Actor::User,
        200,
        OpKind::Batch,
        vec![
            status(y.id, Status::Done),
            set(y.id, ItemField::Priority, high()),
        ],
    )
    .await;
    run(
        &db,
        Actor::System,
        200,
        OpKind::SetStatus,
        vec![status(y.id, Status::Cancelled)],
    )
    .await;
    let out = db.store.undo(undo_input(300)).await.unwrap();
    assert_eq!(out.skipped.len(), 1);
    let y2 = get(&db, y.id).await;
    assert_eq!(y2.core.priority, Priority::None);
    assert_eq!(y2.derived.staleness.status_entered_at, Some(ts(200)));
    assert_eq!(y2.derived.staleness.last_user_touch_at, Some(ts(100)));
}

#[tokio::test]
async fn undo_restores_the_staleness_of_items_it_deletes() {
    // Delete, then undelete, with a subtask the delete took along. Undoing
    // the undelete deletes both again with the touches they had as
    // tombstones, so undoing the delete then finds its own touches and
    // restores the ones from before: nothing keeps a touch of an undone op.
    let db = open();
    let p = db.store.capture(input("p", 100)).await.unwrap().0;
    let mut req = input("c", 100);
    req.parent_id = Some(p.id);
    let c = db.store.capture(req).await.unwrap().0;
    let delete = vec![mutation(p.id, MutationOp::Delete)];
    run(&db, Actor::User, 200, OpKind::Delete, delete).await;
    let deleted = [get(&db, p.id).await, get(&db, c.id).await];
    let undelete = vec![mutation(p.id, MutationOp::Undelete)];
    run(&db, Actor::User, 300, OpKind::Undelete, undelete).await;
    let undeleted = [get(&db, p.id).await, get(&db, c.id).await];
    assert_eq!(
        undeleted[1].derived.staleness.last_user_touch_at,
        Some(ts(300))
    );
    let touches = |items: &[Item]| -> Vec<StalenessSignals> {
        items.iter().map(|i| i.derived.staleness).collect()
    };

    db.store.undo(undo_input(400)).await.unwrap();
    let now = [get(&db, p.id).await, get(&db, c.id).await];
    assert!(now.iter().all(Item::is_deleted));
    assert_eq!(touches(&now), touches(&deleted));
    let out = db.store.undo(undo_input(500)).await.unwrap();
    assert_eq!(out.skipped, []);
    let now = [get(&db, p.id).await, get(&db, c.id).await];
    assert_eq!(touches(&now), touches(&[p.clone(), c.clone()]));
    assert_no_live_orphans(&db).await;

    db.store.redo(undo_input(600)).await.unwrap();
    let now = [get(&db, p.id).await, get(&db, c.id).await];
    assert_eq!(touches(&now), touches(&deleted));
    let out = db.store.redo(undo_input(700)).await.unwrap();
    assert_eq!(out.skipped, []);
    let now = [get(&db, p.id).await, get(&db, c.id).await];
    assert_eq!(touches(&now), touches(&undeleted));
    assert_no_live_orphans(&db).await;
}

#[tokio::test]
async fn delete_cascades_to_live_descendants_and_undelete_brings_them_back() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture_under(&db, "b", a.id).await;
    let c = capture_under(&db, "c", b.id).await;
    let e = capture_under(&db, "e", b.id).await;
    let d = capture(&db, "d").await;
    run(
        &db,
        Actor::User,
        500,
        OpKind::Delete,
        vec![mutation(e.id, MutationOp::Delete)],
    )
    .await;
    let since = db.store.current_revision();

    let delete = run(
        &db,
        Actor::User,
        1_000,
        OpKind::Delete,
        vec![mutation(a.id, MutationOp::Delete)],
    )
    .await;
    assert_eq!(delete.items.len(), 1);
    for id in [a.id, b.id, c.id] {
        assert_eq!(get(&db, id).await.derived.deleted_at, Some(ts(1_000)));
    }
    assert_eq!(get(&db, e.id).await.derived.deleted_at, Some(ts(500)));
    assert!(!get(&db, d.id).await.is_deleted());
    // Deepest first, then the Item; a revision and change row each.
    let ids = |items: &[&Item]| items.iter().map(|i| i.id.to_string()).collect::<Vec<_>>();
    assert_eq!(
        deleted_at_events(&db, delete.result.op_id).await,
        ids(&[&c, &b, &a])
    );
    let mut changed: Vec<String> = change_rows(&db, since as i64)
        .await
        .into_iter()
        .map(|(_, id, _)| id)
        .collect();
    changed.sort();
    let mut want = ids(&[&a, &b, &c]);
    want.sort();
    assert_eq!(changed, want);
    assert_eq!(
        op_stack(&db, delete.result.op_id).await,
        (
            Some("done".into()),
            Some("delete \"a\" and 2 subtasks".into())
        )
    );
    assert_no_live_orphans(&db).await;

    // Undo undeletes top-down; e stays deleted.
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    assert_eq!(
        deleted_at_events(&db, out.result.op_id).await,
        ids(&[&a, &b, &c])
    );
    assert_eq!(
        out.items.iter().map(|i| i.id).collect::<Vec<_>>(),
        [a.id, b.id, c.id]
    );
    for id in [a.id, b.id, c.id] {
        assert!(!get(&db, id).await.is_deleted());
    }
    assert!(get(&db, e.id).await.is_deleted());
    assert_no_live_orphans(&db).await;

    // Redo deletes bottom-up, with the original deleted_at.
    let out = redo(&db).await;
    assert_eq!(out.skipped, []);
    assert_eq!(
        deleted_at_events(&db, out.result.op_id).await,
        ids(&[&c, &b, &a])
    );
    for id in [a.id, b.id, c.id] {
        assert_eq!(get(&db, id).await.derived.deleted_at, Some(ts(1_000)));
    }
    assert_no_live_orphans(&db).await;

    // UndeleteItem restores what the Item's delete took, and only that.
    let undelete = run(
        &db,
        Actor::User,
        4_000,
        OpKind::Undelete,
        vec![mutation(a.id, MutationOp::Undelete)],
    )
    .await;
    assert_eq!(
        deleted_at_events(&db, undelete.result.op_id).await,
        ids(&[&a, &b, &c])
    );
    for id in [a.id, b.id, c.id] {
        assert!(!get(&db, id).await.is_deleted());
    }
    assert!(get(&db, e.id).await.is_deleted());
    assert_eq!(
        op_stack(&db, undelete.result.op_id).await.1.as_deref(),
        Some("undelete \"a\" and 2 subtasks")
    );
    assert_no_live_orphans(&db).await;

    // A child cannot come back while its parent is deleted.
    run(
        &db,
        Actor::User,
        5_000,
        OpKind::Delete,
        vec![mutation(a.id, MutationOp::Delete)],
    )
    .await;
    let before = snapshot(&db).await;
    for (child, parent) in [(c.id, b.id), (e.id, b.id)] {
        let req = mutate(
            OpKind::Undelete,
            vec![mutation(child, MutationOp::Undelete)],
        );
        match db.store.mutate(req).await {
            Err(StoreError::ParentDeleted {
                index,
                item_id,
                parent_id,
            }) => {
                assert_eq!((index, item_id, parent_id), (0, child, parent));
            }
            other => panic!("expected ParentDeleted, got {other:?}"),
        }
    }
    assert_eq!(snapshot(&db).await, before);
    assert_no_live_orphans(&db).await;
}

#[tokio::test]
async fn the_cascade_follows_the_batch_working_copies() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture_under(&db, "b", a.id).await;
    let x = capture(&db, "x").await;
    // x moves under a and goes with it; b moves out and stays.
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Batch,
        vec![
            parent(x.id, a.id),
            set(b.id, ItemField::ParentId, FieldValue::Cleared),
            mutation(a.id, MutationOp::Delete),
        ],
    )
    .await;
    assert!(get(&db, x.id).await.is_deleted());
    assert!(!get(&db, b.id).await.is_deleted());
    assert_no_live_orphans(&db).await;

    // Deleted and undeleted in one batch: the cascade comes back with it.
    run(
        &db,
        Actor::User,
        2_000,
        OpKind::Undelete,
        vec![mutation(a.id, MutationOp::Undelete)],
    )
    .await;
    assert!(!get(&db, x.id).await.is_deleted());
    run(
        &db,
        Actor::User,
        3_000,
        OpKind::Batch,
        vec![
            mutation(a.id, MutationOp::Delete),
            mutation(a.id, MutationOp::Undelete),
        ],
    )
    .await;
    assert!(!get(&db, x.id).await.is_deleted());
    // A child undeleted after its parent was deleted in the same batch.
    let before = snapshot(&db).await;
    let req = mutate(
        OpKind::Batch,
        vec![
            mutation(a.id, MutationOp::Delete),
            mutation(x.id, MutationOp::Undelete),
        ],
    );
    match db.store.mutate(req).await {
        Err(StoreError::ParentDeleted {
            index,
            item_id,
            parent_id,
        }) => {
            assert_eq!((index, item_id, parent_id), (1, x.id, a.id));
        }
        other => panic!("expected ParentDeleted, got {other:?}"),
    }
    assert_eq!(snapshot(&db).await, before);
    assert_no_live_orphans(&db).await;
}

#[tokio::test]
async fn a_batch_checks_the_parent_rule_once_it_is_done() {
    let db = open();
    let p = capture(&db, "p").await;
    let c = capture_under(&db, "c", p.id).await;
    let g = capture_under(&db, "g", c.id).await;
    let q = capture_under(&db, "q", p.id).await;
    let all = [p.id, c.id, g.id, q.id];
    let delete_p = |at| {
        run(
            &db,
            Actor::User,
            at,
            OpKind::Delete,
            vec![mutation(p.id, MutationOp::Delete)],
        )
    };
    let undelete = |id| mutation(id, MutationOp::Undelete);
    let parent_deleted = |result: Result<MutateOutput, StoreError>| match result {
        Err(StoreError::ParentDeleted {
            index,
            item_id,
            parent_id,
        }) => (index, item_id, parent_id),
        other => panic!("expected ParentDeleted, got {other:?}"),
    };

    // The child first or the parent first: either way all four come back.
    for (n, order) in [[c.id, p.id], [p.id, c.id], [g.id, p.id]]
        .into_iter()
        .enumerate()
    {
        delete_p(1_000 + n as i64).await;
        for &id in &all {
            assert!(get(&db, id).await.is_deleted());
        }
        let out = run(
            &db,
            Actor::User,
            2_000,
            OpKind::Batch,
            order.iter().map(|&id| undelete(id)).collect(),
        )
        .await;
        for &id in &all {
            assert!(!get(&db, id).await.is_deleted(), "{order:?}");
        }
        assert_eq!(
            op_stack(&db, out.result.op_id).await.1.as_deref(),
            Some("undelete 2 items and 2 subtasks"),
            "{order:?}"
        );
        assert_no_live_orphans(&db).await;
    }

    // What counts is where the batch leaves an undeleted Item.
    delete_p(5_000).await;
    run(
        &db,
        Actor::User,
        6_000,
        OpKind::Batch,
        vec![
            undelete(c.id),
            set(c.id, ItemField::ParentId, FieldValue::Cleared),
        ],
    )
    .await;
    assert!(!get(&db, c.id).await.is_deleted());
    assert!(!get(&db, g.id).await.is_deleted());
    assert!(get(&db, p.id).await.is_deleted());
    assert_no_live_orphans(&db).await;

    // Still under a deleted parent when the batch ends: the last mutation
    // that undeleted it is named, and nothing is written.
    let p2 = capture(&db, "p2").await;
    let c2 = capture_under(&db, "c2", p2.id).await;
    let g2 = capture_under(&db, "g2", c2.id).await;
    run(
        &db,
        Actor::User,
        7_000,
        OpKind::Delete,
        vec![mutation(p2.id, MutationOp::Delete)],
    )
    .await;
    let before = snapshot(&db).await;
    let rows = [
        (vec![undelete(c2.id)], (0, c2.id, p2.id)),
        // Deleting a deleted Item changes nothing, so cascades nothing.
        (
            vec![undelete(c2.id), mutation(p2.id, MutationOp::Delete)],
            (0, c2.id, p2.id),
        ),
        (vec![undelete(g2.id), undelete(c2.id)], (1, c2.id, p2.id)),
        (
            vec![
                undelete(c2.id),
                mutation(c2.id, MutationOp::Delete),
                undelete(c2.id),
            ],
            (2, c2.id, p2.id),
        ),
    ];
    for (mutations, want) in rows {
        let req = mutate(OpKind::Batch, mutations);
        assert_eq!(parent_deleted(db.store.mutate(req).await), want);
        assert_eq!(snapshot(&db).await, before);
    }
    assert_no_live_orphans(&db).await;
}

#[tokio::test]
async fn a_batch_that_undeletes_a_child_first_undoes_and_redoes_whole() {
    let db = open();
    let p = capture(&db, "p").await;
    let c = capture_under(&db, "c", p.id).await;
    let g = capture_under(&db, "g", c.id).await;
    let q = capture_under(&db, "q", p.id).await;
    let all = [p.id, c.id, g.id, q.id];
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Delete,
        vec![mutation(p.id, MutationOp::Delete)],
    )
    .await;
    let mut deleted = Vec::new();
    for id in all {
        deleted.push(content(&get(&db, id).await));
    }
    let batch = run(
        &db,
        Actor::User,
        2_000,
        OpKind::Batch,
        vec![
            mutation(c.id, MutationOp::Undelete),
            title(c.id, "c2"),
            mutation(p.id, MutationOp::Undelete),
        ],
    )
    .await;
    let mut live = Vec::new();
    for id in all {
        live.push(content(&get(&db, id).await));
    }

    // Undo runs in reverse, through the moment the parent is deleted and
    // its subtask is live, as the batch did.
    let out = undo(&db).await;
    assert_eq!(out.entry.unwrap().op_id, batch.result.op_id);
    assert_eq!(out.skipped, []);
    for (id, want) in all.iter().zip(&deleted) {
        assert_eq!(&content(&get(&db, *id).await), want);
    }
    assert_no_live_orphans(&db).await;
    let out = redo(&db).await;
    assert_eq!(out.skipped, []);
    for (id, want) in all.iter().zip(&live) {
        assert_eq!(&content(&get(&db, *id).await), want);
    }
    assert_no_live_orphans(&db).await;
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    assert!(get(&db, p.id).await.is_deleted());
    assert_no_live_orphans(&db).await;
}

#[tokio::test]
async fn undo_never_leaves_a_live_item_under_a_deleted_parent() {
    let db = open();
    // Undoing the capture would delete p, whose child arrived off the stack.
    let (p, captured) = capture_op(&db, "p").await;
    let mut child = input("c", 2);
    child.parent_id = Some(p.id);
    child.created_via = CreatedVia::Restore;
    db.store.capture(child).await.unwrap();
    assert_eq!(tops(&db).await, (Some(captured), None));
    let out = undo(&db).await;
    let invalid = |reason: &str| SkipReason::Invalid(reason.into());
    assert_eq!(
        out.skipped,
        [skip(
            p.id,
            &[ItemField::DeletedAt],
            invalid("the Item has live subtasks")
        )]
    );
    assert_eq!(
        undo_state_of(&db, captured).await.as_deref(),
        Some("dropped")
    );
    assert!(!get(&db, p.id).await.is_deleted());
    assert_no_live_orphans(&db).await;

    // Undoing a delete under a parent deleted since.
    let q = capture(&db, "q").await;
    let r = capture_under(&db, "r", q.id).await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Delete,
        vec![mutation(r.id, MutationOp::Delete)],
    )
    .await;
    run(
        &db,
        Actor::System,
        1_500,
        OpKind::Delete,
        vec![mutation(q.id, MutationOp::Delete)],
    )
    .await;
    let out = undo(&db).await;
    assert_eq!(
        out.skipped,
        [skip(
            r.id,
            &[ItemField::DeletedAt],
            invalid("the parent is deleted")
        )]
    );
    assert!(get(&db, r.id).await.is_deleted());

    // Undoing a move away from a parent deleted since.
    let s = capture(&db, "s").await;
    let t = capture_under(&db, "t", s.id).await;
    run(
        &db,
        Actor::User,
        1_000,
        OpKind::Update,
        vec![set(t.id, ItemField::ParentId, FieldValue::Cleared)],
    )
    .await;
    run(
        &db,
        Actor::System,
        1_500,
        OpKind::Delete,
        vec![mutation(s.id, MutationOp::Delete)],
    )
    .await;
    let out = undo(&db).await;
    assert_eq!(
        out.skipped,
        [skip(
            t.id,
            &[ItemField::ParentId],
            invalid("the parent is deleted")
        )]
    );
    assert_eq!(get(&db, t.id).await.core.parent_id, None);
    assert_no_live_orphans(&db).await;
}

/// The parent rule holds for where an undo leaves the Items, not for each
/// step: a batch may pass through "live under a deleted parent", and so may
/// its undo, in reverse.
#[tokio::test]
async fn undo_checks_the_parent_rule_once_it_is_done() {
    let db = open();
    let p = capture(&db, "p").await;
    let c = capture_under(&db, "c", p.id).await;
    let q = capture(&db, "q").await;
    let delete_p = run(
        &db,
        Actor::User,
        100,
        OpKind::Delete,
        vec![mutation(p.id, MutationOp::Delete)],
    )
    .await;
    let deleted = [
        content(&get(&db, p.id).await),
        content(&get(&db, c.id).await),
    ];
    let state = async || {
        [
            content(&get(&db, p.id).await),
            content(&get(&db, c.id).await),
        ]
    };

    // c comes back for a moment under its deleted parent, and goes again:
    // the batch is accepted, and its undo gives c its first deleted_at back.
    for (n, mutations) in [
        vec![
            mutation(c.id, MutationOp::Undelete),
            mutation(c.id, MutationOp::Delete),
        ],
        vec![
            mutation(c.id, MutationOp::Undelete),
            title(c.id, "c2"),
            mutation(c.id, MutationOp::Delete),
        ],
        // Moved out from under it and deleted there.
        vec![
            mutation(c.id, MutationOp::Undelete),
            parent(c.id, q.id),
            mutation(c.id, MutationOp::Delete),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let at = 300 + n as i64;
        let batch = run(&db, Actor::User, at, OpKind::Batch, mutations).await;
        assert_eq!(get(&db, c.id).await.derived.deleted_at, Some(ts(at)));
        let after = state().await;
        let out = undo(&db).await;
        assert_eq!(out.entry.unwrap().op_id, batch.result.op_id, "{n}");
        assert_eq!(out.skipped, [], "{n}");
        assert_eq!(state().await, deleted, "{n}");
        assert_no_live_orphans(&db).await;
        let out = redo(&db).await;
        assert_eq!(out.skipped, [], "{n}");
        assert_eq!(state().await, after, "{n}");
        assert_no_live_orphans(&db).await;
        assert_eq!(undo(&db).await.skipped, [], "{n}");
    }

    // c kept its first deleted_at, so undoing the delete brings it back.
    let out = undo(&db).await;
    assert_eq!(out.entry.unwrap().op_id, delete_p.result.op_id);
    assert_eq!(out.skipped, []);
    assert!(!get(&db, p.id).await.is_deleted());
    assert!(!get(&db, c.id).await.is_deleted());
    assert_eq!(get(&db, c.id).await.core.parent_id, Some(p.id));
    assert_no_live_orphans(&db).await;

    // Undeleted under a deleted parent and moved to a live one: undo moves
    // it back, deleted, and the delete's undo brings both back.
    let delete_p = run(
        &db,
        Actor::User,
        400,
        OpKind::Delete,
        vec![mutation(p.id, MutationOp::Delete)],
    )
    .await;
    let deleted = state().await;
    run(
        &db,
        Actor::User,
        500,
        OpKind::Batch,
        vec![mutation(c.id, MutationOp::Undelete), parent(c.id, q.id)],
    )
    .await;
    assert_eq!(undo(&db).await.skipped, []);
    assert_eq!(state().await, deleted);
    assert_no_live_orphans(&db).await;
    let out = undo(&db).await;
    assert_eq!(out.entry.unwrap().op_id, delete_p.result.op_id);
    assert_eq!(out.skipped, []);
    assert!(!get(&db, c.id).await.is_deleted());
    assert_eq!(get(&db, c.id).await.core.parent_id, Some(p.id));
    assert_no_live_orphans(&db).await;
}

/// A batch that deletes a parent and then undeletes and deletes its child
/// undoes exactly, with nothing reported.
#[tokio::test]
async fn undo_reports_only_what_it_leaves_alone() {
    let db = open();
    let p = capture(&db, "p").await;
    let c = capture_under(&db, "c", p.id).await;
    let before = [
        content(&get(&db, p.id).await),
        content(&get(&db, c.id).await),
    ];
    run(
        &db,
        Actor::User,
        300,
        OpKind::Batch,
        vec![
            mutation(p.id, MutationOp::Delete),
            mutation(c.id, MutationOp::Undelete),
            mutation(c.id, MutationOp::Delete),
        ],
    )
    .await;
    let after = [
        content(&get(&db, p.id).await),
        content(&get(&db, c.id).await),
    ];
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    assert_eq!(
        [
            content(&get(&db, p.id).await),
            content(&get(&db, c.id).await)
        ],
        before
    );
    let out = redo(&db).await;
    assert_eq!(out.skipped, []);
    assert_eq!(
        [
            content(&get(&db, p.id).await),
            content(&get(&db, c.id).await)
        ],
        after
    );
    assert_no_live_orphans(&db).await;
}

fn relation(from: &Item, to: &Item, kind: RelationKind) -> Relation {
    Relation::new(from.id, to.id, kind).unwrap()
}

fn relation_input(relation: Relation) -> RelationInput {
    RelationInput {
        request_id: new_ulid().to_string(),
        relation,
        now: ts(1_000),
        today: today(),
    }
}

async fn add(db: &Db, relation: Relation) -> RelationOutput {
    db.store
        .add_relation(relation_input(relation))
        .await
        .unwrap()
}

async fn remove(db: &Db, relation: Relation) -> RelationOutput {
    db.store
        .remove_relation(relation_input(relation))
        .await
        .unwrap()
}

/// `(item_id, kind, old, new)` of an op's relation events.
async fn relation_events(
    db: &Db,
    op_id: Ulid,
) -> Vec<(String, String, Option<String>, Option<String>)> {
    db.store
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT item_id, kind, old, new FROM events
                 WHERE op_id = ?1 AND kind LIKE 'relation_%' ORDER BY ord",
            )?;
            let rows = stmt.query_map([op_id.to_string()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn relations_change_as_ops_on_both_ends() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture(&db, "b").await;
    let c = capture(&db, "c").await;
    let blocks = relation(&a, &b, RelationKind::Blocks);
    let mut rx = db.store.subscribe();
    let req = relation_input(blocks);
    let out = db.store.add_relation(req.clone()).await.unwrap();
    assert_eq!(out.result.revision, 5);
    assert_eq!((out.from_item.id, out.to_item.id), (a.id, b.id));
    assert_eq!((out.from_item.revision, out.to_item.revision), (4, 5));
    assert_eq!(out.from_item.updated_at, ts(1_000));
    assert_eq!(
        (
            out.from_item.relations.as_slice(),
            out.to_item.relations.as_slice()
        ),
        ([blocks].as_slice(), [blocks].as_slice())
    );
    assert_eq!(
        change_rows(&db, 3).await,
        [(4, a.id.to_string(), false), (5, b.id.to_string(), false)]
    );
    let change = rx.try_recv().unwrap();
    assert_eq!(
        change.resource,
        Some(Resource::Item(out.from_item.clone().into()))
    );
    let json = serde_json::to_string(&blocks).unwrap();
    assert_eq!(
        relation_events(&db, out.result.op_id).await,
        [(
            a.id.to_string(),
            "relation_add".into(),
            None,
            Some(json.clone())
        )]
    );
    assert_eq!(op_row(&db, out.result.op_id).await.0, "add_relation");
    assert_eq!(
        op_stack(&db, out.result.op_id).await,
        (
            Some("done".into()),
            Some("relate \"a\" blocks \"b\"".into())
        )
    );
    // A replay, and a request id reused for another relation.
    assert_eq!(
        db.store.add_relation(req.clone()).await.unwrap(),
        RelationOutput { ..out.clone() }
    );
    let other = RelationInput {
        relation: relation(&c, &b, RelationKind::Blocks),
        ..req.clone()
    };
    match db.store.add_relation(other).await {
        Err(StoreError::RequestIdReused) => {}
        other => panic!("expected RequestIdReused, got {other:?}"),
    }

    // Adding it again changes nothing and is no entry.
    let (revision, [ops, events_n, changes, requests], items) = snapshot(&db).await;
    let again = add(&db, blocks).await;
    assert_eq!(again.result.revision, revision);
    assert_eq!(undo_state_of(&db, again.result.op_id).await, None);
    assert_eq!(
        snapshot(&db).await,
        (revision, [ops + 1, events_n, changes, requests + 1], items)
    );
    assert_eq!(tops(&db).await.0, Some(out.result.op_id));

    // relates_to is symmetric: stored once, from the lower id.
    let relates = Relation::new(c.id, a.id, RelationKind::RelatesTo).unwrap();
    assert_eq!(
        relates,
        Relation::new(a.id, c.id, RelationKind::RelatesTo).unwrap()
    );
    let out = add(&db, relates).await;
    assert_eq!(out.from_item.id, a.id.min(c.id));
    assert_eq!(out.to_item.id, a.id.max(c.id));
    let stored: (String, String) = db
        .store
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT from_id, to_id FROM relations WHERE kind = 'relates_to'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        stored,
        (a.id.min(c.id).to_string(), a.id.max(c.id).to_string())
    );

    // Every read attaches both ends' relations, sorted.
    let mut on_a = vec![blocks, relates];
    on_a.sort();
    assert_eq!(get(&db, a.id).await.relations, on_a);
    assert_eq!(get(&db, b.id).await.relations, [blocks]);
    assert_eq!(get(&db, c.id).await.relations, [relates]);
    let page = db.store.list_items(ListQuery::default()).await.unwrap();
    let mut listed: Vec<(ItemId, Vec<Relation>)> = page
        .items
        .into_iter()
        .map(|i| (i.id, i.relations))
        .collect();
    listed.sort();
    let mut want = vec![
        (a.id, on_a.clone()),
        (b.id, vec![blocks]),
        (c.id, vec![relates]),
    ];
    want.sort();
    assert_eq!(listed, want);
    let replayed = db.store.changes_since(3).await.unwrap();
    let last = replayed
        .iter()
        .rev()
        .find(|ch| ch.id == a.id.to_string())
        .unwrap();
    match &last.resource {
        Some(Resource::Item(item)) => assert_eq!(item.relations, on_a),
        other => panic!("expected an Item, got {other:?}"),
    }

    // Both ends must exist and, to add, be live.
    let missing = new_ulid();
    let end = |result: Result<RelationOutput, StoreError>| match result {
        Err(StoreError::RelationEnd { item_id, problem }) => (item_id, problem),
        other => panic!("expected RelationEnd, got {other:?}"),
    };
    let before = snapshot(&db).await;
    let to_missing = Relation::new(a.id, missing, RelationKind::Blocks).unwrap();
    assert_eq!(
        end(db.store.add_relation(relation_input(to_missing)).await),
        (missing, EndProblem::Missing)
    );
    assert_eq!(
        end(db.store.remove_relation(relation_input(to_missing)).await),
        (missing, EndProblem::Missing)
    );
    assert_eq!(snapshot(&db).await, before);
    run(
        &db,
        Actor::User,
        1_500,
        OpKind::Delete,
        vec![mutation(b.id, MutationOp::Delete)],
    )
    .await;
    for deleted_end in [
        relation(&b, &c, RelationKind::Duplicates),
        relation(&c, &b, RelationKind::Blocks),
    ] {
        assert_eq!(
            end(db.store.add_relation(relation_input(deleted_end)).await),
            (b.id, EndProblem::Deleted)
        );
    }

    // Kept when an end is deleted, and removable then.
    assert_eq!(get(&db, a.id).await.relations, on_a);
    assert_eq!(get(&db, b.id).await.relations, [blocks]);
    let revision = db.store.current_revision();
    let out = remove(&db, blocks).await;
    assert_eq!(out.result.revision, revision + 2);
    assert!(out.to_item.is_deleted());
    assert_eq!(out.to_item.relations, []);
    assert_eq!(out.from_item.relations, [relates]);
    assert_eq!(
        relation_events(&db, out.result.op_id).await,
        [(a.id.to_string(), "relation_remove".into(), Some(json), None)]
    );
    assert_eq!(
        op_stack(&db, out.result.op_id).await.1.as_deref(),
        Some("unrelate \"a\" blocks \"b\"")
    );
    // Removing a relation that does not exist changes nothing.
    let again = remove(&db, blocks).await;
    assert_eq!(again.result.revision, revision + 2);
    assert_eq!(undo_state_of(&db, again.result.op_id).await, None);
}

#[tokio::test]
async fn a_relation_request_id_replays_only_the_same_relation() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture(&db, "b").await;
    let c = capture(&db, "c").await;
    let blocks = relation(&a, &b, RelationKind::Blocks);
    // Each from a, the end a request id used to be matched by.
    let others = [
        relation(&a, &c, RelationKind::Blocks),
        relation(&a, &b, RelationKind::Duplicates),
        relation(&a, &b, RelationKind::RelatesTo),
        relation(&b, &a, RelationKind::Blocks),
    ];
    let reused = |result: Result<RelationOutput, StoreError>| match result {
        Err(StoreError::RequestIdReused) => {}
        other => panic!("expected RequestIdReused, got {other:?}"),
    };

    // A change records the relation in its event, a no-op in its op.
    let changed = relation_input(blocks);
    let unchanged = relation_input(blocks);
    let missing = relation_input(relation(&b, &c, RelationKind::Duplicates));
    let first = db.store.add_relation(changed.clone()).await.unwrap();
    let again = db.store.add_relation(unchanged.clone()).await.unwrap();
    assert_eq!(again.result.revision, first.result.revision);
    let gone = db.store.remove_relation(missing.clone()).await.unwrap();
    assert_eq!(gone.result.revision, first.result.revision);
    let before = snapshot(&db).await;
    for (req, add, out) in [
        (&changed, true, &first),
        (&unchanged, true, &again),
        (&missing, false, &gone),
    ] {
        let store = &db.store;
        let replay = |req: RelationInput| async move {
            if add {
                store.add_relation(req).await
            } else {
                store.remove_relation(req).await
            }
        };
        assert_eq!(replay(req.clone()).await.unwrap(), *out);
        for relation in others.iter().chain([&blocks]) {
            if *relation == req.relation {
                continue;
            }
            let other = RelationInput {
                relation: *relation,
                ..req.clone()
            };
            reused(replay(other).await);
        }
    }
    assert_eq!(snapshot(&db).await, before);
}

#[tokio::test]
async fn relation_changes_undo_and_redo() {
    let db = open();
    let a = capture(&db, "a").await;
    let b = capture(&db, "b").await;
    let blocks = relation(&a, &b, RelationKind::Blocks);
    let added = add(&db, blocks).await;
    let out = undo(&db).await;
    assert_eq!(out.entry.unwrap().op_id, added.result.op_id);
    assert_eq!(out.skipped, []);
    assert_eq!(
        out.items.iter().map(|i| i.id).collect::<Vec<_>>(),
        [a.id, b.id]
    );
    assert!(out.items.iter().all(|i| i.relations.is_empty()));
    assert_eq!(get(&db, a.id).await.relations, []);
    redo(&db).await;
    assert_eq!(get(&db, b.id).await.relations, [blocks]);

    let removed = remove(&db, blocks).await;
    assert_eq!(undo(&db).await.entry.unwrap().op_id, removed.result.op_id);
    assert_eq!(get(&db, a.id).await.relations, [blocks]);
    redo(&db).await;
    assert_eq!(get(&db, a.id).await.relations, []);

    // A relation changed since (here behind the store's back) is skipped.
    let added = add(&db, blocks).await;
    db.store
        .write(|tx| {
            tx.conn().execute("DELETE FROM relations", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let out = undo(&db).await;
    assert_eq!(
        out.skipped,
        [UndoSkipRow {
            item_id: a.id,
            fields: vec![],
            reason: SkipReason::ChangedSince,
            relation: Some(blocks),
        }]
    );
    assert_eq!(
        undo_state_of(&db, added.result.op_id).await.as_deref(),
        Some("dropped")
    );

    // Undo and redo may change relations of deleted Items.
    add(&db, blocks).await;
    run(
        &db,
        Actor::System,
        1_500,
        OpKind::Delete,
        vec![mutation(b.id, MutationOp::Delete)],
    )
    .await;
    let out = undo(&db).await;
    assert_eq!(out.skipped, []);
    assert_eq!(get(&db, b.id).await.relations, []);
    redo(&db).await;
    assert_eq!(get(&db, b.id).await.relations, [blocks]);
}

#[tokio::test]
async fn ops_record_their_summary() {
    let db = open();
    let summary_of = |op| op_stack(&db, op);
    let long = "x".repeat(50);
    let (a, captured) = capture_op(&db, &long).await;
    assert_eq!(
        summary_of(captured).await.1,
        Some(format!("capture \"{}…\"", "x".repeat(39)))
    );
    let b = capture(&db, "b").await;
    let c = capture(&db, "c").await;
    let rows = [
        (
            OpKind::Batch,
            vec![
                status(a.id, Status::Done),
                status(b.id, Status::Done),
                status(c.id, Status::Done),
            ],
            "mark done (3 items)",
        ),
        (
            OpKind::SetStatus,
            vec![status(b.id, Status::InProgress)],
            "start \"b\"",
        ),
        (
            OpKind::Batch,
            vec![title(b.id, "b2"), mutation(c.id, MutationOp::Archive)],
            "change 2 items",
        ),
        (OpKind::Update, vec![title(c.id, "c2")], "edit \"c2\""),
        (
            OpKind::Batch,
            vec![
                mutation(b.id, MutationOp::Archive),
                mutation(c.id, MutationOp::Unarchive),
            ],
            "change 2 items",
        ),
    ];
    for (kind, mutations, want) in rows {
        let out = run(&db, Actor::User, 1_000, kind, mutations).await;
        assert_eq!(summary_of(out.result.op_id).await.1.as_deref(), Some(want));
    }
    let related = add(&db, relation(&b, &c, RelationKind::Duplicates)).await;
    assert_eq!(
        summary_of(related.result.op_id).await.1.as_deref(),
        Some("relate \"b2\" duplicates \"c2\"")
    );
    let out = undo(&db).await;
    assert_eq!(
        out.entry.unwrap().description,
        "relate \"b2\" duplicates \"c2\""
    );
    assert_eq!(
        summary_of(out.result.op_id).await.1.as_deref(),
        Some("undo: relate \"b2\" duplicates \"c2\"")
    );
    let out = redo(&db).await;
    assert_eq!(
        summary_of(out.result.op_id).await.1.as_deref(),
        Some("redo: relate \"b2\" duplicates \"c2\"")
    );
    let state = db.store.undo_state().await.unwrap();
    let entry = state.undo.unwrap();
    assert_eq!(
        (entry.kind, entry.item_ids.as_slice()),
        (OpKind::AddRelation, [b.id, c.id].as_slice())
    );
    assert_eq!(state.redo, None);
}

#[tokio::test]
async fn a_summary_counts_only_the_subtasks_the_op_does_not_name() {
    // Deleting a parent together with its own subtasks (a multi-select) reads
    // the same in either order; only the subtask nobody named is counted.
    let db = open();
    for parent_first in [true, false] {
        let p = capture(&db, "p").await;
        let c = capture_under(&db, "c", p.id).await;
        let d = capture_under(&db, "d", p.id).await;
        let mut mutations = vec![
            mutation(p.id, MutationOp::Delete),
            mutation(c.id, MutationOp::Delete),
        ];
        if !parent_first {
            mutations.reverse();
        }
        let out = run(&db, Actor::User, 1_000, OpKind::Batch, mutations).await;
        assert!(get(&db, d.id).await.is_deleted());
        assert_eq!(
            op_stack(&db, out.result.op_id).await.1.as_deref(),
            Some("delete 2 items and 1 subtask"),
            "parent first: {parent_first}"
        );
    }
}

#[tokio::test]
async fn migrates_an_m1_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("todo.db");
    let item = "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z";
    let (capture_op, update_op) = ("01JB8X4Z5Q6R7S8T9V0W1X2Y40", "01JB8X4Z5Q6R7S8T9V0W1X2Y41");
    {
        // The M1 schema with M1 rows: a capture and an update of two fields,
        // whose event ids sort against their insertion order.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch(&format!(
            r#"
            INSERT INTO items (id, created_via, created_at, updated_at, revision, title,
                               status_category, priority, last_user_touch_at, status_entered_at)
            VALUES ('{item}', 'cli', 1000, 2000, 2, 'renamed', 'todo', 'high', 2000, 1000);
            INSERT INTO ops (id, kind, actor, at, request_id, revision) VALUES
                ('{capture_op}', 'capture', 'user', 1000, 'r1', 1),
                ('{update_op}', 'update', 'user', 2000, 'r2', 2);
            INSERT INTO events (id, op_id, item_id, at, actor, kind, field, old, new) VALUES
                ('E9', '{capture_op}', '{item}', 1000, 'user', 'create', NULL, NULL, '"call"'),
                ('E8', '{update_op}', '{item}', 2000, 'user', 'set', 10,
                 '{{"string":"call"}}', '{{"string":"renamed"}}'),
                ('E7', '{update_op}', '{item}', 2000, 'user', 'set', 16,
                 '{{"priority":"none"}}', '{{"priority":"high"}}');
            UPDATE meta SET revision = 2;
            "#
        ))
        .unwrap();
    }
    let db = Db {
        store: Store::open(&path).unwrap(),
        path,
        _dir: dir,
    };
    let rows = |sql: &'static str| {
        let store = db.store.clone();
        async move {
            store
                .read(move |c| {
                    let mut stmt = c.prepare(sql)?;
                    let rows = stmt.query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                        ))
                    })?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
                .unwrap()
        }
    };
    let version: u32 = Connection::open(&db.path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    // seq follows insertion; no old op is on the stack.
    assert_eq!(
        rows("SELECT id, seq, undo_state FROM ops ORDER BY seq").await,
        [
            (capture_op.to_owned(), Some(1), None),
            (update_op.to_owned(), Some(2), None)
        ]
    );
    // ord numbers each op's events in insertion order.
    assert_eq!(
        rows("SELECT id, ord, op_id FROM events ORDER BY op_id, ord").await,
        [
            ("E9".to_owned(), Some(0), Some(capture_op.to_owned())),
            ("E8".to_owned(), Some(0), Some(update_op.to_owned())),
            ("E7".to_owned(), Some(1), Some(update_op.to_owned())),
        ]
    );
    assert_eq!(db.store.undo_state().await.unwrap(), UndoState::default());
    assert_eq!(undo(&db).await.entry, None);

    // New ops work on the old data and are undoable.
    let id: ItemId = item.parse().unwrap();
    let out = run(
        &db,
        Actor::User,
        3_000,
        OpKind::Update,
        vec![title(id, "again")],
    )
    .await;
    assert_eq!(tops(&db).await, (Some(out.result.op_id), None));
    assert_eq!(undo(&db).await.entry.unwrap().op_id, out.result.op_id);
    let back = get(&db, id).await;
    assert_eq!(
        (back.core.title.as_str(), back.core.priority),
        ("renamed", Priority::High)
    );
    assert_eq!(back.relations, []);
    assert_eq!(
        rows("SELECT id, seq, undo_state FROM ops ORDER BY seq")
            .await
            .into_iter()
            .map(|(_, seq, state)| (seq, state))
            .collect::<Vec<_>>(),
        [
            (Some(1), None),
            (Some(2), None),
            (Some(3), None),
            (Some(4), Some("undone".to_owned())),
            (Some(5), None)
        ]
    );
    // Relations work too.
    let other = capture(&db, "other").await;
    add(
        &db,
        Relation::new(id, other.id, RelationKind::RelatesTo).unwrap(),
    )
    .await;
    assert_eq!(get(&db, id).await.relations.len(), 1);
}
