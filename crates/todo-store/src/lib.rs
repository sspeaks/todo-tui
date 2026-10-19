//! SQLite persistence, migrations, revisions and events.
//!
//! The daemon is the only process that opens the database. All writes run on
//! one dedicated writer thread, in one SQLite transaction per [`Store::write`]
//! call; reads run on a small pool of read-only connections (WAL mode).

use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use todo_core::{CreatedVia, Item, ItemId, MutationResult, Revision, Topic};
use tokio::sync::broadcast;

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
    /// The revision the page was read at; later pages are pinned to the first
    /// page's revision.
    pub revision: Revision,
}

/// The writer-thread transaction handed to [`Store::write`] closures.
#[allow(dead_code)] // Used once S0.4 lands.
pub struct WriteTx<'conn> {
    tx: rusqlite::Transaction<'conn>,
    changes: Vec<ChangeRow>,
}

#[allow(unused_variables)]
impl WriteTx<'_> {
    /// The open transaction, for SQL the helpers don't cover.
    pub fn conn(&self) -> &rusqlite::Connection {
        &self.tx
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
        todo!()
    }
}

/// Handle to the database. Cheap to clone; all clones share the writer
/// thread, read pool and broadcast channel.
#[derive(Clone)]
#[allow(dead_code)] // Used once S0.4 lands.
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {}

#[allow(unused_variables)]
impl Store {
    /// Opens (creating if needed) the database at `path`: WAL mode, runs
    /// pending migrations (D10), starts the writer thread and read pool.
    /// Fails with [`StoreError::SchemaTooNew`] for a newer database.
    pub fn open(path: &Path) -> Result<Store> {
        todo!()
    }

    /// Runs `f` on the writer thread inside one transaction. On `Ok` the
    /// transaction commits, then every [`ChangeRow`] recorded through
    /// [`WriteTx::record_change`] is published to subscribers in revision
    /// order. On `Err` it rolls back and nothing is published.
    pub async fn write<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut WriteTx<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        todo!()
    }

    /// Runs `f` on a pooled read-only connection, off the async runtime.
    pub async fn read<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        todo!()
    }

    /// Creates an inbox Item (one op, one revision). Idempotent on
    /// `request_id` and on `spool_id`: a replay returns the original Item and
    /// result without writing.
    pub async fn capture(&self, input: CaptureInput) -> Result<(Item, MutationResult)> {
        todo!()
    }

    /// Lists Items matching `query`, ordered by `created_at` then id.
    pub async fn list_items(&self, query: ListQuery) -> Result<ItemPage> {
        todo!()
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
        todo!()
    }

    /// The latest committed revision.
    pub fn current_revision(&self) -> Revision {
        todo!()
    }

    /// Live [`ChangeRow`]s published after each commit. Subscribe before
    /// calling [`Store::changes_since`] so no revision falls in between; a
    /// lagging receiver gets `RecvError::Lagged` and must replay.
    pub fn subscribe(&self) -> broadcast::Receiver<ChangeRow> {
        todo!()
    }
}
