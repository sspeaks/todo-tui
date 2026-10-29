//! The hierarchy Items show without being stored with them: each Item's
//! subtask counts and its parent's title, attached when it is read.

use std::collections::HashMap;

use rusqlite::Connection;
use todo_core::{Item, ItemId, Status};

use crate::Result;

/// Sets each Item's `subtasks_total`, `subtasks_done` and `parent_title`
/// from the stored rows, in two queries. Call it once the rows it reads
/// are written (`write_op` attaches after it updates them).
pub(crate) fn attach(conn: &Connection, items: &mut [Item]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = items.iter().map(|i| i.id.to_string()).collect();
    let mut counts: HashMap<String, (u32, u32)> = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT parent_id, COUNT(*), COALESCE(SUM(status = 'done'), 0) FROM items
         WHERE parent_id IN (SELECT value FROM json_each(?1))
           AND deleted_at IS NULL AND status != 'cancelled'
         GROUP BY parent_id",
    )?;
    let mut rows = stmt.query([serde_json::Value::from(ids).to_string()])?;
    while let Some(row) = rows.next()? {
        counts.insert(row.get(0)?, (row.get(1)?, row.get(2)?));
    }

    let parents: Vec<String> = items
        .iter()
        .filter_map(|i| i.core.parent_id.as_ref().map(ItemId::to_string))
        .collect();
    let mut titles: HashMap<String, String> = HashMap::new();
    if !parents.is_empty() {
        let mut stmt = conn.prepare_cached(
            "SELECT id, title FROM items WHERE id IN (SELECT value FROM json_each(?1))",
        )?;
        let mut rows = stmt.query([serde_json::Value::from(parents).to_string()])?;
        while let Some(row) = rows.next()? {
            titles.insert(row.get(0)?, row.get(1)?);
        }
    }

    for item in items {
        let (total, done) = counts
            .get(&item.id.to_string())
            .copied()
            .unwrap_or_default();
        item.derived.subtasks_total = total;
        item.derived.subtasks_done = done;
        item.derived.parent_title = item
            .core
            .parent_id
            .and_then(|p| titles.get(&p.to_string()).cloned());
    }
    Ok(())
}

/// What an Item adds to its parent's counts: the parent, and whether it
/// counts and is done. A change to it re-emits the old and new parent.
pub(crate) fn contribution(item: &Item) -> (Option<ItemId>, bool, bool) {
    let status = item.status();
    let counted = !item.is_deleted() && status != Status::Cancelled;
    (
        item.core.parent_id,
        counted,
        counted && status == Status::Done,
    )
}
