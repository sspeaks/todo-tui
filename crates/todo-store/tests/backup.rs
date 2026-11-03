//! Backups (design 7.5): a copy of a live store, and what restore does to a
//! file before the daemon opens it.

use todo_core::{CaptureDraft, CreatedVia, new_ulid};
use todo_store::{CaptureInput, ChangesSinceError, Store, inspect, prepare_restored};

fn capture(title: &str) -> CaptureInput {
    CaptureInput {
        request_id: new_ulid().to_string(),
        draft: CaptureDraft::from_title(title),
        created_via: CreatedVia::Cli,
        parent_id: None,
        spool_id: None,
        now: jiff::Timestamp::now(),
        today: jiff::civil::date(2026, 11, 3),
    }
}

#[tokio::test]
async fn a_backup_is_a_consistent_copy_taken_while_writes_go_on() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("todo.db")).unwrap();
    for i in 0..20 {
        store.capture(capture(&format!("item {i}"))).await.unwrap();
    }
    let writer = {
        let store = store.clone();
        tokio::spawn(async move {
            for i in 20..60 {
                store.capture(capture(&format!("item {i}"))).await.unwrap();
            }
        })
    };
    let to = dir.path().join("backups/todo-test.db");
    let bytes = store.backup_to(to.clone()).await.unwrap();
    writer.await.unwrap();
    assert!(bytes > 0);
    assert!(!to.with_extension("db.tmp").exists());
    let info = inspect(&to).unwrap();
    assert!((20..=60).contains(&info.items), "{info:?}");
    // The copy opens as a store of its own, at the revision it was taken at.
    let copy = Store::open_with(&to, None).unwrap();
    assert_eq!(copy.current_revision(), info.revision);
}

#[test]
fn inspect_refuses_what_is_not_a_todo_database() {
    let dir = tempfile::tempdir().unwrap();
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "hello").unwrap();
    assert!(inspect(&text).is_err());
    let empty = dir.path().join("empty.db");
    rusqlite::Connection::open(&empty)
        .unwrap()
        .execute_batch("CREATE TABLE t (x)")
        .unwrap();
    assert!(
        inspect(&empty)
            .unwrap_err()
            .to_string()
            .contains("no schema")
    );
    let newer = dir.path().join("newer.db");
    let conn = rusqlite::Connection::open(&newer).unwrap();
    conn.pragma_update(None, "user_version", 999).unwrap();
    assert!(matches!(
        inspect(&newer),
        Err(todo_store::StoreError::SchemaTooNew { .. })
    ));
}

#[tokio::test]
async fn a_restored_database_sends_every_client_to_resync() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("todo.db");
    let store = Store::open(&path).unwrap();
    store.capture(capture("a")).await.unwrap();
    let old = store.current_revision();
    drop(store);
    // As restore leaves it: past the revision clients of the old one saw.
    prepare_restored(&path, 100).unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.current_revision(), 101);
    for since in [0, old, 100] {
        assert!(
            matches!(
                store.changes_since(since).await,
                Err(ChangesSinceError::Expired(_))
            ),
            "{since}"
        );
    }
    assert!(store.changes_since(101).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_backup_never_replaces_another_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("todo.db")).unwrap();
    store.capture(capture("a")).await.unwrap();
    let to = dir.path().join("backups/todo-x.db");
    store.backup_to(to.clone()).await.unwrap();
    store.capture(capture("b")).await.unwrap();
    assert!(store.backup_to(to.clone()).await.is_err());
    assert_eq!(inspect(&to).unwrap().items, 1, "the first backup is kept");
    // Two at once on two free names: both whole.
    let (p, q) = (
        todo_store::free_backup_path(&dir.path().join("backups"), "todo", "s"),
        dir.path().join("backups/todo-s2.db"),
    );
    let (x, y) = tokio::join!(store.backup_to(p.clone()), store.backup_to(q.clone()));
    x.unwrap();
    y.unwrap();
    assert_eq!(
        (inspect(&p).unwrap().items, inspect(&q).unwrap().items),
        (2, 2)
    );
    let names: Vec<_> = std::fs::read_dir(dir.path().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|n| n.ends_with(".db")), "{names:?}");
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn a_backup_file_copies_the_wal_of_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("todo.db");
    let store = Store::open(&path).unwrap();
    for t in ["a", "b", "c"] {
        store.capture(capture(t)).await.unwrap();
    }
    // As a file-level copy of a live home: the WAL not checkpointed.
    let copy = dir.path().join("copy");
    std::fs::create_dir(&copy).unwrap();
    for f in ["todo.db", "todo.db-wal", "todo.db-shm"] {
        let _ = std::fs::copy(dir.path().join(f), copy.join(f));
    }
    let out = dir.path().join("out.db");
    todo_store::backup_file(&copy.join("todo.db"), &out).unwrap();
    assert_eq!(inspect(&out).unwrap().items, 3);
    drop(store);
}
