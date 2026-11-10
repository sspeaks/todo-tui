//! The SuggestionService's store (design 3.11, 2.6): listing what waits,
//! applying chosen patches as one op by the person, and rejecting. P2's
//! Suggestions are the sync kinds, each about one Item.
//!
//! A patch sets one field while it still holds `expected_old`, checked on
//! the Item as the op's earlier patches left it. A Suggestion's status (its
//! category, with the resolution kind when it has one) is one pair: naming
//! either patch applies both, through `SetStatus`, so a linked Item's
//! transition is queued as one. Applying goes through the guard a direct
//! edit does: a value refused there (a field that no longer pushes,
//! someone else's issue unconfirmed) refuses the whole apply.

use std::sync::Arc;

use jiff::Timestamp;
use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};
use todo_core::suggestion::{PatchState, Suggestion, SuggestionState};
use todo_core::{
    Actor, ApplyCtx, FieldUpdate, FieldValue, Item, ItemField, ItemId, MutationOp, MutationResult,
    ResolutionKind, Status, StatusCategory, TodoRefinement, Topic, apply,
};
use ulid::Ulid;

use crate::codec::{self, SqlEnum as _, rev_to_sql, ts_to_sql};
use crate::outbox::{has_pending, load_suggestion, same_value};
use crate::{
    Batch, OpKind, OpRow, REQUEST_SQL, Resource, Result, StoreError, WriteTx, load_items,
    prune_request_ids, recorded_op, write_op,
};

/// The most patches one apply takes, as a batch takes mutations.
pub const MAX_PATCHES: usize = 1000;

/// Input for [`crate::Store::apply_suggestion_patches`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyInput {
    /// Client ULID for idempotency (24h window).
    pub request_id: String,
    /// The patches to apply; a repeat counts once.
    pub patch_ids: Vec<String>,
    /// The person confirmed changing, in the tracker, Items assigned to
    /// someone else (design 5.9).
    pub confirm_not_mine: bool,
    pub now: Timestamp,
    /// "Today" in the user's zone, for `is_overdue` on the returned Items.
    pub today: Date,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyOutput {
    pub result: MutationResult,
    /// The Items the op changed, after it.
    pub items: Vec<Item>,
    /// Each Suggestion a named patch belongs to, as the op left it.
    pub suggestions: Vec<Suggestion>,
    /// The patches named but not applied, in request order: stale, no
    /// longer pending, or on a deleted Item.
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RejectOutput {
    pub suggestion: Suggestion,
    pub result: MutationResult,
}

/// The Suggestions that wait (pending, or partly applied), oldest first:
/// only `item`'s when given. A deleted Item's are left out until an
/// undelete brings it back.
pub(crate) fn list_tx(
    conn: &Connection,
    item: Option<ItemId>,
) -> Result<(Vec<Suggestion>, crate::Revision)> {
    let tx = conn.unchecked_transaction()?;
    let revision = crate::current_revision_in(&tx)?;
    let mut stmt = tx.prepare_cached(
        "SELECT s.id FROM suggestions s JOIN items i ON i.id = s.item_id
         WHERE s.state IN ('pending', 'partially_applied') AND i.deleted_at IS NULL
           AND (?1 IS NULL OR s.item_id = ?1)
         ORDER BY s.created_at, s.id",
    )?;
    let ids = stmt
        .query_map([item.map(|i| i.to_string())], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.extend(load_suggestion(&tx, &id)?);
    }
    Ok((out, revision))
}

/// A patch, as stored.
struct Stored {
    id: String,
    suggestion: String,
    item: ItemId,
    field: ItemField,
    expected_old: FieldValue,
    new: FieldValue,
    pending: bool,
}

const PATCH_SQL: &str =
    "SELECT p.id, p.suggestion_id, s.item_id, p.field, p.expected_old_json, p.new_json, p.state
     FROM suggestion_patches p JOIN suggestions s ON s.id = p.suggestion_id";

fn stored(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, u32, String, String, String)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    ))
}

fn decode(row: (String, String, String, u32, String, String, String)) -> Result<Stored> {
    let (id, suggestion, item, field, old, new, state) = row;
    let field = ItemField::from_number(field)
        .ok_or_else(|| StoreError::Corrupt(format!("patch {id} has field {field}")))?;
    Ok(Stored {
        item: codec::item_id_from_str(&item)?,
        expected_old: codec::value_from_sql(&old)?,
        new: codec::value_from_sql(&new)?,
        pending: state == "pending",
        id,
        suggestion,
        field,
    })
}

fn load_patch(conn: &Connection, id: &str) -> Result<Option<Stored>> {
    conn.prepare_cached(&format!("{PATCH_SQL} WHERE p.id = ?1"))?
        .query_row([id], stored)
        .optional()?
        .map(decode)
        .transpose()
}

/// The pending patches of `suggestion`, in order.
fn pending_of(conn: &Connection, suggestion: &str) -> Result<Vec<Stored>> {
    let mut stmt = conn.prepare_cached(&format!(
        "{PATCH_SQL} WHERE p.suggestion_id = ?1 AND p.state = 'pending' ORDER BY p.ord"
    ))?;
    let rows = stmt
        .query_map([suggestion], stored)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(decode).collect()
}

/// A status patch: the category, or the resolution kind, which apply as a
/// pair.
fn is_status(field: ItemField) -> bool {
    matches!(field, ItemField::StatusCategory | ItemField::ResolutionKind)
}

/// The status a Suggestion's pair sets on `item`: its category (else the
/// Item's), the resolution kind for a close (Done when it has none), and
/// for a to-do the Item's own refinement, as `SetStatus` keeps it.
fn target_status(item: &Item, pair: &[&Stored]) -> Status {
    let category = pair
        .iter()
        .find_map(|p| match p.new {
            FieldValue::StatusCategory(c) => Some(c),
            _ => None,
        })
        .unwrap_or(item.core.status_category);
    let kind = pair.iter().find_map(|p| match p.new {
        FieldValue::ResolutionKind(k) => Some(k),
        _ => None,
    });
    match category {
        StatusCategory::Todo => match item.overlay.todo_refinement {
            TodoRefinement::Inbox => Status::Inbox,
            TodoRefinement::Next => Status::Next,
            TodoRefinement::Someday => Status::Someday,
        },
        StatusCategory::InProgress => Status::InProgress,
        StatusCategory::Done if kind == Some(ResolutionKind::Cancelled) => Status::Cancelled,
        StatusCategory::Done => Status::Done,
    }
}

/// Whether `item`'s `field` still holds `expected` (labels as a set).
fn holds(item: &Item, field: ItemField, expected: &FieldValue) -> bool {
    FieldValue::of(item, field).is_some_and(|v| same_value(&v, expected))
}

fn set_patch(conn: &Connection, id: &str, state: PatchState) -> Result<()> {
    conn.prepare_cached("UPDATE suggestion_patches SET state = ?2 WHERE id = ?1")?
        .execute(params![id, state.as_sql()])?;
    Ok(())
}

/// A Suggestion's state from its patches (design 3.11): partly applied
/// while some wait after one was applied, applied once none waits and one
/// was, rejected when all were rejected, expired when none can apply any
/// more; else it still waits.
fn settle(conn: &Connection, id: &str, at: Timestamp) -> Result<()> {
    let states: Vec<String> = conn
        .prepare_cached("SELECT state FROM suggestion_patches WHERE suggestion_id = ?1")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let count = |s: &str| states.iter().filter(|x| *x == s).count();
    let state = match (count("pending"), count("applied"), count("rejected")) {
        (p, a, _) if p > 0 && a > 0 => SuggestionState::PartiallyApplied,
        (p, _, _) if p > 0 => SuggestionState::Pending,
        (_, a, _) if a > 0 => SuggestionState::Applied,
        (_, _, r) if r > 0 => SuggestionState::Rejected,
        _ => SuggestionState::Expired,
    };
    conn.prepare_cached("UPDATE suggestions SET state = ?2, updated_at = ?3 WHERE id = ?1")?
        .execute(params![id, state.as_sql(), ts_to_sql(at)?])?;
    Ok(())
}

/// `item`'s `has_conflict` and `has_push_failure`, from what waits.
fn set_flags(conn: &Connection, item: &mut Item) -> Result<()> {
    item.derived.has_conflict = has_pending(conn, item.id, "sync_conflict")?;
    item.derived.has_push_failure = has_pending(conn, item.id, "push_failed")?;
    Ok(())
}

pub(crate) fn apply_tx(tx: &mut WriteTx<'_>, input: ApplyInput) -> Result<ApplyOutput> {
    let mut op = OpRow::new(OpKind::ApplySuggestion, Actor::User, &input.request_id)?;
    let today = input.today;
    let mut named: Vec<String> = Vec::new();
    for id in &input.patch_ids {
        if !named.contains(id) {
            named.push(id.clone());
        }
    }
    let conn = tx.conn();
    prune_request_ids(conn, op.at)?;
    if let Some(request_id) = op.request_id
        && let Some((op_id, revision, _)) =
            recorded_op(conn, REQUEST_SQL, request_id, OpKind::ApplySuggestion)?
    {
        return replay(conn, op_id, revision, &named, today);
    }
    // The named patches, then the other half of each status pair named.
    let mut chosen: Vec<Stored> = Vec::new();
    for id in &named {
        let p = load_patch(conn, id)?.ok_or_else(|| StoreError::PatchNotFound(id.clone()))?;
        if is_status(p.field) && p.pending {
            for q in pending_of(conn, &p.suggestion)? {
                if is_status(q.field) && q.id != p.id && !chosen.iter().any(|c| c.id == q.id) {
                    chosen.push(q);
                }
            }
        }
        if !chosen.iter().any(|c| c.id == p.id) {
            chosen.push(p);
        }
    }
    let mut suggestions: Vec<String> = Vec::new();
    for p in &chosen {
        if !suggestions.contains(&p.suggestion) {
            suggestions.push(p.suggestion.clone());
        }
    }
    // A person's op; its actor names the (first) Suggestion applied.
    let actor = Actor::Suggestion(
        suggestions
            .first()
            .and_then(|s| s.parse::<Ulid>().ok())
            .unwrap_or_else(todo_core::new_ulid),
    );
    op.actor = codec::actor_to_sql(actor);
    let ctx = ApplyCtx {
        now: input.now,
        actor,
        pushes: tx.pushes(),
        confirm_not_mine: input.confirm_not_mine,
        local: tx.local(),
    };
    let conn = tx.conn();
    let mut batch = Batch::default();
    let mut applied: Vec<String> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    let mut titles: Vec<(ItemId, String)> = Vec::new();
    let mut items: Vec<ItemId> = Vec::new();
    for p in &chosen {
        if !items.contains(&p.item) {
            items.push(p.item);
        }
    }
    for &item_id in &items {
        let slot = batch.load(conn, item_id, today)?;
        // A deleted Item takes no patch: they wait for an undelete.
        if batch.items[slot].is_deleted() {
            continue;
        }
        let refused = |error| StoreError::PatchRefused { item_id, error };
        let mine: Vec<&Stored> = chosen
            .iter()
            .filter(|p| p.item == item_id && p.pending)
            .collect();
        let before = applied.len();
        // Field patches one at a time, each on the Item as the last left it.
        for p in mine.iter().filter(|p| !is_status(p.field)) {
            if !holds(&batch.items[slot], p.field, &p.expected_old) {
                stale.push(p.id.clone());
                continue;
            }
            let update = MutationOp::Update(vec![FieldUpdate {
                field: p.field,
                value: p.new.clone(),
            }]);
            let a = apply(&batch.items[slot], &update, &ctx).map_err(refused)?;
            if let Some(error) = batch.change_error(conn, item_id, &a.changes, false)? {
                return Err(refused(error));
            }
            batch.record(slot, a);
            applied.push(p.id.clone());
        }
        // Each Suggestion's status pair as one SetStatus.
        let mut pairs: Vec<&str> = Vec::new();
        for p in mine.iter().filter(|p| is_status(p.field)) {
            if !pairs.contains(&p.suggestion.as_str()) {
                pairs.push(&p.suggestion);
            }
        }
        for s in pairs {
            let pair: Vec<&Stored> = mine
                .iter()
                .copied()
                .filter(|p| is_status(p.field) && p.suggestion == s)
                .collect();
            if !pair
                .iter()
                .all(|p| holds(&batch.items[slot], p.field, &p.expected_old))
            {
                stale.extend(pair.iter().map(|p| p.id.clone()));
                continue;
            }
            let target = target_status(&batch.items[slot], &pair);
            let a =
                apply(&batch.items[slot], &MutationOp::SetStatus(target), &ctx).map_err(refused)?;
            batch.record(slot, a);
            applied.extend(pair.iter().map(|p| p.id.clone()));
        }
        if applied.len() > before {
            titles.push((item_id, batch.items[slot].core.title.clone()));
        }
    }
    let at = input.now;
    for id in &applied {
        set_patch(conn, id, PatchState::Applied)?;
    }
    for id in &stale {
        set_patch(conn, id, PatchState::Stale)?;
    }
    // The Suggestions whose patches changed state: the others stay as
    // they were (every patch named had been applied or rejected before).
    let changed: Vec<String> = suggestions
        .iter()
        .filter(|s| {
            chosen
                .iter()
                .any(|p| &p.suggestion == *s && (applied.contains(&p.id) || stale.contains(&p.id)))
        })
        .cloned()
        .collect();
    for id in &changed {
        settle(conn, id, at)?;
    }
    // A refusal re-applied is dealt with (its value queues afresh), and so
    // is one gone stale (its field moved on): neither counts as refused.
    let mut cleared: Vec<String> = Vec::new();
    for p in chosen
        .iter()
        .filter(|p| applied.contains(&p.id) || stale.contains(&p.id))
    {
        if refusal(conn, &p.suggestion)? {
            cleared.extend(crate::outbox::clear_refused(conn, p.item, &[p.field], at)?);
        }
    }
    // Every Item a Suggestion named: what still waits sets its flags.
    for &item_id in &items {
        let slot = batch.slots[&item_id];
        set_flags(conn, &mut batch.items[slot])?;
    }
    op.summary = summary(&titles);
    // What it applied, for a replay's answer and for undo and redo, which
    // reopen and apply them again; and the refusals it cleared, which an
    // undo puts back.
    let report = Report {
        applied: applied.clone(),
        cleared,
    };
    op.report = Some(report.to_sql());
    let (revision, slots) = write_op(tx, &op, &mut batch, today)?;
    let mut last = revision;
    let mut out = Vec::with_capacity(suggestions.len());
    for id in &suggestions {
        if changed.contains(id) {
            last = crate::outbox::record_suggestion(tx, id)?;
        }
        out.extend(load_suggestion(tx.conn(), id)?);
    }
    let conn = tx.conn();
    // The op's revision is the last it allocated, a Suggestion's too.
    conn.prepare_cached("UPDATE ops SET revision = ?2 WHERE id = ?1")?
        .execute(params![op.id.to_string(), rev_to_sql(last)?])?;
    op.insert_request_id(conn, last, None)?;
    let skipped = named
        .into_iter()
        .filter(|id| !applied.contains(id))
        .collect();
    Ok(ApplyOutput {
        result: MutationResult {
            op_id: op.id,
            revision: last,
        },
        items: slots.into_iter().map(|s| batch.items[s].clone()).collect(),
        suggestions: out,
        skipped,
    })
}

/// The op's toast text: `apply suggestion "x"`, `apply suggestions (3
/// items)`, or `apply nothing` when every patch was skipped.
fn summary(titles: &[(ItemId, String)]) -> String {
    match titles {
        [] => "apply nothing".to_owned(),
        [(_, title)] => format!("apply suggestion {}", crate::summary::quoted(title)),
        many => format!("apply suggestions ({} items)", many.len()),
    }
}

/// The patches the apply `op_id` applied, from its report.
/// An apply op's `ops.report`, `{"applied": [patch ids], "cleared":
/// [outbox row ids]}`: the patches it applied, and the refused rows it
/// cleared.
#[derive(Default)]
struct Report {
    applied: Vec<String>,
    cleared: Vec<String>,
}

impl Report {
    fn to_sql(&self) -> String {
        serde_json::json!({ "applied": self.applied, "cleared": self.cleared }).to_string()
    }
}

/// The report of the apply `op_id`.
fn report_of(conn: &Connection, op_id: &str) -> Result<Report> {
    let report: Option<String> = conn
        .prepare_cached("SELECT report FROM ops WHERE id = ?1 AND kind = 'apply_suggestion'")?
        .query_row([op_id], |r| r.get(0))
        .optional()?
        .flatten();
    let Some(report) = report else {
        return Ok(Report::default());
    };
    let v: serde_json::Value = serde_json::from_str(&report)
        .map_err(|e| StoreError::Corrupt(format!("apply report: {e}")))?;
    let ids = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(Report {
        applied: ids("applied"),
        cleared: ids("cleared"),
    })
}

/// The patches the apply `op_id` applied.
fn applied_by(conn: &Connection, op_id: &str) -> Result<Vec<String>> {
    Ok(report_of(conn, op_id)?.applied)
}

/// The original answer of an apply, with the Items and Suggestions as they
/// are now.
fn replay(
    conn: &Connection,
    op_id: Ulid,
    revision: crate::Revision,
    named: &[String],
    today: Date,
) -> Result<ApplyOutput> {
    let mut stmt =
        conn.prepare_cached("SELECT item_id FROM events WHERE op_id = ?1 ORDER BY ord")?;
    let mut items: Vec<ItemId> = Vec::new();
    for id in stmt.query_map([op_id.to_string()], |r| r.get::<_, String>(0))? {
        let id = codec::item_id_from_str(&id?)?;
        if !items.contains(&id) {
            items.push(id);
        }
    }
    let applied = applied_by(conn, &op_id.to_string())?;
    let mut suggestions: Vec<Suggestion> = Vec::new();
    for id in named {
        if let Some(p) = load_patch(conn, id)?
            && !suggestions.iter().any(|s| s.id == p.suggestion)
        {
            suggestions.extend(load_suggestion(conn, &p.suggestion)?);
        }
    }
    Ok(ApplyOutput {
        result: MutationResult { op_id, revision },
        items: load_items(conn, &items, today)?,
        suggestions,
        skipped: named
            .iter()
            .filter(|id| !applied.contains(id))
            .cloned()
            .collect(),
    })
}

/// Undo of an apply (design 3.10): each patch it applied whose field the
/// undo restored waits again, so the pane offers it once more; a redo
/// applies it again. A patch whose field the undo or redo left alone
/// (changed since) keeps its state. (A reject takes the apply off the redo
/// stack, so a redo never finds one rejected.) Sets the flags of the Items in `batch`, and returns the
/// Suggestions changed, to stream once the op is written.
pub(crate) fn on_undo(
    conn: &Connection,
    entry: Ulid,
    redo: bool,
    restored: &std::collections::HashMap<ItemId, Vec<ItemField>>,
    batch: &mut Batch,
    at: Timestamp,
) -> Result<Vec<String>> {
    let (from, to) = if redo {
        ("pending", "applied")
    } else {
        ("applied", "pending")
    };
    let mut changed: Vec<String> = Vec::new();
    // The (Item, field) of each patch it moved: only their refusals follow.
    let mut moved: Vec<(ItemId, ItemField)> = Vec::new();
    for id in applied_by(conn, &entry.to_string())? {
        let Some(p) = load_patch(conn, &id)? else {
            continue;
        };
        // A status pair is restored as one unit: either field stands for it.
        let back = restored.get(&p.item).is_some_and(|fields| {
            fields
                .iter()
                .any(|&f| f == p.field || (is_status(f) && is_status(p.field)))
        });
        if !back {
            continue;
        }
        let n = conn
            .prepare_cached(
                "UPDATE suggestion_patches SET state = ?3 WHERE id = ?1 AND state = ?2",
            )?
            .execute(params![id, from, to])?;
        if n == 1 {
            moved.push((p.item, p.field));
            if !changed.contains(&p.suggestion) {
                changed.push(p.suggestion);
            }
        }
    }
    for id in &changed {
        settle(conn, id, at)?;
    }
    // The refusals the apply cleared count again while their patch waits
    // again; a redo clears them again. Only those of a patch it moved: one
    // whose field a pull moved since keeps its state, and its refusal.
    let cleared: Vec<String> = report_of(conn, &entry.to_string())?
        .cleared
        .into_iter()
        .filter(|row| {
            crate::outbox::row_field(conn, row)
                .ok()
                .flatten()
                .is_some_and(|(item, field)| {
                    moved.iter().any(|&(i, f)| {
                        i == item && (f == field || (is_status(f) && is_status(field)))
                    })
                })
        })
        .collect();
    if redo {
        for id in &cleared {
            conn.prepare_cached(
                "UPDATE outbox SET state = 'cancelled', updated_at = ?2 WHERE id = ?1 AND state = 'failed'",
            )?
            .execute(params![id, ts_to_sql(at)?])?;
        }
    } else {
        crate::outbox::restore_refused(conn, &cleared, at)?;
    }
    for item in restored.keys() {
        if let Some(&slot) = batch.slots.get(item) {
            set_flags(conn, &mut batch.items[slot])?;
        }
    }
    Ok(changed)
}

pub(crate) fn reject_tx(
    tx: &mut WriteTx<'_>,
    request_id: String,
    id: String,
    now: Timestamp,
    today: Date,
) -> Result<RejectOutput> {
    let mut op = OpRow::new(OpKind::RejectSuggestion, Actor::User, &request_id)?;
    op.stackable = false;
    let conn = tx.conn();
    prune_request_ids(conn, op.at)?;
    let not_found = || StoreError::SuggestionNotFound(id.clone());
    if let Some(request_id) = op.request_id
        && let Some((op_id, revision, recorded)) =
            recorded_op(conn, REQUEST_SQL, request_id, OpKind::RejectSuggestion)?
    {
        if recorded.as_deref() != Some(id.as_str()) {
            return Err(StoreError::RequestIdReused);
        }
        return Ok(RejectOutput {
            suggestion: load_suggestion(conn, &id)?.ok_or_else(not_found)?,
            result: MutationResult { op_id, revision },
        });
    }
    let before = load_suggestion(conn, &id)?.ok_or_else(not_found)?;
    op.summary = format!("reject suggestion on {}", title_of(conn, before.item_id)?);
    let waiting = before
        .patches
        .iter()
        .any(|p| p.state == PatchState::Pending);
    let revision = if waiting {
        conn.prepare_cached(
            "UPDATE suggestion_patches SET state = 'rejected' WHERE suggestion_id = ?1 AND state = 'pending'",
        )?
        .execute([&id])?;
        settle(conn, &id, now)?;
        // A refusal let go is dealt with: it no longer counts as failed.
        if refusal(conn, &id)? {
            let fields: Vec<ItemField> = before
                .patches
                .iter()
                .filter(|p| p.state == PatchState::Pending)
                .map(|p| p.field)
                .collect();
            crate::outbox::clear_refused(conn, before.item_id, &fields, now)?;
        }
        // A redo of an apply would set again what was just let go: off the
        // redo stack (design 3.10: only a new entry clears it otherwise).
        discard_redo(conn, &id)?;
        // The Item's markers go with it: a revision of the Item, so every
        // client sees them go.
        let mut item = codec::load_item(conn, &before.item_id.to_string(), Some(today))?
            .ok_or(StoreError::NotFound(before.item_id))?;
        let flags = (item.derived.has_conflict, item.derived.has_push_failure);
        set_flags(conn, &mut item)?;
        if flags != (item.derived.has_conflict, item.derived.has_push_failure) {
            item.revision = tx.next_revision();
            codec::update_item(tx.conn(), &item)?;
            crate::attach(tx.conn(), std::slice::from_mut(&mut item))?;
            let resource = Resource::Item(Arc::new(item.clone()));
            tx.record_change(Topic::Item, &item.id.to_string(), false, Some(resource))?;
        }
        crate::outbox::record_suggestion(tx, &id)?
    } else {
        // Nothing waits: recorded for the request id, with no revision.
        tx.next_revision() - 1
    };
    let conn = tx.conn();
    op.insert(conn, revision, 0)?;
    op.insert_request_id(conn, revision, Some(&id))?;
    Ok(RejectOutput {
        suggestion: load_suggestion(conn, &id)?.ok_or_else(not_found)?,
        result: MutationResult {
            op_id: op.id,
            revision,
        },
    })
}

/// Whether the Suggestion `id` is a push_failed one: Jira refused it.
fn refusal(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT kind = 'push_failed' FROM suggestions WHERE id = ?1")?
        .query_row([id], |r| r.get(0))
        .optional()?
        .unwrap_or(false))
}

/// Takes off the redo stack each undone apply of the Suggestion `id`.
fn discard_redo(conn: &Connection, id: &str) -> Result<()> {
    let undone: Vec<String> = conn
        .prepare_cached(
            "SELECT id FROM ops WHERE kind = 'apply_suggestion' AND undo_state = 'undone'",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for op in undone {
        let mine = applied_by(conn, &op)?
            .iter()
            .filter_map(|p| load_patch(conn, p).ok().flatten())
            .any(|p| p.suggestion == id);
        if mine {
            conn.prepare_cached("UPDATE ops SET undo_state = 'discarded' WHERE id = ?1")?
                .execute([&op])?;
        }
    }
    Ok(())
}

/// The Item's title, quoted as summaries quote it.
fn title_of(conn: &Connection, id: ItemId) -> Result<String> {
    let title: Option<String> = conn
        .prepare_cached("SELECT title FROM items WHERE id = ?1")?
        .query_row([id.to_string()], |r| r.get(0))
        .optional()?;
    Ok(crate::summary::quoted(&title.unwrap_or_default()))
}
