//! Export and import (design 7.4): every record of the database as rows a
//! converter writes out, in dependency order, and the same rows put back
//! into an empty database, for a full-fidelity round trip.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use rusqlite::{Connection, params};
use todo_core::view::View;
use todo_core::{FieldValue, Item, ItemField, Relation, Revision, StalenessSignals};

use crate::codec::{
    self, ITEM_COLUMNS, SqlEnum as _, item_from_row, rev_from_sql, rev_to_sql, ts_from_sql,
    ts_to_sql,
};
use crate::schema::SCHEMA_VERSION;
use crate::{Result, StoreError, WriteTx, current_revision_in, view};
use todo_core::Topic;

/// One exported record, in the store's own types.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportRow {
    /// An Item as stored: the fields computed when it is read (status and
    /// effective priority aside, which follow from it) are their defaults.
    Item(Box<Item>),
    Relation {
        relation: Relation,
        created_at: Timestamp,
    },
    View(Box<View>),
    ViewSeed {
        seed: String,
        seeded_at: Timestamp,
    },
    Op(Box<OpRecord>),
    Event(Box<EventRecord>),
    /// A `sync_cursors` row (design 5.3).
    SyncCursor {
        instance_id: String,
        scope: String,
        cursor_at: Timestamp,
    },
}

/// An `ops` row, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRecord {
    pub id: String,
    pub kind: String,
    pub actor: String,
    pub at: Timestamp,
    pub request_id: Option<String>,
    pub revision: Revision,
    pub seq: i64,
    pub undo_state: Option<String>,
    pub entry_op: Option<String>,
    pub summary: String,
    pub report: Option<String>,
}

/// An `events` row, its values decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
    pub id: String,
    pub op_id: String,
    pub item_id: String,
    pub at: Timestamp,
    pub actor: String,
    pub kind: String,
    pub field: Option<ItemField>,
    pub old: Option<EventValue>,
    pub new: Option<EventValue>,
    pub ord: u32,
}

/// What an event recorded.
#[derive(Debug, Clone, PartialEq)]
pub enum EventValue {
    Field(FieldValue),
    Staleness(StalenessSignals),
    Relation(Relation),
    /// A create's title.
    Title(String),
}

/// An export: what the database was when it was read, then its rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    pub schema_version: u32,
    pub revision: Revision,
    pub rows: Vec<ExportRow>,
}

fn corrupt(what: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(what.to_string())
}

/// Every record, read in one transaction, in dependency order: Items with
/// each parent (and merge target) before it, relations, views, view seeds,
/// then with `history` ops (in log order) and events (in op order).
pub(crate) fn export_tx(conn: &Connection, history: bool) -> Result<Export> {
    let tx = conn.unchecked_transaction()?;
    let revision = current_revision_in(&tx)?;
    let mut rows = Vec::new();

    let mut items = Vec::new();
    {
        let mut stmt = tx.prepare(&format!("SELECT {ITEM_COLUMNS} FROM items ORDER BY seq"))?;
        let mut q = stmt.query([])?;
        while let Some(row) = q.next()? {
            items.push(stored(item_from_row(row, None)?));
        }
    }
    for item in parents_first(items)? {
        rows.push(ExportRow::Item(Box::new(item)));
    }

    {
        let mut stmt = tx.prepare(
            "SELECT from_id, to_id, kind, created_at FROM relations
             ORDER BY created_at, from_id, to_id, kind",
        )?;
        let mut q = stmt.query([])?;
        while let Some(row) = q.next()? {
            let (from, to, kind): (String, String, String) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            rows.push(ExportRow::Relation {
                relation: codec::relation_from_row(&from, &to, &kind)?,
                created_at: ts_from_sql(row.get(3)?)?,
            });
        }
    }

    for v in view::load_all(&tx)? {
        rows.push(ExportRow::View(Box::new(v)));
    }
    {
        let mut stmt =
            tx.prepare("SELECT seed, seeded_at FROM view_seeds ORDER BY seeded_at, seed")?;
        let mut q = stmt.query([])?;
        while let Some(row) = q.next()? {
            rows.push(ExportRow::ViewSeed {
                seed: row.get(0)?,
                seeded_at: ts_from_sql(row.get(1)?)?,
            });
        }
    }
    {
        let mut stmt = tx.prepare(
            "SELECT instance_id, scope, cursor_at FROM sync_cursors ORDER BY instance_id, scope",
        )?;
        let mut q = stmt.query([])?;
        while let Some(row) = q.next()? {
            rows.push(ExportRow::SyncCursor {
                instance_id: row.get(0)?,
                scope: row.get(1)?,
                cursor_at: ts_from_sql(row.get(2)?)?,
            });
        }
    }

    if history {
        let mut stmt = tx.prepare(
            "SELECT id, kind, actor, at, request_id, revision, seq, undo_state, entry_op,
                    summary, report
             FROM ops ORDER BY seq",
        )?;
        let mut q = stmt.query([])?;
        while let Some(row) = q.next()? {
            rows.push(ExportRow::Op(Box::new(OpRecord {
                id: row.get(0)?,
                kind: row.get(1)?,
                actor: row.get(2)?,
                at: ts_from_sql(row.get(3)?)?,
                request_id: row.get(4)?,
                revision: rev_from_sql(row.get(5)?)?,
                seq: row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                undo_state: row.get(7)?,
                entry_op: row.get(8)?,
                summary: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                report: row.get(10)?,
            })));
        }
        drop(q);
        let mut stmt = tx.prepare(
            "SELECT e.id, e.op_id, e.item_id, e.at, e.actor, e.kind, e.field, e.old, e.new, e.ord
             FROM events e JOIN ops o ON o.id = e.op_id
             ORDER BY o.seq, e.ord, e.id",
        )?;
        let mut q = stmt.query([])?;
        while let Some(row) = q.next()? {
            let kind: String = row.get(5)?;
            let field: Option<u32> = row.get(6)?;
            let field = field
                .map(|n| {
                    ItemField::from_number(n)
                        .ok_or_else(|| corrupt(format_args!("event field {n}")))
                })
                .transpose()?;
            let decode = |json: Option<String>| -> Result<Option<EventValue>> {
                let Some(json) = json else { return Ok(None) };
                Ok(Some(match kind.as_str() {
                    "create" => EventValue::Title(serde_json::from_str(&json).map_err(corrupt)?),
                    "set" => EventValue::Field(codec::value_from_sql(&json)?),
                    "derived" => EventValue::Staleness(codec::staleness_from_sql(&json)?),
                    "relation_add" | "relation_remove" => {
                        EventValue::Relation(codec::relation_from_sql(&json)?)
                    }
                    other => return Err(corrupt(format_args!("event kind {other:?}"))),
                }))
            };
            rows.push(ExportRow::Event(Box::new(EventRecord {
                id: row.get(0)?,
                op_id: row.get(1)?,
                item_id: row.get(2)?,
                at: ts_from_sql(row.get(3)?)?,
                actor: row.get(4)?,
                field,
                old: decode(row.get(7)?)?,
                new: decode(row.get(8)?)?,
                ord: row.get::<_, Option<u32>>(9)?.unwrap_or(0),
                kind,
            })));
        }
    }
    Ok(Export {
        schema_version: SCHEMA_VERSION,
        revision,
        rows,
    })
}

/// `item` as stored: what reading it computes is left at its default.
fn stored(mut item: Item) -> Item {
    item.derived.is_overdue = false;
    item.derived.subtasks_total = 0;
    item.derived.subtasks_done = 0;
    item.derived.parent_title = None;
    item.relations.clear();
    item
}

/// `items` (in creation order) with every parent and merge target before the
/// Items that name it, for the foreign keys of an import.
fn parents_first(items: Vec<Item>) -> Result<Vec<Item>> {
    let by_id: HashMap<_, _> = items
        .iter()
        .enumerate()
        .map(|(i, item)| (item.id, i))
        .collect();
    let mut placed = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    let mut slots: Vec<Option<Item>> = items.into_iter().map(Some).collect();
    fn place(
        i: usize,
        slots: &mut Vec<Option<Item>>,
        by_id: &HashMap<todo_core::ItemId, usize>,
        placed: &mut HashSet<usize>,
        out: &mut Vec<Item>,
        depth: usize,
    ) -> Result<()> {
        if placed.contains(&i) {
            return Ok(());
        }
        if depth > by_id.len() {
            return Err(StoreError::Corrupt("a cycle of parents".to_owned()));
        }
        let Some(item) = slots[i].as_ref() else {
            return Ok(());
        };
        let before: Vec<usize> = [item.core.parent_id, item.derived.merged_into]
            .into_iter()
            .flatten()
            .filter_map(|id| by_id.get(&id).copied())
            .collect();
        for j in before {
            place(j, slots, by_id, placed, out, depth + 1)?;
        }
        placed.insert(i);
        out.push(slots[i].take().expect("placed once"));
        Ok(())
    }
    for i in 0..slots.len() {
        place(i, &mut slots, &by_id, &mut placed, &mut out, 0)?;
    }
    Ok(out)
}

/// What an import put in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportOutput {
    pub items: u64,
    pub records: u64,
    /// The database's revision after it.
    pub revision: Revision,
}

/// Whether the database is empty, as an import needs it: no Item and no op
/// but sync's. (The default views are seeded into every database, and an
/// edit of one is an op.) What sync pulled before the import, when a home
/// starts with `[[instances]]` in its config, is only a copy of the
/// tracker's: the import replaces it, and the export's cursors take over.
fn is_empty(conn: &Connection) -> Result<bool> {
    let any: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM items WHERE created_via != 'remote_import')
             OR EXISTS (SELECT 1 FROM ops WHERE kind != 'sync' OR actor != 'sync')",
        [],
        |r| r.get(0),
    )?;
    Ok(!any)
}

/// Puts `rows` into an empty database, all or nothing: the seeded views give
/// way to the export's, each record keeps its ids and times, and the
/// revision moves past both the export's and the database's. The change log
/// is emptied and holds one change at that revision, so every client
/// resyncs: a live subscriber sees the gap before it and replays, and any
/// replay from before it has expired. The `Err` names the record (0-based)
/// that could not go in.
pub(crate) fn import_tx(
    tx: &mut WriteTx<'_>,
    export_revision: Revision,
    rows: Vec<ExportRow>,
) -> Result<ImportOutput> {
    let conn = tx.conn();
    if !is_empty(conn)? {
        return Err(StoreError::NotEmpty);
    }
    conn.execute_batch(
        "DELETE FROM events; DELETE FROM relations; DELETE FROM items; DELETE FROM ops;
         DELETE FROM sync_cursors;
         DELETE FROM views; DELETE FROM view_seeds; DELETE FROM changes;",
    )?;
    let mut items = 0;
    let records = rows.len() as u64;
    for (i, row) in rows.into_iter().enumerate() {
        let at = |e: StoreError| StoreError::Import {
            record: i,
            error: e.to_string(),
        };
        put(conn, row, &mut items).map_err(at)?;
    }
    // Inside the writer's transaction, so the next write numbers after it.
    // One revision skipped, so that even past an export older than this
    // database there is a gap for every client to see.
    let revision = current_revision_in(conn)?.max(export_revision) + 2;
    let _ = conn;
    tx.skip_to(revision);
    // The config change: harmless to every client, which reads the config
    // again, and the one row a replay finds.
    tx.record_change(Topic::Config, crate::CONFIG_ID, false, None)?;
    Ok(ImportOutput {
        items,
        records,
        revision,
    })
}

fn put(conn: &Connection, row: ExportRow, items: &mut u64) -> Result<()> {
    match row {
        ExportRow::Item(item) => {
            codec::insert_item(conn, &item)?;
            *items += 1;
        }
        ExportRow::Relation {
            relation,
            created_at,
        } => {
            conn.execute(
                "INSERT INTO relations (from_id, to_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![
                    relation.from().to_string(),
                    relation.to().to_string(),
                    relation.kind().as_sql(),
                    ts_to_sql(created_at)?
                ],
            )?;
        }
        ExportRow::View(v) => view::insert(conn, &v)?,
        ExportRow::ViewSeed { seed, seeded_at } => {
            conn.execute(
                "INSERT INTO view_seeds (seed, seeded_at) VALUES (?1, ?2)",
                params![seed, ts_to_sql(seeded_at)?],
            )?;
        }
        ExportRow::SyncCursor {
            instance_id,
            scope,
            cursor_at,
        } => {
            conn.execute(
                "INSERT INTO sync_cursors (instance_id, scope, cursor_at) VALUES (?1, ?2, ?3)",
                params![instance_id, scope, ts_to_sql(cursor_at)?],
            )?;
        }
        ExportRow::Op(op) => {
            conn.execute(
                "INSERT INTO ops (id, kind, actor, at, request_id, revision, seq, undo_state,
                                  entry_op, summary, report)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    op.id,
                    op.kind,
                    op.actor,
                    ts_to_sql(op.at)?,
                    op.request_id,
                    rev_to_sql(op.revision)?,
                    op.seq,
                    op.undo_state,
                    op.entry_op,
                    op.summary,
                    op.report
                ],
            )?;
        }
        ExportRow::Event(e) => {
            let encode = |v: Option<EventValue>| -> Result<Option<String>> {
                Ok(match v {
                    None => None,
                    Some(EventValue::Title(t)) => Some(serde_json::to_string(&t).map_err(corrupt)?),
                    Some(EventValue::Field(v)) => Some(codec::value_to_sql(&v)?),
                    Some(EventValue::Staleness(s)) => Some(codec::staleness_to_sql(s)?),
                    Some(EventValue::Relation(r)) => Some(codec::relation_to_sql(&r)?),
                })
            };
            conn.execute(
                "INSERT INTO events (id, op_id, item_id, at, actor, kind, field, old, new, ord)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    e.id,
                    e.op_id,
                    e.item_id,
                    ts_to_sql(e.at)?,
                    e.actor,
                    e.kind,
                    e.field.map(ItemField::number),
                    encode(e.old)?,
                    encode(e.new)?,
                    e.ord
                ],
            )?;
        }
    }
    Ok(())
}
