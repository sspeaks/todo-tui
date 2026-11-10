//! SQLite persistence, migrations, revisions and events.
//!
//! The daemon is the only process that opens the database. All writes run on
//! one dedicated writer thread, in one SQLite transaction per [`Store::write`]
//! call; reads run on a small pool of read-only connections (WAL mode).

mod archive;
mod codec;
mod export;
mod hierarchy;
pub mod outbox;
mod query;
mod relation;
mod report;
mod restore;
mod schema;
mod suggestion;
mod summary;
mod sync;
mod undo;
mod view;

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use todo_core::filter::SortKey;
use todo_core::view::{View, ViewId};
use todo_core::{
    Actor, Applied, ApplyCtx, CaptureDraft, CreatedVia, FieldChange, FieldValue, Filter,
    FilterError, Item, ItemField, ItemId, ItemRecurrence, Mutation, MutationError, MutationOp,
    MutationResult, PersonRef, Relation, Revision, SkipReason, StatusFields, Topic, apply,
    new_ulid, next_instance,
};
use tokio::sync::{Semaphore, broadcast, oneshot};
use ulid::Ulid;

use codec::{SqlEnum, is_overdue, load_item, rev_from_sql, rev_to_sql, ts_to_sql};
pub use export::{
    EventRecord, EventValue, Export, ExportRow, ImportOutput, OpRecord, OutboxRecord, PatchRecord,
    SuggestionRecord,
};
pub use report::{MAX_NEXT, Report, ReportInput, ReportLine};
pub use restore::{DbInfo, backup_file, inspect, prepare_restored};
pub use schema::{SCHEMA_VERSION, free_backup_path};
pub use suggestion::{ApplyInput, ApplyOutput, MAX_PATCHES, RejectOutput};
use summary::Action;
pub use sync::{PullOutcome, PullPage, Reconcile, ReconcileOutcome, Tracked};
pub use view::{
    CreateViewInput, MAX_VIEW_NAME_CHARS, MAX_VIEW_OFFSET_DAYS, MoveViewInput, UpdateViewInput,
    ViewOutput, ViewUpdate, check_view_filter, check_view_name,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: u32, supported: u32 },
    /// A backup could not be written; before a migration, the migration
    /// does not run.
    #[error("{0}")]
    Backup(String),
    /// An import into a database that holds Items or ops (design 7.4: into
    /// an empty one only).
    #[error("the database is not empty: import restores into an empty one only")]
    NotEmpty,
    /// Record `record` (0-based, after the header) of an import could not
    /// go in; nothing was imported.
    #[error("record {record}: {error}")]
    Import { record: usize, error: String },
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
    /// A listing names a view that does not exist. The daemon maps it to
    /// NOT_FOUND with ErrorInfo `VIEW_NOT_FOUND` (`view_id`).
    #[error("view {0} not found")]
    ViewNotFound(ViewId),
    /// Another view has the name, ignoring case. The daemon maps it to
    /// ALREADY_EXISTS with ErrorInfo `VIEW_NAME_TAKEN` (`name`).
    #[error("a view named {0:?} exists")]
    ViewNameTaken(String),
    /// The name cannot name a view (blank, too long, a control character).
    #[error("name {0}")]
    ViewName(String),
    #[error("view {id}: revision conflict: expected {expected}, found {actual}")]
    ViewRevisionMismatch {
        id: ViewId,
        expected: Revision,
        actual: Revision,
    },
    /// A listing's filter or sort is invalid, or valid but not built yet
    /// ([`todo_core::FilterErrorKind`]). The path is relative to the filter
    /// (`all_of[0].predicate.op`, empty for the whole filter) or starts with
    /// `sort`.
    #[error("{0}")]
    Filter(FilterError),
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
    /// No Suggestion has the patch. The daemon maps it to NOT_FOUND with
    /// ErrorInfo `PATCH_NOT_FOUND` (`patch_id`).
    #[error("no suggestion has the patch {0}")]
    PatchNotFound(String),
    /// The daemon maps it to NOT_FOUND with ErrorInfo `SUGGESTION_NOT_FOUND`
    /// (`suggestion_id`).
    #[error("suggestion {0} not found")]
    SuggestionNotFound(String),
    /// A patch's value is refused on `item_id` as a direct edit would be
    /// (ADR-0007: a field that no longer pushes; design 5.9: someone else's
    /// issue, unconfirmed). Nothing is applied.
    #[error("item {item_id}: {error}")]
    PatchRefused {
        item_id: ItemId,
        #[source]
        error: MutationError,
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

/// The resource id of every [`Topic::Config`] change.
pub const CONFIG_ID: &str = "config";

/// A resource snapshot attached to a [`ChangeRow`].
#[derive(Debug, Clone, PartialEq)]
pub enum Resource {
    Item(Arc<Item>),
    View(Arc<View>),
    Suggestion(Arc<todo_core::suggestion::Suggestion>),
}

/// One row of the `changes` table: one revision (D9) for one resource.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeRow {
    pub revision: Revision,
    pub topic: Topic,
    /// Resource id (an Item ULID for [`Topic::Item`], a view ULID for
    /// [`Topic::View`]).
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
    /// `SetRecurrence`: sets, replaces or clears an Item's repeat rule.
    SetRecurrence,
    /// The view edits (design 2.4): never undo stack entries, no events.
    CreateView,
    UpdateView,
    DeleteView,
    MoveView,
    /// [`Store::auto_archive`]: the scheduler's archive of done Items and
    /// its healing (design 3.7). Actor system, so never an undo stack entry.
    AutoArchive,
    /// [`Store::apply_remote`]: a page of a tracker pull (design 5.4). Actor
    /// sync, so never an undo stack entry.
    Sync,
    /// [`Store::apply_suggestion_patches`] (design 3.11): an undo stack
    /// entry when it changed a field.
    ApplySuggestion,
    /// [`Store::reject_suggestion`]: never an undo stack entry.
    RejectSuggestion,
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
                | OpKind::SetRecurrence
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
    /// The person confirmed changing, in the tracker, Items assigned to
    /// someone else (design 5.9).
    pub confirm_not_mine: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MutateOutput {
    /// One per mutation, in request order, each in its state after the
    /// whole batch. Descendants a delete or undelete cascaded to are not
    /// listed; their changes arrive on the change stream.
    pub items: Vec<Item>,
    /// The Items the op created, in creation order: the next instance of
    /// each series an instance the batch completed repeats (design 3.8).
    pub generated: Vec<Item>,
    pub result: MutationResult,
}

/// Input for [`Store::auto_archive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoArchiveInput {
    /// The scheduler's clock: an Item closed at or before `now - after` is
    /// archived, at `now`.
    pub now: Timestamp,
    /// How long a done or cancelled Item stays out of Archive
    /// (`archive_done_after_days` × 24 h).
    pub after: SignedDuration,
    /// "Today" in the user's zone, for `is_overdue` on the written Items.
    pub today: Date,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoArchiveOutput {
    /// The Items archived `auto_done`, oldest close first.
    pub archived: Vec<ItemId>,
    /// The Items archived `auto_done` that were open again and are
    /// unarchived, in archive order.
    pub healed: Vec<ItemId>,
    /// Whether more Items were due than one op takes: call again.
    pub more: bool,
    /// The op; `None` when nothing was due, which records nothing.
    pub result: Option<MutationResult>,
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

/// Input for [`Store::list_items`] (`ListItemsRequest`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListInput {
    /// Matched with the implicit rules its flags leave (filter.proto). An
    /// invalid one fails with [`StoreError::Filter`].
    pub filter: Filter,
    /// Applied in order, then the tie-break `created_at`, `id`; empty for
    /// just the tie-break.
    pub sort: Vec<SortKey>,
    /// Narrows this view: the effective filter is the view's AND `filter`
    /// (as one group), each include flag is set in either, and a non-empty
    /// `sort` replaces the view's. A view that does not exist fails with
    /// [`StoreError::ViewNotFound`].
    pub view_id: Option<ViewId>,
    /// 0 means the default (100); values above 1000 are clamped.
    pub page_size: u32,
    /// Opaque token from a previous [`ItemPage`] of the same listing.
    pub page_token: Option<String>,
    /// "Today" in the user's zone, for relative dates, `is_overdue` and the
    /// defer rule; `None` for the system zone's. Ignored with a page token,
    /// which keeps the first page's.
    pub today: Option<Date>,
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

/// [`Store::status_counts`] (`GetStatusCountsResponse`). Each count passes
/// the implicit rules: deleted, archived, done, cancelled and deferred
/// Items are not counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusCounts {
    /// `status = inbox`.
    pub inbox: u32,
    /// `due_date <= today`: due today or overdue.
    pub due: u32,
    /// The part of `due` with `due_date < today`.
    pub overdue: u32,
    /// When asked for: each view whose `show_count` is set, in tab order,
    /// with the number of Items listing it returns.
    pub views: Vec<(ViewId, u32)>,
    /// The revision this was read at.
    pub revision: Revision,
}

/// The writer-thread transaction handed to [`Store::write`] closures.
pub struct WriteTx<'conn> {
    tx: rusqlite::Transaction<'conn>,
    changes: Vec<ChangeRow>,
    next_revision: Revision,
    /// Whether pushed fields are editable on linked Items (ADR-0007).
    pushes: todo_core::Pushes,
    /// This write queued a push: the sync loop is woken after the commit.
    queued: bool,
}

impl WriteTx<'_> {
    /// Whether pushed fields are editable on linked Items (ADR-0007).
    pub fn pushes(&self) -> todo_core::Pushes {
        self.pushes
    }

    /// The open transaction, for SQL the helpers don't cover.
    pub fn conn(&self) -> &rusqlite::Connection {
        &self.tx
    }

    /// Makes `revision` the next one [`WriteTx::record_change`] allocates;
    /// never earlier than it would have been.
    pub(crate) fn skip_to(&mut self, revision: Revision) {
        self.next_revision = self.next_revision.max(revision);
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
    /// The instance whose linked Items take a person's edit of their pushed
    /// fields, queued for it (ADR-0007), as the config says; none, read-only.
    pushes: Mutex<todo_core::Pushes>,
    /// Notified when an op queued a push, after its commit.
    queued: Arc<tokio::sync::Notify>,
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
    /// The instance whose linked Items take a person's edit of their pushed
    /// fields, to be queued for it (ADR-0007): the daemon sets it from the
    /// config's `push`. None (read-only, ADR-0006) until set.
    pub fn set_pushes(&self, instance: Option<&str>) {
        let to = instance.map_or(todo_core::Pushes::None, todo_core::Pushes::instance);
        *self
            .inner
            .shared
            .pushes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = to;
    }

    /// Opens (creating if needed) the database at `path`: WAL mode, runs
    /// pending migrations (D10), seeds the default views not seeded yet (in
    /// one transaction, a revision each), starts the writer thread and read
    /// pool. Fails with [`StoreError::SchemaTooNew`] for a newer database.
    pub fn open(path: &Path) -> Result<Store> {
        let backups = path.parent().map(|dir| dir.join("backups"));
        Store::open_with(path, backups.as_deref())
    }

    /// [`Store::open`], with a migration's backup taken into `backup_dir`
    /// (none: no backup).
    pub fn open_with(path: &Path, backup_dir: Option<&Path>) -> Result<Store> {
        let mut writer_conn = schema::open_writer(path, backup_dir)?;
        let revision = current_revision_in(&writer_conn)?;
        let readers = (0..READ_POOL_SIZE)
            .map(|_| schema::open_reader(path))
            .collect::<Result<Vec<_>>>()?;

        let shared = Arc::new(Shared {
            revision: AtomicU64::new(revision),
            changes: broadcast::channel(BROADCAST_CAPACITY).0,
            pushes: Mutex::new(todo_core::Pushes::None),
            queued: Arc::new(tokio::sync::Notify::new()),
        });
        run_write(&mut writer_conn, &shared, view::seed_tx)?;
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

    /// Writes a consistent copy of the database to `to` (design 7.5), from
    /// a read connection, so writes go on meanwhile. Returns its size.
    pub async fn backup_to(&self, to: PathBuf) -> Result<u64> {
        self.read(move |conn| {
            schema::backup_into(conn, &to)
                .map_err(|e| StoreError::Backup(format!("{}: {e}", to.display())))?;
            Ok(std::fs::metadata(&to).map(|m| m.len()).unwrap_or(0))
        })
        .await
    }

    /// Every record, read at one revision (design 7.4); without `history`,
    /// no ops or events.
    pub async fn export(&self, history: bool) -> Result<Export> {
        self.read(move |conn| export::export_tx(conn, history))
            .await
    }

    /// Puts an export's rows into this database, which must be empty (no
    /// Item, no op), all or nothing. The revision moves past both the
    /// export's and the database's, and the change log is emptied, so
    /// every client resyncs.
    pub async fn import(
        &self,
        export_revision: Revision,
        rows: Vec<ExportRow>,
    ) -> Result<ImportOutput> {
        self.write(move |tx| export::import_tx(tx, export_revision, rows))
            .await
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
    /// Recurrence (design 3.8): after the mutations, the same op creates the
    /// next instance of each series an instance the batch closed repeats,
    /// as [`MutateOutput::generated`]. It runs on the batch's final state,
    /// so `[x A, dd A]` generates nothing, and it never creates an instance
    /// while another instance of the series is live and open.
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

    /// The scheduler's sweep (design 3.7), as at most one op of kind
    /// [`OpKind::AutoArchive`] by [`Actor::System`], so it is never an undo
    /// stack entry and leaves the redo stack alone. It selects and writes in
    /// one transaction, and changes at most 1000 Items; with
    /// [`AutoArchiveOutput::more`] set the caller calls again.
    ///
    /// - **Healing** first: every live Item archived `auto_done` that is
    ///   open again (an undo of its completion reopens it without the
    ///   `SetStatus` that would unarchive it) is unarchived.
    /// - **Archive:** every live, unarchived done or cancelled Item whose
    ///   `status_entered_at` (else `updated_at`) is at least `after` before
    ///   `now` is archived `auto_done`, which bumps its `updated_at`.
    ///   An Item whose archive a user or sync op set either way since it
    ///   closed is skipped, so a `z` after closing sticks until the Item
    ///   closes again (which includes done → cancelled). Only a decision
    ///   still in effect counts: not an undone `z`, nor the restores an
    ///   undo or redo writes.
    ///
    /// When nothing is due, nothing is recorded: no op, no revision.
    pub async fn auto_archive(&self, input: AutoArchiveInput) -> Result<AutoArchiveOutput> {
        self.write(move |tx| archive::auto_archive_tx(tx, input))
            .await
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

    /// Lists the Items `input` matches, in its sort order (design 4.8).
    ///
    /// Later pages continue after the previous page's last row (keyset
    /// paging) and read live data at their own revision: Items changed since
    /// the first page appear in their current state, and the caller also
    /// receives those changes (and any Item created behind the cursor) by
    /// subscribing from the first page's revision. A token keeps the first
    /// page's today and is only accepted by a listing with the same
    /// effective filter and sort; any other token fails with
    /// [`StoreError::InvalidPageToken`].
    pub async fn list_items(&self, input: ListInput) -> Result<ItemPage> {
        self.read(move |conn| query::list_items_tx(conn, input))
            .await
    }

    /// Records that the daemon's config changed: one [`Topic::Config`]
    /// change with id [`CONFIG_ID`] and no resource. The config is not
    /// stored, so whoever sends the change attaches the current one.
    pub async fn record_config_change(&self) -> Result<Revision> {
        self.write(|tx| tx.record_change(Topic::Config, CONFIG_ID, false, None))
            .await
    }

    /// Creates a view (design 2.4) at its position, moving the views from
    /// there on one place later.
    pub async fn create_view(&self, input: CreateViewInput) -> Result<ViewOutput> {
        self.write(move |tx| view::create_view_tx(tx, input)).await
    }

    /// Changes a view's name, filter, sort or count flag.
    pub async fn update_view(&self, input: UpdateViewInput) -> Result<ViewOutput> {
        self.write(move |tx| view::update_view_tx(tx, input)).await
    }

    /// Deletes a view, closing the gap it leaves; a deleted default is never
    /// seeded again.
    pub async fn delete_view(&self, request_id: String, id: ViewId) -> Result<ViewOutput> {
        self.write(move |tx| view::delete_view_tx(tx, request_id, id))
            .await
    }

    /// Moves a view to `position`, the ones between moving one place.
    pub async fn move_view(&self, input: MoveViewInput) -> Result<ViewOutput> {
        self.write(move |tx| view::move_view_tx(tx, input)).await
    }

    /// Every view, in tab order, with the revision they were read at.
    pub async fn list_views(&self) -> Result<(Vec<View>, Revision)> {
        self.read(view::list_views_tx).await
    }

    /// How many Items a resolved filter lists, as [`Store::list_items`]
    /// would: a view's tab count.
    pub async fn count_items(&self, filter: todo_core::Resolved) -> Result<u32> {
        self.read(move |conn| query::count_tx(conn, &filter)).await
    }

    /// A pulled page (design 5.4): imports and Core updates as one op of
    /// actor sync, then the scope's cursor.
    pub async fn apply_pull(&self, page: PullPage) -> Result<PullOutcome> {
        self.write(move |tx| sync::apply_page_tx(tx, page)).await
    }

    /// The scope reconciliation (design 5.5) of the Items no scope lists.
    pub async fn reconcile(&self, input: Reconcile) -> Result<ReconcileOutcome> {
        self.write(move |tx| sync::reconcile_tx(tx, input)).await
    }

    /// `instance`'s tracked set: each live linked Item, its remote id and
    /// the scopes it matched last.
    pub async fn tracked(&self, instance: String) -> Result<Vec<Tracked>> {
        self.read(move |conn| sync::tracked_tx(conn, &instance))
            .await
    }

    /// The Items of `instance` with pushes due (design 5.6), oldest first.
    pub async fn due_pushes(
        &self,
        instance: String,
        now: Timestamp,
        limit: usize,
    ) -> Result<Vec<outbox::DuePush>> {
        self.read(move |conn| outbox::due(conn, &instance, now, limit))
            .await
    }

    /// Marks `rows` in flight with the value each sends (design 5.6),
    /// before the worker sends them; a row edited since it was read is left
    /// for the next try. Returns the ids in flight.
    pub async fn begin_push(
        &self,
        rows: Vec<(outbox::Waiting, todo_core::FieldValue)>,
        now: Timestamp,
    ) -> Result<Vec<String>> {
        self.write(move |tx| outbox::begin(tx.conn(), &rows, now))
            .await
    }

    /// When the first push of `instance` waiting on a retry is due.
    pub async fn next_push_at(&self, instance: String) -> Result<Option<Timestamp>> {
        self.read(move |conn| outbox::next_attempt(conn, &instance))
            .await
    }

    /// What pushing `rows` of `item` came to (design 5.6): sent (in flight
    /// until a pull reads it back), to retry, or refused (failed, with a
    /// push_failed Suggestion streamed). The pull that follows writes the
    /// Item's own fields.
    pub async fn record_push(
        &self,
        item: ItemId,
        key: String,
        rows: Vec<outbox::Waiting>,
        result: outbox::PushResult,
        now: Timestamp,
    ) -> Result<()> {
        self.write(move |tx| {
            let made = outbox::record_result(tx.conn(), item, &key, &rows, &result, now)?;
            if let Some(id) = made {
                outbox::record_suggestion(tx, &id)?;
            }
            Ok(())
        })
        .await
    }

    /// A push is waiting (ADR-0007): the sync loop wakes on it.
    pub fn pushes_queued(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.inner.shared.queued)
    }

    /// The outbox of `instance`: changes waiting to push, and those Jira
    /// refused (design 5.11).
    pub async fn outbox_counts(&self, instance: String) -> Result<(u32, u32)> {
        self.read(move |conn| outbox::counts(conn, &instance)).await
    }

    /// What waits to push, and what was refused while its Suggestion waits
    /// (design 5.6), oldest first; `instance` only, when given.
    pub async fn outbox(&self, instance: Option<String>) -> Result<Vec<outbox::Entry>> {
        self.read(move |conn| outbox::entries(conn, instance.as_deref()))
            .await
    }

    /// The Suggestions that wait (design 3.11), oldest first, with the
    /// revision they were read at; `item`'s only, when given.
    pub async fn list_suggestions(
        &self,
        item: Option<ItemId>,
    ) -> Result<(Vec<todo_core::suggestion::Suggestion>, Revision)> {
        self.read(move |conn| suggestion::list_tx(conn, item)).await
    }

    /// Applies the patches named as one op by the person (design 3.11):
    /// see `suggestion_service.proto`.
    pub async fn apply_suggestion_patches(&self, input: ApplyInput) -> Result<ApplyOutput> {
        self.write(move |tx| suggestion::apply_tx(tx, input)).await
    }

    /// Rejects what waits of the Suggestion `id`.
    pub async fn reject_suggestion(
        &self,
        request_id: String,
        id: String,
        now: Timestamp,
        today: Date,
    ) -> Result<RejectOutput> {
        self.write(move |tx| suggestion::reject_tx(tx, request_id, id, now, today))
            .await
    }

    /// A scope's cursor, `None` before its first pull.
    pub async fn sync_cursor(&self, instance: String, scope: String) -> Result<Option<Timestamp>> {
        self.read(move |conn| sync::cursor_tx(conn, &instance, &scope))
            .await
    }

    /// The L48 / N48 report (design 6.6), both halves read at one revision.
    pub async fn activity_report(&self, input: ReportInput) -> Result<Report> {
        self.read(move |conn| report::report_tx(conn, input)).await
    }

    /// The status counts on `today` and, with `include_views`, the count of
    /// each view whose `show_count` is set, all read at one revision. A
    /// view's count is the number of Items [`Store::list_items`] lists for
    /// it with nothing to narrow it.
    pub async fn status_counts(&self, today: Date, include_views: bool) -> Result<StatusCounts> {
        self.read(move |conn| query::status_counts_tx(conn, today, include_views))
            .await
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
        pushes: *shared.pushes.lock().unwrap_or_else(|e| e.into_inner()),
        queued: false,
    };
    let value = f(&mut wtx)?;
    let WriteTx {
        tx,
        changes,
        queued,
        ..
    } = wtx;
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
    if queued {
        shared.queued.notify_one();
    }
    Ok(value)
}

fn actor_for(via: CreatedVia) -> Actor {
    match via {
        CreatedVia::Tui
        | CreatedVia::Cli
        | CreatedVia::Mcp
        | CreatedVia::Mobile
        | CreatedVia::Split => Actor::User,
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
    /// By a person (the user or a suggestion, undo and redo too): its edits
    /// of a linked Item's pushed fields are queued (ADR-0007).
    by_person: bool,
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
            by_person: matches!(actor, Actor::User | Actor::Suggestion(_)),
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

/// The Items `ids`, in their current state with their relations and hierarchy.
fn load_items(conn: &Connection, ids: &[ItemId], today: Date) -> Result<Vec<Item>> {
    let mut items = ids
        .iter()
        .map(|&id| load_item(conn, &id.to_string(), Some(today))?.ok_or(StoreError::NotFound(id)))
        .collect::<Result<Vec<_>>>()?;
    attach(conn, &mut items)?;
    Ok(items)
}

/// Attaches what an Item shows from other rows: its relations, subtask
/// counts and parent title.
pub(crate) fn attach(conn: &Connection, items: &mut [Item]) -> Result<()> {
    relation::attach(conn, items)?;
    hierarchy::attach(conn, items)
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

    // A subtask re-emits its parent first (its counts change), so the
    // capture's own revision is still the op's last.
    let revision = tx.next_revision() + Revision::from(input.parent_id.is_some());
    let now = input.now;
    let mut item = Item::new_capture(new_ulid(), draft.title, input.created_via, now);
    item.revision = revision;
    item.core.parent_id = input.parent_id;
    item.core.priority = draft.priority;
    item.core.due_date = draft.due_date;
    item.core.labels = draft.labels;
    item.overlay.energy = draft.energy;
    item.set_status_fields(StatusFields::from_status(draft.status));
    if let Some(rule) = draft.recurrence {
        // A rule on an Item without recurrence (design 3.8): a new series,
        // scheduled on the draft's first date, which also becomes the due
        // date when no `^` token set one. The instance is never deferred:
        // the capture is the user's answer to "this exists now".
        item.core.due_date = item.core.due_date.or(draft.recurrence_first);
        item.overlay.recurrence = Some(ItemRecurrence {
            series_id: new_ulid(),
            occurrence_date: draft.recurrence_first,
            rule: Some(rule),
        });
    }
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
    if let Some(parent_id) = input.parent_id {
        let mut parent = load_item(conn, &parent_id.to_string(), Some(input.today))?
            .ok_or(StoreError::NotFound(parent_id))?;
        parent.revision = tx.next_revision();
        codec::update_item(tx.conn(), &parent)?;
        attach(tx.conn(), std::slice::from_mut(&mut parent))?;
        item.derived.parent_title = Some(parent.core.title.clone());
        let resource = Resource::Item(Arc::new(parent));
        tx.record_change(Topic::Item, &parent_id.to_string(), false, Some(resource))?;
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
    /// The Item was created by this op ([`Batch::create`]). The payload is
    /// the bare title, as a capture's `create` event records it.
    Create {
        item_id: ItemId,
        title: String,
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
    /// ones with events, relation ends, and the fan-out.
    dirty: HashSet<usize>,
    /// Slots re-emitted only because a parent's counts or a subtask's
    /// parent title changed: not part of the op's answer.
    fanout: HashSet<usize>,
    /// Slots the op creates ([`Batch::create`]), in creation order: their
    /// rows are inserted, not updated.
    created: Vec<usize>,
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

    /// Adds `item` as a new Item of this op, with a `create` event, and
    /// returns its slot. [`write_op`] inserts its row.
    ///
    /// Its "loaded" twin is the same Item soft-deleted at `now`, with
    /// identical staleness: a created slot therefore always compares
    /// unequal (so it is touched), contributes nothing to its parent's
    /// counts as loaded (so [`Batch::fan_out`] re-emits the parent) and
    /// writes no `derived` event (its signals did not change).
    fn create(&mut self, item: Item, now: Timestamp) -> usize {
        let mut twin = item.clone();
        twin.derived.deleted_at = Some(now);
        let slot = self.items.len();
        self.events.push(NewEvent::Create {
            item_id: item.id,
            title: item.core.title.clone(),
        });
        self.slots.insert(item.id, slot);
        self.loaded.push(twin);
        self.items.push(item);
        self.created.push(slot);
        self.dirty.insert(slot);
        slot
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

    /// Loads and marks dirty the Items whose derived hierarchy fields this
    /// op changes without changing them itself: the old and new parent of
    /// each Item whose contribution to their counts changed, and the
    /// subtasks of each Item whose title changed. One level, since a
    /// relative loaded here does not change. `updated_at` stays, so a
    /// parent does not move in the views sorted by it.
    fn fan_out(&mut self, conn: &Connection, today: Date) -> Result<()> {
        let mut parents = Vec::new();
        let mut retitled = Vec::new();
        for (item, loaded) in self.items.iter().zip(&self.loaded) {
            let (now, was) = (
                hierarchy::contribution(item),
                hierarchy::contribution(loaded),
            );
            if now != was {
                parents.extend([now.0, was.0].into_iter().flatten());
            }
            if item.core.title != loaded.core.title {
                retitled.push(item.id);
            }
        }
        // parent_id is a foreign key, so every parent exists.
        let mut relatives = parents;
        for id in retitled {
            relatives.extend(self.children(conn, id)?.into_iter().map(|(child, _)| child));
        }
        for id in relatives {
            let slot = self.load(conn, id, today)?;
            if !self.dirty.contains(&slot) && self.items[slot] == self.loaded[slot] {
                self.fanout.insert(slot);
            }
            self.dirty.insert(slot);
        }
        Ok(())
    }

    /// The Item in `slot` when this op completed it and it still repeats:
    /// it was open before the op, is done or cancelled now (so done →
    /// cancelled is no completion), is live, and kept a rule. It is the
    /// template of the next instance (design 3.8).
    fn completed_instance(&self, slot: usize) -> Option<&Item> {
        let item = &self.items[slot];
        let closed_here = !self.loaded[slot].status().is_closed() && item.status().is_closed();
        let repeats = item
            .overlay
            .recurrence
            .as_ref()
            .is_some_and(|r| r.rule.is_some());
        (closed_here && !item.is_deleted() && repeats).then_some(item)
    }

    /// Whether an instance of `series_id` other than `but` is still live and
    /// open, as of this point in the batch: a working copy (which includes
    /// the instances this op has created), or a stored row the batch has
    /// not loaded.
    fn series_is_open(&self, conn: &Connection, series_id: Ulid, but: ItemId) -> Result<bool> {
        let open = |item: &Item| {
            item.id != but
                && !item.is_deleted()
                && !item.status().is_closed()
                && item
                    .overlay
                    .recurrence
                    .as_ref()
                    .is_some_and(|r| r.series_id == series_id)
        };
        if self.items.iter().any(open) {
            return Ok(true);
        }
        let mut stmt = conn.prepare_cached(
            "SELECT id FROM items
             WHERE recurrence_series_id = ?1 AND deleted_at IS NULL
                   AND status_category <> 'done'",
        )?;
        let mut rows = stmt.query([series_id.to_string()])?;
        while let Some(row) = rows.next()? {
            let id = codec::item_id_from_str(&row.get::<_, String>(0)?)?;
            // The working copies above are this op's truth for the rows it
            // loaded, whatever the stored ones still say.
            if id != but && !self.slots.contains_key(&id) {
                return Ok(true);
            }
        }
        Ok(false)
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
    /// parent rule once they are done) or a descendant, or a person. Core
    /// refuses values too large for their nanosecond columns.
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
/// changed. The fan-out ([`Batch::fan_out`]) is touched too, without
/// events. Attaches every working copy's relations and hierarchy. Returns
/// the op's last revision (the current one if it allocates none) and the
/// touched slots but the fan-out, in the order their events first name them.
fn write_op(
    tx: &mut WriteTx<'_>,
    op: &OpRow<'_>,
    batch: &mut Batch,
    today: Date,
) -> Result<(Revision, Vec<usize>)> {
    batch.fan_out(tx.conn(), today)?;
    let touched = batch.touched();
    let derived: Vec<usize> = touched
        .iter()
        .copied()
        .filter(|&slot| batch.items[slot].derived.staleness != batch.loaded[slot].derived.staleness)
        .collect();
    let first = tx.next_revision();
    let revision = first + touched.len() as Revision - 1;
    op.insert(tx.conn(), revision, batch.events.len() + derived.len())?;
    // Pushes (ADR-0007): a person's changes of pushed fields queue (only
    // with pushes on: a waiting row is updated or cancelled either way, as
    // undo restores without the guard), a delete cancels what waits; each
    // Item whose outbox changed has its pending fields again.
    if op.by_person {
        let at = Timestamp::now();
        let conn = tx.conn();
        let create = tx.pushes();
        let by_id = |id: ItemId| batch.slots.get(&id).map(|&s| batch.items[s].clone());
        let mut queued =
            outbox::enqueue(conn, &op.id.to_string(), &batch.events, by_id, create, at)?;
        let woke = !queued.is_empty();
        for &slot in &touched {
            let item = &batch.items[slot];
            if item.remote_link.is_some() && item.is_deleted() && !batch.loaded[slot].is_deleted() {
                outbox::cancel_item(conn, item.id, at)?;
                queued.push(item.id);
            }
        }
        // Only where the outbox changed: elsewhere the column stands.
        for id in queued {
            if let Some(&slot) = batch.slots.get(&id) {
                batch.items[slot].derived.pending_fields = outbox::pending_fields(conn, id)?;
            }
        }
        tx.queued |= woke;
    }
    for item in &mut batch.items {
        item.derived.is_overdue = is_overdue(item, Some(today));
    }
    for (&slot, rev) in touched.iter().zip(first..) {
        let item = &mut batch.items[slot];
        item.revision = rev;
        if batch.created.contains(&slot) {
            // A created row has nothing to update; the FTS triggers index it.
            codec::insert_item(tx.conn(), item)?;
        } else {
            codec::update_item(tx.conn(), item)?;
        }
    }
    // After every row is written, so the counts are the op's.
    attach(tx.conn(), &mut batch.items)?;
    for &slot in &touched {
        let item = &batch.items[slot];
        let resource = Resource::Item(Arc::new(item.clone()));
        let recorded =
            tx.record_change(Topic::Item, &item.id.to_string(), false, Some(resource))?;
        debug_assert_eq!(recorded, item.revision);
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
            NewEvent::Create { item_id, title } => {
                named(*item_id);
                op.insert_event(
                    conn,
                    ord,
                    &item_id.to_string(),
                    "create",
                    None,
                    None,
                    Some(serde_json::Value::from(title.as_str()).to_string()),
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
    // Every touched Item but the fan-out has an event; this only keeps the
    // list complete. The fan-out reaches clients through the change stream.
    for slot in touched {
        if !slots.contains(&slot) && !batch.fanout.contains(&slot) {
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
            generated: load_items(conn, &created_by(conn, op_id)?, today)?,
            result: MutationResult { op_id, revision },
        });
    }

    let ctx = ApplyCtx {
        now: input.now,
        actor: input.actor,
        pushes: tx.pushes(),
        confirm_not_mine: input.confirm_not_mine,
    };
    let conn = tx.conn();
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
    let generated = generate_tx(conn, &mut batch, &slots, &input)?;

    let actions: Vec<(Action, ItemId, &str)> = input
        .mutations
        .iter()
        .zip(&slots)
        .map(|(m, &slot)| {
            let action = match &m.op {
                MutationOp::SetStatus(status) => Action::SetStatus(*status),
                MutationOp::Archive(_) => Action::Archive,
                MutationOp::Unarchive => Action::Unarchive,
                MutationOp::Delete => Action::Delete,
                MutationOp::Undelete => Action::Undelete,
                MutationOp::SetRecurrence { rule, .. } => Action::SetRecurrence(rule.is_some()),
                MutationOp::Update(_) | MutationOp::Restore(_) => Action::Edit,
            };
            (action, m.id, batch.items[slot].core.title.as_str())
        })
        .collect();
    let dates: Vec<Date> = generated
        .iter()
        .filter_map(|&slot| batch.items[slot].core.due_date)
        .collect();
    op.summary = summary::mutation(&actions, subtasks.len(), &dates, today);
    let (revision, _) = write_op(tx, &op, &mut batch, today)?;
    let first = input.mutations.first().map(|m| m.id.to_string());
    op.insert_request_id(tx.conn(), revision, first.as_deref())?;
    Ok(MutateOutput {
        items: slots
            .into_iter()
            .map(|slot| batch.items[slot].clone())
            .collect(),
        generated: generated
            .into_iter()
            .map(|slot| batch.items[slot].clone())
            .collect(),
        result: MutationResult {
            op_id: op.id,
            revision,
        },
    })
}

/// The recurrence post-pass (design 3.8): for each Item the batch
/// completed, in mutation order, the next instance of its series, created in
/// the same op. Returns their slots.
///
/// It runs on the batch's final state, after every mutation and the parent
/// rule, so `[x A, s A→next]`, `[x A, dd A]` and `[x A, @clear A]` generate
/// nothing. It never creates an instance while another instance of the
/// series is live and open (reopening a completed one can still leave two,
/// and then neither generates until both are closed): the guard sees
/// the batch's working copies, the instances this pass has made, and the
/// stored rows it has not loaded.
fn generate_tx(
    conn: &Connection,
    batch: &mut Batch,
    slots: &[usize],
    input: &MutateInput,
) -> Result<Vec<usize>> {
    let mut generated = Vec::new();
    let mut seen = HashSet::new();
    for &slot in slots {
        if !seen.insert(slot) {
            continue;
        }
        let mut next = {
            let Some(completed) = batch.completed_instance(slot) else {
                continue;
            };
            // `completed_instance` found a rule, hence a series.
            let recurrence = completed.overlay.recurrence.as_ref().expect("it recurs");
            if batch.series_is_open(conn, recurrence.series_id, completed.id)? {
                continue;
            }
            next_instance(completed, new_ulid(), input.today, input.now)
                .expect("a completed instance has a rule")
        };
        // An instance live under a deleted parent is an M1 database's
        // legacy orphan, which ops tolerate. The next instance is a new
        // Item and keeps the parent rule (design 3.7): it starts at the
        // top level instead.
        if let Some(parent) = next.core.parent_id
            && batch.unusable_parent(conn, parent)?.is_some()
        {
            next.core.parent_id = None;
        }
        generated.push(batch.create(next, input.now));
    }
    Ok(generated)
}

/// The Items op `op_id` created, in the order of its `create` events: what
/// a replayed request answers as [`MutateOutput::generated`].
fn created_by(conn: &Connection, op_id: Ulid) -> Result<Vec<ItemId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT item_id FROM events WHERE op_id = ?1 AND kind = 'create' ORDER BY ord, rowid",
    )?;
    let rows = stmt.query_map([op_id.to_string()], |r| r.get::<_, String>(0))?;
    rows.map(|id| codec::item_id_from_str(&id?))
        .collect::<Result<Vec<_>>>()
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
    attach(&tx, std::slice::from_mut(&mut item))?;
    Ok(GetItemOutput {
        resolved_from: (item.id != id).then_some(id),
        item,
        revision,
    })
}

/// 64-bit FNV-1a, for fingerprints that bind page tokens to their query.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn today_local() -> jiff::civil::Date {
    jiff::Zoned::now().date()
}

pub(crate) fn current_revision_in(conn: &Connection) -> Result<Revision> {
    rev_from_sql(conn.query_row("SELECT revision FROM meta", [], |r| r.get(0))?)
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
    // Each Item and view is loaded once, and the Items' relations in one
    // query.
    let mut slots: HashMap<String, usize> = HashMap::new();
    let mut items: Vec<Item> = Vec::new();
    let mut views: HashMap<String, Option<Arc<View>>> = HashMap::new();
    while let Some(row) = rows.next().map_err(StoreError::from)? {
        let revision = rev_from_sql(row.get(0).map_err(StoreError::from)?)?;
        let topic = Topic::from_sql(&row.get::<_, String>(1).map_err(StoreError::from)?)?;
        let id: String = row.get(2).map_err(StoreError::from)?;
        let mut deleted: bool = row.get(3).map_err(StoreError::from)?;
        let replayed = match topic {
            Topic::Item if !deleted => Replayed::Item(match slots.get(&id) {
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
            Topic::View if !deleted => {
                let view = match views.get(&id) {
                    Some(view) => view.clone(),
                    None => {
                        let view_id: ViewId = id.parse().map_err(|_| {
                            StoreError::Corrupt(format!("change {revision} names view {id:?}"))
                        })?;
                        let view = view::load(&tx, view_id)?.map(Arc::new);
                        views.insert(id.clone(), view.clone());
                        view
                    }
                };
                // A view deleted since replays as deleted: views leave no
                // tombstone.
                deleted = view.is_none();
                view.map_or(Replayed::None, Replayed::View)
            }
            Topic::Suggestion if !deleted => match outbox::load_suggestion(&tx, &id)? {
                Some(s) => Replayed::Suggestion(Arc::new(s)),
                None => {
                    deleted = true;
                    Replayed::None
                }
            },
            _ => Replayed::None,
        };
        changes.push((revision, topic, id, deleted, replayed));
    }
    attach(&tx, &mut items)?;
    let items: Vec<Arc<Item>> = items.into_iter().map(Arc::new).collect();
    Ok(changes
        .into_iter()
        .map(|(revision, topic, id, deleted, replayed)| ChangeRow {
            revision,
            topic,
            id,
            deleted,
            resource: match replayed {
                Replayed::Item(slot) => Some(Resource::Item(Arc::clone(&items[slot]))),
                Replayed::View(view) => Some(Resource::View(view)),
                Replayed::Suggestion(s) => Some(Resource::Suggestion(s)),
                Replayed::None => None,
            },
        })
        .collect())
}

/// The resource a replayed change carries.
enum Replayed {
    /// The slot of the Item, loaded once per replay.
    Item(usize),
    View(Arc<View>),
    Suggestion(Arc<todo_core::suggestion::Suggestion>),
    None,
}
