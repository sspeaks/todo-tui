//! SQLite persistence, migrations, revisions and events.
//!
//! The daemon is the only process that opens the database. All writes run on
//! one dedicated writer thread, in one SQLite transaction per [`Store::write`]
//! call; reads run on a small pool of read-only connections (WAL mode).

mod codec;
mod schema;

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use todo_core::{CreatedVia, Item, ItemId, MutationResult, Revision, Status, Topic, new_ulid};
use tokio::sync::{Semaphore, broadcast, oneshot};

use codec::{
    ITEM_COLUMNS, SqlEnum, date_to_sql, item_from_row, load_item, rev_from_sql, rev_to_sql,
    ts_to_sql,
};
pub use schema::SCHEMA_VERSION;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("item {0} not found")]
    NotFound(ItemId),
    #[error("revision conflict: expected {expected}, found {actual}")]
    RevisionMismatch {
        expected: Revision,
        actual: Revision,
    },
    #[error("invalid page token")]
    InvalidPageToken,
    #[error("corrupt row: {0}")]
    Corrupt(String),
    #[error("store is closed")]
    Closed,
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// `since` is older than the retained `changes` window; the client must reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("revision {since} is older than the replay window (oldest retained: {oldest})")]
pub struct Expired {
    pub since: Revision,
    pub oldest: Revision,
}

#[derive(Debug, thiserror::Error)]
pub enum ChangesSinceError {
    #[error(transparent)]
    Expired(#[from] Expired),
    /// `since` is newer than the current revision (the database was replaced
    /// while the client kept its revision); the client must reload.
    #[error("revision {since} is ahead of the current revision {current}")]
    Ahead { since: Revision, current: Revision },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A resource snapshot attached to a [`ChangeRow`].
#[derive(Debug, Clone, PartialEq)]
pub enum Resource {
    Item(Arc<Item>),
}

/// One row of the `changes` table: one revision (D9) for one resource.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeRow {
    pub revision: Revision,
    pub topic: Topic,
    /// Resource id (an Item ULID for [`Topic::Item`]).
    pub id: String,
    pub deleted: bool,
    /// The resource's state, `None` when `deleted`. On the live broadcast it
    /// is the state as of `revision`; from [`Store::changes_since`] it is the
    /// current state (which may be newer than `revision`).
    pub resource: Option<Resource>,
}

/// Input for [`Store::capture`]. Parsing has already happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureInput {
    /// Client ULID for idempotency (24h window).
    pub request_id: String,
    pub title: String,
    pub created_via: CreatedVia,
    pub parent_id: Option<ItemId>,
    /// Spool entry id; a spool id already imported returns the original result.
    pub spool_id: Option<String>,
    /// Creation time; the daemon passes the capture's reference time or now.
    pub now: Timestamp,
}

/// P0 query: the implicit view rules and paging. P1 replaces this with a
/// compiled `todo.v1.Filter`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQuery {
    /// `status IN (...)`; empty matches any status.
    pub statuses: Vec<Status>,
    pub include_archived: bool,
    pub include_done: bool,
    pub include_deferred: bool,
    pub include_deleted: bool,
    /// 0 means the default (100); values above 1000 are clamped.
    pub page_size: u32,
    /// Opaque token from a previous [`ItemPage`].
    pub page_token: Option<String>,
    /// "Today" for `defer_until`, in the user's zone.
    pub today: Option<jiff::civil::Date>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ItemPage {
    pub items: Vec<Item>,
    /// `None` on the last page.
    pub next_page_token: Option<String>,
    /// The revision this page was read at. Each page reads live data, so a
    /// later page may report a higher revision than the first; a consumer
    /// that needs a consistent view subscribes from the first page's revision.
    pub revision: Revision,
}

/// The writer-thread transaction handed to [`Store::write`] closures.
pub struct WriteTx<'conn> {
    tx: rusqlite::Transaction<'conn>,
    changes: Vec<ChangeRow>,
    next_revision: Revision,
}

impl WriteTx<'_> {
    /// The open transaction, for SQL the helpers don't cover.
    pub fn conn(&self) -> &rusqlite::Connection {
        &self.tx
    }

    /// The revision the next [`WriteTx::record_change`] call will allocate,
    /// so a resource can be stored with its revision before it is recorded.
    pub fn next_revision(&self) -> Revision {
        self.next_revision
    }

    /// Allocates the next revision for one changed resource and appends its
    /// `changes` row. Revisions allocated in one transaction are contiguous
    /// (D9). The row is broadcast only after commit.
    pub fn record_change(
        &mut self,
        topic: Topic,
        id: &str,
        deleted: bool,
        resource: Option<Resource>,
    ) -> Result<Revision> {
        let revision = self.next_revision;
        self.tx
            .prepare_cached(
                "INSERT INTO changes (revision, topic, resource_id, deleted, at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                rev_to_sql(revision)?,
                topic.as_sql(),
                id,
                deleted,
                ts_to_sql(Timestamp::now())?
            ])?;
        self.next_revision += 1;
        self.changes.push(ChangeRow {
            revision,
            topic,
            id: id.to_owned(),
            deleted,
            resource: if deleted { None } else { resource },
        });
        Ok(revision)
    }
}

/// Handle to the database. Cheap to clone; all clones share the writer
/// thread, read pool and broadcast channel.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

const READ_POOL_SIZE: usize = 4;
const BROADCAST_CAPACITY: usize = 1024;
const DEFAULT_PAGE_SIZE: u32 = 100;
const MAX_PAGE_SIZE: u32 = 1000;
const REQUEST_ID_TTL_NS: i64 = 24 * 60 * 60 * 1_000_000_000;

/// State the writer thread shares with handles.
struct Shared {
    revision: AtomicU64,
    changes: broadcast::Sender<ChangeRow>,
}

struct Inner {
    shared: Arc<Shared>,
    jobs: Option<mpsc::Sender<Job>>,
    writer: Option<JoinHandle<()>>,
    readers: Arc<ReadPool>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(writer) = self.writer.take()
            && writer.thread().id() != thread::current().id()
        {
            let _ = writer.join();
        }
    }
}

struct ReadPool {
    conns: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
}

/// A connection checked out of the pool; returned on drop, even on panic.
struct Pooled {
    conn: Option<Connection>,
    pool: Arc<ReadPool>,
}

impl Drop for Pooled {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool
                .conns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(conn);
        }
    }
}

impl Store {
    /// Opens (creating if needed) the database at `path`: WAL mode, runs
    /// pending migrations (D10), starts the writer thread and read pool.
    /// Fails with [`StoreError::SchemaTooNew`] for a newer database.
    pub fn open(path: &Path) -> Result<Store> {
        let mut writer_conn = schema::open_writer(path)?;
        let revision = current_revision_in(&writer_conn)?;
        let readers = (0..READ_POOL_SIZE)
            .map(|_| schema::open_reader(path))
            .collect::<Result<Vec<_>>>()?;

        let shared = Arc::new(Shared {
            revision: AtomicU64::new(revision),
            changes: broadcast::channel(BROADCAST_CAPACITY).0,
        });
        let (jobs, rx) = mpsc::channel::<Job>();
        let writer = thread::Builder::new()
            .name("todo-store-writer".into())
            .spawn(move || {
                for job in rx {
                    if catch_unwind(AssertUnwindSafe(|| job(&mut writer_conn))).is_err() {
                        tracing::error!("store write panicked; transaction rolled back");
                    }
                }
            })
            .map_err(|e| StoreError::Corrupt(format!("spawning writer thread: {e}")))?;

        Ok(Store {
            inner: Arc::new(Inner {
                shared,
                jobs: Some(jobs),
                writer: Some(writer),
                readers: Arc::new(ReadPool {
                    conns: Mutex::new(readers),
                    permits: Arc::new(Semaphore::new(READ_POOL_SIZE)),
                }),
            }),
        })
    }

    /// Runs `f` on the writer thread inside one transaction. On `Ok` the
    /// transaction commits, then every [`ChangeRow`] recorded through
    /// [`WriteTx::record_change`] is published to subscribers in revision
    /// order. On `Err` it rolls back and nothing is published. A panic in `f`
    /// rolls back and surfaces as [`StoreError::Closed`].
    pub async fn write<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut WriteTx<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (reply, rx) = oneshot::channel();
        let shared = Arc::clone(&self.inner.shared);
        let job: Job = Box::new(move |conn| {
            let _ = reply.send(run_write(conn, &shared, f));
        });
        self.inner
            .jobs
            .as_ref()
            .ok_or(StoreError::Closed)?
            .send(job)
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }

    /// Runs `f` on a pooled read-only connection, off the async runtime.
    pub async fn read<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let pool = Arc::clone(&self.inner.readers);
        let permit = Arc::clone(&pool.permits)
            .acquire_owned()
            .await
            .map_err(|_| StoreError::Closed)?;
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let conn = pool
                .conns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop()
                .expect("a permit guarantees a pooled connection");
            let pooled = Pooled {
                conn: Some(conn),
                pool,
            };
            f(pooled.conn.as_ref().expect("checked out"))
        });
        match task.await {
            Ok(result) => result,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => Err(StoreError::Closed),
        }
    }

    /// Creates an inbox Item (one op, one revision). Idempotent on
    /// `request_id` and on `spool_id`: a replay returns the original Item and
    /// result without writing.
    pub async fn capture(&self, input: CaptureInput) -> Result<(Item, MutationResult)> {
        self.write(move |tx| capture_tx(tx, input)).await
    }

    /// Lists Items matching `query`, ordered by `created_at` then id.
    ///
    /// Later pages continue after the previous page's last key and read live
    /// data at their own revision: Items changed since the first page appear
    /// in their current state, and the caller also receives those changes
    /// (and any Item created behind the cursor) by subscribing from the first
    /// page's revision. A token is only accepted by the query that issued it.
    pub async fn list_items(&self, query: ListQuery) -> Result<ItemPage> {
        self.read(move |conn| list_items_tx(conn, query)).await
    }

    /// Changes with revision greater than `since`, ascending, up to the
    /// current revision. Fails with [`Expired`] when rows after `since` have
    /// been pruned, and with [`ChangesSinceError::Ahead`] when `since` is
    /// greater than [`Store::current_revision`]. The daemon maps both to
    /// `ResyncRequired`.
    pub async fn changes_since(
        &self,
        since: Revision,
    ) -> Result<Vec<ChangeRow>, ChangesSinceError> {
        self.read(move |conn| Ok(changes_since_tx(conn, since)))
            .await?
    }

    /// The latest committed revision.
    pub fn current_revision(&self) -> Revision {
        self.inner.shared.revision.load(Ordering::Acquire)
    }

    /// Live [`ChangeRow`]s published after each commit. Subscribe before
    /// calling [`Store::changes_since`] so no revision falls in between; a
    /// lagging receiver gets `RecvError::Lagged` and must replay.
    pub fn subscribe(&self) -> broadcast::Receiver<ChangeRow> {
        self.inner.shared.changes.subscribe()
    }
}

fn run_write<F, T>(conn: &mut Connection, shared: &Shared, f: F) -> Result<T>
where
    F: FnOnce(&mut WriteTx<'_>) -> Result<T>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut wtx = WriteTx {
        tx,
        changes: Vec::new(),
        next_revision: shared.revision.load(Ordering::Acquire) + 1,
    };
    let value = f(&mut wtx)?;
    let WriteTx { tx, changes, .. } = wtx;
    let last = changes.last().map(|c| c.revision);
    if let Some(last) = last {
        tx.execute("UPDATE meta SET revision = ?1", [rev_to_sql(last)?])?;
    }
    tx.commit()?;
    if let Some(last) = last {
        shared.revision.store(last, Ordering::Release);
    }
    for change in changes {
        // No receivers is fine.
        let _ = shared.changes.send(change);
    }
    Ok(value)
}

fn actor_for(via: CreatedVia) -> &'static str {
    match via {
        CreatedVia::Tui | CreatedVia::Cli | CreatedVia::Mcp | CreatedVia::Split => "user",
        CreatedVia::RemoteImport => "sync",
        CreatedVia::Restore | CreatedVia::Recurrence => "system",
    }
}

/// The original result of an idempotent capture, if any.
fn capture_replay(
    conn: &Connection,
    sql: &str,
    key: &str,
) -> Result<Option<(Item, MutationResult)>> {
    let Some((op_id, revision, item_id)) = conn
        .prepare_cached(sql)?
        .query_row([key], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .optional()?
    else {
        return Ok(None);
    };
    let item_id = item_id
        .ok_or_else(|| StoreError::Corrupt(format!("request {key} did not create an item")))?;
    let item = load_item(conn, &item_id, None)?.ok_or_else(|| {
        StoreError::Corrupt(format!("item {item_id} for request {key} is missing"))
    })?;
    let revision = rev_from_sql(revision)?;
    let op_id = op_id
        .parse()
        .map_err(|_| StoreError::Corrupt(format!("bad op id {op_id:?}")))?;
    Ok(Some((item, MutationResult { op_id, revision })))
}

fn capture_tx(tx: &mut WriteTx<'_>, input: CaptureInput) -> Result<(Item, MutationResult)> {
    let now = ts_to_sql(Timestamp::now())?;
    let conn = tx.conn();
    conn.prepare_cached("DELETE FROM request_ids WHERE at < ?1")?
        .execute([now - REQUEST_ID_TTL_NS])?;
    if !input.request_id.is_empty()
        && let Some(replay) = capture_replay(
            conn,
            "SELECT op_id, revision, item_id FROM request_ids WHERE request_id = ?1",
            &input.request_id,
        )?
    {
        return Ok(replay);
    }
    if let Some(spool_id) = &input.spool_id
        && let Some(replay) = capture_replay(
            conn,
            "SELECT op_id, revision, item_id FROM spool_imports WHERE spool_id = ?1",
            spool_id,
        )?
    {
        return Ok(replay);
    }
    if let Some(parent) = input.parent_id {
        let exists = conn
            .prepare_cached("SELECT 1 FROM items WHERE id = ?1 AND deleted_at IS NULL")?
            .exists([parent.to_string()])?;
        if !exists {
            return Err(StoreError::NotFound(parent));
        }
    }

    let op_id = new_ulid();
    let revision = tx.next_revision();
    let mut item = Item::new_capture(new_ulid(), input.title, input.created_via, input.now);
    item.revision = revision;
    item.core.parent_id = input.parent_id;
    item.derived.staleness.status_entered_at = Some(input.now);
    if actor_for(input.created_via) == "user" {
        item.derived.staleness.last_user_touch_at = Some(input.now);
    }
    let item_id = item.id.to_string();
    let op_str = op_id.to_string();
    let actor = actor_for(input.created_via);
    let request_id = (!input.request_id.is_empty()).then_some(input.request_id.as_str());

    codec::insert_item(conn, &item)?;
    conn.prepare_cached(
        "INSERT INTO ops (id, kind, actor, at, request_id, revision)
         VALUES (?1, 'capture', ?2, ?3, ?4, ?5)",
    )?
    .execute(params![
        op_str,
        actor,
        now,
        request_id,
        rev_to_sql(revision)?
    ])?;
    conn.prepare_cached(
        "INSERT INTO events (id, op_id, item_id, at, actor, kind, new)
         VALUES (?1, ?2, ?3, ?4, ?5, 'create', ?6)",
    )?
    .execute(params![
        new_ulid().to_string(),
        op_str,
        item_id,
        now,
        actor,
        serde_json::Value::from(item.core.title.as_str()).to_string(),
    ])?;
    if let Some(request_id) = request_id {
        conn.prepare_cached(
            "INSERT INTO request_ids (request_id, op_id, revision, item_id, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            request_id,
            op_str,
            rev_to_sql(revision)?,
            item_id,
            now
        ])?;
    }
    if let Some(spool_id) = &input.spool_id {
        conn.prepare_cached(
            "INSERT INTO spool_imports (spool_id, op_id, revision, item_id, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            spool_id,
            op_str,
            rev_to_sql(revision)?,
            item_id,
            now
        ])?;
    }
    let recorded = tx.record_change(
        Topic::Item,
        &item_id,
        false,
        Some(Resource::Item(Arc::new(item.clone()))),
    )?;
    debug_assert_eq!(recorded, revision);
    Ok((item, MutationResult { op_id, revision }))
}

/// `p2.<query fingerprint>.<created_at ns>.<id>`: the last key, bound to the
/// query it came from.
struct PageToken {
    query: u64,
    created_at: i64,
    id: String,
}

impl PageToken {
    fn encode(&self) -> String {
        format!("p2.{:016x}.{}.{}", self.query, self.created_at, self.id)
    }

    fn decode(s: &str, query: u64) -> Result<Self> {
        let mut parts = s.split('.');
        let (Some("p2"), Some(fp), Some(at), Some(id), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(StoreError::InvalidPageToken);
        };
        if u64::from_str_radix(fp, 16) != Ok(query) {
            return Err(StoreError::InvalidPageToken);
        }
        codec::item_id_from_str(id).map_err(|_| StoreError::InvalidPageToken)?;
        Ok(PageToken {
            query,
            created_at: at.parse().map_err(|_| StoreError::InvalidPageToken)?,
            id: id.to_owned(),
        })
    }
}

/// FNV-1a over the parts of a [`ListQuery`] that select rows, so a token is
/// only accepted by the query that issued it. `today` is left out: paging
/// across midnight stays valid.
fn query_fingerprint(query: &ListQuery) -> u64 {
    let mut statuses: Vec<&str> = query.statuses.iter().map(|s| s.as_sql()).collect();
    statuses.sort_unstable();
    statuses.dedup();
    let flags = [
        query.include_archived,
        query.include_done,
        query.include_deferred,
        query.include_deleted,
    ]
    .map(|b| if b { "1" } else { "0" })
    .concat();
    let key = format!("{}|{flags}", statuses.join(","));
    key.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn today_local() -> jiff::civil::Date {
    jiff::Zoned::now().date()
}

fn current_revision_in(conn: &Connection) -> Result<Revision> {
    rev_from_sql(conn.query_row("SELECT revision FROM meta", [], |r| r.get(0))?)
}

fn list_items_tx(conn: &Connection, query: ListQuery) -> Result<ItemPage> {
    use rusqlite::types::Value;

    let page_size = match query.page_size {
        0 => DEFAULT_PAGE_SIZE,
        n => n.min(MAX_PAGE_SIZE),
    } as usize;
    let fingerprint = query_fingerprint(&query);
    let token = query
        .page_token
        .as_deref()
        .map(|t| PageToken::decode(t, fingerprint))
        .transpose()?;
    let today = query.today.unwrap_or_else(today_local);

    let tx = conn.unchecked_transaction()?;
    let revision = current_revision_in(&tx)?;

    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    if !query.statuses.is_empty() {
        let marks = vec!["?"; query.statuses.len()].join(", ");
        clauses.push(format!("status IN ({marks})"));
        args.extend(
            query
                .statuses
                .iter()
                .map(|s| Value::Text(s.as_sql().into())),
        );
    }
    if !query.include_deleted {
        clauses.push("deleted_at IS NULL".into());
    }
    if !query.include_archived {
        clauses.push("archived_at IS NULL".into());
    }
    if !query.include_done {
        clauses.push("status NOT IN ('done', 'cancelled')".into());
    }
    if !query.include_deferred {
        clauses.push("(defer_until IS NULL OR defer_until <= ?)".into());
        args.push(Value::Text(date_to_sql(today)));
    }
    if let Some(t) = &token {
        clauses.push("(created_at, id) > (?, ?)".into());
        args.push(Value::Integer(t.created_at));
        args.push(Value::Text(t.id.clone()));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    args.push(Value::Integer(page_size as i64 + 1));
    let sql = format!(
        "SELECT {ITEM_COLUMNS}, created_at FROM items {where_sql}
         ORDER BY created_at, id LIMIT ?"
    );

    let mut stmt = tx.prepare(&sql)?;
    let created_at_col = stmt.column_count() - 1;
    let mut rows = stmt.query(rusqlite::params_from_iter(args))?;
    let mut items = Vec::new();
    let mut last_created_at = 0;
    let mut has_more = false;
    while let Some(row) = rows.next()? {
        if items.len() == page_size {
            has_more = true;
            break;
        }
        items.push(item_from_row(row, Some(today))?);
        last_created_at = row.get(created_at_col)?;
    }
    let next_page_token = match items.last() {
        Some(last) if has_more => Some(
            PageToken {
                query: fingerprint,
                created_at: last_created_at,
                id: last.id.to_string(),
            }
            .encode(),
        ),
        _ => None,
    };
    Ok(ItemPage {
        items,
        next_page_token,
        revision,
    })
}

fn changes_since_tx(
    conn: &Connection,
    since: Revision,
) -> Result<Vec<ChangeRow>, ChangesSinceError> {
    let tx = conn.unchecked_transaction().map_err(StoreError::from)?;
    let current = current_revision_in(&tx)?;
    if since > current {
        return Err(ChangesSinceError::Ahead { since, current });
    }
    let oldest: Option<i64> = tx
        .query_row("SELECT MIN(revision) FROM changes", [], |r| r.get(0))
        .map_err(StoreError::from)?;
    let oldest = oldest.map(rev_from_sql).transpose()?.unwrap_or(current + 1);
    if since + 1 < oldest {
        return Err(Expired { since, oldest }.into());
    }

    let today = today_local();
    let mut items: HashMap<String, Option<Arc<Item>>> = HashMap::new();
    let mut stmt = tx
        .prepare_cached(
            "SELECT revision, topic, resource_id, deleted FROM changes
             WHERE revision > ?1 ORDER BY revision",
        )
        .map_err(StoreError::from)?;
    let mut rows = stmt.query([rev_to_sql(since)?]).map_err(StoreError::from)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(StoreError::from)? {
        let revision = rev_from_sql(row.get(0).map_err(StoreError::from)?)?;
        let topic = Topic::from_sql(&row.get::<_, String>(1).map_err(StoreError::from)?)?;
        let id: String = row.get(2).map_err(StoreError::from)?;
        let deleted: bool = row.get(3).map_err(StoreError::from)?;
        let resource = match topic {
            Topic::Item if !deleted => {
                let item = match items.get(&id) {
                    Some(item) => item.clone(),
                    None => {
                        let item = load_item(&tx, &id, Some(today))?.map(Arc::new);
                        items.insert(id.clone(), item.clone());
                        item
                    }
                };
                let item = item.ok_or_else(|| {
                    StoreError::Corrupt(format!("change {revision} names missing item {id}"))
                })?;
                Some(Resource::Item(item))
            }
            _ => None,
        };
        out.push(ChangeRow {
            revision,
            topic,
            id,
            deleted,
            resource,
        });
    }
    Ok(out)
}
