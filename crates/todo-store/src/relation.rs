//! Local relations between Items (design 3.6): the `relations` table, the
//! relation ops, and attaching each Item's relations when it is read.

use std::collections::HashMap;

use jiff::Timestamp;
use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};
use todo_core::{Actor, Item, ItemId, MutationResult, Relation};
use ulid::Ulid;

use crate::codec::{self, SqlEnum, ts_to_sql};
use crate::{
    Batch, EndProblem, NewEvent, OpKind, OpRow, REQUEST_SQL, RelationInput, RelationOutput, Result,
    StoreError, WriteTx, load_items, prune_request_ids, recorded_op, summary, write_op,
};

/// Sets each Item's relations to every relation with the Item at either end,
/// sorted, in one query.
pub(crate) fn attach(conn: &Connection, items: &mut [Item]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let mut slots: HashMap<ItemId, Vec<usize>> = HashMap::new();
    for (slot, item) in items.iter_mut().enumerate() {
        item.relations.clear();
        slots.entry(item.id).or_default().push(slot);
    }
    let ids: Vec<String> = slots.keys().map(ItemId::to_string).collect();
    let mut stmt = conn.prepare_cached(
        "SELECT from_id, to_id, kind FROM relations
         WHERE from_id IN (SELECT value FROM json_each(?1))
         UNION
         SELECT from_id, to_id, kind FROM relations
         WHERE to_id IN (SELECT value FROM json_each(?1))",
    )?;
    let mut rows = stmt.query([serde_json::Value::from(ids).to_string()])?;
    while let Some(row) = rows.next()? {
        let relation = codec::relation_from_row(
            &row.get::<_, String>(0)?,
            &row.get::<_, String>(1)?,
            &row.get::<_, String>(2)?,
        )?;
        for end in [relation.from(), relation.to()] {
            for &slot in slots.get(&end).into_iter().flatten() {
                items[slot].relations.push(relation);
            }
        }
    }
    for item in items {
        item.relations.sort_unstable();
    }
    Ok(())
}

/// What [`Batch::change_relation`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelationChange {
    Changed,
    /// The relation was already present (add) or absent (remove).
    Unchanged,
    /// This end names no Item.
    Missing(ItemId),
}

impl Batch {
    /// Adds `relation` if it is absent, or removes it if it is present, as an
    /// event of this op that bumps both ends (`updated_at` is `now`). Ends
    /// may be deleted.
    pub(crate) fn change_relation(
        &mut self,
        conn: &Connection,
        relation: Relation,
        add: bool,
        now: Timestamp,
        today: Date,
    ) -> Result<RelationChange> {
        for end in [relation.from(), relation.to()] {
            if self.parent_of(conn, end)?.is_none() {
                return Ok(RelationChange::Missing(end));
            }
        }
        let (from, to, kind) = (
            relation.from().to_string(),
            relation.to().to_string(),
            relation.kind().as_sql(),
        );
        let changed = if add {
            conn.prepare_cached(
                "INSERT INTO relations (from_id, to_id, kind, created_at)
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            )?
            .execute(params![from, to, kind, ts_to_sql(now)?])?
        } else {
            conn.prepare_cached(
                "DELETE FROM relations WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3",
            )?
            .execute(params![from, to, kind])?
        };
        if changed == 0 {
            return Ok(RelationChange::Unchanged);
        }
        for end in [relation.from(), relation.to()] {
            let slot = self.load(conn, end, today)?;
            self.items[slot].updated_at = now;
            self.dirty.insert(slot);
        }
        self.events.push(if add {
            NewEvent::RelationAdd(relation)
        } else {
            NewEvent::RelationRemove(relation)
        });
        Ok(RelationChange::Changed)
    }
}

/// [`crate::Store::add_relation`] (`add`) or
/// [`crate::Store::remove_relation`].
pub(crate) fn relation_tx(
    tx: &mut WriteTx<'_>,
    input: RelationInput,
    add: bool,
) -> Result<RelationOutput> {
    let kind = if add {
        OpKind::AddRelation
    } else {
        OpKind::RemoveRelation
    };
    let mut op = OpRow::new(kind, Actor::User, &input.request_id)?;
    let relation = input.relation;
    let (from, to) = (relation.from(), relation.to());
    let today = input.today;
    let conn = tx.conn();
    prune_request_ids(conn, op.at)?;
    if let Some(request_id) = op.request_id
        && let Some((op_id, revision, item_id)) = recorded_op(conn, REQUEST_SQL, request_id, kind)?
    {
        // The same request names the same relation, whole.
        let same = match recorded_relation(conn, op_id)? {
            Some(recorded) => recorded == relation,
            // No relation op lacks one; this only keeps the from end check.
            None => item_id == Some(from.to_string()),
        };
        if !same {
            return Err(StoreError::RequestIdReused);
        }
        let [from_item, to_item] = load_items(conn, &[from, to], today)?
            .try_into()
            .expect("two ids, two items");
        return Ok(RelationOutput {
            from_item,
            to_item,
            result: MutationResult { op_id, revision },
        });
    }

    let mut batch = Batch::default();
    for end in [from, to] {
        let problem = match batch.parent_of(conn, end)? {
            None => EndProblem::Missing,
            Some((_, true)) if add => EndProblem::Deleted,
            Some(_) => continue,
        };
        return Err(StoreError::RelationEnd {
            item_id: end,
            problem,
        });
    }
    let from_slot = batch.load(conn, from, today)?;
    let to_slot = batch.load(conn, to, today)?;
    match batch.change_relation(conn, relation, add, input.now, today)? {
        RelationChange::Changed => {}
        // No event records the relation, so the op does, for replays.
        RelationChange::Unchanged => op.report = Some(codec::relation_to_sql(&relation)?),
        RelationChange::Missing(end) => {
            return Err(StoreError::Corrupt(format!("relation end {end} vanished")));
        }
    }
    op.summary = summary::relation(
        add,
        relation.kind(),
        &batch.items[from_slot].core.title,
        &batch.items[to_slot].core.title,
    );
    let (revision, _) = write_op(tx, &op, &mut batch, today)?;
    op.insert_request_id(tx.conn(), revision, Some(&from.to_string()))?;
    Ok(RelationOutput {
        from_item: batch.items[from_slot].clone(),
        to_item: batch.items[to_slot].clone(),
        result: MutationResult {
            op_id: op.id,
            revision,
        },
    })
}

/// The relation that relation op `op_id` was asked for: the one its event
/// records or, when it changed nothing, the one in its `report`.
fn recorded_relation(conn: &Connection, op_id: Ulid) -> Result<Option<Relation>> {
    let id = op_id.to_string();
    let event: Option<String> = conn
        .prepare_cached(
            "SELECT COALESCE(new, old) FROM events
             WHERE op_id = ?1 AND kind IN ('relation_add', 'relation_remove')
             ORDER BY ord LIMIT 1",
        )?
        .query_row([&id], |r| r.get(0))
        .optional()?;
    let json = match event {
        Some(json) => Some(json),
        None => conn
            .prepare_cached("SELECT report FROM ops WHERE id = ?1")?
            .query_row([&id], |r| r.get(0))?,
    };
    json.as_deref().map(codec::relation_from_sql).transpose()
}
