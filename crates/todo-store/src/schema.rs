//! Connections and embedded migrations (D10). Forward only: each schema
//! change is a new migration, as databases have applied the earlier ones
//! (design 7.2).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::{Result, StoreError};

/// One schema step. `rebuilds` marks a migration that rebuilds a table
/// referenced by foreign keys (SQLite's "other kinds of table schema
/// changes"): it runs with `foreign_keys` OFF, which cannot change inside a
/// transaction, and commits only if `foreign_key_check` finds nothing.
struct Migration {
    sql: &'static str,
    rebuilds: bool,
}

const fn plain(sql: &'static str) -> Migration {
    Migration {
        sql,
        rebuilds: false,
    }
}

/// Migration `i` takes the schema from version `i` to `i + 1`.
const MIGRATIONS: &[Migration] = &[
    plain(include_str!("../migrations/0001_init.sql")),
    plain(include_str!("../migrations/0002_undo_relations.sql")),
    plain(include_str!("../migrations/0003_views_fts.sql")),
    plain(include_str!("../migrations/0004_recurrence.sql")),
    Migration {
        sql: include_str!("../migrations/0005_created_via_mobile.sql"),
        rebuilds: true,
    },
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
    for (version, m) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let to = version as u32 + 1;
        if m.rebuilds {
            let fk: bool = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
            conn.pragma_update(None, "foreign_keys", false)?;
            let applied = apply(conn, m, to);
            // Restore it whatever happened; the migration's error wins.
            let restored = conn.pragma_update(None, "foreign_keys", fk);
            applied?;
            restored?;
        } else {
            apply(conn, m, to)?;
        }
        tracing::info!(version = to, "applied migration");
    }
    Ok(())
}

fn apply(conn: &mut Connection, m: &Migration, to: u32) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    tx.execute_batch(m.sql)?;
    if m.rebuilds {
        let violations: i64 =
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })?;
        if violations > 0 {
            return Err(StoreError::Corrupt(format!(
                "migration to version {to} left {violations} foreign key violations"
            )));
        }
    }
    tx.pragma_update(None, "user_version", to)?;
    tx.commit()?;
    Ok(())
}
