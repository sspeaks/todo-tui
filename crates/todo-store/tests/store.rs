use std::collections::BTreeSet;
use std::path::PathBuf;

use jiff::Timestamp;
use rusqlite::Connection;
use todo_core::{CreatedVia, Item, Status, StatusFields, Topic, new_ulid};
use todo_store::{
    CaptureInput, ChangesSinceError, Expired, ListQuery, Resource, SCHEMA_VERSION, Store,
    StoreError,
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

fn input(title: &str, at: i64) -> CaptureInput {
    CaptureInput {
        request_id: new_ulid().to_string(),
        title: title.into(),
        created_via: CreatedVia::Cli,
        parent_id: None,
        spool_id: None,
        now: Timestamp::from_second(at).unwrap(),
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
