//! Editing saved views (design 2.4): create, update, delete and move, with
//! dense positions, idempotent request ids, and no undo stack entries.

use todo_core::filter::{Operand, Operator, Predicate};
use todo_core::view::{View, ViewId};
use todo_core::{FieldValue, Filter, ItemField, SortKey, Status, new_ulid};
use todo_store::{CreateViewInput, MoveViewInput, Store, StoreError, UpdateViewInput, ViewUpdate};

struct Db {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
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

fn status_is(s: Status) -> Filter {
    Filter {
        all_of: vec![
            Predicate::new(
                ItemField::Status,
                Operator::In,
                vec![Operand::Value(FieldValue::Status(s))],
            )
            .into(),
        ],
        ..Filter::default()
    }
}

fn view(name: &str) -> View {
    View {
        id: None,
        name: name.to_owned(),
        filter: status_is(Status::Next),
        sort: vec![SortKey::desc(ItemField::Priority)],
        show_count: true,
        position: 0,
        seed: None,
        revision: 0,
        created_at: None,
        updated_at: None,
    }
}

async fn create(db: &Db, name: &str, position: Option<u32>) -> View {
    db.store
        .create_view(CreateViewInput {
            request_id: new_ulid().to_string(),
            view: view(name),
            position,
        })
        .await
        .unwrap()
        .view
        .unwrap()
}

/// The views' names in tab order, checking positions are 0..n.
async fn names(db: &Db) -> Vec<String> {
    let (views, _) = db.store.list_views().await.unwrap();
    for (i, v) in views.iter().enumerate() {
        assert_eq!(v.position as usize, i, "{views:?}");
    }
    views.into_iter().map(|v| v.name).collect()
}

async fn id_of(db: &Db, name: &str) -> ViewId {
    let (views, _) = db.store.list_views().await.unwrap();
    views
        .into_iter()
        .find(|v| v.name == name)
        .unwrap()
        .id
        .unwrap()
}

/// (stack entries, events): view edits add to neither.
async fn stack_and_events(db: &Db) -> (i64, i64) {
    db.store
        .read(|c| {
            Ok((
                c.query_row(
                    "SELECT COUNT(*) FROM ops WHERE undo_state IS NOT NULL",
                    [],
                    |r| r.get(0),
                )?,
                c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap()
}

const DEFAULTS: [&str; 6] = ["Inbox", "Today", "Next", "Waiting", "Someday", "Archive"];

#[tokio::test]
async fn create_puts_a_view_where_asked_and_shifts_the_rest() {
    let db = open();
    let mut rx = db.store.subscribe();
    let work = create(&db, "Work", None).await;
    assert_eq!(work.position, 6);
    assert_eq!(work.seed, None);
    assert!(work.id.is_some() && work.created_at.is_some());
    assert_eq!(rx.try_recv().unwrap().id, work.id.unwrap().to_string());

    let first = create(&db, "First", Some(0)).await;
    assert_eq!(first.position, 0);
    let mut want = vec!["First"];
    want.extend(DEFAULTS);
    want.push("Work");
    assert_eq!(names(&db).await, want);
    // Each shifted view got a revision and a change; only First is new.
    let (views, _) = db.store.list_views().await.unwrap();
    assert!(
        views[1..]
            .iter()
            .all(|v| v.revision > work.revision || v.name == "Work")
    );
    assert_eq!(views[1].updated_at, views[1].created_at);
    // Past the end is last.
    assert_eq!(create(&db, "Last", Some(99)).await.position, 8);
    assert_eq!(stack_and_events(&db).await, (0, 0));
}

#[tokio::test]
async fn names_are_unique_ignoring_case_trimmed_and_checked() {
    let db = open();
    let err = db
        .store
        .create_view(CreateViewInput {
            request_id: new_ulid().to_string(),
            view: view(" inbox "),
            position: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::ViewNameTaken(n) if n == "inbox"));
    assert_eq!(create(&db, "  Work  ", None).await.name, "Work");
    for bad in ["", "   ", &"x".repeat(65), "a\u{7}b"] {
        let err = db
            .store
            .create_view(CreateViewInput {
                request_id: new_ulid().to_string(),
                view: view(bad),
                position: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::ViewName(_)), "{bad:?}: {err}");
    }
    // A rename to another view's name, in any case, is refused; to its own is not.
    let next = id_of(&db, "Next").await;
    let rename = |name: &str| UpdateViewInput {
        request_id: new_ulid().to_string(),
        id: next,
        update: ViewUpdate {
            name: Some(name.to_owned()),
            ..ViewUpdate::default()
        },
        expected_revision: None,
    };
    assert!(matches!(
        db.store.update_view(rename("WORK")).await.unwrap_err(),
        StoreError::ViewNameTaken(_)
    ));
    assert_eq!(
        db.store
            .update_view(rename("NEXT"))
            .await
            .unwrap()
            .view
            .unwrap()
            .name,
        "NEXT"
    );
}

#[tokio::test]
async fn update_changes_only_what_it_names_and_checks_the_revision() {
    let db = open();
    let next = id_of(&db, "Next").await;
    let (views, _) = db.store.list_views().await.unwrap();
    let before = views.into_iter().find(|v| v.id == Some(next)).unwrap();
    let out = db
        .store
        .update_view(UpdateViewInput {
            request_id: new_ulid().to_string(),
            id: next,
            update: ViewUpdate {
                filter: Some(status_is(Status::Someday)),
                show_count: Some(false),
                ..ViewUpdate::default()
            },
            expected_revision: Some(before.revision),
        })
        .await
        .unwrap();
    let after = out.view.unwrap();
    assert_eq!(after.filter, status_is(Status::Someday));
    assert!(!after.show_count);
    assert_eq!((after.name.as_str(), &after.sort), ("Next", &before.sort));
    assert!(after.revision > before.revision);
    assert!(after.updated_at > before.updated_at);

    let stale = db
        .store
        .update_view(UpdateViewInput {
            request_id: new_ulid().to_string(),
            id: next,
            update: ViewUpdate {
                show_count: Some(true),
                ..ViewUpdate::default()
            },
            expected_revision: Some(before.revision),
        })
        .await
        .unwrap_err();
    assert!(matches!(stale, StoreError::ViewRevisionMismatch { .. }));

    // A bad filter is refused with its path.
    let bad = Filter {
        all_of: vec![
            Predicate::new(
                ItemField::Status,
                Operator::Lt,
                vec![Operand::Value(FieldValue::Status(Status::Done))],
            )
            .into(),
        ],
        ..Filter::default()
    };
    let err = db
        .store
        .update_view(UpdateViewInput {
            request_id: new_ulid().to_string(),
            id: next,
            update: ViewUpdate {
                filter: Some(bad),
                ..ViewUpdate::default()
            },
            expected_revision: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Filter(e) if e.path == "all_of[0].predicate.op"));

    // A no-op update allocates no revision.
    let revision = db.store.current_revision();
    db.store
        .update_view(UpdateViewInput {
            request_id: new_ulid().to_string(),
            id: next,
            update: ViewUpdate {
                show_count: Some(false),
                ..ViewUpdate::default()
            },
            expected_revision: None,
        })
        .await
        .unwrap();
    assert_eq!(db.store.current_revision(), revision);
    assert_eq!(stack_and_events(&db).await, (0, 0));
}

#[tokio::test]
async fn delete_closes_the_gap_and_a_deleted_default_stays_gone() {
    let db = open();
    let today = id_of(&db, "Today").await;
    let mut rx = db.store.subscribe();
    let out = db
        .store
        .delete_view(new_ulid().to_string(), today)
        .await
        .unwrap();
    assert_eq!(out.view, None);
    assert_eq!(out.moved.len(), 4);
    let first = rx.try_recv().unwrap();
    assert!(first.deleted && first.id == today.to_string());
    assert_eq!(
        names(&db).await,
        ["Inbox", "Next", "Waiting", "Someday", "Archive"]
    );
    let err = db
        .store
        .delete_view(new_ulid().to_string(), today)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::ViewNotFound(id) if id == today));
    // Replayed from the database, the deleted view is deleted.
    let rows = db
        .store
        .changes_since(out.result.revision - 5)
        .await
        .unwrap();
    assert!(rows.iter().any(|r| r.id == today.to_string() && r.deleted));

    // Reopening does not seed Today again.
    let Db { _dir, path, store } = db;
    drop(store);
    let store = Store::open(&path).unwrap();
    let db = Db { _dir, path, store };
    assert_eq!(
        names(&db).await,
        ["Inbox", "Next", "Waiting", "Someday", "Archive"]
    );
}

#[tokio::test]
async fn move_rotates_the_range_between() {
    let db = open();
    let archive = id_of(&db, "Archive").await;
    let mv = |id, position| MoveViewInput {
        request_id: new_ulid().to_string(),
        id,
        position,
    };
    let out = db.store.move_view(mv(archive, 1)).await.unwrap();
    assert_eq!(out.moved.len(), 5);
    assert_eq!(
        names(&db).await,
        ["Inbox", "Archive", "Today", "Next", "Waiting", "Someday"]
    );
    let inbox = id_of(&db, "Inbox").await;
    db.store.move_view(mv(inbox, 99)).await.unwrap();
    assert_eq!(
        names(&db).await,
        ["Archive", "Today", "Next", "Waiting", "Someday", "Inbox"]
    );
    // To where it is: nothing moves, no revision.
    let revision = db.store.current_revision();
    let out = db.store.move_view(mv(inbox, 5)).await.unwrap();
    assert!(out.moved.is_empty());
    assert_eq!(db.store.current_revision(), revision);
    assert_eq!(stack_and_events(&db).await, (0, 0));
}

#[tokio::test]
async fn positions_stay_dense_through_any_sequence() {
    let db = open();
    let mut n = 0u32;
    for step in 0..60u32 {
        let (views, _) = db.store.list_views().await.unwrap();
        let pick = views[(step as usize * 7) % views.len()].id.unwrap();
        match step % 4 {
            0 => {
                n += 1;
                create(&db, &format!("v{n}"), Some(step % 9)).await;
            }
            1 => {
                db.store
                    .move_view(MoveViewInput {
                        request_id: new_ulid().to_string(),
                        id: pick,
                        position: (step * 5) % 11,
                    })
                    .await
                    .unwrap();
            }
            2 if views.len() > 2 => {
                db.store
                    .delete_view(new_ulid().to_string(), pick)
                    .await
                    .unwrap();
            }
            _ => {}
        }
        names(&db).await;
    }
}

#[tokio::test]
async fn a_request_id_replays_its_view_edit_and_is_kept_to_it() {
    let db = open();
    let request_id = new_ulid().to_string();
    let input = CreateViewInput {
        request_id: request_id.clone(),
        view: view("Work"),
        position: None,
    };
    let first = db.store.create_view(input.clone()).await.unwrap();
    let again = db.store.create_view(input).await.unwrap();
    assert_eq!(again.result, first.result);
    assert_eq!(
        again.view.as_ref().map(|v| v.id),
        first.view.as_ref().map(|v| v.id)
    );
    assert_eq!(names(&db).await.len(), 7);
    // Reused for another kind, or another view: refused.
    let next = id_of(&db, "Next").await;
    let err = db
        .store
        .delete_view(request_id.clone(), next)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::RequestIdReused));
    let move_id = new_ulid().to_string();
    db.store
        .move_view(MoveViewInput {
            request_id: move_id.clone(),
            id: next,
            position: 0,
        })
        .await
        .unwrap();
    let inbox = id_of(&db, "Inbox").await;
    let err = db
        .store
        .move_view(MoveViewInput {
            request_id: move_id,
            id: inbox,
            position: 0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::RequestIdReused));
    // A delete replays after the view is gone.
    let work = id_of(&db, "Work").await;
    let delete_id = new_ulid().to_string();
    let deleted = db.store.delete_view(delete_id.clone(), work).await.unwrap();
    let replayed = db.store.delete_view(delete_id, work).await.unwrap();
    assert_eq!(replayed.result, deleted.result);
    let _ = &db.path;
}

#[tokio::test]
async fn names_are_unique_ignoring_case_in_every_script() {
    let db = open();
    create(&db, "Ärger", None).await;
    let err = db
        .store
        .create_view(CreateViewInput {
            request_id: new_ulid().to_string(),
            view: view("ärger"),
            position: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::ViewNameTaken(_)), "{err}");
    // A rename of its own case is fine.
    let id = id_of(&db, "Ärger").await;
    let out = db
        .store
        .update_view(UpdateViewInput {
            request_id: new_ulid().to_string(),
            id,
            update: ViewUpdate {
                name: Some("ÄRGER".into()),
                ..ViewUpdate::default()
            },
            expected_revision: None,
        })
        .await
        .unwrap();
    assert_eq!(out.view.unwrap().name, "ÄRGER");
}

#[tokio::test]
async fn a_replay_after_the_view_was_deleted_answers_the_original_result() {
    let db = open();
    let request_id = new_ulid().to_string();
    let input = CreateViewInput {
        request_id: request_id.clone(),
        view: view("Work"),
        position: None,
    };
    let first = db.store.create_view(input.clone()).await.unwrap();
    let id = first.view.unwrap().id.unwrap();
    db.store
        .delete_view(new_ulid().to_string(), id)
        .await
        .unwrap();
    let again = db.store.create_view(input).await.unwrap();
    assert_eq!(again.result, first.result);
    assert_eq!(again.view, None);
}

#[tokio::test]
async fn a_view_offset_resolves_on_any_day() {
    use todo_core::filter::{Anchor, RelativeDate};
    let db = open();
    let far = |days| Filter {
        all_of: vec![
            Predicate::new(
                ItemField::DueDate,
                Operator::Lt,
                vec![RelativeDate::new(Anchor::Today, days).into()],
            )
            .into(),
        ],
        ..Filter::default()
    };
    let mut v = view("Far");
    v.filter = far(36_526);
    let err = db
        .store
        .create_view(CreateViewInput {
            request_id: new_ulid().to_string(),
            view: v.clone(),
            position: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, StoreError::Filter(e) if e.path == "all_of[0].predicate.operands[0].relative_date.offset_days"),
        "{err}"
    );
    v.filter = far(-36_525);
    db.store
        .create_view(CreateViewInput {
            request_id: new_ulid().to_string(),
            view: v,
            position: None,
        })
        .await
        .unwrap();
    // The counts still read every view.
    db.store
        .status_counts(jiff::civil::date(2026, 11, 2), true)
        .await
        .unwrap();
}
