//! A database file outside the daemon (design 7.5): what one holds, before
//! `todo restore` puts it in place, and making a restored one safe to open.
//! Only for files no daemon has open.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::schema::{SCHEMA_VERSION, backup_into};
use crate::{Result, StoreError};

/// What a todo database file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbInfo {
    pub schema_version: u32,
    pub revision: u64,
    /// Live Items (not deleted).
    pub items: u64,
}

/// Reads `path` as a todo database without changing it: SQLite, an intact
/// file, a schema this build reads (an older one is migrated when opened),
/// and a revision.
pub fn inspect(path: &Path) -> Result<DbInfo> {
    let not_ours = |why: &str| {
        StoreError::Corrupt(format!("{} is not a todo database: {why}", path.display()))
    };
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let schema_version: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| not_ours("not an SQLite file"))?;
    if schema_version == 0 {
        return Err(not_ours("it has no schema"));
    }
    if schema_version > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: schema_version,
            supported: SCHEMA_VERSION,
        });
    }
    let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        return Err(not_ours(&format!("integrity check: {ok}")));
    }
    let revision: Option<i64> = conn
        .query_row("SELECT revision FROM meta WHERE id = 1", [], |r| r.get(0))
        .optional()
        .map_err(|_| not_ours("it has no meta table"))?;
    let revision = revision.ok_or_else(|| not_ours("its meta table is empty"))?;
    let items: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(|_| not_ours("it has no items table"))?;
    Ok(DbInfo {
        schema_version,
        revision: u64::try_from(revision).unwrap_or(0),
        items: u64::try_from(items).unwrap_or(0),
    })
}

/// A consistent copy of the database file `from` at `to`, its WAL included
/// (a file beside it as `-wal`), mode 0600. `to` must not exist.
pub fn backup_file(from: &Path, to: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(
        from,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    backup_into(&conn, to).map_err(|e| StoreError::Backup(format!("{}: {e}", to.display())))
}

/// Makes a database just put in place safe for clients that knew the one
/// it replaced: its revision is moved past both, and the change log
/// emptied, so a client resuming from any revision it saw gets
/// RESYNC_REQUIRED and never another database's rows as changes.
pub fn prepare_restored(path: &Path, at_least: u64) -> Result<()> {
    let conn = Connection::open(path)?;
    let revision: i64 =
        conn.query_row("SELECT revision FROM meta WHERE id = 1", [], |r| r.get(0))?;
    let next = u64::try_from(revision).unwrap_or(0).max(at_least) + 1;
    let next = i64::try_from(next)
        .map_err(|_| StoreError::Corrupt(format!("revision {next} out of range")))?;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    conn.execute("UPDATE meta SET revision = ?1 WHERE id = 1", [next])?;
    conn.execute("DELETE FROM changes", [])?;
    conn.execute_batch("COMMIT")?;
    Ok(())
}
