//! The sync engine's writes (design 5.4, 5.5): pulled pages, the scope
//! reconciliation, cursors, and the read-only guard on linked Items.

use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::civil::{Date, date};
use todo_core::remote::{RemoteIssue, RemoteUser};
use todo_core::{
    Actor, ArchiveReason, FieldUpdate, FieldValue, ItemField, LinkState, Mutation, MutationError,
    MutationOp, Priority, Status, StatusCategory, new_ulid,
};
use todo_store::{MutateInput, OpKind, PullPage, Reconcile, Store, StoreError, UndoInput};

fn ts(s: i64) -> Timestamp {
    Timestamp::from_second(s).unwrap()
}

const TODAY: Date = date(2026, 11, 4);

fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("todo.db")).unwrap();
    (dir, store)
}

fn issue(n: u32, title: &str) -> RemoteIssue {
    RemoteIssue {
        tracker_kind: "jira".into(),
        instance_id: "work-jira".into(),
        remote_id: format!("{}", 10_000 + n),
        key: format!("PLAT-{n}"),
        url: format!("https://jira.example.com/browse/PLAT-{n}"),
        title: title.into(),
        description: String::new(),
        item_type: "Task".into(),
        status_category: StatusCategory::Todo,
        resolution_kind: None,
        remote_status_name: "Open".into(),
        resolution: None,
        priority: Priority::Medium,
        assignee: Some(RemoteUser {
            id: "JIRAUSER1".into(),
            display_name: "Dana Example".into(),
        }),
        reporter: None,
        labels: vec![],
        components: vec![],
        fix_versions: vec![],
        due_date: None,
        parent_remote_id: None,
        sprint: None,
        story_points: None,
        original_estimate: None,
        remaining_estimate: None,
        created_at: ts(100),
        updated_at: ts(200),
        extras: BTreeMap::new(),
        is_mine: true,
    }
}

fn page(issues: Vec<RemoteIssue>, cursor: Option<i64>) -> PullPage {
    PullPage {
        instance_id: "work-jira".into(),
        issues: issues
            .into_iter()
            .map(|i| (i, vec!["mine".into()]))
            .collect(),
        cursor: cursor.map(|s| ("mine".into(), ts(s))),
        now: ts(1_000),
        today: TODAY,
    }
}

async fn by_key(store: &Store, key: &str) -> todo_core::Item {
    let tracked = store.tracked("work-jira".into()).await.unwrap();
    for (id, _, _) in tracked {
        let item = store.get_item(id, TODAY).await.unwrap().item;
        if item.remote_link.as_ref().is_some_and(|l| l.key == key) {
            return item;
        }
    }
    panic!("no {key}");
}

fn user_edit(id: todo_core::ItemId, field: ItemField, value: FieldValue) -> MutateInput {
    MutateInput {
        request_id: new_ulid().to_string(),
        actor: Actor::User,
        kind: OpKind::Update,
        mutations: vec![Mutation {
            id,
            expected_revision: None,
            op: MutationOp::Update(vec![FieldUpdate { field, value }]),
        }],
        now: ts(2_000),
        today: TODAY,
    }
}

#[tokio::test]
async fn a_page_imports_updates_and_moves_the_cursor() {
    let (_dir, store) = open();
    let mut parent = issue(1, "Release 2.3");
    parent.status_category = StatusCategory::InProgress;
    let mut child = issue(2, "Write the notes");
    child.parent_remote_id = Some(parent.remote_id.clone());
    // The subtask comes first; the parent is still found.
    let out = store
        .apply_pull(page(vec![child.clone(), parent.clone()], Some(200)))
        .await
        .unwrap();
    assert_eq!((out.imported.len(), out.updated.len()), (2, 0));
    assert_eq!(
        store
            .sync_cursor("work-jira".into(), "mine".into())
            .await
            .unwrap(),
        Some(ts(200))
    );
    let p = by_key(&store, "PLAT-1").await;
    let c = by_key(&store, "PLAT-2").await;
    assert_eq!(p.status(), Status::InProgress);
    assert_eq!(c.status(), Status::Next);
    assert_eq!(c.core.parent_id, Some(p.id));
    assert_eq!(p.derived.subtasks_total, 1);
    assert_eq!(
        c.remote_link.as_ref().unwrap().url,
        "https://jira.example.com/browse/PLAT-2"
    );
    // The same page again changes nothing and writes no op.
    let again = store
        .apply_pull(page(vec![child.clone(), parent.clone()], Some(200)))
        .await
        .unwrap();
    assert_eq!(
        (again.imported.len(), again.updated.len(), again.result),
        (0, 0, None)
    );
    // A remote edit lands; Overlay set locally survives it.
    store
        .mutate(user_edit(
            c.id,
            ItemField::PrivateNotes,
            FieldValue::Markdown("mine".into()),
        ))
        .await
        .unwrap();
    child.title = "Write the release notes".into();
    child.status_category = StatusCategory::Done;
    child.updated_at = ts(300);
    let edit = store
        .apply_pull(page(vec![child], Some(300)))
        .await
        .unwrap();
    assert_eq!(edit.updated, [c.id]);
    let c = by_key(&store, "PLAT-2").await;
    assert_eq!(
        (c.core.title.as_str(), c.status()),
        ("Write the release notes", Status::Done)
    );
    assert_eq!(c.overlay.private_notes, "mine");
    // The cursor never goes back.
    store.apply_pull(page(vec![], Some(100))).await.unwrap();
    assert_eq!(
        store
            .sync_cursor("work-jira".into(), "mine".into())
            .await
            .unwrap(),
        Some(ts(300))
    );
}

#[tokio::test]
async fn a_linked_item_refuses_core_edits_but_takes_overlay_ones() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "PLAT one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    let err = store
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine".into()),
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::Mutation {
                error: MutationError::LinkedReadOnly(ItemField::Title),
                ..
            }
        ),
        "{err:?}"
    );
    store
        .mutate(user_edit(
            item.id,
            ItemField::Energy,
            FieldValue::Energy(todo_core::Energy::Low),
        ))
        .await
        .unwrap();
    // Sync ops are never undo entries; the energy edit is.
    let undone = store
        .undo(UndoInput {
            request_id: new_ulid().to_string(),
            expected_op_id: None,
            now: ts(3_000),
            today: TODAY,
        })
        .await
        .unwrap();
    assert!(undone.entry.is_some());
    let again = store
        .undo(UndoInput {
            request_id: new_ulid().to_string(),
            expected_op_id: None,
            now: ts(3_001),
            today: TODAY,
        })
        .await
        .unwrap();
    assert!(again.entry.is_none(), "the import is not undoable");
    assert!(
        store
            .get_item(item.id, TODAY)
            .await
            .unwrap()
            .item
            .is_linked()
    );
}

#[tokio::test]
async fn a_deleted_item_is_left_alone_by_pulls() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    let mut del = user_edit(item.id, ItemField::Title, FieldValue::Cleared);
    del.kind = OpKind::Delete;
    del.mutations[0].op = MutationOp::Delete;
    store.mutate(del).await.unwrap();
    let mut edited = issue(1, "renamed");
    edited.updated_at = ts(900);
    let out = store.apply_pull(page(vec![edited], None)).await.unwrap();
    assert_eq!((out.skipped_deleted, out.imported.len()), (1, 0));
    assert_eq!(
        store
            .get_item(item.id, TODAY)
            .await
            .unwrap()
            .item
            .core
            .title,
        "one"
    );
}

#[tokio::test]
async fn reconciliation_archives_what_left_scope_and_breaks_what_is_gone() {
    let (_dir, store) = open();
    let mut done = issue(3, "resolved elsewhere");
    store
        .apply_pull(page(
            vec![issue(1, "moved away"), issue(2, "deleted"), done.clone()],
            None,
        ))
        .await
        .unwrap();
    let (a, b, c) = (
        by_key(&store, "PLAT-1").await,
        by_key(&store, "PLAT-2").await,
        by_key(&store, "PLAT-3").await,
    );
    done.status_category = StatusCategory::Done;
    done.updated_at = ts(500);
    let out = store
        .reconcile(Reconcile {
            instance_id: "work-jira".into(),
            out_of_scope: vec![
                (a.id, Some(issue(1, "moved away"))),
                (b.id, None),
                (c.id, Some(done)),
            ],
            now: ts(1_500),
            today: TODAY,
        })
        .await
        .unwrap();
    assert_eq!((out.left_scope, out.broken), (vec![a.id], vec![b.id]));
    let a = store.get_item(a.id, TODAY).await.unwrap().item;
    assert_eq!(
        a.overlay.archived.map(|x| x.reason),
        Some(ArchiveReason::LeftScope)
    );
    assert!(a.derived.scopes.is_empty());
    let b = store.get_item(b.id, TODAY).await.unwrap().item;
    assert_eq!(
        b.remote_link.as_ref().unwrap().link_state,
        LinkState::Broken
    );
    assert_eq!(
        b.overlay.archived.map(|x| x.reason),
        Some(ArchiveReason::RemoteUnavailable)
    );
    // Resolved in Jira: done, not archived for leaving scope.
    let c = store.get_item(c.id, TODAY).await.unwrap().item;
    assert_eq!((c.status(), c.overlay.archived), (Status::Done, None));
    // Back in scope: unarchived.
    let mut back = issue(1, "moved away");
    back.updated_at = ts(2_000);
    store.apply_pull(page(vec![back], None)).await.unwrap();
    let a = store.get_item(a.id, TODAY).await.unwrap().item;
    assert_eq!(
        (a.overlay.archived, a.derived.scopes.clone()),
        (None, vec!["mine".to_owned()])
    );
}

#[tokio::test]
async fn parents_resolve_at_any_depth_in_any_order_but_never_to_a_tombstone() {
    let (_dir, store) = open();
    let epic = issue(1, "epic");
    let mut story = issue(2, "story");
    story.parent_remote_id = Some(epic.remote_id.clone());
    let mut sub = issue(3, "sub-task");
    sub.parent_remote_id = Some(story.remote_id.clone());
    store
        .apply_pull(page(vec![sub.clone(), story.clone(), epic], None))
        .await
        .unwrap();
    let (e, s, t) = (
        by_key(&store, "PLAT-1").await,
        by_key(&store, "PLAT-2").await,
        by_key(&store, "PLAT-3").await,
    );
    assert_eq!(
        (s.core.parent_id, t.core.parent_id),
        (Some(e.id), Some(s.id))
    );
    // A subtask whose parent was deleted here lands at the top level.
    let mut del = user_edit(e.id, ItemField::Title, FieldValue::Cleared);
    del.kind = OpKind::Delete;
    del.mutations[0].op = MutationOp::Delete;
    store.mutate(del).await.unwrap();
    let mut late = issue(4, "late child");
    late.parent_remote_id = Some("10001".into());
    store.apply_pull(page(vec![late], None)).await.unwrap();
    assert_eq!(by_key(&store, "PLAT-4").await.core.parent_id, None);
}

#[tokio::test]
async fn a_subtask_finds_a_parent_that_comes_in_a_later_page() {
    let (_dir, store) = open();
    let story = issue(1, "story");
    let mut sub = issue(2, "sub-task");
    sub.parent_remote_id = Some(story.remote_id.clone());
    store.apply_pull(page(vec![sub], Some(200))).await.unwrap();
    assert_eq!(by_key(&store, "PLAT-2").await.core.parent_id, None);
    let out = store
        .apply_pull(page(vec![story], Some(300)))
        .await
        .unwrap();
    let (s, t) = (
        by_key(&store, "PLAT-1").await,
        by_key(&store, "PLAT-2").await,
    );
    assert_eq!(t.core.parent_id, Some(s.id));
    assert!(
        !t.remote_extras
            .contains_key(todo_core::remote::PENDING_PARENT)
    );
    assert_eq!(s.derived.subtasks_total, 1);
    assert!(out.updated.contains(&t.id));
}

#[tokio::test]
async fn an_unavailable_item_comes_back_when_readable_again() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    let a = by_key(&store, "PLAT-1").await;
    store
        .reconcile(Reconcile {
            instance_id: "work-jira".into(),
            out_of_scope: vec![(a.id, None)],
            now: ts(1_500),
            today: TODAY,
        })
        .await
        .unwrap();
    let mut back = issue(1, "one");
    back.updated_at = ts(3_000);
    store.apply_pull(page(vec![back], None)).await.unwrap();
    let a = store.get_item(a.id, TODAY).await.unwrap().item;
    assert_eq!(
        (
            a.overlay.archived,
            a.remote_link.as_ref().unwrap().link_state
        ),
        (None, LinkState::Ok)
    );
    // A person's own archive is never lifted by sync.
    let mut z = user_edit(a.id, ItemField::Title, FieldValue::Cleared);
    z.kind = OpKind::Archive;
    z.mutations[0].op = MutationOp::Archive(ArchiveReason::User);
    store.mutate(z).await.unwrap();
    let mut again = issue(1, "one");
    again.updated_at = ts(4_000);
    store.apply_pull(page(vec![again], None)).await.unwrap();
    let a = store.get_item(a.id, TODAY).await.unwrap().item;
    assert_eq!(
        a.overlay.archived.map(|x| x.reason),
        Some(ArchiveReason::User)
    );
}
