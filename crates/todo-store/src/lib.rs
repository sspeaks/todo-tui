//! SQLite persistence, migrations, revisions and events.
//!
//! The daemon is the only process that opens the database. All writes run on
//! one dedicated writer thread, in one SQLite transaction per [`Store::write`]
//! call; reads run on a small pool of read-only connections (WAL mode).

mod codec;
mod relation;
mod schema;
mod summary;
mod undo;

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use jiff::Timestamp;
use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use todo_core::{
    Actor, Applied, ApplyCtx, CaptureDraft, CreatedVia, FieldChange, FieldValue, Item, ItemField,
    ItemId, Mutation, MutationError, MutationOp, MutationResult, PersonRef, Relation, Revision,
    SkipReason, Status, StatusFields, Topic, apply, new_ulid,
};
use tokio::sync::{Semaphore, broadcast, oneshot};
use ulid::Ulid;

use codec::{
    ITEM_COLUMNS, SqlEnum, date_to_sql, is_overdue, item_from_row, load_item, rev_from_sql,
    rev_to_sql, ts_to_sql,
};
pub use schema::SCHEMA_VERSION;
use summary::Action;

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
    /// mutation of another [`OpKind`]) or, for a single-item mutation, for
    /// another Item: the client reused it for a different request, and
    /// replaying the original would answer the wrong call.
    #[error("the request id was already used for a different request")]
    RequestIdReused,
    #[error("invalid page token")]
    InvalidPageToken,
    /// [`Store::undo`] or [`Store::redo`] was guarded by `expected`, but the
    /// entry it would act on is `top` (`None`: that stack is empty). Nothing
    /// is written. The daemon maps it to ABORTED with ErrorInfo
    /// `UNDO_STACK_CHANGED` (`expected_op_id`, and `top_op_id`, empty for
    /// `None`).
    #[error("the undo stack changed: expected entry {expected}, found {top:?}")]
    UndoStackChanged { expected: Ulid, top: Option<Ulid> },
    /// Mutation `index` of a batch undeleted `item_id`, but its parent
    /// `parent_id` is still deleted when the batch ends (design 3.7): the
    /// parent must be undeleted first, in this batch or before it. The
    /// daemon maps it to FAILED_PRECONDITION with ErrorInfo `PARENT_DELETED`
    /// (`item_id`, `parent_id`).
    #[error("mutation {index}: item {item_id}: its parent {parent_id} is deleted")]
    ParentDeleted {
        index: usize,
        item_id: ItemId,
        parent_id: ItemId,
    },
    /// A relation end names no Item, or (for an add) a deleted one. The store
    /// sees the canonical [`Relation`], whose ends a `relates_to` may have
    /// swapped, so the daemon maps by the end of the *request* that `item_id`
    /// is: the request's `from_id` gives NOT_FOUND (ErrorInfo
    /// `ITEM_NOT_FOUND`) or FAILED_PRECONDITION (ErrorInfo `ITEM_DELETED`),
    /// its `to_id` INVALID_ARGUMENT on `relation.to_id`. When both ends are
    /// wrong this names the canonical `from` end, which the daemon checks
    /// against the request's `from_id`.
    #[error("relation end {item_id} {problem}")]
    RelationEnd {
        item_id: ItemId,
        problem: EndProblem,
    },
    /// [`Store::mutate`] was handed something that is not a mutation: a
    /// [`MutationOp::Restore`] (internal to undo and redo) or an [`OpKind`]
    /// of another call. A bug in the caller; the daemon maps it to INTERNAL.
    #[error("not a mutation: {0}")]
    NotAMutation(&'static str),
    #[error("corrupt row: {0}")]
    Corrupt(String),
    #[error("store is closed")]
    Closed,
}

/// What is wrong with a relation end ([`StoreError::RelationEnd`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EndProblem {
    #[error("names no item")]
    Missing,
    #[error("is deleted")]
    Deleted,
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
    /// A missing or deleted parent fails with [`StoreError::Mutation`].
    pub parent_id: Option<ItemId>,
    /// Spool entry id; a spool id already imported returns the original result.
    pub spool_id: Option<String>,
    /// Creation time; the daemon passes the capture's reference time or now.
    pub now: Timestamp,
    /// "Today" in the user's zone, for `is_overdue` on the returned Item.
    pub today: Date,
}

/// What an op does, recorded as `ops.kind` (proto `OpKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// [`Store::capture`], including spool imports.
    Capture,
    Update,
    SetStatus,
    Archive,
    Unarchive,
    Delete,
    Undelete,
    /// `BatchMutate`.
    Batch,
    Undo,
    Redo,
    AddRelation,
    RemoveRelation,
}

impl OpKind {
    /// The kinds [`Store::mutate`] records.
    fn is_mutation(self) -> bool {
        matches!(
            self,
            OpKind::Update
                | OpKind::SetStatus
                | OpKind::Archive
                | OpKind::Unarchive
                | OpKind::Delete
                | OpKind::Undelete
                | OpKind::Batch
        )
    }
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
    /// whole batch. Descendants a delete or undelete cascaded to are not
    /// listed; their changes arrive on the change stream.
    pub items: Vec<Item>,
    pub result: MutationResult,
}

/// Input for [`Store::undo`] and [`Store::redo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoInput {
    /// Client ULID for idempotency (24h window); empty for none. Undo and
    /// redo share the namespace of every other call.
    pub request_id: String,
    /// The entry the caller means to act on ([`UndoState`]); another one, or
    /// an empty stack, fails with [`StoreError::UndoStackChanged`].
    pub expected_op_id: Option<Ulid>,
    /// `updated_at` of the Items it changes, and the soft delete that undoes
    /// a capture. Recorded values are restored exactly, never this.
    pub now: Timestamp,
    /// "Today" in the user's zone, for `is_overdue` on the returned Items.
    pub today: Date,
}

/// An undo stack entry (proto `OpSummary`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpSummaryRow {
    pub op_id: Ulid,
    pub kind: OpKind,
    /// The Items the op changed, in the order of its events (both ends of a
    /// relation).
    pub item_ids: Vec<ItemId>,
    /// When the op was recorded (wall clock).
    pub at: Timestamp,
    /// Toast text, e.g. `mark done (3 items)`.
    pub description: String,
}

/// A change [`Store::undo`] or [`Store::redo`] left as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoSkipRow {
    /// The Item; for a relation step, the relation's `from` end.
    pub item_id: ItemId,
    /// The fields restored together, in order (the status triple is one
    /// unit); empty for a relation step.
    pub fields: Vec<ItemField>,
    pub reason: SkipReason,
    /// The relation a relation step did not add or remove.
    pub relation: Option<Relation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UndoOutput {
    /// The undo or redo op; at the current revision when it changed nothing.
    pub result: MutationResult,
    /// The entry undone, redone or dropped; `None` when the stack was empty.
    pub entry: Option<OpSummaryRow>,
    /// The Items it changed, in their state after it, in event order.
    pub items: Vec<Item>,
    pub skipped: Vec<UndoSkipRow>,
}

/// The entries [`Store::undo`] and [`Store::redo`] would act on next.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UndoState {
    pub undo: Option<OpSummaryRow>,
    pub redo: Option<OpSummaryRow>,
}

/// Input for [`Store::add_relation`] and [`Store::remove_relation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationInput {
    /// Client ULID for idempotency (24h window); empty for none.
    pub request_id: String,
    pub relation: Relation,
    /// `updated_at` of both ends when the relation changes.
    pub now: Timestamp,
    /// "Today" in the user's zone, for `is_overdue` on the returned Items.
    pub today: Date,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelationOutput {
    /// The relation's ends as stored, in their state after the op.
    pub from_item: Item,
    pub to_item: Item,
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
/// Why an Item cannot be moved, or undeleted, under its parent.
const PARENT_IS_DELETED: &str = "the parent is deleted";

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

    /// Creates an Item from a parsed draft (one op, one revision). A capture
    /// by the user is an undo stack entry; undoing it soft-deletes the Item.
    /// Idempotent on `request_id` and on `spool_id`: a replay returns the
    /// original result with the Item's current state, without writing (an
    /// undone capture stays deleted). A `request_id` recorded by another
    /// kind of op fails with [`StoreError::RequestIdReused`].
    pub async fn capture(&self, input: CaptureInput) -> Result<(Item, MutationResult)> {
        self.write(move |tx| capture_tx(tx, input)).await
    }

    /// Applies the mutations in order as one op, all or nothing. Each touched
    /// Item gets one revision (contiguous, in first-touch order) and one
    /// `changes` row, and each changed field one event. A batch that changes
    /// nothing still records its op, at the current revision. Idempotent on
    /// `request_id`: a replay returns the original result with the Items'
    /// current state, without checking `expected_revision` or writing. A
    /// `request_id` recorded by a capture, by a mutation of another
    /// [`OpKind`] or, for a single-item kind, by a mutation of another Item
    /// fails with [`StoreError::RequestIdReused`]. A [`OpKind::Batch`]
    /// replay is matched by kind only, not by the Items it names.
    ///
    /// Parent rules (design 3.7): a delete also soft-deletes every live
    /// descendant, with the same `deleted_at`; an undelete also restores the
    /// descendants the Item's delete took with it, and fails with
    /// [`StoreError::ParentDeleted`] when the Item is live under a deleted
    /// parent at the end of the batch, so the order of a batch's undeletes
    /// does not matter. No live Item ever has a deleted parent.
    ///
    /// An op by the user or a suggestion that writes an event becomes the
    /// top undo stack entry and discards the redo stack (design 3.10).
    /// [`MutationOp::Restore`] and the kinds of other calls fail with
    /// [`StoreError::NotAMutation`].
    pub async fn mutate(&self, input: MutateInput) -> Result<MutateOutput> {
        self.write(move |tx| mutate_tx(tx, input)).await
    }

    /// Reverts the top undo stack entry as a new op (kind `undo`, actor
    /// user) and moves it to the redo stack (design 3.10). Each recorded
    /// change is restored only where the field still holds the value the
    /// entry wrote; the rest is reported in [`UndoOutput::skipped`]. When
    /// nothing is restored the entry is dropped from both stacks and no Item
    /// is written. With an empty stack it succeeds with no entry. Idempotent
    /// on `request_id` (a replay returns the original entry and skips, with
    /// the Items' current state); see [`UndoInput::expected_op_id`].
    pub async fn undo(&self, input: UndoInput) -> Result<UndoOutput> {
        self.write(move |tx| undo::undo_tx(tx, input, false)).await
    }

    /// Re-applies the most recently undone entry, by reverting what its last
    /// undo restored under the same guard, and moves it back to the undo
    /// stack. Otherwise as [`Store::undo`].
    pub async fn redo(&self, input: UndoInput) -> Result<UndoOutput> {
        self.write(move |tx| undo::undo_tx(tx, input, true)).await
    }

    /// The entries [`Store::undo`] and [`Store::redo`] would act on next.
    pub async fn undo_state(&self) -> Result<UndoState> {
        self.read(undo::undo_state_tx).await
    }

    /// Adds a relation (design 3.6) as one op that gives both ends a new
    /// revision. Both ends must exist and be live
    /// ([`StoreError::RelationEnd`]). Adding a relation that exists changes
    /// nothing. Idempotent on `request_id`: a replay returns the original
    /// result with both ends' current state, and the id reused for another
    /// relation (any of its ends or its kind) or another kind of op fails
    /// with [`StoreError::RequestIdReused`].
    pub async fn add_relation(&self, input: RelationInput) -> Result<RelationOutput> {
        self.write(move |tx| relation::relation_tx(tx, input, true))
            .await
    }

    /// Removes a relation, as [`Store::add_relation`] adds one. Either end
    /// may be deleted; removing a relation that does not exist changes
    /// nothing.
    pub async fn remove_relation(&self, input: RelationInput) -> Result<RelationOutput> {
        self.write(move |tx| relation::relation_tx(tx, input, false))
            .await
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
    kind: OpKind,
    actor: String,
    /// Whether the op becomes an undo stack entry once it writes an event:
    /// its actor is the user or a suggestion, and it is not an undo or redo.
    stackable: bool,
    /// When the write happened (wall clock), for `ops`, `events` and
    /// `request_ids`.
    at: i64,
    request_id: Option<&'a str>,
    /// `ops.summary`, the toast text.
    summary: String,
    /// `ops.entry_op`: the entry an undo or redo acted on.
    entry_op: Option<Ulid>,
    /// `ops.report`, kept for replays of the op's request id. For an undo or
    /// redo, the JSON of the changes it skipped, so a replay returns the
    /// original report. For a relation op that changed nothing, the JSON of
    /// the relation it was asked for, so a replay can check that it names
    /// the same relation (one that changed it records the relation in its
    /// event instead). The 0002 migration's comment names only the first
    /// use; it stays as databases applied it.
    report: Option<String>,
}

impl<'a> OpRow<'a> {
    /// A new op; an empty `request_id` means none.
    fn new(kind: OpKind, actor: Actor, request_id: &'a str) -> Result<Self> {
        Ok(OpRow {
            id: new_ulid(),
            kind,
            actor: codec::actor_to_sql(actor),
            stackable: matches!(actor, Actor::User | Actor::Suggestion(_))
                && !matches!(kind, OpKind::Undo | OpKind::Redo),
            at: ts_to_sql(Timestamp::now())?,
            request_id: (!request_id.is_empty()).then_some(request_id),
            summary: String::new(),
            entry_op: None,
            report: None,
        })
    }

    /// Inserts the op with the next `seq`. `revision` is the last revision
    /// the op allocates (the current one if it allocates none), and `events`
    /// how many events it writes: a stackable op with events is pushed onto
    /// the undo stack, which discards the redo stack (design 3.10).
    fn insert(&self, conn: &Connection, revision: Revision, events: usize) -> Result<()> {
        let entry = self.stackable && events > 0;
        if entry {
            conn.prepare_cached(
                "UPDATE ops SET undo_state = 'discarded' WHERE undo_state = 'undone'",
            )?
            .execute([])?;
        }
        conn.prepare_cached(
            "INSERT INTO ops (id, kind, actor, at, request_id, revision, seq, undo_state,
                              entry_op, summary, report)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, (SELECT COALESCE(MAX(seq), 0) + 1 FROM ops),
                     ?7, ?8, ?9, ?10)",
        )?
        .execute(params![
            self.id.to_string(),
            self.kind.as_sql(),
            self.actor,
            self.at,
            self.request_id,
            rev_to_sql(revision)?,
            entry.then_some("done"),
            self.entry_op.map(|id| id.to_string()),
            self.summary,
            self.report
        ])?;
        Ok(())
    }

    /// Inserts the op's event number `ord` (0, 1, ... across its Items).
    #[allow(clippy::too_many_arguments)]
    fn insert_event(
        &self,
        conn: &Connection,
        ord: usize,
        item_id: &str,
        kind: &str,
        field: Option<ItemField>,
        old: Option<String>,
        new: Option<String>,
    ) -> Result<()> {
        conn.prepare_cached(
            "INSERT INTO events (id, op_id, item_id, at, actor, kind, field, old, new, ord)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            new,
            ord as i64
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
/// [`StoreError::RequestIdReused`] when that op is not of `kind`.
fn recorded_op(
    conn: &Connection,
    sql: &str,
    key: &str,
    kind: OpKind,
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
    if recorded_kind != kind.as_sql() {
        return Err(StoreError::RequestIdReused);
    }
    let op_id = op_id
        .parse()
        .map_err(|_| StoreError::Corrupt(format!("bad op id {op_id:?}")))?;
    Ok(Some((op_id, rev_from_sql(revision)?, item_id)))
}

/// The Items `ids`, in their current state with their relations.
fn load_items(conn: &Connection, ids: &[ItemId], today: Date) -> Result<Vec<Item>> {
    let mut items = ids
        .iter()
        .map(|&id| load_item(conn, &id.to_string(), Some(today))?.ok_or(StoreError::NotFound(id)))
        .collect::<Result<Vec<_>>>()?;
    relation::attach(conn, &mut items)?;
    Ok(items)
}

/// The original result of an idempotent capture, if any.
fn capture_replay(
    conn: &Connection,
    sql: &str,
    key: &str,
    today: Date,
) -> Result<Option<(Item, MutationResult)>> {
    let Some((op_id, revision, item_id)) = recorded_op(conn, sql, key, OpKind::Capture)? else {
        return Ok(None);
    };
    let item_id = item_id
        .ok_or_else(|| StoreError::Corrupt(format!("request {key} did not create an item")))?;
    let item_id = codec::item_id_from_str(&item_id)?;
    let [item] = load_items(conn, &[item_id], today)
        .map_err(|e| match e {
            StoreError::NotFound(_) => {
                StoreError::Corrupt(format!("item {item_id} for request {key} is missing"))
            }
            e => e,
        })?
        .try_into()
        .expect("one id, one item");
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
    let mut op = OpRow::new(OpKind::Capture, actor, &input.request_id)?;
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
    if let Some(parent) = input.parent_id
        && let Some(reason) = Batch::default().unusable_parent(conn, parent)?
    {
        return Err(invalid(MutationError::Invalid {
            field: ItemField::ParentId,
            reason: reason.to_owned(),
        }));
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
    op.summary = summary::capture(&item.core.title);

    codec::insert_item(conn, &item)?;
    op.insert(conn, revision, 1)?;
    op.insert_event(
        conn,
        0,
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

/// An event of the op being built, in op order. The `derived` events are
/// added when the op is written.
// Nearly every event is a `Set`; boxing it would only add allocations.
#[allow(clippy::large_enum_variant)]
enum NewEvent {
    Set {
        item_id: ItemId,
        change: FieldChange,
    },
    RelationAdd(Relation),
    RelationRemove(Relation),
}

/// Which op last soft-deleted an Item: the one being built, or a stored one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeletedBy {
    ThisOp,
    Op(String),
}

/// The working copies of the Items an op has loaded, in first-touch order,
/// with the events it has made so far.
#[derive(Default)]
struct Batch {
    items: Vec<Item>,
    /// Each Item as loaded, to tell which ones the op changed.
    loaded: Vec<Item>,
    slots: HashMap<ItemId, usize>,
    /// Slots that get a revision even if their Item compares equal: the
    /// ones with events, and relation ends.
    dirty: HashSet<usize>,
    events: Vec<NewEvent>,
    /// Items this op soft-deleted and has not undeleted since.
    deleted_here: HashSet<ItemId>,
}

impl Batch {
    /// The slot of `id`'s working copy, loading it on first use.
    fn load(&mut self, conn: &Connection, id: ItemId, today: Date) -> Result<usize> {
        if let Some(&slot) = self.slots.get(&id) {
            return Ok(slot);
        }
        let item =
            load_item(conn, &id.to_string(), Some(today))?.ok_or(StoreError::NotFound(id))?;
        self.loaded.push(item.clone());
        self.items.push(item);
        self.slots.insert(id, self.items.len() - 1);
        Ok(self.items.len() - 1)
    }

    /// Takes `applied` as the slot's Item, with an event per change.
    fn record(&mut self, slot: usize, applied: Applied) {
        let item_id = applied.item.id;
        for change in &applied.changes {
            if change.field == ItemField::DeletedAt {
                match change.new {
                    FieldValue::Cleared => self.deleted_here.remove(&item_id),
                    _ => self.deleted_here.insert(item_id),
                };
            }
        }
        if !applied.changes.is_empty() {
            self.dirty.insert(slot);
        }
        self.events.extend(
            applied
                .changes
                .into_iter()
                .map(|change| NewEvent::Set { item_id, change }),
        );
        self.items[slot] = applied.item;
    }

    /// The touched slots, in first-touch order: changed, or with events.
    fn touched(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&slot| self.dirty.contains(&slot) || self.items[slot] != self.loaded[slot])
            .collect()
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

    /// Whether `id` is a deleted Item, as of this point in the batch.
    fn is_deleted(&self, conn: &Connection, id: ItemId) -> Result<bool> {
        Ok(self
            .parent_of(conn, id)?
            .is_some_and(|(_, deleted)| deleted))
    }

    /// `id`'s children and whether each is deleted, as of this point in the
    /// batch: stored children the batch has not moved away, then working
    /// copies the batch moved under `id`.
    fn children(&self, conn: &Connection, id: ItemId) -> Result<Vec<(ItemId, bool)>> {
        let mut stmt = conn.prepare_cached(
            "SELECT id, deleted_at IS NOT NULL FROM items WHERE parent_id = ?1
             ORDER BY created_at, id",
        )?;
        let mut rows = stmt.query([id.to_string()])?;
        let mut children = Vec::new();
        while let Some(row) = rows.next()? {
            let child = codec::item_id_from_str(&row.get::<_, String>(0)?)?;
            match self.slots.get(&child) {
                Some(&slot) => {
                    let item = &self.items[slot];
                    if item.core.parent_id == Some(id) {
                        children.push((child, item.is_deleted()));
                    }
                }
                None => children.push((child, row.get(1)?)),
            }
        }
        for item in &self.items {
            if item.core.parent_id == Some(id) && !children.iter().any(|(c, _)| *c == item.id) {
                children.push((item.id, item.is_deleted()));
            }
        }
        Ok(children)
    }

    /// `id`'s live descendants, top-down (breadth first).
    fn live_descendants(&self, conn: &Connection, id: ItemId) -> Result<Vec<ItemId>> {
        let mut seen = HashSet::from([id]);
        let mut queue = VecDeque::from([id]);
        let mut out = Vec::new();
        while let Some(parent) = queue.pop_front() {
            for (child, deleted) in self.children(conn, parent)? {
                if deleted {
                    continue;
                }
                if !seen.insert(child) {
                    return Err(StoreError::Corrupt(format!(
                        "the descendants of {id} loop at {child}"
                    )));
                }
                out.push(child);
                queue.push_back(child);
            }
        }
        Ok(out)
    }

    /// The op of `id`'s latest `deleted_at` event, as of this point in the
    /// batch; `None` for an Item never deleted through an op.
    fn deleted_by(&self, conn: &Connection, id: ItemId) -> Result<Option<DeletedBy>> {
        if self.deleted_here.contains(&id) {
            return Ok(Some(DeletedBy::ThisOp));
        }
        let op = conn
            .prepare_cached(
                "SELECT e.op_id FROM events e JOIN ops ON ops.id = e.op_id
                 WHERE e.item_id = ?1 AND e.kind = 'set' AND e.field = ?2
                 ORDER BY ops.seq DESC, e.ord DESC LIMIT 1",
            )?
            .query_row(
                params![id.to_string(), ItemField::DeletedAt.number()],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(op.map(DeletedBy::Op))
    }

    /// Undeletes, top-down, the descendants of the just undeleted `id` that
    /// `by` deleted, stopping at the ones it did not. Returns them.
    fn undelete_descendants(
        &mut self,
        conn: &Connection,
        id: ItemId,
        by: &DeletedBy,
        ctx: &ApplyCtx,
        today: Date,
    ) -> Result<Vec<ItemId>> {
        let mut seen = HashSet::from([id]);
        let mut queue = VecDeque::from([id]);
        let mut restored = Vec::new();
        while let Some(parent) = queue.pop_front() {
            for (child, deleted) in self.children(conn, parent)? {
                if !deleted || self.deleted_by(conn, child)?.as_ref() != Some(by) {
                    continue;
                }
                if !seen.insert(child) {
                    return Err(StoreError::Corrupt(format!(
                        "the descendants of {id} loop at {child}"
                    )));
                }
                let slot = self.load(conn, child, today)?;
                let applied = apply(&self.items[slot], &MutationOp::Undelete, ctx)
                    .map_err(|e| StoreError::Corrupt(format!("undeleting {child}: {e}")))?;
                self.record(slot, applied);
                restored.push(child);
                queue.push_back(child);
            }
        }
        Ok(restored)
    }

    /// The parent rule for the Items an op undeleted, `(mutation, Item)` in
    /// op order, as the op leaves them: one still live under a deleted parent
    /// is a [`StoreError::ParentDeleted`] of the last mutation that undeleted
    /// it. Only these Items are checked, so a live Item that was already
    /// under a deleted parent (an M1 database, from before deletes cascaded)
    /// does not fail an op that only touches it.
    fn orphan(
        &self,
        conn: &Connection,
        undeleted: &[(usize, ItemId)],
    ) -> Result<Option<StoreError>> {
        let last: HashMap<ItemId, usize> = undeleted
            .iter()
            .enumerate()
            .map(|(at, &(_, id))| (id, at))
            .collect();
        for (at, &(index, item_id)) in undeleted.iter().enumerate() {
            if last[&item_id] != at {
                continue;
            }
            let item = &self.items[self.slots[&item_id]];
            if let Some(parent_id) = item.core.parent_id
                && !item.is_deleted()
                && self.is_deleted(conn, parent_id)?
            {
                return Ok(Some(StoreError::ParentDeleted {
                    index,
                    item_id,
                    parent_id,
                }));
            }
        }
        Ok(None)
    }

    /// Why `parent` cannot be any Item's parent (it is missing or deleted),
    /// if it cannot.
    fn unusable_parent(&self, conn: &Connection, parent: ItemId) -> Result<Option<&'static str>> {
        Ok(match self.parent_of(conn, parent)? {
            None => Some("no such Item"),
            Some((_, true)) => Some(PARENT_IS_DELETED),
            Some(_) => None,
        })
    }

    /// Why `parent` cannot become `id`'s parent, if it cannot. A deleted
    /// `parent` can when `deleted_ok`.
    fn parent_problem(
        &self,
        conn: &Connection,
        id: ItemId,
        parent: ItemId,
        deleted_ok: bool,
    ) -> Result<Option<&'static str>> {
        match self.unusable_parent(conn, parent)? {
            Some(PARENT_IS_DELETED) if deleted_ok => {}
            Some(reason) => return Ok(Some(reason)),
            None => {}
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
    /// missing, deleted (unless `deleted_parent_ok`: undo and redo check the
    /// parent rule once they are done) or a descendant, a person, or a
    /// duration too long for its nanosecond column.
    fn change_error(
        &self,
        conn: &Connection,
        id: ItemId,
        changes: &[FieldChange],
        deleted_parent_ok: bool,
    ) -> Result<Option<MutationError>> {
        for change in changes {
            let error = match &change.new {
                FieldValue::ItemId(parent) if change.field == ItemField::ParentId => self
                    .parent_problem(conn, id, *parent, deleted_parent_ok)?
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

/// Writes the op a [`Batch`] built: the `ops` row (onto the undo stack if it
/// is an entry, see [`OpRow::insert`]); one revision, `items` row and
/// `changes` row per touched Item, in first-touch order; the events in op
/// order; and one `derived` event per touched Item whose staleness signals
/// changed. Attaches every working copy's relations. Returns the op's last
/// revision (the current one if it allocates none) and the touched slots in
/// the order their events first name them.
fn write_op(
    tx: &mut WriteTx<'_>,
    op: &OpRow<'_>,
    batch: &mut Batch,
    today: Date,
) -> Result<(Revision, Vec<usize>)> {
    let touched = batch.touched();
    let derived: Vec<usize> = touched
        .iter()
        .copied()
        .filter(|&slot| batch.items[slot].derived.staleness != batch.loaded[slot].derived.staleness)
        .collect();
    let revision = tx.next_revision() + touched.len() as Revision - 1;
    op.insert(tx.conn(), revision, batch.events.len() + derived.len())?;
    for item in &mut batch.items {
        item.derived.is_overdue = is_overdue(item, Some(today));
    }
    relation::attach(tx.conn(), &mut batch.items)?;
    for &slot in &touched {
        let item = &mut batch.items[slot];
        item.revision = tx.next_revision();
        codec::update_item(tx.conn(), item)?;
        let resource = Resource::Item(Arc::new(item.clone()));
        tx.record_change(Topic::Item, &item.id.to_string(), false, Some(resource))?;
    }
    debug_assert_eq!(tx.next_revision() - 1, revision);

    let conn = tx.conn();
    let mut order: Vec<ItemId> = Vec::new();
    let mut named = |id: ItemId| {
        if !order.contains(&id) {
            order.push(id);
        }
    };
    let mut ord = 0;
    for event in &batch.events {
        match event {
            NewEvent::Set { item_id, change } => {
                named(*item_id);
                op.insert_event(
                    conn,
                    ord,
                    &item_id.to_string(),
                    "set",
                    Some(change.field),
                    Some(codec::value_to_sql(&change.old)?),
                    Some(codec::value_to_sql(&change.new)?),
                )?;
            }
            NewEvent::RelationAdd(relation) | NewEvent::RelationRemove(relation) => {
                named(relation.from());
                named(relation.to());
                let json = Some(codec::relation_to_sql(relation)?);
                let (kind, old, new) = match event {
                    NewEvent::RelationAdd(_) => ("relation_add", None, json),
                    _ => ("relation_remove", json, None),
                };
                op.insert_event(
                    conn,
                    ord,
                    &relation.from().to_string(),
                    kind,
                    None,
                    old,
                    new,
                )?;
            }
        }
        ord += 1;
    }
    for &slot in &derived {
        let item_id = batch.items[slot].id;
        named(item_id);
        op.insert_event(
            conn,
            ord,
            &item_id.to_string(),
            "derived",
            Some(ItemField::Staleness),
            Some(codec::staleness_to_sql(
                batch.loaded[slot].derived.staleness,
            )?),
            Some(codec::staleness_to_sql(
                batch.items[slot].derived.staleness,
            )?),
        )?;
        ord += 1;
    }
    let mut slots: Vec<usize> = order.iter().map(|id| batch.slots[id]).collect();
    // Every touched Item has an event; this only keeps the list complete.
    for slot in touched {
        if !slots.contains(&slot) {
            slots.push(slot);
        }
    }
    Ok((revision, slots))
}

fn mutate_tx(tx: &mut WriteTx<'_>, input: MutateInput) -> Result<MutateOutput> {
    if !input.kind.is_mutation() {
        return Err(StoreError::NotAMutation("the op kind of another call"));
    }
    if input
        .mutations
        .iter()
        .any(|m| matches!(m.op, MutationOp::Restore(_)))
    {
        return Err(StoreError::NotAMutation(
            "Restore is internal to undo and redo",
        ));
    }
    let mut op = OpRow::new(input.kind, input.actor, &input.request_id)?;
    let today = input.today;
    let conn = tx.conn();
    prune_request_ids(conn, op.at)?;
    if let Some(request_id) = op.request_id
        && let Some((op_id, revision, item_id)) =
            recorded_op(conn, REQUEST_SQL, request_id, op.kind)?
    {
        // A single-item op recorded its Item, so the same request id on
        // another Item is a different request. A batch recorded only its
        // first Item and is matched by kind alone.
        if input.kind != OpKind::Batch
            && item_id != input.mutations.first().map(|m| m.id.to_string())
        {
            return Err(StoreError::RequestIdReused);
        }
        let ids: Vec<ItemId> = input.mutations.iter().map(|m| m.id).collect();
        return Ok(MutateOutput {
            items: load_items(conn, &ids, today)?,
            result: MutationResult { op_id, revision },
        });
    }

    let ctx = ApplyCtx {
        now: input.now,
        actor: input.actor,
    };
    let mut batch = Batch::default();
    let mut slots = Vec::with_capacity(input.mutations.len());
    // For the summary: the Items a cascade took along that no mutation
    // names, so deleting a parent with its own subtasks reads the same in
    // either order.
    let named: HashSet<ItemId> = input.mutations.iter().map(|m| m.id).collect();
    let mut subtasks = HashSet::new();
    // Each undelete with the mutation that made it, for the parent rule,
    // which holds for the batch as a whole.
    let mut undeleted: Vec<(usize, ItemId)> = Vec::new();
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
        // Read before an undelete replaces it.
        let deleted_by = match m.op {
            MutationOp::Undelete => batch.deleted_by(conn, m.id)?,
            _ => None,
        };
        let applied = apply(&batch.items[slot], &m.op, &ctx).map_err(invalid)?;
        if let Some(error) = batch.change_error(conn, m.id, &applied.changes, false)? {
            return Err(invalid(error));
        }
        let changed = !applied.changes.is_empty();
        match m.op {
            MutationOp::Delete if changed => {
                // The live descendants go too, deepest first, then the Item:
                // undo works in reverse, so it undeletes top-down.
                let descendants = batch.live_descendants(conn, m.id)?;
                for &id in descendants.iter().rev() {
                    let slot = batch.load(conn, id, today)?;
                    let applied =
                        apply(&batch.items[slot], &MutationOp::Delete, &ctx).map_err(invalid)?;
                    batch.record(slot, applied);
                }
                subtasks.extend(descendants.into_iter().filter(|id| !named.contains(id)));
                batch.record(slot, applied);
            }
            MutationOp::Undelete if changed => {
                batch.record(slot, applied);
                undeleted.push((index, m.id));
                if let Some(by) = deleted_by {
                    let restored = batch.undelete_descendants(conn, m.id, &by, &ctx, today)?;
                    undeleted.extend(restored.iter().map(|&id| (index, id)));
                    subtasks.extend(restored.into_iter().filter(|id| !named.contains(id)));
                }
            }
            _ => batch.record(slot, applied),
        }
        slots.push(slot);
    }
    if let Some(error) = batch.orphan(conn, &undeleted)? {
        return Err(error);
    }

    let actions: Vec<(Action, ItemId, &str)> = input
        .mutations
        .iter()
        .zip(&slots)
        .map(|(m, &slot)| {
            let action = match m.op {
                MutationOp::SetStatus(status) => Action::SetStatus(status),
                MutationOp::Archive => Action::Archive,
                MutationOp::Unarchive => Action::Unarchive,
                MutationOp::Delete => Action::Delete,
                MutationOp::Undelete => Action::Undelete,
                MutationOp::Update(_) | MutationOp::Restore(_) => Action::Edit,
            };
            (action, m.id, batch.items[slot].core.title.as_str())
        })
        .collect();
    op.summary = summary::mutation(&actions, subtasks.len());
    let (revision, _) = write_op(tx, &op, &mut batch, today)?;
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
    relation::attach(&tx, std::slice::from_mut(&mut item))?;
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
    relation::attach(&tx, &mut items)?;
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
    let mut stmt = tx
        .prepare_cached(
            "SELECT revision, topic, resource_id, deleted FROM changes
             WHERE revision > ?1 ORDER BY revision",
        )
        .map_err(StoreError::from)?;
    let mut rows = stmt.query([rev_to_sql(since)?]).map_err(StoreError::from)?;
    let mut changes = Vec::new();
    // Each Item is loaded once, and their relations in one query.
    let mut slots: HashMap<String, usize> = HashMap::new();
    let mut items: Vec<Item> = Vec::new();
    while let Some(row) = rows.next().map_err(StoreError::from)? {
        let revision = rev_from_sql(row.get(0).map_err(StoreError::from)?)?;
        let topic = Topic::from_sql(&row.get::<_, String>(1).map_err(StoreError::from)?)?;
        let id: String = row.get(2).map_err(StoreError::from)?;
        let deleted: bool = row.get(3).map_err(StoreError::from)?;
        let slot = match topic {
            Topic::Item if !deleted => Some(match slots.get(&id) {
                Some(&slot) => slot,
                None => {
                    let item = load_item(&tx, &id, Some(today))?.ok_or_else(|| {
                        StoreError::Corrupt(format!("change {revision} names missing item {id}"))
                    })?;
                    items.push(item);
                    slots.insert(id.clone(), items.len() - 1);
                    items.len() - 1
                }
            }),
            _ => None,
        };
        changes.push((revision, topic, id, deleted, slot));
    }
    relation::attach(&tx, &mut items)?;
    let items: Vec<Arc<Item>> = items.into_iter().map(Arc::new).collect();
    Ok(changes
        .into_iter()
        .map(|(revision, topic, id, deleted, slot)| ChangeRow {
            revision,
            topic,
            id,
            deleted,
            resource: slot.map(|slot| Resource::Item(Arc::clone(&items[slot]))),
        })
        .collect())
}
