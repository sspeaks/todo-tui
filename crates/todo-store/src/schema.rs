//! Connections and embedded migrations (D10). Forward only: each schema
//! change is a new migration, as databases have applied the earlier ones
//! (design 7.2).

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Zoned;
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
    plain(include_str!("../migrations/0005_energy.sql")),
    plain(include_str!("../migrations/0006_sync.sql")),
    Migration {
        sql: include_str!("../migrations/0007_created_via_mobile.sql"),
        rebuilds: true,
    },
    plain(include_str!("../migrations/0008_outbox.sql")),
];

pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens the writer, migrating the schema; a database that needs migrating
/// is first backed up into `backup_dir` (design 7.2), and a migration never
/// runs without its backup.
pub(crate) fn open_writer(path: &Path, backup_dir: Option<&Path>) -> Result<Connection> {
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
    let found = user_version(&conn)?;
    if found > 0
        && found < SCHEMA_VERSION
        && let Some(dir) = backup_dir
    {
        let to = premigrate_path(dir, found);
        backup_into(&conn, &to)
            .map_err(|e| StoreError::Backup(format!("{}: {e}", to.display())))?;
        tracing::info!(path = %to.display(), "backed up the database before migrating");
    }
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
    adopt_phone_app_v5(conn)?;
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

/// The phone-app branch shipped the mobile rebuild as its migration 5,
/// before energy (5 here) and sync (6): a database it made says version 5
/// and has `'mobile'` in its `created_via` CHECK but no `energy` column.
/// It is brought to version 7 here, adding what 0005 and 0006 add; its
/// items table then matches 0007's but for where `energy` sits, which
/// nothing reads by position. Any other database is left as it is.
fn adopt_phone_app_v5(conn: &mut Connection) -> Result<()> {
    if user_version(conn)? != 5 {
        return Ok(());
    }
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'items'",
        [],
        |r| r.get(0),
    )?;
    let energy: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('items') WHERE name = 'energy')",
        [],
        |r| r.get(0),
    )?;
    if !sql.contains("'mobile'") || energy {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    tx.execute_batch(MIGRATIONS[4].sql)?;
    tx.execute_batch(MIGRATIONS[5].sql)?;
    tx.pragma_update(None, "user_version", 7)?;
    tx.commit()?;
    tracing::info!("brought a phone-app database (version 5, mobile) to version 7");
    Ok(())
}

/// `backups/todo-premigrate-v{found}-YYYYMMDD-HHMMSS.db`, local time.
fn premigrate_path(dir: &Path, found: u32) -> PathBuf {
    let at = Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
    free_backup_path(dir, &format!("todo-premigrate-v{found}"), &at)
}

/// A consistent copy of the database `conn` is open on (its WAL included),
/// through `VACUUM INTO`, at `to`, which must not exist: an existing
/// backup is never replaced. It is written to a temporary file of its own
/// first and moved into place only whole, so a file named `.db` is never
/// half written, and a failed copy leaves nothing behind.
pub(crate) fn backup_into(conn: &Connection, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if to.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "a file of that name exists",
        ));
    }
    let tmp = to.with_extension(format!("db.{}.tmp", ulid::Ulid::generate()));
    let remove_tmp = || {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(tmp.with_extension("tmp-journal"));
    };
    let result = (|| {
        let target = tmp
            .to_str()
            .ok_or_else(|| std::io::Error::other("the backup path is not UTF-8"))?;
        conn.execute("VACUUM INTO ?1", [target])
            .map_err(std::io::Error::other)?;
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        // A hard link fails when `to` exists, unlike a rename, so a backup
        // made meanwhile under the same name is kept.
        std::fs::hard_link(&tmp, to)
    })();
    remove_tmp();
    result
}

/// A path in `dir` for `{prefix}-{stamp}.db` that no file has: `-2`, `-3`,
/// … after the stamp when it is taken.
pub fn free_backup_path(dir: &Path, prefix: &str, stamp: &str) -> PathBuf {
    let mut path = dir.join(format!("{prefix}-{stamp}.db"));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{prefix}-{stamp}-{n}.db"));
    }
    path
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
