//! Saved views (design 2.4): the `views` table, seeding the defaults, and
//! reading views.

use std::sync::Arc;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};
use todo_core::view::{View, ViewId, defaults};
use todo_core::{Revision, Topic, new_ulid};

use crate::codec::{rev_from_sql, rev_to_sql, ts_from_sql, ts_to_sql};
use crate::{
    OpKind, OpRow, REQUEST_SQL, Resource, Result, StoreError, WriteTx, current_revision_in,
    prune_request_ids, recorded_op,
};

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

pub(crate) fn insert(conn: &Connection, view: &View) -> Result<()> {
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
    let mut candidate = name.to_owned();
    for n in 2.. {
        if !name_taken(conn, &candidate, None)? {
            break;
        }
        candidate = format!("{name} {n}");
    }
    Ok(candidate)
}

/// Whether a view other than `except` has `name`, ignoring case in every
/// script: `Ärger` and `ärger` are one name. (The table's NOCASE index
/// folds only A–Z; it stays as a backstop.)
fn name_taken(conn: &Connection, name: &str, except: Option<ViewId>) -> Result<bool> {
    let folded = name.to_lowercase();
    let except = except.map(|id| id.to_string());
    let mut stmt = conn.prepare_cached("SELECT id, name FROM views")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let (id, held): (String, String) = (row.get(0)?, row.get(1)?);
        if except.as_deref() != Some(id.as_str()) && held.to_lowercase() == folded {
            return Ok(true);
        }
    }
    Ok(false)
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

// ---- editing views (design 2.4) -------------------------------------------
//
// A view edit is an op (actor user, kind create_view / update_view /
// delete_view / move_view) so its request id makes it idempotent, but it is
// never an undo stack entry and writes no events. Positions stay dense: a
// view that moves gets a revision and a change, and only the edited view a
// new updated_at.

/// Input for [`create_view_tx`]: a view without its id, revision or times.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateViewInput {
    pub request_id: String,
    pub view: View,
    /// Where it goes, 0-based; `None` or past the end is last.
    pub position: Option<u32>,
}

/// The fields [`update_view_tx`] may change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewUpdate {
    pub name: Option<String>,
    pub filter: Option<todo_core::Filter>,
    pub sort: Option<Vec<todo_core::SortKey>>,
    pub show_count: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateViewInput {
    pub request_id: String,
    pub id: ViewId,
    pub update: ViewUpdate,
    /// Fails with [`StoreError::ViewRevisionMismatch`] when the view has
    /// changed since.
    pub expected_revision: Option<Revision>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MoveViewInput {
    pub request_id: String,
    pub id: ViewId,
    /// 0-based; past the end is last.
    pub position: u32,
}

/// What a view edit did: the view as it is now (`None` once deleted), every
/// view whose position it changed, and its op.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewOutput {
    pub view: Option<View>,
    pub moved: Vec<View>,
    pub result: todo_core::MutationResult,
}

/// The longest view name, in characters.
pub const MAX_VIEW_NAME_CHARS: usize = 64;

/// `name` trimmed, or why it cannot name a view.
pub fn check_view_name(name: &str) -> std::result::Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        Err("must not be blank")
    } else if name.chars().count() > MAX_VIEW_NAME_CHARS {
        Err("is longer than 64 characters")
    } else if name.chars().any(char::is_control) {
        Err("must not hold a control character")
    } else {
        Ok(name.to_owned())
    }
}

/// The view a replayed request made, now: its op must be of `kind` for the
/// same view.
fn replay(
    tx: &WriteTx<'_>,
    op: &OpRow<'_>,
    kind: OpKind,
    id: Option<ViewId>,
) -> Result<Option<ViewOutput>> {
    let conn = tx.conn();
    let Some(request_id) = op.request_id else {
        return Ok(None);
    };
    let Some((op_id, revision, recorded)) = recorded_op(conn, REQUEST_SQL, request_id, kind)?
    else {
        return Ok(None);
    };
    let recorded: Option<ViewId> = recorded.and_then(|r| r.parse().ok());
    if id.is_some() && recorded != id {
        return Err(StoreError::RequestIdReused);
    }
    // The view as it is now; one deleted since is none: the request still
    // did what it did.
    let view = match recorded {
        Some(id) if kind != OpKind::DeleteView => load(conn, id)?,
        _ => None,
    };
    Ok(Some(ViewOutput {
        view,
        moved: Vec::new(),
        result: todo_core::MutationResult { op_id, revision },
    }))
}

fn op_for<'a>(kind: OpKind, request_id: &'a str) -> Result<OpRow<'a>> {
    let mut op = OpRow::new(kind, todo_core::Actor::User, request_id)?;
    op.stackable = false;
    Ok(op)
}

/// Fails with [`StoreError::ViewNameTaken`] when another view has `name`,
/// ignoring case.
fn check_free(conn: &Connection, name: &str, except: Option<ViewId>) -> Result<()> {
    if name_taken(conn, name, except)? {
        return Err(StoreError::ViewNameTaken(name.to_owned()));
    }
    Ok(())
}

/// The longest relative-date offset a stored view takes: about 100 years,
/// so its date resolves on any day this software meets. A request's only
/// has to resolve today; a view's is resolved on every later day.
pub const MAX_VIEW_OFFSET_DAYS: i32 = 36_525;

/// A view's filter: valid, and every relative date within
/// [`MAX_VIEW_OFFSET_DAYS`]. The error's path is relative to the filter.
pub fn check_view_filter(
    filter: &todo_core::Filter,
) -> std::result::Result<(), todo_core::FilterError> {
    match check_filter(filter) {
        Err(StoreError::Filter(e)) => Err(e),
        _ => Ok(()),
    }
}

fn check_filter(filter: &todo_core::Filter) -> Result<()> {
    use todo_core::filter::{Clause, Operand, child_path};
    fn walk(path: &str, all_of: &[Clause], any_of: &[Clause]) -> Result<()> {
        for (list, clauses) in [("all_of", all_of), ("any_of", any_of)] {
            for (i, clause) in clauses.iter().enumerate() {
                let at = child_path(path, format_args!("{list}[{i}]"));
                match clause {
                    Clause::Predicate(p) => {
                        for (k, operand) in p.operands.iter().enumerate() {
                            if let Operand::RelativeDate(d) = operand
                                && d.offset_days.unsigned_abs()
                                    > MAX_VIEW_OFFSET_DAYS.unsigned_abs()
                            {
                                return Err(StoreError::Filter(todo_core::FilterError::invalid(
                                    format!(
                                        "{at}.predicate.operands[{k}].relative_date.offset_days"
                                    ),
                                    format!(
                                        "a view's offset is at most {MAX_VIEW_OFFSET_DAYS} days"
                                    ),
                                )));
                            }
                        }
                    }
                    Clause::Group(g) => walk(&format!("{at}.group"), &g.all_of, &g.any_of)?,
                }
            }
        }
        Ok(())
    }
    filter.validate().map_err(StoreError::Filter)?;
    walk("", &filter.all_of, &filter.any_of)
}

/// The count of views.
fn view_count(conn: &Connection) -> Result<u32> {
    let n: i64 = conn
        .prepare_cached("SELECT COUNT(*) FROM views")?
        .query_row([], |r| r.get(0))?;
    u32::try_from(n).map_err(|_| corrupt(format_args!("{n} views")))
}

/// Moves every view with a position in `range` by `by`, giving each a
/// revision and a change, and returns them.
fn shift(
    tx: &mut WriteTx<'_>,
    range: std::ops::RangeInclusive<u32>,
    by: i64,
    except: Option<ViewId>,
) -> Result<Vec<View>> {
    if range.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = tx
        .conn()
        .prepare_cached(
            "SELECT id FROM views WHERE position BETWEEN ?1 AND ?2 AND id IS NOT ?3
             ORDER BY position, id",
        )?
        .query_map(
            params![range.start(), range.end(), except.map(|id| id.to_string())],
            |r| r.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    let mut moved = Vec::with_capacity(ids.len());
    for id in ids {
        let revision = tx.next_revision();
        tx.conn()
            .prepare_cached(
                "UPDATE views SET position = position + ?1, revision = ?2 WHERE id = ?3",
            )?
            .execute(params![by, rev_to_sql(revision)?, id])?;
        let view_id: ViewId = id
            .parse()
            .map_err(|_| corrupt(format_args!("view id {id:?}")))?;
        let view = load(tx.conn(), view_id)?.ok_or(StoreError::ViewNotFound(view_id))?;
        tx.record_change(
            Topic::View,
            &id,
            false,
            Some(Resource::View(Arc::new(view.clone()))),
        )?;
        moved.push(view);
    }
    Ok(moved)
}

/// Records the op and the request id, for the view `id`, after the writes.
fn finish(tx: &WriteTx<'_>, op: &OpRow<'_>, id: ViewId) -> Result<todo_core::MutationResult> {
    let revision = tx.next_revision() - 1;
    op.insert(tx.conn(), revision, 0)?;
    op.insert_request_id(tx.conn(), revision, Some(&id.to_string()))?;
    Ok(todo_core::MutationResult {
        op_id: op.id,
        revision,
    })
}

pub(crate) fn create_view_tx(tx: &mut WriteTx<'_>, input: CreateViewInput) -> Result<ViewOutput> {
    let mut op = op_for(OpKind::CreateView, &input.request_id)?;
    prune_request_ids(tx.conn(), op.at)?;
    if let Some(out) = replay(tx, &op, OpKind::CreateView, None)? {
        return Ok(out);
    }
    let mut view = input.view;
    view.name = check_view_name(&view.name).map_err(|why| StoreError::ViewName(why.to_owned()))?;
    check_filter(&view.filter)?;
    todo_core::filter::validate_sort(&view.sort).map_err(StoreError::Filter)?;
    check_free(tx.conn(), &view.name, None)?;
    let count = view_count(tx.conn())?;
    let position = input.position.unwrap_or(count).min(count);
    let moved = match count.checked_sub(1) {
        Some(last) => shift(tx, position..=last, 1, None)?,
        None => Vec::new(),
    };
    let now = Timestamp::now();
    let id = new_ulid();
    view.id = Some(id);
    view.seed = None;
    view.position = position;
    view.revision = tx.next_revision();
    view.created_at = Some(now);
    view.updated_at = Some(now);
    insert(tx.conn(), &view)?;
    tx.record_change(
        Topic::View,
        &id.to_string(),
        false,
        Some(Resource::View(Arc::new(view.clone()))),
    )?;
    op.summary = format!("create view {:?}", view.name);
    let result = finish(tx, &op, id)?;
    Ok(ViewOutput {
        view: Some(view),
        moved,
        result,
    })
}

pub(crate) fn update_view_tx(tx: &mut WriteTx<'_>, input: UpdateViewInput) -> Result<ViewOutput> {
    let mut op = op_for(OpKind::UpdateView, &input.request_id)?;
    prune_request_ids(tx.conn(), op.at)?;
    if let Some(out) = replay(tx, &op, OpKind::UpdateView, Some(input.id))? {
        return Ok(out);
    }
    let mut view = load(tx.conn(), input.id)?.ok_or(StoreError::ViewNotFound(input.id))?;
    if let Some(expected) = input.expected_revision
        && expected != view.revision
    {
        return Err(StoreError::ViewRevisionMismatch {
            id: input.id,
            expected,
            actual: view.revision,
        });
    }
    let u = input.update;
    if let Some(name) = u.name {
        view.name = check_view_name(&name).map_err(|why| StoreError::ViewName(why.to_owned()))?;
        check_free(tx.conn(), &view.name, Some(input.id))?;
    }
    if let Some(filter) = u.filter {
        check_filter(&filter)?;
        view.filter = filter;
    }
    if let Some(sort) = u.sort {
        todo_core::filter::validate_sort(&sort).map_err(StoreError::Filter)?;
        view.sort = sort;
    }
    if let Some(show_count) = u.show_count {
        view.show_count = show_count;
    }
    let before = load(tx.conn(), input.id)?.ok_or(StoreError::ViewNotFound(input.id))?;
    if view == before {
        // Nothing changed: an op for the request id, and no revision.
        let result = finish(tx, &op, input.id)?;
        return Ok(ViewOutput {
            view: Some(view),
            moved: Vec::new(),
            result,
        });
    }
    view.revision = tx.next_revision();
    view.updated_at = Some(Timestamp::now());
    tx.conn()
        .prepare_cached(
            "UPDATE views SET name = ?1, filter = ?2, sort = ?3, show_count = ?4, revision = ?5,
                              updated_at = ?6
             WHERE id = ?7",
        )?
        .execute(params![
            view.name,
            serde_json::to_string(&view.filter).map_err(corrupt)?,
            serde_json::to_string(&view.sort).map_err(corrupt)?,
            view.show_count,
            rev_to_sql(view.revision)?,
            ts_to_sql(view.updated_at.expect("set above"))?,
            input.id.to_string()
        ])?;
    tx.record_change(
        Topic::View,
        &input.id.to_string(),
        false,
        Some(Resource::View(Arc::new(view.clone()))),
    )?;
    op.summary = format!("edit view {:?}", view.name);
    let result = finish(tx, &op, input.id)?;
    Ok(ViewOutput {
        view: Some(view),
        moved: Vec::new(),
        result,
    })
}

pub(crate) fn delete_view_tx(
    tx: &mut WriteTx<'_>,
    request_id: String,
    id: ViewId,
) -> Result<ViewOutput> {
    let mut op = op_for(OpKind::DeleteView, &request_id)?;
    prune_request_ids(tx.conn(), op.at)?;
    if let Some(out) = replay(tx, &op, OpKind::DeleteView, Some(id))? {
        return Ok(out);
    }
    let view = load(tx.conn(), id)?.ok_or(StoreError::ViewNotFound(id))?;
    tx.conn()
        .prepare_cached("DELETE FROM views WHERE id = ?1")?
        .execute([id.to_string()])?;
    // Its view_seeds row stays: a deleted default is never seeded again.
    tx.record_change(Topic::View, &id.to_string(), true, None)?;
    let count = view_count(tx.conn())?;
    let moved = if view.position < count {
        shift(tx, view.position + 1..=count, -1, None)?
    } else {
        Vec::new()
    };
    op.summary = format!("delete view {:?}", view.name);
    let result = finish(tx, &op, id)?;
    Ok(ViewOutput {
        view: None,
        moved,
        result,
    })
}

pub(crate) fn move_view_tx(tx: &mut WriteTx<'_>, input: MoveViewInput) -> Result<ViewOutput> {
    let mut op = op_for(OpKind::MoveView, &input.request_id)?;
    prune_request_ids(tx.conn(), op.at)?;
    if let Some(out) = replay(tx, &op, OpKind::MoveView, Some(input.id))? {
        return Ok(out);
    }
    let mut view = load(tx.conn(), input.id)?.ok_or(StoreError::ViewNotFound(input.id))?;
    let last = view_count(tx.conn())? - 1;
    let to = input.position.min(last);
    let from = view.position;
    op.summary = format!("move view {:?}", view.name);
    if to == from {
        let result = finish(tx, &op, input.id)?;
        return Ok(ViewOutput {
            view: Some(view),
            moved: Vec::new(),
            result,
        });
    }
    let mut moved = if to < from {
        shift(tx, to..=from - 1, 1, Some(input.id))?
    } else {
        shift(tx, from + 1..=to, -1, Some(input.id))?
    };
    view.position = to;
    view.revision = tx.next_revision();
    view.updated_at = Some(Timestamp::now());
    tx.conn()
        .prepare_cached(
            "UPDATE views SET position = ?1, revision = ?2, updated_at = ?3 WHERE id = ?4",
        )?
        .execute(params![
            to,
            rev_to_sql(view.revision)?,
            ts_to_sql(view.updated_at.expect("set above"))?,
            input.id.to_string()
        ])?;
    tx.record_change(
        Topic::View,
        &input.id.to_string(),
        false,
        Some(Resource::View(Arc::new(view.clone()))),
    )?;
    moved.push(view.clone());
    let result = finish(tx, &op, input.id)?;
    Ok(ViewOutput {
        view: Some(view),
        moved,
        result,
    })
}
