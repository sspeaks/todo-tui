//! The sync engine's writes (design 5.4, 5.5): a pulled page applied as one
//! op of actor `sync` (never an undo entry), the scope reconciliation, and
//! the cursors. What each issue does to its Item is todo_core::remote's.

use std::collections::HashMap;

use jiff::Timestamp;
use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};
use todo_core::remote::{PENDING_PARENT, RemoteIssue, adopt, break_link, import, update};
use todo_core::{
    Actor, ApplyCtx, ArchiveReason, FieldChange, ItemId, MutationOp, MutationResult, apply,
    new_ulid,
};

use crate::codec::{self, ts_from_sql, ts_to_sql};
use crate::{Batch, OpKind, OpRow, Result, StoreError, WriteTx, write_op};

/// One pulled page (design 5.4 step 3): its issues, each with the scopes it
/// matched; parents before their subtasks is not required.
#[derive(Debug, Clone, PartialEq)]
pub struct PullPage {
    pub instance_id: String,
    pub issues: Vec<(RemoteIssue, Vec<String>)>,
    /// The scopes given are each issue's whole membership (reconciliation
    /// listed every scope); otherwise they are added to what an Item has, as
    /// one scope's pull knows nothing of the others, and only
    /// reconciliation takes a scope away (5.5).
    pub replace_scopes: bool,
    /// Moves the scope's cursor once the page is committed (5.4 step 6).
    pub cursor: Option<(String, Timestamp)>,
    pub now: Timestamp,
    pub today: Date,
}

/// What a page did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullOutcome {
    pub imported: Vec<ItemId>,
    pub updated: Vec<ItemId>,
    /// Tombstoned locally: left alone (5.4 step 3).
    pub skipped_deleted: usize,
    /// `None` when the page changed nothing.
    pub result: Option<MutationResult>,
}

/// The Item linked to `(instance, remote_id)`, deleted or not.
pub(crate) fn linked(conn: &Connection, instance: &str, remote_id: &str) -> Result<Option<ItemId>> {
    let id: Option<String> = conn
        .prepare_cached("SELECT id FROM items WHERE remote_instance_id = ?1 AND remote_id = ?2")?
        .query_row(params![instance, remote_id], |r| r.get(0))
        .optional()?;
    id.map(|id| codec::item_id_from_str(&id)).transpose()
}

pub(crate) fn apply_page_tx(tx: &mut WriteTx<'_>, page: PullPage) -> Result<PullOutcome> {
    let pushes = tx.pushes();
    let conn = tx.conn();
    let ctx = ApplyCtx {
        now: page.now,
        actor: Actor::Sync,
        pushes: todo_core::Pushes::None,
        confirm_not_mine: false,
    };
    let mut batch = Batch::default();
    let mut out = PullOutcome::default();
    // Suggestions a conflict made, streamed after the op.
    let mut suggested: Vec<String> = Vec::new();
    // Every issue's Item first, existing or to be imported, so a subtask
    // finds its parent at any depth and in any order (design 3.6).
    let issues = page.issues;
    let mut ids: HashMap<String, (ItemId, bool)> = HashMap::new();
    for (issue, _) in &issues {
        let found = linked(conn, &page.instance_id, &issue.remote_id)?;
        let deleted = match found {
            Some(id) => is_deleted(conn, id)?,
            None => false,
        };
        ids.insert(
            issue.remote_id.clone(),
            (found.unwrap_or_else(new_ulid), deleted),
        );
    }
    let mut created: Vec<ItemId> = Vec::new();
    // New rows are inserted in creation order, under a parent_id foreign
    // key: a new parent goes before its new subtasks, whatever the depth.
    let issues = parents_first(issues, &ids);
    for (issue, scopes) in &issues {
        let parent = live_parent(conn, &page.instance_id, issue, &ids)?;
        let (id, _) = ids[&issue.remote_id];
        match linked(conn, &page.instance_id, &issue.remote_id)? {
            None if created.contains(&id) => {}
            None => {
                let item = import(id, issue, parent, scopes, page.now);
                created.push(id);
                out.imported.push(item.id);
                batch.create(item, page.now);
            }
            Some(id) => {
                let slot = batch.load(conn, id, page.today)?;
                let current = batch.items[slot].clone();
                if current.is_deleted() {
                    out.skipped_deleted += 1;
                    // The cursor moves past this change: once undeleted,
                    // the Item is read again by id (`stale`).
                    forget_pulled(conn, id)?;
                    continue;
                }
                let scopes = if page.replace_scopes {
                    scopes.clone()
                } else {
                    let mut all = current.derived.scopes.clone();
                    all.extend(
                        scopes
                            .iter()
                            .filter(|s| !all.contains(s))
                            .cloned()
                            .collect::<Vec<_>>(),
                    );
                    all
                };
                // Waiting pushes first (design 5.6): the issue as the Item
                // takes it, Jira's or, where a push still waits, the local.
                let settled = crate::outbox::settle_pull(conn, &current, issue, pushes, page.now)?;
                suggested.extend(settled.suggested);
                let (mut next, changes) =
                    update(&current, &settled.issue, parent, &scopes, page.now);
                let mut changes = changes;
                next.derived.pending_fields = crate::outbox::pending_fields(conn, id)?;
                next.derived.has_conflict = crate::outbox::has_pending(conn, id, "sync_conflict")?;
                next.derived.has_push_failure =
                    crate::outbox::has_pending(conn, id, "push_failed")?;
                // Back in a scope, or readable again: a left_scope or
                // remote_unavailable archive lifts (5.5 step 3); one still in
                // no scope is archived left_scope instead, as 5.3 has it.
                let unavailable = next
                    .overlay
                    .archived
                    .is_some_and(|a| a.reason == ArchiveReason::RemoteUnavailable);
                if is_sync_archived(&next) && (!scopes.is_empty() || unavailable) {
                    let a = apply(&next, &MutationOp::Unarchive, &ctx).map_err(|e| sweep(id, e))?;
                    next = a.item;
                    changes.extend(a.changes);
                    let pinned = next.remote_link.as_ref().is_some_and(|l| l.pinned);
                    if scopes.is_empty() && !pinned && !next.status().is_closed() {
                        let a = apply(&next, &MutationOp::Archive(ArchiveReason::LeftScope), &ctx)
                            .map_err(|e| sweep(id, e))?;
                        next = a.item;
                        changes.extend(a.changes);
                    }
                }
                if changes.is_empty() && next == current {
                    continue;
                }
                if !changes.is_empty() {
                    out.updated.push(id);
                }
                record(&mut batch, slot, next, changes);
            }
        }
    }
    // Stored subtasks waiting on an issue this page imported (an earlier
    // page brought them before their parent).
    for &parent in &created {
        let remote = issues
            .iter()
            .find(|(i, _)| ids[&i.remote_id].0 == parent)
            .map(|(i, _)| i.remote_id.clone());
        let Some(remote) = remote else { continue };
        for child in waiting_on(conn, &page.instance_id, &remote)? {
            let slot = batch.load(conn, child, page.today)?;
            if let Some((next, changes)) = adopt(&batch.items[slot], parent, page.now) {
                record(&mut batch, slot, next, changes);
                out.updated.push(child);
            }
        }
    }
    if let Some((scope, at)) = &page.cursor {
        set_cursor(conn, &page.instance_id, scope, *at)?;
    }
    if batch.touched().is_empty() {
        for id in &suggested {
            crate::outbox::record_suggestion(tx, id)?;
        }
        return Ok(out);
    }
    let mut op = OpRow::new(OpKind::Sync, Actor::Sync, "")?;
    op.summary = summary(out.imported.len(), out.updated.len());
    let (mut revision, _) = write_op(tx, &op, &mut batch, page.today)?;
    for id in suggested {
        revision = crate::outbox::record_suggestion(tx, &id)?;
    }
    out.result = Some(MutationResult {
        op_id: op.id,
        revision,
    });
    Ok(out)
}

/// The scope reconciliation of one pass (design 5.5): `out_of_scope` are
/// linked Items no scope lists any more, as fetched (`None` when the issue
/// is gone or forbidden).
#[derive(Debug, Clone, PartialEq)]
pub struct Reconcile {
    pub instance_id: String,
    pub out_of_scope: Vec<(ItemId, Option<RemoteIssue>)>,
    pub now: Timestamp,
    pub today: Date,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub left_scope: Vec<ItemId>,
    pub broken: Vec<ItemId>,
    pub result: Option<MutationResult>,
}

pub(crate) fn reconcile_tx(tx: &mut WriteTx<'_>, input: Reconcile) -> Result<ReconcileOutcome> {
    let pushes = tx.pushes();
    let conn = tx.conn();
    let ctx = ApplyCtx {
        now: input.now,
        actor: Actor::Sync,
        pushes: todo_core::Pushes::None,
        confirm_not_mine: false,
    };
    let mut batch = Batch::default();
    let mut out = ReconcileOutcome::default();
    let mut forget = Vec::new();
    let mut suggested: Vec<String> = Vec::new();
    for (id, fetched) in &input.out_of_scope {
        let slot = batch.load(conn, *id, input.today)?;
        let current = batch.items[slot].clone();
        if current.is_deleted() {
            continue;
        }
        let (mut next, mut changes) = match fetched {
            // Applied as a pull, so a resolution in Jira becomes done; then
            // out of every scope.
            Some(issue) => {
                let parent = live_parent(conn, &input.instance_id, issue, &HashMap::new())?;
                let settled = crate::outbox::settle_pull(conn, &current, issue, pushes, input.now)?;
                suggested.extend(settled.suggested);
                let (mut next, changes) = update(&current, &settled.issue, parent, &[], input.now);
                next.derived.pending_fields = crate::outbox::pending_fields(conn, *id)?;
                next.derived.has_conflict = crate::outbox::has_pending(conn, *id, "sync_conflict")?;
                next.derived.has_push_failure =
                    crate::outbox::has_pending(conn, *id, "push_failed")?;
                (next, changes)
            }
            None => {
                // Read again by id each pass (`stale`), so that it heals
                // once readable, whatever its update time.
                forget.push(*id);
                match break_link(&current) {
                    Some((next, change)) => {
                        out.broken.push(*id);
                        (next, vec![change])
                    }
                    None => (current.clone(), Vec::new()),
                }
            }
        };
        let pinned = next.remote_link.as_ref().is_some_and(|l| l.pinned);
        let reason = match fetched {
            None => Some(ArchiveReason::RemoteUnavailable),
            Some(_) if !pinned && !next.status().is_closed() => Some(ArchiveReason::LeftScope),
            Some(_) => None,
        };
        // Readable again: an unavailable archive no longer says why; gone
        // now: unavailable says it better than left_scope did.
        let stale = match (&next.overlay.archived, reason) {
            (Some(a), Some(r)) => a.reason != r && is_sync_archived(&next),
            (Some(a), None) => a.reason == ArchiveReason::RemoteUnavailable,
            _ => false,
        };
        if stale {
            let a = apply(&next, &MutationOp::Unarchive, &ctx).map_err(|e| sweep(*id, e))?;
            next = a.item;
            changes.extend(a.changes);
        }
        if let Some(reason) = reason
            && next.overlay.archived.is_none()
        {
            let a = apply(&next, &MutationOp::Archive(reason), &ctx).map_err(|e| sweep(*id, e))?;
            next = a.item;
            changes.extend(a.changes);
            if reason == ArchiveReason::LeftScope {
                out.left_scope.push(*id);
            }
        }
        if next != current {
            record(&mut batch, slot, next, changes);
        }
    }
    if batch.touched().is_empty() {
        for id in &suggested {
            crate::outbox::record_suggestion(tx, id)?;
        }
    } else {
        let mut op = OpRow::new(OpKind::Sync, Actor::Sync, "")?;
        op.summary = format!(
            "sync: {} left scope, {} unavailable",
            out.left_scope.len(),
            out.broken.len()
        );
        let (mut revision, _) = write_op(tx, &op, &mut batch, input.today)?;
        for id in &suggested {
            revision = crate::outbox::record_suggestion(tx, id)?;
        }
        out.result = Some(MutationResult {
            op_id: op.id,
            revision,
        });
    }
    // After the op, which writes the link as the Item holds it.
    for id in forget {
        forget_pulled(tx.conn(), id)?;
    }
    Ok(out)
}

/// `issues` ordered so each one comes after its parent issue when that is
/// in the page too (a cycle, which Jira does not make, keeps page order).
fn parents_first(
    issues: Vec<(RemoteIssue, Vec<String>)>,
    ids: &HashMap<String, (ItemId, bool)>,
) -> Vec<(RemoteIssue, Vec<String>)> {
    let parent: HashMap<String, String> = issues
        .iter()
        .filter_map(|(i, _)| Some((i.remote_id.clone(), i.parent_remote_id.clone()?)))
        .filter(|(_, p)| ids.contains_key(p))
        .collect();
    let depth = |id: &str| {
        let (mut at, mut d) = (id, 0usize);
        while let Some(p) = parent.get(at)
            && d < parent.len()
        {
            at = p;
            d += 1;
        }
        d
    };
    let mut keyed: Vec<(usize, (RemoteIssue, Vec<String>))> = issues
        .into_iter()
        .map(|i| (depth(&i.0.remote_id), i))
        .collect();
    keyed.sort_by_key(|(d, _)| *d);
    keyed.into_iter().map(|(_, i)| i).collect()
}

/// The Item `issue`'s parent issue is, from the page's own (`ids`) or the
/// store: `None` when there is none or it is deleted, so a live subtask is
/// never under a tombstone (design 3.7).
fn live_parent(
    conn: &Connection,
    instance: &str,
    issue: &RemoteIssue,
    ids: &HashMap<String, (ItemId, bool)>,
) -> Result<Option<ItemId>> {
    let Some(p) = &issue.parent_remote_id else {
        return Ok(None);
    };
    if let Some(&(id, deleted)) = ids.get(p) {
        return Ok((!deleted).then_some(id));
    }
    match linked(conn, instance, p)? {
        Some(id) if !is_deleted(conn, id)? => Ok(Some(id)),
        _ => Ok(None),
    }
}

fn is_deleted(conn: &Connection, id: ItemId) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT deleted_at IS NOT NULL FROM items WHERE id = ?1")?
        .query_row([id.to_string()], |r| r.get(0))?)
}

/// Archived by sync, not by a person (`z`) or the scheduler.
fn is_sync_archived(item: &todo_core::Item) -> bool {
    item.overlay.archived.is_some_and(|a| {
        matches!(
            a.reason,
            ArchiveReason::LeftScope | ArchiveReason::RemoteUnavailable
        )
    })
}

/// The live linked Items of `instance` waiting on the issue `remote_id` as
/// their parent ([`PENDING_PARENT`]).
fn waiting_on(conn: &Connection, instance: &str, remote_id: &str) -> Result<Vec<ItemId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM items
         WHERE remote_instance_id = ?1 AND deleted_at IS NULL
               AND json_extract(remote_extras, '$.\"' || ?2 || '\"') = ?3",
    )?;
    let rows = stmt.query_map(params![instance, PENDING_PARENT, remote_id], |r| {
        r.get::<_, String>(0)
    })?;
    rows.map(|id| codec::item_id_from_str(&id?)).collect()
}

/// Takes `next` as the slot's Item with `changes` as its events.
fn record(batch: &mut Batch, slot: usize, next: todo_core::Item, changes: Vec<FieldChange>) {
    batch.record(
        slot,
        todo_core::Applied {
            item: next,
            changes,
            skipped: Vec::new(),
        },
    );
}

fn sweep(id: ItemId, e: todo_core::MutationError) -> StoreError {
    StoreError::Corrupt(format!("syncing {id}: {e}"))
}

fn summary(imported: usize, updated: usize) -> String {
    match (imported, updated) {
        (0, n) => format!("sync: {n} updated"),
        (n, 0) => format!("sync: {n} imported"),
        (i, u) => format!("sync: {i} imported, {u} updated"),
    }
}

/// Clears the Item's pulled version: a pull has not brought it up to date
/// (it was deleted) or could not read it (broken). Bookkeeping, outside the
/// op, as the pulled version is.
fn forget_pulled(conn: &Connection, id: ItemId) -> Result<()> {
    conn.prepare_cached("UPDATE items SET remote_last_pulled_version = NULL WHERE id = ?1")?
        .execute([id.to_string()])?;
    Ok(())
}

/// One Item of the tracked set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracked {
    pub id: ItemId,
    pub remote_id: String,
    /// The scopes it matched last.
    pub scopes: Vec<String>,
    /// No pull has brought it up to date since it was deleted, or since its
    /// issue could not be read: it is read again by id, whatever the cursors.
    pub stale: bool,
}

/// The linked, live Items of `instance`: its tracked set (design 5.3), by
/// remote id, with the scopes each matched last.
pub(crate) fn tracked_tx(conn: &Connection, instance: &str) -> Result<Vec<Tracked>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, remote_id, scopes, remote_last_pulled_version IS NULL FROM items
         WHERE remote_instance_id = ?1 AND deleted_at IS NULL ORDER BY seq",
    )?;
    let rows = stmt.query_map([instance], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, bool>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (id, remote_id, scopes, stale) = row?;
        let scopes: Vec<String> =
            serde_json::from_str(&scopes).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        Ok(Tracked {
            id: codec::item_id_from_str(&id)?,
            remote_id,
            scopes,
            stale,
        })
    })
    .collect()
}

pub(crate) fn cursor_tx(
    conn: &Connection,
    instance: &str,
    scope: &str,
) -> Result<Option<Timestamp>> {
    let at: Option<i64> = conn
        .prepare_cached("SELECT cursor_at FROM sync_cursors WHERE instance_id = ?1 AND scope = ?2")?
        .query_row(params![instance, scope], |r| r.get(0))
        .optional()?;
    at.map(ts_from_sql).transpose()
}

/// Never moves a cursor back.
fn set_cursor(conn: &Connection, instance: &str, scope: &str, at: Timestamp) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO sync_cursors (instance_id, scope, cursor_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (instance_id, scope) DO UPDATE SET cursor_at = MAX(cursor_at, excluded.cursor_at)",
    )?
    .execute(params![instance, scope, ts_to_sql(at)?])?;
    Ok(())
}
