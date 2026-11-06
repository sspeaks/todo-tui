//! The scheduler's archive of done Items (design 3.7): the `auto_done`
//! archive and its healing, as system ops that are never undo stack
//! entries.

use rusqlite::{Connection, params};
use todo_core::{
    Actor, ApplyCtx, ArchiveReason, ItemField, ItemId, MutationError, MutationOp, MutationResult,
    apply,
};

use crate::codec::{self, SqlEnum, ts_to_sql};
use crate::{
    AutoArchiveInput, AutoArchiveOutput, Batch, OpKind, OpRow, Result, StoreError, WriteTx,
    summary, write_op,
};

/// The most Items one [`crate::Store::auto_archive`] op changes.
pub(crate) const AUTO_ARCHIVE_LIMIT: usize = 1000;

/// [`crate::Store::auto_archive`].
pub(crate) fn auto_archive_tx(
    tx: &mut WriteTx<'_>,
    input: AutoArchiveInput,
) -> Result<AutoArchiveOutput> {
    let conn = tx.conn();
    // Healing first: an Item open again is in Archive by mistake now, while
    // a done one can wait for the next op.
    let mut healed = reopened(conn, AUTO_ARCHIVE_LIMIT + 1)?;
    let mut more = healed.len() > AUTO_ARCHIVE_LIMIT;
    healed.truncate(AUTO_ARCHIVE_LIMIT);
    let room = AUTO_ARCHIVE_LIMIT - healed.len();
    let mut archived = if more {
        Vec::new()
    } else {
        // In the columns' nanoseconds; an `after` past their range is a
        // cutoff before (or after) every row.
        let cutoff = i128::from(ts_to_sql(input.now)?) - input.after.as_nanos();
        let cutoff = i64::try_from(cutoff).unwrap_or(if cutoff < 0 { i64::MIN } else { i64::MAX });
        closed_before(conn, cutoff, room + 1)?
    };
    more |= archived.len() > room;
    archived.truncate(room);
    if healed.is_empty() && archived.is_empty() {
        return Ok(AutoArchiveOutput {
            archived,
            healed,
            more,
            result: None,
        });
    }

    let ctx = ApplyCtx {
        now: input.now,
        actor: Actor::System,
        pushes: todo_core::Pushes::None,
        confirm_not_mine: false,
    };
    let unarchive = MutationOp::Unarchive;
    let archive = MutationOp::Archive(ArchiveReason::AutoDone);
    let mut batch = Batch::default();
    for (ids, op) in [(&healed, &unarchive), (&archived, &archive)] {
        for &id in ids {
            let slot = batch.load(conn, id, input.today)?;
            let applied = apply(&batch.items[slot], op, &ctx).map_err(|e| sweep_error(id, e))?;
            batch.record(slot, applied);
        }
    }
    let mut op = OpRow::new(OpKind::AutoArchive, Actor::System, "")?;
    op.summary = summary::auto_archive(archived.len(), healed.len());
    let (revision, _) = write_op(tx, &op, &mut batch, input.today)?;
    Ok(AutoArchiveOutput {
        archived,
        healed,
        more,
        result: Some(MutationResult {
            op_id: op.id,
            revision,
        }),
    })
}

/// The scan only selects rows `apply` takes: a live, unarchived Item to
/// archive, a live, archived one to unarchive.
fn sweep_error(id: ItemId, error: MutationError) -> StoreError {
    StoreError::Corrupt(format!("auto-archiving {id}: {error}"))
}

/// Up to `limit` live Items archived `auto_done` that are open again (an
/// undo of their completion, or an edit that reopened them without
/// `SetStatus`), in archive order.
fn reopened(conn: &Connection, limit: usize) -> Result<Vec<ItemId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM items
         WHERE archived_reason = 'auto_done' AND status_category <> 'done'
               AND deleted_at IS NULL
         ORDER BY archived_at, seq LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| r.get::<_, String>(0))?;
    rows.map(|id| codec::item_id_from_str(&id?)).collect()
}

/// Up to `limit` live, unarchived done or cancelled Items that closed at or
/// before `cutoff` (nanoseconds), oldest first: `status_entered_at`, or
/// `updated_at` for a row written before it was recorded.
///
/// An Item whose archive the user or sync has set since it closed (a `z`,
/// either way) keeps that decision until it closes again: a `set` event on
/// `archived` at or after the close skips it. Only a decision still in
/// effect counts. The scheduler's own events do not, so an Item healed and
/// then closed again by a redo, back at its old close time, is archived
/// again rather than stuck. Nor do those of an undo or redo, which only
/// restore, or of an op that is undone (on the redo stack, or discarded
/// from it): a `z` and its undo leave the Item to the sweep. A redone `z`
/// counts again, as its redo puts its op back on the undo stack. Events are
/// stamped with the wall clock, as is the `now` the daemon gives its ops
/// (and so `status_entered_at`); an op that closes and archives an Item
/// stamps its event after the close.
fn closed_before(conn: &Connection, cutoff: i64, limit: usize) -> Result<Vec<ItemId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM items
         WHERE status_category = 'done' AND archived_at IS NULL AND deleted_at IS NULL
               AND COALESCE(status_entered_at, updated_at) <= ?1
               AND NOT EXISTS (
                   SELECT 1 FROM events e JOIN ops o ON o.id = e.op_id
                   WHERE e.item_id = items.id
                         AND e.at >= COALESCE(items.status_entered_at, items.updated_at)
                         AND e.kind = 'set' AND e.field = ?2 AND e.actor <> 'system'
                         AND o.kind NOT IN (?4, ?5)
                         AND COALESCE(o.undo_state, '') NOT IN ('undone', 'discarded'))
         ORDER BY COALESCE(status_entered_at, updated_at), seq LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![
            cutoff,
            ItemField::Archived.number(),
            limit as i64,
            OpKind::Undo.as_sql(),
            OpKind::Redo.as_sql(),
        ],
        |r| r.get::<_, String>(0),
    )?;
    rows.map(|id| codec::item_id_from_str(&id?)).collect()
}
