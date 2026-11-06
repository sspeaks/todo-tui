//! The outbox (design 5.6, ADR-0007): a person's edit of a pushed field of a
//! linked Item, queued in the op's own transaction for the push worker.
//! Each (Item, field) has at most one waiting row: a later edit replaces its
//! `new`, keeps its `base`, and an edit back to the base cancels it. While
//! a row is in flight its `sent` is the value on its way to Jira, and an
//! edit meanwhile changes `new` only: once Jira reads back at `sent`, the
//! row's base moves there and `new` is pushed in turn.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use todo_core::suggestion::{Patch, PatchState, Suggestion, SuggestionKind, SuggestionState};
use todo_core::{FieldValue, Item, ItemField, new_ulid, pushed};

use crate::codec::{self, SqlEnum as _, ts_to_sql};
use crate::{NewEvent, Result};

/// The fields one transition sends: the category and how it was resolved.
fn kind_of(field: ItemField) -> &'static str {
    match field {
        ItemField::StatusCategory | ItemField::ResolutionKind => "transition",
        _ => "update",
    }
}

/// Queues the pushed changes of `events` on the linked Items in `items`
/// (by id). Called for an op by a person: an edit, a suggestion applied,
/// an undo or a redo (which restore Core without the guard).
pub(crate) fn enqueue(
    conn: &Connection,
    op_id: &str,
    events: &[NewEvent],
    item: impl Fn(todo_core::ItemId) -> Option<Item>,
    create: todo_core::Pushes,
    at: Timestamp,
) -> Result<Vec<todo_core::ItemId>> {
    let now = ts_to_sql(at)?;
    let mut touched = Vec::new();
    for event in events {
        let NewEvent::Set { item_id, change } = event else {
            continue;
        };
        if !pushed(change.field) {
            continue;
        }
        let Some(item) = item(*item_id) else { continue };
        let Some(link) = &item.remote_link else {
            continue;
        };
        let id = item_id.to_string();
        let field = change.field.number();
        let waiting: Option<(String, String, Option<String>)> = conn
            .prepare_cached(
                "SELECT id, base_json, sent_json FROM outbox
                 WHERE item_id = ?1 AND field = ?2 AND state IN ('pending', 'in_flight')",
            )?
            .query_row(params![id, field], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let new = codec::value_to_sql(&change.new)?;
        // Labels are a set (Jira keeps them sorted): a reorder is no change.
        if same_value(&change.old, &change.new) {
            continue;
        }
        match waiting {
            // Back where Jira has it, and nothing on its way: nothing to push.
            Some((row, base, None)) if same_value(&codec::value_from_sql(&base)?, &change.new) => {
                conn.prepare_cached(
                    "UPDATE outbox SET state = 'cancelled', updated_at = ?2 WHERE id = ?1",
                )?
                .execute(params![row, now])?;
            }
            // A later value. One in flight keeps what it sent: the read-back
            // of that moves its base, and this is pushed next (even back to
            // the old base).
            Some((row, _, _)) => {
                conn.prepare_cached(
                    "UPDATE outbox SET new_json = ?2, op_id = ?3, attempts = 0,
                            next_attempt_at = NULL, last_error = NULL, updated_at = ?4
                     WHERE id = ?1",
                )?
                .execute(params![row, new, op_id, now])?;
            }
            // A new row only for the instance that pushes, a whole link.
            None if create != todo_core::Pushes::instance(&link.instance_id)
                || link.link_state != todo_core::LinkState::Ok =>
            {
                continue;
            }
            None => {
                conn.prepare_cached(
                    "INSERT INTO outbox (id, item_id, instance_id, op_id, kind, field, base_json,
                                         new_json, state, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?9)",
                )?
                .execute(params![
                    new_ulid().to_string(),
                    id,
                    link.instance_id,
                    op_id,
                    kind_of(change.field),
                    field,
                    codec::value_to_sql(&change.old)?,
                    new,
                    now
                ])?;
            }
        }
        if !touched.contains(item_id) {
            touched.push(*item_id);
        }
    }
    Ok(touched)
}

/// Equal as values a push compares: labels as a set.
pub(crate) fn same_value(a: &FieldValue, b: &FieldValue) -> bool {
    match (a, b) {
        (FieldValue::StringSet(x), FieldValue::StringSet(y)) => {
            let (mut x, mut y) = (x.clone(), y.clone());
            x.sort();
            y.sort();
            x == y
        }
        _ => a == b,
    }
}

/// Cancels what `item` has waiting: it was deleted (design 3.7), so its
/// edits never reach the tracker; an undelete does not revive them.
pub(crate) fn cancel_item(conn: &Connection, item: todo_core::ItemId, at: Timestamp) -> Result<()> {
    conn.prepare_cached(
        "UPDATE outbox SET state = 'cancelled', updated_at = ?2
         WHERE item_id = ?1 AND state = 'pending'",
    )?
    .execute(params![item.to_string(), ts_to_sql(at)?])?;
    Ok(())
}

/// The pushed fields `item` has waiting, in field order: its
/// `pending_fields` (design 3.2).
pub(crate) fn pending_fields(conn: &Connection, item: todo_core::ItemId) -> Result<Vec<ItemField>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT field FROM outbox
         WHERE item_id = ?1 AND state IN ('pending', 'in_flight') ORDER BY field",
    )?;
    let rows = stmt.query_map([item.to_string()], |r| r.get::<_, u32>(0))?;
    let mut out = Vec::new();
    for n in rows {
        if let Some(f) = ItemField::from_number(n?) {
            out.push(f);
        }
    }
    Ok(out)
}

/// A waiting patch, as the pull's conflict rule and the worker read it.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    pub id: String,
    pub instance_id: String,
    pub field: ItemField,
    pub base: FieldValue,
    pub new: FieldValue,
    /// In flight: the value sent.
    pub sent: Option<FieldValue>,
}

/// What `item` has waiting (pending or in flight).
pub(crate) fn waiting(conn: &Connection, item: todo_core::ItemId) -> Result<Vec<Waiting>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, instance_id, field, base_json, new_json, sent_json FROM outbox
         WHERE item_id = ?1 AND state IN ('pending', 'in_flight') ORDER BY field",
    )?;
    let rows = stmt.query_map([item.to_string()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u32>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, instance_id, field, base, new, sent) = row?;
        let Some(field) = ItemField::from_number(field) else {
            continue;
        };
        out.push(Waiting {
            id,
            instance_id,
            field,
            base: codec::value_from_sql(&base)?,
            new: codec::value_from_sql(&new)?,
            sent: sent.as_deref().map(codec::value_from_sql).transpose()?,
        });
    }
    Ok(out)
}

/// The rows the worker is about to send, each with the value it sends
/// (`new`, or what Jira will read it back as: a priority Jira has no name
/// of its own for). A row edited since it was read is left out: its new
/// value goes on the next try. Returns the ids now in flight.
pub(crate) fn begin(
    conn: &Connection,
    rows: &[(Waiting, FieldValue)],
    at: Timestamp,
) -> Result<Vec<String>> {
    let now = ts_to_sql(at)?;
    let mut begun = Vec::new();
    for (w, send) in rows {
        let send = codec::value_to_sql(send)?;
        let n = conn
            .prepare_cached(
                "UPDATE outbox SET state = 'in_flight', sent_json = ?3, new_json = ?3, updated_at = ?4
                 WHERE id = ?1 AND new_json = ?2 AND state IN ('pending', 'in_flight')",
            )?
            .execute(params![w.id, codec::value_to_sql(&w.new)?, send, now])?;
        if n == 1 {
            begun.push(w.id.clone());
        }
    }
    Ok(begun)
}

/// The earliest a retry of `instance`'s outbox is due, if one waits.
pub(crate) fn next_attempt(conn: &Connection, instance: &str) -> Result<Option<Timestamp>> {
    let at: Option<i64> = conn
        .prepare_cached(
            "SELECT MIN(next_attempt_at) FROM outbox
             WHERE instance_id = ?1 AND state IN ('pending', 'in_flight')",
        )?
        .query_row([instance], |r| r.get(0))?;
    at.map(codec::ts_from_sql).transpose()
}

/// A row in flight whose push Jira took, while `new` moved on: Jira is at
/// what was sent, so that is the base `new` is pushed from.
fn rebase(conn: &Connection, w: &Waiting, at: Timestamp) -> Result<()> {
    let base = w.sent.as_ref().unwrap_or(&w.base);
    conn.prepare_cached(
        "UPDATE outbox SET base_json = ?2, sent_json = NULL, state = 'pending', updated_at = ?3
         WHERE id = ?1",
    )?
    .execute(params![w.id, codec::value_to_sql(base)?, ts_to_sql(at)?])?;
    Ok(())
}

/// Ends a waiting row: `done` (Jira has it) or `cancelled` (Jira won).
pub(crate) fn settle(conn: &Connection, id: &str, state: &str, at: Timestamp) -> Result<()> {
    conn.prepare_cached("UPDATE outbox SET state = ?2, updated_at = ?3 WHERE id = ?1")?
        .execute(params![id, state, ts_to_sql(at)?])?;
    Ok(())
}

/// A `sync_conflict` Suggestion (design 3.11, 5.6): Jira changed `field`
/// while a local edit of it waited, and won. Its one patch, unchecked in
/// the pane, sets the local value again (`expected_old` Jira's).
pub(crate) fn conflict(
    conn: &Connection,
    item: todo_core::ItemId,
    field: ItemField,
    jira: &FieldValue,
    mine: &FieldValue,
    rationale: &str,
    at: Timestamp,
) -> Result<String> {
    let now = ts_to_sql(at)?;
    let id = new_ulid().to_string();
    conn.prepare_cached(
        "INSERT INTO suggestions (id, item_id, kind, source, rationale, state, created_at, updated_at)
         VALUES (?1, ?2, 'sync_conflict', 'sync', ?3, 'pending', ?4, ?4)",
    )?
    .execute(params![
        id,
        item.to_string(),
        rationale,
        now
    ])?;
    conn.prepare_cached(
        "INSERT INTO suggestion_patches (id, suggestion_id, ord, field, expected_old_json, new_json, state)
         VALUES (?1, ?2, 0, ?3, ?4, ?5, 'pending')",
    )?
    .execute(params![
        new_ulid().to_string(),
        id,
        field.number(),
        codec::value_to_sql(jira)?,
        codec::value_to_sql(mine)?
    ])?;
    Ok(id)
}

/// Whether `item` has a pending Suggestion of `kind`: `has_conflict` and
/// `has_push_failure` (design 3.2).
pub(crate) fn has_pending(conn: &Connection, item: todo_core::ItemId, kind: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM suggestions WHERE item_id = ?1 AND kind = ?2 AND state = 'pending')",
        )?
        .query_row(params![item.to_string(), kind], |r| r.get(0))?)
}

/// A pushed field in a person's words.
pub(crate) fn field_word(field: ItemField) -> &'static str {
    match field {
        ItemField::Title => "the title",
        ItemField::Priority => "the priority",
        ItemField::Labels => "the labels",
        ItemField::DueDate => "the due date",
        ItemField::StatusCategory | ItemField::ResolutionKind => "the status",
        _ => "a field",
    }
}

/// The Suggestion `id`, with its patches in order.
pub(crate) fn load_suggestion(conn: &Connection, id: &str) -> Result<Option<Suggestion>> {
    let row = conn
        .prepare_cached(
            "SELECT item_id, kind, rationale, error, state, created_at FROM suggestions WHERE id = ?1",
        )?
        .query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .optional()?;
    let Some((item, kind, rationale, error, state, created)) = row else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(
        "SELECT id, field, expected_old_json, new_json, state FROM suggestion_patches
         WHERE suggestion_id = ?1 ORDER BY ord",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut patches = Vec::new();
    for row in rows {
        let (pid, field, old, new, state) = row?;
        let Some(field) = ItemField::from_number(field) else {
            continue;
        };
        patches.push(Patch {
            id: pid,
            field,
            expected_old: codec::value_from_sql(&old)?,
            new: codec::value_from_sql(&new)?,
            state: PatchState::from_sql(&state)?,
        });
    }
    Ok(Some(Suggestion {
        id: id.to_owned(),
        item_id: codec::item_id_from_str(&item)?,
        kind: SuggestionKind::from_sql(&kind)?,
        rationale,
        error,
        state: SuggestionState::from_sql(&state)?,
        patches,
        created_at: codec::ts_from_sql(created)?,
    }))
}

/// The outbox's counts for `instance`: waiting, and refused (design 5.11).
pub(crate) fn counts(conn: &Connection, instance: &str) -> Result<(u32, u32)> {
    Ok(conn
        .prepare_cached(
            "SELECT COALESCE(SUM(state IN ('pending', 'in_flight')), 0), COALESCE(SUM(state = 'failed'), 0)
             FROM outbox WHERE instance_id = ?1",
        )?
        .query_row([instance], |r| Ok((r.get(0)?, r.get(1)?)))?)
}

/// Streams the Suggestion `id` as it is now, on `TOPIC_SUGGESTION`.
pub(crate) fn record_suggestion(tx: &mut crate::WriteTx<'_>, id: &str) -> Result<crate::Revision> {
    let s = load_suggestion(tx.conn(), id)?;
    tx.record_change(
        todo_core::Topic::Suggestion,
        id,
        s.is_none(),
        s.map(|s| crate::Resource::Suggestion(std::sync::Arc::new(s))),
    )
}

/// What [`settle_pull`] decided for an Item's waiting pushes.
pub(crate) struct Settled {
    /// The issue as the Item should take it: Jira's, with each field whose
    /// push still waits (Jira still at its base) holding the local value.
    pub issue: todo_core::remote::RemoteIssue,
    /// Suggestions made (Jira won a conflict), to stream after the op.
    pub suggested: Vec<String>,
}

/// The conflict rule (design 5.6) for `item`'s waiting pushes against the
/// pulled `issue`, before the pull applies it. Per field: Jira still at the
/// base keeps the local value; Jira at the local value has it (the push
/// landed, or someone made the same change); Jira elsewhere wins, and the
/// local value becomes a `sync_conflict` Suggestion. The status (category
/// and resolution kind) is judged as one pair, as one transition sets it.
/// Labels compare as sets: Jira keeps them sorted.
///
/// A row in flight is judged on what was sent: Jira at it means the push
/// landed, and a later edit (`new` moved on) is pushed from there. Rows of
/// an instance that does not push (`pushes`, the kill switch) are left
/// waiting and the pull applies Jira's values: they go when it pushes again.
pub(crate) fn settle_pull(
    conn: &Connection,
    item: &Item,
    issue: &todo_core::remote::RemoteIssue,
    pushes: todo_core::Pushes,
    at: Timestamp,
) -> Result<Settled> {
    use todo_core::remote::RemoteIssue;
    let mut out = Settled {
        issue: issue.clone(),
        suggested: Vec::new(),
    };
    let rows: Vec<Waiting> = waiting(conn, item.id)?
        .into_iter()
        .filter(|w| pushes == todo_core::Pushes::instance(&w.instance_id))
        .collect();
    if rows.is_empty() {
        return Ok(out);
    }
    let key = item
        .remote_link
        .as_ref()
        .map(|l| l.key.clone())
        .unwrap_or_default();
    let jira_of = |i: &RemoteIssue, field: ItemField| -> Option<FieldValue> {
        Some(match field {
            ItemField::Title => FieldValue::String(i.title.clone()),
            ItemField::Priority => FieldValue::Priority(i.priority),
            ItemField::Labels => FieldValue::StringSet(i.labels.clone()),
            ItemField::DueDate => i.due_date.map_or(FieldValue::Cleared, FieldValue::Date),
            ItemField::StatusCategory => FieldValue::StatusCategory(i.status_category),
            ItemField::ResolutionKind => i
                .resolution_kind
                .filter(|_| i.status_category == todo_core::StatusCategory::Done)
                .map_or(FieldValue::Cleared, FieldValue::ResolutionKind),
            _ => return None,
        })
    };
    let same = same_value;
    // The status pair: a field not queued holds its pre-edit value, which
    // is the Item's (only a push or a pull changes it on a linked Item).
    let status = [ItemField::StatusCategory, ItemField::ResolutionKind];
    let in_pair = |f: ItemField| status.contains(&f);
    let pair: Vec<&Waiting> = rows.iter().filter(|w| in_pair(w.field)).collect();
    let mut units: Vec<Vec<&Waiting>> = rows
        .iter()
        .filter(|w| !in_pair(w.field))
        .map(|w| vec![w])
        .collect();
    if !pair.is_empty() {
        units.push(pair);
    }
    for unit in units {
        let value = |w: Option<&&Waiting>, f: ItemField, pick: fn(&Waiting) -> &FieldValue| {
            w.map(|w| pick(w).clone())
                .or_else(|| FieldValue::of(item, f))
                .unwrap_or(FieldValue::Cleared)
        };
        let fields: Vec<ItemField> = if in_pair(unit[0].field) {
            status.to_vec()
        } else {
            vec![unit[0].field]
        };
        let find = |f: ItemField| unit.iter().find(|w| w.field == f);
        let base: Vec<FieldValue> = fields
            .iter()
            .map(|&f| value(find(f), f, |w| &w.base))
            .collect();
        let new: Vec<FieldValue> = fields
            .iter()
            .map(|&f| value(find(f), f, |w| &w.new))
            .collect();
        // What Jira has once the push lands: what was sent, while in flight.
        let landed: Vec<FieldValue> = fields
            .iter()
            .map(|&f| value(find(f), f, |w| w.sent.as_ref().unwrap_or(&w.new)))
            .collect();
        let moved_on = unit
            .iter()
            .any(|w| w.sent.as_ref().is_some_and(|s| !same(s, &w.new)));
        let jira: Vec<FieldValue> = fields
            .iter()
            .map(|&f| jira_of(issue, f).unwrap_or(FieldValue::Cleared))
            .collect();
        let all = |a: &[FieldValue], b: &[FieldValue]| a.iter().zip(b).all(|(x, y)| same(x, y));
        if all(&jira, &base) && all(&new, &base) {
            // Edited back to where Jira is (after an edit that was sent):
            // nothing left to push.
            for w in &unit {
                settle(conn, &w.id, "done", at)?;
            }
        } else if all(&jira, &landed) && moved_on {
            // The push landed and the person edited again meanwhile: that
            // goes next, from where Jira is now.
            for w in &unit {
                rebase(conn, w, at)?;
            }
            for (f, v) in fields.iter().zip(&new) {
                put(&mut out.issue, *f, v);
            }
        } else if all(&jira, &base) {
            // Still waiting: the local value stands over the pull.
            for (f, v) in fields.iter().zip(&new) {
                put(&mut out.issue, *f, v);
            }
        } else if all(&jira, &landed) {
            for w in &unit {
                settle(conn, &w.id, "done", at)?;
            }
        } else {
            for w in &unit {
                settle(conn, &w.id, "cancelled", at)?;
            }
            // One Suggestion per unit; the status's carries the category,
            // with the resolution kind when the edit set one.
            let field = fields[0];
            let why = format!(
                "Jira changed {} on {key} while yours waited to push",
                field_word(field)
            );
            out.suggested
                .push(conflict(conn, item.id, field, &jira[0], &new[0], &why, at)?);
            if fields.len() == 2 && !same(&jira[1], &new[1]) {
                add_patch(
                    conn,
                    out.suggested.last().unwrap(),
                    fields[1],
                    &jira[1],
                    &new[1],
                )?;
            }
        }
    }
    Ok(out)
}

/// Sets a pushed field of `issue` to `value` (its local value).
fn put(issue: &mut todo_core::remote::RemoteIssue, field: ItemField, value: &FieldValue) {
    match (field, value) {
        (ItemField::Title, FieldValue::String(s)) => issue.title = s.clone(),
        (ItemField::Priority, FieldValue::Priority(p)) => issue.priority = *p,
        (ItemField::Labels, FieldValue::StringSet(l)) => issue.labels = l.clone(),
        (ItemField::DueDate, FieldValue::Date(d)) => issue.due_date = Some(*d),
        (ItemField::DueDate, FieldValue::Cleared) => issue.due_date = None,
        (ItemField::StatusCategory, FieldValue::StatusCategory(c)) => issue.status_category = *c,
        (ItemField::ResolutionKind, FieldValue::ResolutionKind(k)) => {
            issue.resolution_kind = Some(*k)
        }
        (ItemField::ResolutionKind, FieldValue::Cleared) => issue.resolution_kind = None,
        _ => {}
    }
}

/// A second patch on the Suggestion `id`.
fn add_patch(
    conn: &Connection,
    id: &str,
    field: ItemField,
    old: &FieldValue,
    new: &FieldValue,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO suggestion_patches (id, suggestion_id, ord, field, expected_old_json, new_json, state)
         VALUES (?1, ?2, (SELECT COUNT(*) FROM suggestion_patches WHERE suggestion_id = ?2), ?3, ?4, ?5, 'pending')",
    )?
    .execute(params![
        new_ulid().to_string(),
        id,
        field.number(),
        codec::value_to_sql(old)?,
        codec::value_to_sql(new)?
    ])?;
    Ok(())
}

/// One Item's waiting pushes, due now: what the worker sends (design 5.6).
#[derive(Debug, Clone, PartialEq)]
pub struct DuePush {
    pub item_id: todo_core::ItemId,
    pub remote_id: String,
    pub key: String,
    /// Its rows, field order.
    pub rows: Vec<Waiting>,
    /// The most tries any of them has had, for the backoff.
    pub attempts: u32,
}

/// The Items of `instance` with rows waiting and due by `now`, oldest
/// first, at most `limit`. In flight counts: a push sent whose answer was
/// lost is read back before it is sent again. A deleted Item or a broken
/// link has none due: its rows were cancelled, or wait for the link.
pub(crate) fn due(
    conn: &Connection,
    instance: &str,
    now: Timestamp,
    limit: usize,
) -> Result<Vec<DuePush>> {
    let mut stmt = conn.prepare_cached(
        "SELECT o.item_id, i.remote_id, i.remote_key, MIN(o.created_at) AS first, MAX(o.attempts)
         FROM outbox o JOIN items i ON i.id = o.item_id
         WHERE o.instance_id = ?1 AND o.state IN ('pending', 'in_flight')
           AND (o.next_attempt_at IS NULL OR o.next_attempt_at <= ?2)
           AND i.deleted_at IS NULL AND i.remote_link_state = 'ok'
         GROUP BY o.item_id ORDER BY first LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![instance, ts_to_sql(now)?, limit as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, u32>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (item, remote, key, attempts) = row?;
        let item_id = codec::item_id_from_str(&item)?;
        let rows = waiting(conn, item_id)?;
        out.push(DuePush {
            item_id,
            remote_id: remote.unwrap_or_default(),
            key: key.unwrap_or_default(),
            rows,
            attempts,
        });
    }
    Ok(out)
}

/// What a push of one Item's rows came to.
#[derive(Debug, Clone, PartialEq)]
pub enum PushResult {
    /// Jira took it: the rows wait in flight for the read-back that
    /// settles them (the worker reads the issue again at once).
    Sent,
    /// Not now (offline, 5xx, 429): try again at `at`, reading first.
    Retry { at: Timestamp, error: String },
    /// Jira refused it: the rows fail, and a push_failed Suggestion offers
    /// yours again over Jira's values (`jira`, field by field). A row
    /// edited since it was sent waits for its own try instead.
    Refused {
        error: String,
        jira: Vec<(ItemField, FieldValue)>,
    },
}

/// Records `result` for the `rows` of `item` the worker sent (in flight,
/// [`begin`]), at `at`. Returns the push_failed Suggestion's id, if one
/// was made.
pub(crate) fn record_result(
    conn: &Connection,
    item: todo_core::ItemId,
    key: &str,
    rows: &[Waiting],
    result: &PushResult,
    at: Timestamp,
) -> Result<Option<String>> {
    let now = ts_to_sql(at)?;
    match result {
        PushResult::Sent => {
            for w in rows {
                conn.prepare_cached(
                    "UPDATE outbox SET attempts = attempts + 1, next_attempt_at = NULL,
                            last_error = NULL, updated_at = ?2
                     WHERE id = ?1 AND state = 'in_flight'",
                )?
                .execute(params![w.id, now])?;
            }
            Ok(None)
        }
        PushResult::Retry { at: next, error } => {
            for w in rows {
                conn.prepare_cached(
                    "UPDATE outbox SET attempts = attempts + 1, next_attempt_at = ?2,
                            last_error = ?3, updated_at = ?4
                     WHERE id = ?1 AND state IN ('pending', 'in_flight')",
                )?
                .execute(params![w.id, ts_to_sql(*next)?, error, now])?;
            }
            Ok(None)
        }
        PushResult::Refused { error, jira } => {
            let mut failed = Vec::new();
            for w in rows {
                // Edited again since it was sent: the new value is not what
                // Jira refused, so it waits for its own try.
                let n = conn
                    .prepare_cached(
                        "UPDATE outbox SET state = 'pending', sent_json = NULL, updated_at = ?2
                         WHERE id = ?1 AND state = 'in_flight' AND new_json != sent_json",
                    )?
                    .execute(params![w.id, now])?;
                if n == 1 {
                    continue;
                }
                let n = conn
                    .prepare_cached(
                        "UPDATE outbox SET state = 'failed', attempts = attempts + 1, last_error = ?2,
                                updated_at = ?3
                         WHERE id = ?1 AND state IN ('pending', 'in_flight')",
                    )?
                    .execute(params![w.id, error, now])?;
                if n == 1 {
                    failed.push(w);
                }
            }
            let Some(first) = failed.first() else {
                return Ok(None);
            };
            // Jira's values go back on the Item with the read-back that
            // follows; should that fail, the next pass reads it by id.
            conn.prepare_cached(
                "UPDATE items SET remote_last_pulled_version = NULL WHERE id = ?1",
            )?
            .execute([item.to_string()])?;
            let id = new_ulid().to_string();
            conn.prepare_cached(
                "INSERT INTO suggestions (id, item_id, kind, source, rationale, error, state,
                                          created_at, updated_at)
                 VALUES (?1, ?2, 'push_failed', 'sync', ?3, ?4, 'pending', ?5, ?5)",
            )?
            .execute(params![
                id,
                item.to_string(),
                format!("Jira refused {} on {key}", field_word(first.field)),
                error,
                now
            ])?;
            for w in failed {
                let old = jira
                    .iter()
                    .find(|(f, _)| *f == w.field)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| w.base.clone());
                add_patch(conn, &id, w.field, &old, &w.new)?;
            }
            Ok(Some(id))
        }
    }
}
