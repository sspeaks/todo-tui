//! SQLite persistence, migrations, revisions and events.
//!
//! The daemon is the only process that opens the database. All writes run on
//! one dedicated writer thread, in one SQLite transaction per [`Store::write`]
//! call; reads run on a small pool of read-only connections (WAL mode).

mod codec;
mod schema;

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use jiff::Timestamp;
use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use todo_core::{
    Actor, ApplyCtx, CaptureDraft, CreatedVia, FieldChange, FieldValue, Item, ItemField, ItemId,
    Mutation, MutationError, MutationResult, PersonRef, Revision, Status, StatusFields, Topic,
    apply, new_ulid,
};
use tokio::sync::{Semaphore, broadcast, oneshot};
use ulid::Ulid;

use codec::{
    ITEM_COLUMNS, SqlEnum, date_to_sql, is_overdue, item_from_row, load_item, rev_from_sql,
    rev_to_sql, ts_to_sql,
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
    #[error("item {id}: revision conflict: expected {expected}, found {actual}")]
    RevisionMismatch {
        id: ItemId,
        expected: Revision,
        actual: Revision,
    },
    /// Mutation `index` of a batch (0 for a capture) is invalid.
    #[error("mutation {index}: {error}")]
    Mutation {
        index: usize,
        #[source]
        error: MutationError,
    },
    /// The request id is recorded for an op of another kind (a capture, or a
    /// mutation of another [`OpKind`]): the client reused it for a different
    /// request, and replaying the original would answer the wrong call.
    #[error("the request id was already used for a different request")]
    RequestIdReused,
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
    /// The parsed text. An empty title fails with [`StoreError::Mutation`];
    /// tokens and warnings are not stored.
    pub draft: CaptureDraft,
    pub created_via: CreatedVia,
    pub parent_id: Option<ItemId>,
    /// Spool entry id; a spool id already imported returns the original result.
    pub spool_id: Option<String>,
    /// Creation time; the daemon passes the capture's reference time or now.
    pub now: Timestamp,
    /// "Today" in the user's zone, for `is_overdue` on the returned Item.
    pub today: Date,
}

/// What an op does, recorded as `ops.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Update,
    SetStatus,
    Archive,
    Unarchive,
    Delete,
    Undelete,
    /// `BatchMutate`.
    Batch,
}

/// Input for [`Store::mutate`].
#[derive(Debug, Clone, PartialEq)]
pub struct MutateInput {
    /// Client ULID for idempotency (24h window); empty for none.
    pub request_id: String,
    pub actor: Actor,
    pub kind: OpKind,
    /// Applied in order; several may target one Item.
    pub mutations: Vec<Mutation>,
    /// The mutation time, for `updated_at` and the staleness signals.
    pub now: Timestamp,
    /// "Today" in the user's zone, for `is_overdue` on the returned Items.
    pub today: Date,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MutateOutput {
    /// One per mutation, in request order, each in its state after the
    /// whole batch.
    pub items: Vec<Item>,
    pub result: MutationResult,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GetItemOutput {
    pub item: Item,
    /// The revision this was read at.
    pub revision: Revision,
    /// The requested id when it named a merged Item; `item` is then the end
    /// of its merge chain.
    pub resolved_from: Option<ItemId>,
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
const REQUEST_SQL: &str = "SELECT r.op_id, r.revision, r.item_id, ops.kind
     FROM request_ids r JOIN ops ON ops.id = r.op_id WHERE r.request_id = ?1";
const SPOOL_SQL: &str = "SELECT s.op_id, s.revision, s.item_id, ops.kind
     FROM spool_imports s JOIN ops ON ops.id = s.op_id WHERE s.spool_id = ?1";
/// `ops.kind` of a capture.
const CAPTURE_KIND: &str = "capture";

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

    /// Creates an Item from a parsed draft (one op, one revision).
    /// Idempotent on `request_id` and on `spool_id`: a replay returns the
    /// original result with the Item's current state, without writing. A
    /// `request_id` recorded by [`Store::mutate`] fails with
    /// [`StoreError::RequestIdReused`].
    pub async fn capture(&self, input: CaptureInput) -> Result<(Item, MutationResult)> {
        self.write(move |tx| capture_tx(tx, input)).await
    }

    /// Applies the mutations in order as one op, all or nothing. Each touched
    /// Item gets one revision (contiguous, in first-touch order) and one
    /// `changes` row, and each changed field one event. A batch that changes
    /// nothing still records its op, at the current revision. Idempotent on
    /// `request_id`: a replay returns the original result with the Items'
    /// current state, without checking `expected_revision` or writing. A
    /// `request_id` recorded by a capture or a mutation of another
    /// [`OpKind`] fails with [`StoreError::RequestIdReused`].
    pub async fn mutate(&self, input: MutateInput) -> Result<MutateOutput> {
        self.write(move |tx| mutate_tx(tx, input)).await
    }

    /// The Item `id`, read with its revision. A soft-deleted Item is returned
    /// with `deleted_at` set; a merged Item resolves to the end of its merge
    /// chain.
    pub async fn get_item(&self, id: ItemId, today: Date) -> Result<GetItemOutput> {
        self.read(move |conn| get_item_tx(conn, id, today)).await
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

fn actor_for(via: CreatedVia) -> Actor {
    match via {
        CreatedVia::Tui | CreatedVia::Cli | CreatedVia::Mcp | CreatedVia::Split => Actor::User,
        CreatedVia::RemoteImport => Actor::Sync,
        CreatedVia::Restore | CreatedVia::Recurrence => Actor::System,
    }
}

/// The `ops` row of one write, and what its events and `request_ids` row
/// share.
struct OpRow<'a> {
    id: Ulid,
    kind: &'static str,
    actor: String,
    /// When the write happened (wall clock), for `ops`, `events` and
    /// `request_ids`.
    at: i64,
    request_id: Option<&'a str>,
}

impl<'a> OpRow<'a> {
    /// A new op; an empty `request_id` means none.
    fn new(kind: &'static str, actor: Actor, request_id: &'a str) -> Result<Self> {
        Ok(OpRow {
            id: new_ulid(),
            kind,
            actor: codec::actor_to_sql(actor),
            at: ts_to_sql(Timestamp::now())?,
            request_id: (!request_id.is_empty()).then_some(request_id),
        })
    }

    /// `revision` is the last revision the op allocates (the current one if
    /// it allocates none).
    fn insert(&self, conn: &Connection, revision: Revision) -> Result<()> {
        conn.prepare_cached(
            "INSERT INTO ops (id, kind, actor, at, request_id, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![
            self.id.to_string(),
            self.kind,
            self.actor,
            self.at,
            self.request_id,
            rev_to_sql(revision)?
        ])?;
        Ok(())
    }

    fn insert_event(
        &self,
        conn: &Connection,
        item_id: &str,
        kind: &str,
        field: Option<ItemField>,
        old: Option<String>,
        new: Option<String>,
    ) -> Result<()> {
        conn.prepare_cached(
            "INSERT INTO events (id, op_id, item_id, at, actor, kind, field, old, new)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?
        .execute(params![
            new_ulid().to_string(),
            self.id.to_string(),
            item_id,
            self.at,
            self.actor,
            kind,
            field.map(ItemField::number),
            old,
            new
        ])?;
        Ok(())
    }

    /// Remembers the request id, if any, for replays.
    fn insert_request_id(
        &self,
        conn: &Connection,
        revision: Revision,
        item_id: Option<&str>,
    ) -> Result<()> {
        let Some(request_id) = self.request_id else {
            return Ok(());
        };
        conn.prepare_cached(
            "INSERT INTO request_ids (request_id, op_id, revision, item_id, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            request_id,
            self.id.to_string(),
            rev_to_sql(revision)?,
            item_id,
            self.at
        ])?;
        Ok(())
    }
}

/// Forgets request ids older than the 24h idempotency window.
fn prune_request_ids(conn: &Connection, now: i64) -> Result<()> {
    conn.prepare_cached("DELETE FROM request_ids WHERE at < ?1")?
        .execute([now - REQUEST_ID_TTL_NS])?;
    Ok(())
}

/// The op recorded under an idempotency key by `sql` ([`REQUEST_SQL`] or
/// [`SPOOL_SQL`]): its id, revision and Item id. Fails with
/// [`StoreError::RequestIdReused`] when that op's `ops.kind` is not `kind`.
fn recorded_op(
    conn: &Connection,
    sql: &str,
    key: &str,
    kind: &str,
) -> Result<Option<(Ulid, Revision, Option<String>)>> {
    let Some((op_id, revision, item_id, recorded_kind)) = conn
        .prepare_cached(sql)?
        .query_row([key], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .optional()?
    else {
        return Ok(None);
    };
    if recorded_kind != kind {
        return Err(StoreError::RequestIdReused);
    }
    let op_id = op_id
        .parse()
        .map_err(|_| StoreError::Corrupt(format!("bad op id {op_id:?}")))?;
    Ok(Some((op_id, rev_from_sql(revision)?, item_id)))
}

/// The original result of an idempotent capture, if any.
fn capture_replay(
    conn: &Connection,
    sql: &str,
    key: &str,
    today: Date,
) -> Result<Option<(Item, MutationResult)>> {
    let Some((op_id, revision, item_id)) = recorded_op(conn, sql, key, CAPTURE_KIND)? else {
        return Ok(None);
    };
    let item_id = item_id
        .ok_or_else(|| StoreError::Corrupt(format!("request {key} did not create an item")))?;
    let item = load_item(conn, &item_id, Some(today))?.ok_or_else(|| {
        StoreError::Corrupt(format!("item {item_id} for request {key} is missing"))
    })?;
    Ok(Some((item, MutationResult { op_id, revision })))
}

/// People arrive in a later milestone; until then only free text is stored.
fn people_unsupported(field: ItemField) -> MutationError {
    MutationError::Unsupported {
        field,
        reason: "people are not available yet".to_owned(),
    }
}

fn capture_tx(tx: &mut WriteTx<'_>, input: CaptureInput) -> Result<(Item, MutationResult)> {
    let actor = actor_for(input.created_via);
    let op = OpRow::new(CAPTURE_KIND, actor, &input.request_id)?;
    let conn = tx.conn();
    prune_request_ids(conn, op.at)?;
    if let Some(request_id) = op.request_id
        && let Some(replay) = capture_replay(conn, REQUEST_SQL, request_id, input.today)?
    {
        return Ok(replay);
    }
    if let Some(spool_id) = &input.spool_id
        && let Some(replay) = capture_replay(conn, SPOOL_SQL, spool_id, input.today)?
    {
        return Ok(replay);
    }
    let draft = input.draft;
    let invalid = |error| StoreError::Mutation { index: 0, error };
    if draft.title.trim().is_empty() {
        return Err(invalid(MutationError::Invalid {
            field: ItemField::Title,
            reason: "must not be empty".to_owned(),
        }));
    }
    if draft
        .waiting
        .as_ref()
        .is_some_and(|w| matches!(w.on, PersonRef::Person(_)))
    {
        return Err(invalid(people_unsupported(ItemField::WaitingOn)));
    }
    if let Some(parent) = input.parent_id {
        let exists = conn
            .prepare_cached("SELECT 1 FROM items WHERE id = ?1 AND deleted_at IS NULL")?
            .exists([parent.to_string()])?;
        if !exists {
            return Err(StoreError::NotFound(parent));
        }
    }

    let revision = tx.next_revision();
    let now = input.now;
    let mut item = Item::new_capture(new_ulid(), draft.title, input.created_via, now);
    item.revision = revision;
    item.core.parent_id = input.parent_id;
    item.core.priority = draft.priority;
    item.core.due_date = draft.due_date;
    item.core.labels = draft.labels;
    item.set_status_fields(StatusFields::from_status(draft.status));
    let staleness = &mut item.derived.staleness;
    staleness.status_entered_at = Some(now);
    staleness.waiting_since = draft.waiting.is_some().then_some(now);
    if actor == Actor::User {
        staleness.last_user_touch_at = Some(now);
    }
    item.overlay.waiting = draft.waiting;
    item.derived.is_overdue = is_overdue(&item, Some(input.today));
    let item_id = item.id.to_string();

    codec::insert_item(conn, &item)?;
    op.insert(conn, revision)?;
    op.insert_event(
        conn,
        &item_id,
        "create",
        None,
        None,
        Some(serde_json::Value::from(item.core.title.as_str()).to_string()),
    )?;
    op.insert_request_id(conn, revision, Some(&item_id))?;
    if let Some(spool_id) = &input.spool_id {
        conn.prepare_cached(
            "INSERT INTO spool_imports (spool_id, op_id, revision, item_id, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            spool_id,
            op.id.to_string(),
            rev_to_sql(revision)?,
            item_id,
            op.at
        ])?;
    }
    let recorded = tx.record_change(
        Topic::Item,
        &item_id,
        false,
        Some(Resource::Item(Arc::new(item.clone()))),
    )?;
    debug_assert_eq!(recorded, revision);
    Ok((
        item,
        MutationResult {
            op_id: op.id,
            revision,
        },
    ))
}

/// The working copies of the Items a batch has loaded, in first-touch order,
/// with the changes applied to each so far.
#[derive(Default)]
struct Batch {
    items: Vec<Item>,
    changes: Vec<Vec<FieldChange>>,
    slots: HashMap<ItemId, usize>,
}

impl Batch {
    /// The slot of `id`'s working copy, loading it on first use.
    fn load(&mut self, conn: &Connection, id: ItemId, today: Date) -> Result<usize> {
        if let Some(&slot) = self.slots.get(&id) {
            return Ok(slot);
        }
        let item =
            load_item(conn, &id.to_string(), Some(today))?.ok_or(StoreError::NotFound(id))?;
        self.items.push(item);
        self.changes.push(Vec::new());
        self.slots.insert(id, self.items.len() - 1);
        Ok(self.items.len() - 1)
    }

    /// `id`'s parent and whether it is deleted, as of this point in the
    /// batch; `None` when no such Item exists.
    fn parent_of(&self, conn: &Connection, id: ItemId) -> Result<Option<(Option<ItemId>, bool)>> {
        if let Some(&slot) = self.slots.get(&id) {
            let item = &self.items[slot];
            return Ok(Some((item.core.parent_id, item.is_deleted())));
        }
        let row = conn
            .prepare_cached("SELECT parent_id, deleted_at IS NOT NULL FROM items WHERE id = ?1")?
            .query_row([id.to_string()], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, bool>(1)?))
            })
            .optional()?;
        row.map(|(parent, deleted)| {
            let parent = parent.as_deref().map(codec::item_id_from_str).transpose()?;
            Ok((parent, deleted))
        })
        .transpose()
    }

    /// Why `parent` cannot become `id`'s parent, if it cannot.
    fn parent_problem(
        &self,
        conn: &Connection,
        id: ItemId,
        parent: ItemId,
    ) -> Result<Option<&'static str>> {
        match self.parent_of(conn, parent)? {
            None => return Ok(Some("no such Item")),
            Some((_, true)) => return Ok(Some("the parent is deleted")),
            Some(_) => {}
        }
        let mut seen = HashSet::new();
        let mut next = Some(parent);
        while let Some(ancestor) = next {
            if ancestor == id {
                return Ok(Some("would create a cycle"));
            }
            if !seen.insert(ancestor) {
                return Err(StoreError::Corrupt(format!(
                    "the parent chain of {parent} loops at {ancestor}"
                )));
            }
            next = self.parent_of(conn, ancestor)?.and_then(|(p, _)| p);
        }
        Ok(None)
    }

    /// The first change to `id` the database cannot take: a parent that is
    /// missing, deleted or a descendant, a person, or a duration too long for
    /// its nanosecond column.
    fn change_error(
        &self,
        conn: &Connection,
        id: ItemId,
        changes: &[FieldChange],
    ) -> Result<Option<MutationError>> {
        for change in changes {
            let error = match &change.new {
                FieldValue::ItemId(parent) if change.field == ItemField::ParentId => self
                    .parent_problem(conn, id, *parent)?
                    .map(|reason| MutationError::Invalid {
                        field: change.field,
                        reason: reason.to_owned(),
                    }),
                FieldValue::Person(PersonRef::Person(_)) => Some(people_unsupported(change.field)),
                FieldValue::Waiting(w) if matches!(w.on, PersonRef::Person(_)) => {
                    Some(people_unsupported(ItemField::WaitingOn))
                }
                FieldValue::Duration(d) if codec::dur_to_sql(Some(*d)).is_err() => {
                    Some(MutationError::Invalid {
                        field: change.field,
                        reason: "too long to store".to_owned(),
                    })
                }
                _ => None,
            };
            if error.is_some() {
                return Ok(error);
            }
        }
        Ok(None)
    }
}

fn mutate_tx(tx: &mut WriteTx<'_>, input: MutateInput) -> Result<MutateOutput> {
    let op = OpRow::new(input.kind.as_sql(), input.actor, &input.request_id)?;
    let today = input.today;
    let conn = tx.conn();
    prune_request_ids(conn, op.at)?;
    if let Some(request_id) = op.request_id
        && let Some((op_id, revision, _)) = recorded_op(conn, REQUEST_SQL, request_id, op.kind)?
    {
        let items = input
            .mutations
            .iter()
            .map(|m| {
                load_item(conn, &m.id.to_string(), Some(today))?.ok_or(StoreError::NotFound(m.id))
            })
            .collect::<Result<_>>()?;
        return Ok(MutateOutput {
            items,
            result: MutationResult { op_id, revision },
        });
    }

    let ctx = ApplyCtx {
        now: input.now,
        actor: input.actor,
    };
    let mut batch = Batch::default();
    let mut slots = Vec::with_capacity(input.mutations.len());
    for (index, m) in input.mutations.iter().enumerate() {
        let slot = batch.load(conn, m.id, today)?;
        // `apply` never changes `revision`, so this is the stored one.
        let stored = batch.items[slot].revision;
        if let Some(expected) = m.expected_revision
            && expected != stored
        {
            return Err(StoreError::RevisionMismatch {
                id: m.id,
                expected,
                actual: stored,
            });
        }
        let invalid = |error| StoreError::Mutation { index, error };
        let applied = apply(&batch.items[slot], &m.op, &ctx).map_err(invalid)?;
        if let Some(error) = batch.change_error(conn, m.id, &applied.changes)? {
            return Err(invalid(error));
        }
        batch.items[slot] = applied.item;
        batch.changes[slot].extend(applied.changes);
        slots.push(slot);
    }

    let touched: Vec<usize> = (0..batch.items.len())
        .filter(|&slot| !batch.changes[slot].is_empty())
        .collect();
    // The last revision this op allocates; the current one when it allocates none.
    let revision = tx.next_revision() + touched.len() as Revision - 1;
    op.insert(tx.conn(), revision)?;
    for item in &mut batch.items {
        item.derived.is_overdue = is_overdue(item, Some(today));
    }
    for slot in touched {
        let item = &mut batch.items[slot];
        item.revision = tx.next_revision();
        let item_id = item.id.to_string();
        codec::update_item(tx.conn(), item)?;
        for change in &batch.changes[slot] {
            op.insert_event(
                tx.conn(),
                &item_id,
                "set",
                Some(change.field),
                Some(codec::value_to_sql(&change.old)?),
                Some(codec::value_to_sql(&change.new)?),
            )?;
        }
        let resource = Resource::Item(Arc::new(item.clone()));
        tx.record_change(Topic::Item, &item_id, false, Some(resource))?;
    }
    debug_assert_eq!(tx.next_revision() - 1, revision);
    let first = input.mutations.first().map(|m| m.id.to_string());
    op.insert_request_id(tx.conn(), revision, first.as_deref())?;
    Ok(MutateOutput {
        items: slots
            .into_iter()
            .map(|slot| batch.items[slot].clone())
            .collect(),
        result: MutationResult {
            op_id: op.id,
            revision,
        },
    })
}

fn get_item_tx(conn: &Connection, id: ItemId, today: Date) -> Result<GetItemOutput> {
    let tx = conn.unchecked_transaction()?;
    let revision = current_revision_in(&tx)?;
    let mut item = load_item(&tx, &id.to_string(), Some(today))?.ok_or(StoreError::NotFound(id))?;
    let mut seen = HashSet::from([id]);
    while let Some(target) = item.derived.merged_into {
        if !seen.insert(target) {
            return Err(StoreError::Corrupt(format!(
                "the merge chain of {id} loops at {target}"
            )));
        }
        item = load_item(&tx, &target.to_string(), Some(today))?.ok_or_else(|| {
            StoreError::Corrupt(format!("{} is merged into missing item {target}", item.id))
        })?;
    }
    Ok(GetItemOutput {
        resolved_from: (item.id != id).then_some(id),
        item,
        revision,
    })
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
