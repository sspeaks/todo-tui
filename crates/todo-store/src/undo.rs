//! Undo and redo (design 3.10).
//!
//! The stack lives in `ops.undo_state`: entries are `done` (the undo stack,
//! top = highest `seq`) or `undone` (the redo stack, top = lowest `seq`), and
//! leave both as `dropped` (an undo or redo restored nothing) or `discarded`
//! (a new entry cleared the redo stack). Undo inverts the entry's events, or
//! those of its latest redo; redo inverts the events of its latest undo. Both
//! are ops of their own, linked to the entry by `entry_op`.

use std::collections::HashMap;

use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};
use todo_core::{
    Actor, ApplyCtx, EventBody, FieldValue, InverseStep, ItemField, ItemId, MutationError,
    MutationOp, MutationResult, RecordedEvent, RestoreUnit, Revision, SkipReason, apply, inverse,
};
use ulid::Ulid;

use crate::codec::{self, SqlEnum, ts_from_sql};
use crate::relation::RelationChange;
use crate::{
    Batch, OpKind, OpRow, OpSummaryRow, REQUEST_SQL, Result, StoreError, UndoInput, UndoOutput,
    UndoSkipRow, UndoState, WriteTx, load_items, prune_request_ids, recorded_op, summary, write_op,
};

pub(crate) fn undo_state_tx(conn: &Connection) -> Result<UndoState> {
    let tx = conn.unchecked_transaction()?;
    Ok(UndoState {
        undo: top(&tx, false)?,
        redo: top(&tx, true)?,
    })
}

/// [`crate::Store::undo`], or [`crate::Store::redo`] when `redo`.
pub(crate) fn undo_tx(tx: &mut WriteTx<'_>, input: UndoInput, redo: bool) -> Result<UndoOutput> {
    let kind = if redo { OpKind::Redo } else { OpKind::Undo };
    let mut op = OpRow::new(kind, Actor::User, &input.request_id)?;
    let today = input.today;
    let conn = tx.conn();
    prune_request_ids(conn, op.at)?;
    if let Some(request_id) = op.request_id
        && let Some((op_id, revision, _)) = recorded_op(conn, REQUEST_SQL, request_id, kind)?
    {
        return replay(conn, op_id, revision, today);
    }
    let entry = top(conn, redo)?;
    if let Some(expected) = input.expected_op_id
        && entry.as_ref().map(|e| e.op_id) != Some(expected)
    {
        return Err(StoreError::UndoStackChanged {
            expected,
            top: entry.map(|e| e.op_id),
        });
    }
    op.summary = summary::undo(redo, entry.as_ref().map(|e| e.description.as_str()));
    let Some(entry) = entry else {
        // Recorded all the same, so a late retry cannot act on a newer entry.
        let revision = tx.next_revision() - 1;
        op.insert(conn, revision, 0)?;
        op.insert_request_id(conn, revision, None)?;
        return Ok(UndoOutput {
            result: MutationResult {
                op_id: op.id,
                revision,
            },
            entry: None,
            items: Vec::new(),
            skipped: Vec::new(),
        });
    };
    op.entry_op = Some(entry.op_id);

    let events = load_events(conn, &source_op(conn, entry.op_id, redo)?)?;
    let ctx = ApplyCtx {
        now: input.now,
        actor: Actor::User,
    };
    let mut batch = Batch::default();
    let mut skipped = Vec::new();
    // The Items a content unit was restored on, and what it restored.
    let mut restored: HashMap<ItemId, Restored> = HashMap::new();
    let mut relations_changed = false;
    let mut signals = Vec::new();
    // Deletes and undeletes the parent rule holds back for now.
    let mut held = Vec::new();
    for step in inverse(&events, input.now) {
        match step {
            InverseStep::Restore { item_id, unit } if is_staleness(&unit) => {
                signals.push((item_id, unit));
            }
            InverseStep::Restore { item_id, unit } => {
                match restore(&mut batch, conn, item_id, &unit, &ctx, today)? {
                    Outcome::Restored => {
                        *restored.entry(item_id).or_default() |= Restored::of(&unit)
                    }
                    Outcome::Skipped(skip) => skipped.push(skip),
                    Outcome::Held(skip) => held.push((item_id, unit, skip)),
                }
            }
            InverseStep::AddRelation(relation) | InverseStep::RemoveRelation(relation) => {
                let add = matches!(step, InverseStep::AddRelation(_));
                let reason = match batch.change_relation(conn, relation, add, input.now, today)? {
                    RelationChange::Changed => {
                        relations_changed = true;
                        continue;
                    }
                    RelationChange::Unchanged => SkipReason::ChangedSince,
                    RelationChange::Missing(end) => {
                        SkipReason::Invalid(format!("item {end} does not exist"))
                    }
                };
                skipped.push(UndoSkipRow {
                    item_id: relation.from(),
                    fields: Vec::new(),
                    reason,
                    relation: Some(relation),
                });
            }
        }
    }
    // A step can wait for a later one: a batch may undelete a subtask before
    // its parent, so its undo comes to the parent's delete while the
    // subtask is still live. Retried until a pass restores none of them.
    loop {
        let before = held.len();
        for (item_id, unit, _) in std::mem::take(&mut held) {
            match restore(&mut batch, conn, item_id, &unit, &ctx, today)? {
                Outcome::Restored => *restored.entry(item_id).or_default() |= Restored::of(&unit),
                Outcome::Skipped(skip) => skipped.push(skip),
                Outcome::Held(skip) => held.push((item_id, unit, skip)),
            }
        }
        if held.len() == before {
            break;
        }
    }
    skipped.extend(held.into_iter().map(|(_, _, skip)| skip));
    op.report = Some(report_to_sql(&skipped)?);

    if restored.is_empty() && !relations_changed {
        // Nothing to revert: the entry leaves both stacks, no Item is written.
        set_state(conn, entry.op_id, "dropped")?;
        let revision = tx.next_revision() - 1;
        op.insert(conn, revision, 0)?;
        op.insert_request_id(conn, revision, None)?;
        return Ok(UndoOutput {
            result: MutationResult {
                op_id: op.id,
                revision,
            },
            entry: Some(entry),
            items: Vec::new(),
            skipped,
        });
    }

    // The staleness signals follow the content they belong to; they are
    // never reported. A tombstone is left alone unless this undo deleted it:
    // then it keeps the signals it had before it was undeleted, or a later
    // undo that brings it back would find a touch of an undone op instead.
    for (item_id, unit) in signals {
        let (Some(what), Some(&slot)) = (restored.get(&item_id), batch.slots.get(&item_id)) else {
            continue;
        };
        let deleted_at = batch.items[slot].derived.deleted_at;
        if (deleted_at.is_some() && !what.deleted_at)
            || !unit.restores.iter().all(|r| what.allows(r.field))
        {
            continue;
        }
        // Content restores leave the signals alone, so restoring them on the
        // live Item is restoring them just before this undo deleted it.
        let mut live = batch.items[slot].clone();
        live.derived.deleted_at = None;
        let mut applied = apply(&live, &MutationOp::Restore(vec![unit]), &ctx)
            .map_err(|e| StoreError::Corrupt(format!("restoring {item_id}: {e}")))?;
        if applied.skipped.is_empty() {
            applied.item.derived.deleted_at = deleted_at;
            batch.record(slot, applied);
        }
    }

    let (revision, slots) = write_op(tx, &op, &mut batch, today)?;
    let conn = tx.conn();
    set_state(conn, entry.op_id, if redo { "done" } else { "undone" })?;
    op.insert_request_id(conn, revision, None)?;
    Ok(UndoOutput {
        result: MutationResult {
            op_id: op.id,
            revision,
        },
        entry: Some(entry),
        items: slots.into_iter().map(|s| batch.items[s].clone()).collect(),
        skipped,
    })
}

/// What [`restore`] did with a unit.
enum Outcome {
    Restored,
    Skipped(UndoSkipRow),
    /// Not restored for now: a delete of an Item with live subtasks, or an
    /// undelete under a deleted parent. A later step may change that.
    Held(UndoSkipRow),
}

/// Applies one content unit to `item_id`'s working copy, or says why not.
/// Besides the core checks, a restore must leave references the database
/// can take, and keep every live Item's parent live (design 3.7).
fn restore(
    batch: &mut Batch,
    conn: &Connection,
    item_id: ItemId,
    unit: &RestoreUnit,
    ctx: &ApplyCtx,
    today: Date,
) -> Result<Outcome> {
    let skip = |reason| UndoSkipRow {
        item_id,
        fields: unit.restores.iter().map(|r| r.field).collect(),
        reason,
        relation: None,
    };
    let invalid = |reason: &str| skip(SkipReason::Invalid(reason.to_owned()));
    let slot = match batch.load(conn, item_id, today) {
        Ok(slot) => slot,
        Err(StoreError::NotFound(_)) => return Ok(Outcome::Skipped(invalid("no such Item"))),
        Err(e) => return Err(e),
    };
    let mut applied = apply(
        &batch.items[slot],
        &MutationOp::Restore(vec![unit.clone()]),
        ctx,
    )
    .map_err(|e| StoreError::Corrupt(format!("restoring {item_id}: {e}")))?;
    if let Some(core_skip) = applied.skipped.pop() {
        return Ok(Outcome::Skipped(skip(core_skip.reason)));
    }
    if let Some(error) = batch.change_error(conn, item_id, &applied.changes)? {
        return Ok(Outcome::Skipped(invalid(&match error {
            MutationError::Invalid { reason, .. } | MutationError::Unsupported { reason, .. } => {
                reason
            }
            other => other.to_string(),
        })));
    }
    for change in &applied.changes {
        if change.field != ItemField::DeletedAt {
            continue;
        }
        if change.new == FieldValue::Cleared {
            if let Some(parent) = applied.item.core.parent_id
                && batch.is_deleted(conn, parent)?
            {
                return Ok(Outcome::Held(invalid("the parent is deleted")));
            }
        } else if batch
            .children(conn, item_id)?
            .iter()
            .any(|&(_, deleted)| !deleted)
        {
            return Ok(Outcome::Held(invalid("the Item has live subtasks")));
        }
    }
    batch.record(slot, applied);
    Ok(Outcome::Restored)
}

/// Which content an undo restored on one Item, for its staleness signals.
#[derive(Debug, Default, Clone, Copy)]
struct Restored {
    status: bool,
    waiting: bool,
    /// `deleted_at`: the Item was live during the undo, so if it ends
    /// deleted, the undo deleted it.
    deleted_at: bool,
}

impl Restored {
    fn of(unit: &RestoreUnit) -> Self {
        let has = |f: fn(ItemField) -> bool| unit.restores.iter().any(|r| f(r.field));
        Restored {
            status: has(|f| {
                matches!(
                    f,
                    ItemField::StatusCategory
                        | ItemField::ResolutionKind
                        | ItemField::TodoRefinement
                )
            }),
            waiting: has(|f| f == ItemField::Waiting),
            deleted_at: has(|f| f == ItemField::DeletedAt),
        }
    }

    /// Whether a signal follows: `status_entered_at` a restored status,
    /// `staleness.waiting_since` a restored `waiting`, and the touch signals
    /// any restored content.
    fn allows(self, field: ItemField) -> bool {
        match field {
            ItemField::StalenessStatusEnteredAt => self.status,
            ItemField::StalenessWaitingSince => self.waiting,
            _ => true,
        }
    }
}

impl std::ops::BitOrAssign for Restored {
    fn bitor_assign(&mut self, other: Self) {
        self.status |= other.status;
        self.waiting |= other.waiting;
        self.deleted_at |= other.deleted_at;
    }
}

/// Whether a unit restores only staleness signals.
fn is_staleness(unit: &RestoreUnit) -> bool {
    !unit.restores.is_empty()
        && unit
            .restores
            .iter()
            .all(|r| r.field.parent() == Some(ItemField::Staleness))
}

fn set_state(conn: &Connection, op_id: Ulid, state: &str) -> Result<()> {
    conn.prepare_cached("UPDATE ops SET undo_state = ?2 WHERE id = ?1")?
        .execute(params![op_id.to_string(), state])?;
    Ok(())
}

/// The entry undo (or redo, when `redo`) would act on.
fn top(conn: &Connection, redo: bool) -> Result<Option<OpSummaryRow>> {
    let sql = if redo {
        "SELECT id FROM ops WHERE undo_state = 'undone' ORDER BY seq LIMIT 1"
    } else {
        "SELECT id FROM ops WHERE undo_state = 'done' ORDER BY seq DESC LIMIT 1"
    };
    let id: Option<String> = conn
        .prepare_cached(sql)?
        .query_row([], |r| r.get(0))
        .optional()?;
    id.map(|id| summary_row(conn, &id)).transpose()
}

fn op_id_from_sql(id: &str) -> Result<Ulid> {
    id.parse()
        .map_err(|_| StoreError::Corrupt(format!("bad op id {id:?}")))
}

/// The [`OpSummaryRow`] of op `id`.
fn summary_row(conn: &Connection, id: &str) -> Result<OpSummaryRow> {
    let (kind, at, description) = conn
        .prepare_cached("SELECT kind, at, summary FROM ops WHERE id = ?1")?
        .query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
    Ok(OpSummaryRow {
        op_id: op_id_from_sql(id)?,
        kind: OpKind::from_sql(&kind)?,
        item_ids: item_ids(conn, id)?,
        at: ts_from_sql(at)?,
        description: description.unwrap_or_default(),
    })
}

/// The Items the events of op `id` name, in event order; a relation event
/// names both ends.
fn item_ids(conn: &Connection, id: &str) -> Result<Vec<ItemId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT item_id, kind, old, new FROM events WHERE op_id = ?1 ORDER BY ord, rowid",
    )?;
    let mut rows = stmt.query([id])?;
    let mut ids = Vec::new();
    let mut push = |id: ItemId| {
        if !ids.contains(&id) {
            ids.push(id);
        }
    };
    while let Some(row) = rows.next()? {
        push(codec::item_id_from_str(&row.get::<_, String>(0)?)?);
        let payload: Option<String> = match row.get::<_, String>(1)?.as_str() {
            "relation_add" => row.get(3)?,
            "relation_remove" => row.get(2)?,
            _ => None,
        };
        if let Some(json) = payload {
            push(codec::relation_from_sql(&json)?.to());
        }
    }
    Ok(ids)
}

/// The op whose events undo (or redo, when `redo`) of `entry` inverts: for
/// undo the entry's latest redo, else the entry; for redo its latest undo.
fn source_op(conn: &Connection, entry: Ulid, redo: bool) -> Result<String> {
    let kind = if redo { OpKind::Undo } else { OpKind::Redo };
    let latest: Option<String> = conn
        .prepare_cached(
            "SELECT id FROM ops WHERE entry_op = ?1 AND kind = ?2 ORDER BY seq DESC LIMIT 1",
        )?
        .query_row(params![entry.to_string(), kind.as_sql()], |r| r.get(0))
        .optional()?;
    match latest {
        Some(id) => Ok(id),
        None if !redo => Ok(entry.to_string()),
        None => Err(StoreError::Corrupt(format!(
            "undone entry {entry} has no undo op"
        ))),
    }
}

/// The events of op `id`, in op order.
fn load_events(conn: &Connection, id: &str) -> Result<Vec<RecordedEvent>> {
    let mut stmt = conn.prepare_cached(
        "SELECT item_id, kind, field, old, new FROM events WHERE op_id = ?1 ORDER BY ord, rowid",
    )?;
    let mut rows = stmt.query([id])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let item_id = codec::item_id_from_str(&row.get::<_, String>(0)?)?;
        let kind: String = row.get(1)?;
        let field: Option<u32> = row.get(2)?;
        let old: Option<String> = row.get(3)?;
        let new: Option<String> = row.get(4)?;
        let corrupt = || StoreError::Corrupt(format!("{kind} event of op {id} is incomplete"));
        let body = match kind.as_str() {
            // The payload is the bare title, not a FieldValue.
            "create" => EventBody::Create,
            "set" => EventBody::Set {
                field: field.and_then(ItemField::from_number).ok_or_else(corrupt)?,
                old: codec::value_from_sql(old.as_deref().ok_or_else(corrupt)?)?,
                new: codec::value_from_sql(new.as_deref().ok_or_else(corrupt)?)?,
            },
            "derived" => EventBody::Derived {
                old: codec::staleness_from_sql(old.as_deref().ok_or_else(corrupt)?)?,
                new: codec::staleness_from_sql(new.as_deref().ok_or_else(corrupt)?)?,
            },
            "relation_add" => EventBody::RelationAdd(codec::relation_from_sql(
                new.as_deref().ok_or_else(corrupt)?,
            )?),
            "relation_remove" => EventBody::RelationRemove(codec::relation_from_sql(
                old.as_deref().ok_or_else(corrupt)?,
            )?),
            _ => {
                return Err(StoreError::Corrupt(format!(
                    "unknown event kind {kind:?} in op {id}"
                )));
            }
        };
        events.push(RecordedEvent { item_id, body });
    }
    Ok(events)
}

/// The original response of an undo or redo op, with the Items' current
/// state.
fn replay(conn: &Connection, op_id: Ulid, revision: Revision, today: Date) -> Result<UndoOutput> {
    let id = op_id.to_string();
    let (entry_op, report): (Option<String>, Option<String>) = conn
        .prepare_cached("SELECT entry_op, report FROM ops WHERE id = ?1")?
        .query_row([&id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(UndoOutput {
        result: MutationResult { op_id, revision },
        entry: entry_op.map(|e| summary_row(conn, &e)).transpose()?,
        items: load_items(conn, &item_ids(conn, &id)?, today)?,
        skipped: report
            .as_deref()
            .map(report_from_sql)
            .transpose()?
            .unwrap_or_default(),
    })
}

/// `ops.report`: the skipped changes as a JSON array.
fn report_to_sql(skipped: &[UndoSkipRow]) -> Result<String> {
    let rows: Vec<serde_json::Value> = skipped
        .iter()
        .map(|s| {
            let (reason, message) = match &s.reason {
                SkipReason::ChangedSince => ("changed_since", None),
                SkipReason::ItemDeleted => ("item_deleted", None),
                SkipReason::Invalid(message) => ("invalid", Some(message.as_str())),
            };
            serde_json::json!({
                "item_id": s.item_id.to_string(),
                "fields": s.fields,
                "reason": reason,
                "message": message,
                "relation": s.relation,
            })
        })
        .collect();
    serde_json::to_string(&rows).map_err(|e| StoreError::Corrupt(e.to_string()))
}

fn report_from_sql(json: &str) -> Result<Vec<UndoSkipRow>> {
    let corrupt = || StoreError::Corrupt(format!("bad undo report {json:?}"));
    let rows: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|_| corrupt())?;
    rows.into_iter()
        .map(|row| {
            let item_id = row["item_id"].as_str().ok_or_else(corrupt)?;
            let fields = serde_json::from_value(row["fields"].clone()).map_err(|_| corrupt())?;
            let reason = match row["reason"].as_str() {
                Some("changed_since") => SkipReason::ChangedSince,
                Some("item_deleted") => SkipReason::ItemDeleted,
                Some("invalid") => {
                    SkipReason::Invalid(row["message"].as_str().unwrap_or_default().to_owned())
                }
                _ => return Err(corrupt()),
            };
            let relation =
                serde_json::from_value(row["relation"].clone()).map_err(|_| corrupt())?;
            Ok(UndoSkipRow {
                item_id: codec::item_id_from_str(item_id)?,
                fields,
                reason,
                relation,
            })
        })
        .collect()
}
