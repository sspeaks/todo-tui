//! Connections and embedded migrations (D10).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::{Result, StoreError};

/// Migration `i` takes the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_undo_relations.sql"),
];

pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn open_writer(path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Corrupt(format!(
            "journal_mode is {mode}, not wal"
        )));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    migrate(&mut conn)?;
    Ok(conn)
}

pub(crate) fn open_reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// Applies pending migrations, each in its own transaction with its
/// `user_version` bump.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let found = user_version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (version, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let to = version as u32 + 1;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", to)?;
        tx.commit()?;
        tracing::info!(version = to, "applied migration");
    }
    Ok(())
}
