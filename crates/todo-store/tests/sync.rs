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
        replace_scopes: false,
        now: ts(1_000),
        today: TODAY,
    }
}

async fn by_key(store: &Store, key: &str) -> todo_core::Item {
    let tracked = store.tracked("work-jira".into()).await.unwrap();
    for todo_store::Tracked { id, .. } in tracked {
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
        confirm_not_mine: false,
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
    // Undeleted (an undo here), it is stale: the pass reads it again by id.
    store
        .undo(UndoInput {
            request_id: new_ulid().to_string(),
            expected_op_id: None,
            now: ts(3_000),
            today: TODAY,
        })
        .await
        .unwrap();
    let tracked = store.tracked("work-jira".into()).await.unwrap();
    assert_eq!(tracked.len(), 1);
    assert!(tracked[0].stale);
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
    // Unreadable: read again by id on every pass until it answers.
    let stale = |t: Vec<todo_store::Tracked>| t.iter().filter(|t| t.stale).count();
    assert_eq!(stale(store.tracked("work-jira".into()).await.unwrap()), 1);
    let mut back = issue(1, "one");
    back.updated_at = ts(3_000);
    store.apply_pull(page(vec![back], None)).await.unwrap();
    assert_eq!(stale(store.tracked("work-jira".into()).await.unwrap()), 0);
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

#[tokio::test]
async fn an_export_carries_the_cursors_and_replaces_what_sync_pulled_first() {
    let (_a, source) = open();
    source
        .apply_pull(page(vec![issue(1, "One")], Some(300)))
        .await
        .unwrap();
    source
        .apply_pull(PullPage {
            cursor: Some(("\u{0}outside".into(), ts(400))),
            ..page(vec![], None)
        })
        .await
        .unwrap();
    let export = source.export(true).await.unwrap();
    // A new home whose config syncs: a pass pulled before the import.
    let (_b, target) = open();
    target
        .apply_pull(page(vec![issue(2, "Two")], Some(250)))
        .await
        .unwrap();
    target.import(export.revision, export.rows).await.unwrap();
    let keys: Vec<String> = {
        let mut out = Vec::new();
        for todo_store::Tracked { id, .. } in target.tracked("work-jira".into()).await.unwrap() {
            let item = target.get_item(id, TODAY).await.unwrap().item;
            out.push(item.remote_link.unwrap().key);
        }
        out
    };
    assert_eq!(keys, ["PLAT-1"]);
    assert_eq!(
        target
            .sync_cursor("work-jira".into(), "mine".into())
            .await
            .unwrap(),
        Some(ts(300))
    );
    assert_eq!(
        target
            .sync_cursor("work-jira".into(), "\u{0}outside".into())
            .await
            .unwrap(),
        Some(ts(400))
    );
    // Once a person has written, the home is not empty.
    let (_c, used) = open();
    used.apply_pull(page(vec![issue(3, "Three")], None))
        .await
        .unwrap();
    let three = by_key(&used, "PLAT-3").await;
    used.mutate(user_edit(
        three.id,
        ItemField::PrivateNotes,
        FieldValue::Markdown("mine".into()),
    ))
    .await
    .unwrap();
    let export = source.export(true).await.unwrap();
    assert!(matches!(
        used.import(export.revision, export.rows).await,
        Err(StoreError::NotEmpty)
    ));
}

#[tokio::test]
async fn a_scope_pull_adds_its_scope_and_only_reconciliation_replaces_them() {
    let (_d, store) = open();
    let scoped = |name: &str, replace: bool, n: i64| PullPage {
        issues: vec![(
            RemoteIssue {
                updated_at: ts(200 + n),
                ..issue(1, "In both")
            },
            vec![name.to_owned()],
        )],
        replace_scopes: replace,
        ..page(vec![], None)
    };
    store.apply_pull(scoped("mine", false, 0)).await.unwrap();
    store.apply_pull(scoped("sprint", false, 1)).await.unwrap();
    assert_eq!(
        by_key(&store, "PLAT-1").await.derived.scopes,
        ["mine", "sprint"]
    );
    // Changed again: each scope's pull leaves the other's membership.
    store.apply_pull(scoped("mine", false, 2)).await.unwrap();
    assert_eq!(
        by_key(&store, "PLAT-1").await.derived.scopes,
        ["mine", "sprint"]
    );
    // Reconciliation, which listed every scope, says what it is in now.
    store.apply_pull(scoped("sprint", true, 3)).await.unwrap();
    assert_eq!(by_key(&store, "PLAT-1").await.derived.scopes, ["sprint"]);
}

/// The outbox rows of `item`: (field number, state, base, new).
async fn outbox(store: &Store, item: todo_core::ItemId) -> Vec<(u32, String, String, String)> {
    store
        .read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT field, state, base_json, new_json FROM outbox WHERE item_id = ?1 ORDER BY created_at, field",
            )?;
            let rows = stmt
                .query_map([item.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap()
}

fn status_edit(id: todo_core::ItemId, status: Status) -> MutateInput {
    MutateInput {
        kind: OpKind::SetStatus,
        mutations: vec![Mutation {
            id,
            expected_revision: None,
            op: MutationOp::SetStatus(status),
        }],
        ..user_edit(id, ItemField::Title, FieldValue::Cleared)
    }
}

#[tokio::test]
async fn with_pushes_on_a_pushed_edit_queues_and_the_rest_stays_jiras() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "PLAT one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    let title = ItemField::Title.number();
    store
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine".into()),
        ))
        .await
        .unwrap();
    let rows = outbox(&store, item.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0, rows[0].1.as_str()), (title, "pending"));
    assert!(
        rows[0].2.contains("PLAT one") && rows[0].3.contains("mine"),
        "{rows:?}"
    );
    let got = store.get_item(item.id, TODAY).await.unwrap().item;
    assert_eq!(
        (got.core.title.as_str(), got.derived.pending_fields.clone()),
        ("mine", vec![ItemField::Title])
    );
    // A second edit replaces the new value and keeps the base.
    store
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine 2".into()),
        ))
        .await
        .unwrap();
    let rows = outbox(&store, item.id).await;
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].2.contains("PLAT one") && rows[0].3.contains("mine 2"),
        "{rows:?}"
    );
    // Back to Jira's value: nothing left to push.
    store
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("PLAT one".into()),
        ))
        .await
        .unwrap();
    assert_eq!(outbox(&store, item.id).await[0].1, "cancelled");
    assert!(
        store
            .get_item(item.id, TODAY)
            .await
            .unwrap()
            .item
            .derived
            .pending_fields
            .is_empty()
    );
    // Done is a transition: the category and the resolution kind.
    store
        .mutate(status_edit(item.id, Status::Done))
        .await
        .unwrap();
    let pending: Vec<u32> = outbox(&store, item.id)
        .await
        .into_iter()
        .filter(|r| r.1 == "pending")
        .map(|r| r.0)
        .collect();
    assert_eq!(
        pending,
        [
            ItemField::StatusCategory.number(),
            ItemField::ResolutionKind.number()
        ]
    );
    // What does not push is still Jira's.
    let err = store
        .mutate(user_edit(
            item.id,
            ItemField::Description,
            FieldValue::Markdown("x".into()),
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::Mutation {
                error: MutationError::LinkedReadOnly(ItemField::Description),
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn undo_of_a_pushed_edit_queues_the_old_value_and_delete_cancels() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "PLAT one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    store
        .mutate(user_edit(
            item.id,
            ItemField::Priority,
            FieldValue::Priority(Priority::High),
        ))
        .await
        .unwrap();
    store
        .undo(UndoInput {
            request_id: new_ulid().to_string(),
            expected_op_id: None,
            now: ts(3_000),
            today: TODAY,
        })
        .await
        .unwrap();
    // Undone back to Jira's value: the queued change is cancelled.
    assert_eq!(outbox(&store, item.id).await[0].1, "cancelled");
    // Redo queues it again.
    store
        .redo(UndoInput {
            request_id: new_ulid().to_string(),
            expected_op_id: None,
            now: ts(3_001),
            today: TODAY,
        })
        .await
        .unwrap();
    assert_eq!(
        outbox(&store, item.id)
            .await
            .iter()
            .filter(|r| r.1 == "pending")
            .count(),
        1
    );
    // A delete cancels what waits.
    let mut del = user_edit(item.id, ItemField::Title, FieldValue::Cleared);
    del.kind = OpKind::Delete;
    del.mutations[0].op = MutationOp::Delete;
    store.mutate(del).await.unwrap();
    assert!(
        outbox(&store, item.id)
            .await
            .iter()
            .all(|r| r.1 == "cancelled")
    );
}

#[tokio::test]
async fn pushes_off_or_a_broken_link_keeps_every_core_field_read_only() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "PLAT one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    let title = || user_edit(item.id, ItemField::Title, FieldValue::String("mine".into()));
    assert!(store.mutate(title()).await.is_err(), "off by default");
    store.set_pushes(Some("work-jira"));
    store
        .reconcile(Reconcile {
            instance_id: "work-jira".into(),
            out_of_scope: vec![(item.id, None)],
            now: ts(1_500),
            today: TODAY,
        })
        .await
        .unwrap();
    assert!(store.mutate(title()).await.is_err(), "a broken link");
    assert!(outbox(&store, item.id).await.is_empty());
}

#[tokio::test]
async fn a_pull_keeps_a_waiting_edit_drops_a_landed_one_and_lets_jira_win_otherwise() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "PLAT one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    store
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine".into()),
        ))
        .await
        .unwrap();
    let pulled = |title: &str, at: i64| RemoteIssue {
        updated_at: ts(at),
        ..issue(1, title)
    };
    let title = |s: &Store| {
        let s = s.clone();
        async move { s.get_item(item.id, TODAY).await.unwrap().item }
    };
    // Jira still at the base (another field changed): the local title stays.
    let mut other = pulled("PLAT one", 300);
    other.priority = Priority::High;
    store.apply_pull(page(vec![other], None)).await.unwrap();
    let got = title(&store).await;
    assert_eq!(
        (got.core.title.as_str(), got.core.priority),
        ("mine", Priority::High)
    );
    assert_eq!(got.derived.pending_fields, [ItemField::Title]);
    // Jira at the local value: the push landed (or someone made it).
    store
        .apply_pull(page(vec![pulled("mine", 400)], None))
        .await
        .unwrap();
    let got = title(&store).await;
    assert!(got.derived.pending_fields.is_empty());
    assert_eq!(outbox(&store, item.id).await[0].1, "done");
    // A new edit, then Jira changes it otherwise: Jira wins, yours is kept.
    store
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine again".into()),
        ))
        .await
        .unwrap();
    store
        .apply_pull(page(vec![pulled("theirs", 500)], None))
        .await
        .unwrap();
    let got = title(&store).await;
    assert_eq!(got.core.title, "theirs");
    assert!(got.derived.pending_fields.is_empty());
    assert!(got.derived.has_conflict);
    let suggestion: (String, String, String, String) = store
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT s.kind, s.rationale, p.expected_old_json, p.new_json
                 FROM suggestions s JOIN suggestion_patches p ON p.suggestion_id = s.id
                 WHERE s.item_id = ?1",
                [item.id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(suggestion.0, "sync_conflict");
    assert!(
        suggestion.1.contains("the title on PLAT-1"),
        "{suggestion:?}"
    );
    assert!(
        suggestion.2.contains("theirs") && suggestion.3.contains("mine again"),
        "{suggestion:?}"
    );
}

#[tokio::test]
async fn an_export_carries_what_waits_to_push_and_the_suggestions() {
    let (_a, source) = open();
    source
        .apply_pull(page(vec![issue(1, "PLAT one")], None))
        .await
        .unwrap();
    let item = by_key(&source, "PLAT-1").await;
    source.set_pushes(Some("work-jira"));
    // A conflict on the title, and a priority still waiting.
    source
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine".into()),
        ))
        .await
        .unwrap();
    source
        .apply_pull(page(
            vec![RemoteIssue {
                updated_at: ts(500),
                ..issue(1, "theirs")
            }],
            None,
        ))
        .await
        .unwrap();
    source
        .mutate(user_edit(
            item.id,
            ItemField::Priority,
            FieldValue::Priority(Priority::High),
        ))
        .await
        .unwrap();
    for history in [true, false] {
        let export = source.export(history).await.unwrap();
        let (_b, target) = open();
        target.import(export.revision, export.rows).await.unwrap();
        let again = target.export(history).await.unwrap();
        let back = source.export(history).await.unwrap();
        let pick = |rows: &[todo_store::ExportRow]| {
            rows.iter()
                .filter_map(|r| match r {
                    todo_store::ExportRow::Outbox(o) => {
                        Some(format!("o {} {} {}", o.field, o.state, o.op_id.is_some()))
                    }
                    todo_store::ExportRow::Suggestion(s) => {
                        Some(format!("s {} {} {}", s.kind, s.state, s.patches.len()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(pick(&again.rows), pick(&back.rows), "history {history}");
        let ours = pick(&back.rows);
        assert!(
            ours.iter()
                .any(|r| r.starts_with("s sync_conflict pending 1")),
            "{ours:?}"
        );
        assert!(
            ours.iter()
                .any(|r| r == &format!("o {} pending {history}", ItemField::Priority.number())),
            "{ours:?}"
        );
        let got = target.get_item(item.id, TODAY).await.unwrap().item;
        assert_eq!(got.derived.pending_fields, [ItemField::Priority]);
        assert!(got.derived.has_conflict);
    }
}

/// The pending Suggestions of `item`: kind and each patch's field number.
async fn suggestions(store: &Store, item: todo_core::ItemId) -> Vec<(String, Vec<u32>)> {
    store
        .read(move |conn| {
            let mut s = conn.prepare("SELECT id, kind FROM suggestions WHERE item_id = ?1 AND state = 'pending' ORDER BY created_at")?;
            let rows: Vec<(String, String)> = s
                .query_map([item.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut out = Vec::new();
            for (id, kind) in rows {
                let mut p = conn.prepare("SELECT field FROM suggestion_patches WHERE suggestion_id = ?1 ORDER BY ord")?;
                let fields = p.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                out.push((kind, fields));
            }
            Ok(out)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn the_status_is_judged_as_one_pair() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    store
        .mutate(status_edit(item.id, Status::Done))
        .await
        .unwrap();
    // Jira moved it to In Progress meanwhile: Jira wins the whole status.
    let mut doing = issue(1, "one");
    doing.status_category = StatusCategory::InProgress;
    doing.updated_at = ts(500);
    store.apply_pull(page(vec![doing], None)).await.unwrap();
    let got = store.get_item(item.id, TODAY).await.unwrap().item;
    assert_eq!(
        (got.status(), got.core.resolution_kind),
        (Status::InProgress, None)
    );
    assert!(
        got.derived.pending_fields.is_empty(),
        "{:?}",
        got.derived.pending_fields
    );
    assert_eq!(
        suggestions(&store, item.id).await,
        [(
            "sync_conflict".to_owned(),
            vec![
                ItemField::StatusCategory.number(),
                ItemField::ResolutionKind.number()
            ]
        )]
    );
}

#[tokio::test]
async fn done_to_cancelled_keeps_a_conflict_when_jira_reopens() {
    let (_dir, store) = open();
    let mut done = issue(1, "one");
    done.status_category = StatusCategory::Done;
    done.resolution_kind = Some(todo_core::ResolutionKind::Done);
    store.apply_pull(page(vec![done], None)).await.unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    store
        .mutate(status_edit(item.id, Status::Cancelled))
        .await
        .unwrap();
    // Only the resolution kind waits; Jira reopens.
    let reopened = RemoteIssue {
        updated_at: ts(500),
        ..issue(1, "one")
    };
    store.apply_pull(page(vec![reopened], None)).await.unwrap();
    let got = store.get_item(item.id, TODAY).await.unwrap().item;
    assert_eq!(got.status(), Status::Next);
    assert!(got.derived.has_conflict);
    assert_eq!(suggestions(&store, item.id).await.len(), 1);
}

#[tokio::test]
async fn labels_compare_as_a_set() {
    let (_dir, store) = open();
    let mut tagged = issue(1, "one");
    tagged.labels = vec!["a".into(), "b".into()];
    store
        .apply_pull(page(vec![tagged.clone()], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    // A reorder is no change to push.
    store
        .mutate(user_edit(
            item.id,
            ItemField::Labels,
            FieldValue::StringSet(vec!["b".into(), "a".into()]),
        ))
        .await
        .unwrap();
    assert!(outbox(&store, item.id).await.is_empty());
    // A new label lands; Jira gives them back sorted: done, not a conflict.
    store
        .mutate(user_edit(
            item.id,
            ItemField::Labels,
            FieldValue::StringSet(vec!["b".into(), "a".into(), "0".into()]),
        ))
        .await
        .unwrap();
    let landed = RemoteIssue {
        labels: vec!["0".into(), "a".into(), "b".into()],
        updated_at: ts(500),
        ..tagged
    };
    store.apply_pull(page(vec![landed], None)).await.unwrap();
    assert_eq!(outbox(&store, item.id).await[0].1, "done");
    assert!(suggestions(&store, item.id).await.is_empty());
}

#[tokio::test]
async fn with_pushes_turned_off_undo_and_delete_still_settle_what_waits() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    store
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine".into()),
        ))
        .await
        .unwrap();
    store.set_pushes(None);
    store
        .undo(UndoInput {
            request_id: new_ulid().to_string(),
            expected_op_id: None,
            now: ts(3_000),
            today: TODAY,
        })
        .await
        .unwrap();
    assert_eq!(outbox(&store, item.id).await[0].1, "cancelled");
    // And the next pull does not bring the undone title back.
    store
        .apply_pull(page(
            vec![RemoteIssue {
                updated_at: ts(500),
                ..issue(1, "one")
            }],
            None,
        ))
        .await
        .unwrap();
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
async fn reconciliation_keeps_a_waiting_edit_as_a_pull_does() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    store
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine".into()),
        ))
        .await
        .unwrap();
    store
        .reconcile(Reconcile {
            instance_id: "work-jira".into(),
            out_of_scope: vec![(item.id, Some(issue(1, "one")))],
            now: ts(1_500),
            today: TODAY,
        })
        .await
        .unwrap();
    let got = store.get_item(item.id, TODAY).await.unwrap().item;
    assert_eq!(
        (got.core.title.as_str(), got.derived.pending_fields.clone()),
        ("mine", vec![ItemField::Title])
    );
}

#[tokio::test]
async fn only_the_pushing_instance_takes_pushed_edits() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("other-jira"));
    assert!(
        store
            .mutate(user_edit(
                item.id,
                ItemField::Title,
                FieldValue::String("mine".into())
            ))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_import_over_one_that_brought_waiting_pushes_replaces_it() {
    let (_a, source) = open();
    source
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    let item = by_key(&source, "PLAT-1").await;
    source.set_pushes(Some("work-jira"));
    source
        .mutate(user_edit(
            item.id,
            ItemField::Title,
            FieldValue::String("mine".into()),
        ))
        .await
        .unwrap();
    let (_b, target) = open();
    for _ in 0..2 {
        // Without the history: only sync's rows and the outbox remain.
        let export = source.export(false).await.unwrap();
        target.import(export.revision, export.rows).await.unwrap();
    }
    assert_eq!(outbox(&target, item.id).await.len(), 1);
}

#[tokio::test]
async fn a_pull_while_a_status_push_waits_leaves_the_item_as_it_was() {
    let (_dir, store) = open();
    store
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    let item = by_key(&store, "PLAT-1").await;
    store.set_pushes(Some("work-jira"));
    store
        .mutate(status_edit(item.id, Status::Done))
        .await
        .unwrap();
    let before = store.get_item(item.id, TODAY).await.unwrap().item;
    // The same issue again (the cursor's overlap): nothing to write.
    let out = store
        .apply_pull(page(vec![issue(1, "one")], None))
        .await
        .unwrap();
    assert_eq!(out.result, None);
    let after = store.get_item(item.id, TODAY).await.unwrap().item;
    assert_eq!(after.revision, before.revision);
    assert_eq!(
        after.derived.staleness.status_entered_at,
        before.derived.staleness.status_entered_at
    );
}
