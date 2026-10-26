//! Saved views (design 2.4): the `views` table, seeding the defaults, and
//! reading views.

use std::sync::Arc;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};
use todo_core::view::{View, ViewId, defaults};
use todo_core::{Revision, Topic, new_ulid};

use crate::codec::{rev_from_sql, rev_to_sql, ts_from_sql, ts_to_sql};
use crate::{Resource, Result, StoreError, WriteTx, current_revision_in};

/// The `views` columns read by [`view_from_row`], in order.
const VIEW_COLUMNS: &str =
    "id, name, filter, sort, show_count, position, seed, revision, created_at, updated_at";

fn corrupt(what: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(what.to_string())
}

fn view_from_row(row: &Row<'_>) -> Result<View> {
    let id: String = row.get(0)?;
    let filter: String = row.get(2)?;
    let sort: String = row.get(3)?;
    let position: i64 = row.get(5)?;
    Ok(View {
        id: Some(
            id.parse()
                .map_err(|_| corrupt(format_args!("bad view id {id:?}")))?,
        ),
        name: row.get(1)?,
        filter: serde_json::from_str(&filter).map_err(corrupt)?,
        sort: serde_json::from_str(&sort).map_err(corrupt)?,
        show_count: row.get(4)?,
        position: u32::try_from(position)
            .map_err(|_| corrupt(format_args!("view {id}: position {position}")))?,
        seed: row.get(6)?,
        revision: rev_from_sql(row.get(7)?)?,
        created_at: Some(ts_from_sql(row.get(8)?)?),
        updated_at: Some(ts_from_sql(row.get(9)?)?),
    })
}

/// The view `id`, if it exists.
pub(crate) fn load(conn: &Connection, id: ViewId) -> Result<Option<View>> {
    let sql = format!("SELECT {VIEW_COLUMNS} FROM views WHERE id = ?1");
    conn.prepare_cached(&sql)?
        .query_row([id.to_string()], |row| Ok(view_from_row(row)))
        .optional()?
        .transpose()
}

/// Every view, in tab order.
pub(crate) fn load_all(conn: &Connection) -> Result<Vec<View>> {
    let sql = format!("SELECT {VIEW_COLUMNS} FROM views ORDER BY position, id");
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut rows = stmt.query([])?;
    let mut views = Vec::new();
    while let Some(row) = rows.next()? {
        views.push(view_from_row(row)?);
    }
    Ok(views)
}

pub(crate) fn list_views_tx(conn: &Connection) -> Result<(Vec<View>, Revision)> {
    let tx = conn.unchecked_transaction()?;
    let revision = current_revision_in(&tx)?;
    Ok((load_all(&tx)?, revision))
}

fn insert(conn: &Connection, view: &View) -> Result<()> {
    let id = view
        .id
        .ok_or_else(|| corrupt("a view is stored with its id"))?;
    let at = |t: Option<Timestamp>| {
        t.ok_or_else(|| corrupt(format_args!("view {id} has no timestamps")))
            .and_then(ts_to_sql)
    };
    conn.prepare_cached(
        "INSERT INTO views (id, name, filter, sort, show_count, position, seed, revision,
                            created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?
    .execute(params![
        id.to_string(),
        view.name,
        serde_json::to_string(&view.filter).map_err(corrupt)?,
        serde_json::to_string(&view.sort).map_err(corrupt)?,
        view.show_count,
        view.position,
        view.seed,
        rev_to_sql(view.revision)?,
        at(view.created_at)?,
        at(view.updated_at)?
    ])?;
    Ok(())
}

/// `name`, or the first of `name 2`, `name 3`, ... that no view has (names
/// are unique ignoring case).
fn free_name(conn: &Connection, name: &str) -> Result<String> {
    let mut stmt =
        conn.prepare_cached("SELECT EXISTS (SELECT 1 FROM views WHERE name = ?1 COLLATE NOCASE)")?;
    let mut candidate = name.to_owned();
    for n in 2.. {
        if !stmt.query_row([&candidate], |r| r.get::<_, bool>(0))? {
            break;
        }
        candidate = format!("{name} {n}");
    }
    Ok(candidate)
}

/// Seeds every default view (`todo_core::view::defaults`) whose seed key is
/// not yet recorded, in order, each after the existing views, and records
/// its key, so that a deleted default is never seeded again and a default
/// added in a later version is seeded once. Each view gets a revision and a
/// `changes` row (topic `view`); no op or event is written.
pub(crate) fn seed_tx(tx: &mut WriteTx<'_>) -> Result<()> {
    let now = Timestamp::now();
    for mut view in defaults() {
        let seed = view
            .seed
            .clone()
            .ok_or_else(|| corrupt(format_args!("default view {:?} has no seed", view.name)))?;
        let conn = tx.conn();
        let seeded: bool = conn
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM view_seeds WHERE seed = ?1)")?
            .query_row([&seed], |r| r.get(0))?;
        if seeded {
            continue;
        }
        let position: i64 = conn
            .prepare_cached("SELECT COALESCE(MAX(position) + 1, 0) FROM views")?
            .query_row([], |r| r.get(0))?;
        view.position =
            u32::try_from(position).map_err(|_| corrupt(format_args!("position {position}")))?;
        view.name = free_name(conn, &view.name)?;
        let id = new_ulid();
        view.id = Some(id);
        view.revision = tx.next_revision();
        view.created_at = Some(now);
        view.updated_at = Some(now);
        insert(conn, &view)?;
        conn.prepare_cached("INSERT INTO view_seeds (seed, seeded_at) VALUES (?1, ?2)")?
            .execute(params![seed, ts_to_sql(now)?])?;
        let resource = Resource::View(Arc::new(view));
        tx.record_change(Topic::View, &id.to_string(), false, Some(resource))?;
    }
    Ok(())
}
