//! Automatic backups and their cleanup (design 7.5). Backups exist only
//! together with cleanup: each run deletes the backups older than
//! `retain_days`, pre-migration and pre-restore ones included, and never
//! another file. A run takes one when the newest is a day old or more.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};
use todo_store::Store;
use tokio::sync::watch;

use crate::config::Config;
use crate::shutdown::ShutdownSignal;

/// How long after the newest backup the next is taken ("daily").
pub(crate) const INTERVAL: SignedDuration = SignedDuration::from_hours(24);

/// How often the task looks whether a backup is due, between config changes.
pub(crate) const CHECK: Duration = Duration::from_secs(60 * 60);

/// What one backup run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    /// The old backups it deleted.
    pub(crate) removed: Vec<PathBuf>,
}

/// Takes a backup now, into `dir` as `todo-YYYYMMDD-HHMM.db` (`-2`, `-3`
/// for one already taken that minute), then, when `retain_days` is given,
/// deletes the backups older than that many days. A cleanup that fails is
/// logged; the backup stands.
pub(crate) async fn run_once(
    store: &Store,
    dir: &Path,
    retain_days: Option<u32>,
) -> todo_store::Result<Run> {
    // One at a time (the Backup RPC and the task can meet), from picking
    // the name to the file in place.
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    let now = Zoned::now();
    let stamp = now.strftime("%Y%m%d-%H%M").to_string();
    let path = todo_store::free_backup_path(dir, "todo", &stamp);
    let bytes = store.backup_to(path.clone()).await?;
    let removed = match retain_days {
        Some(days) => cleanup(dir, now.timestamp(), days, &path).unwrap_or_else(|e| {
            tracing::warn!(error = %e, dir = %dir.display(), "cleaning up old backups failed");
            Vec::new()
        }),
        None => Vec::new(),
    };
    tracing::info!(path = %path.display(), bytes, removed = removed.len(), "backed up the database");
    Ok(Run {
        path,
        bytes,
        removed,
    })
}

/// When the backup at `path` was taken, by its name: `todo-YYYYMMDD-HHMM`,
/// `todo-premigrate-vN-YYYYMMDD-HHMMSS` or `todo-prerestore-YYYYMMDD-HHMM`,
/// in local time. `None` for any other file, which cleanup never touches.
pub(crate) fn taken_at(path: &Path) -> Option<Timestamp> {
    let name = path.file_name()?.to_str()?.strip_suffix(".db")?;
    let rest = name.strip_prefix("todo-")?;
    let rest = rest
        .strip_prefix("premigrate-")
        .map(|r| {
            r.split_once('-')
                .map_or("", |(v, at)| if v.starts_with('v') { at } else { "" })
        })
        .or_else(|| rest.strip_prefix("prerestore-"))
        .unwrap_or(rest);
    let (date, time) = rest.split_once('-')?;
    // A same-minute suffix (-2) after the time.
    let time = time.split('-').next()?;
    let format = match time.len() {
        4 => "%Y%m%d%H%M",
        6 => "%Y%m%d%H%M%S",
        _ => return None,
    };
    let at = DateTime::strptime(format, format!("{date}{time}")).ok()?;
    at.to_zoned(TimeZone::system()).ok().map(|z| z.timestamp())
}

/// Deletes the backups in `dir` taken more than `days` days before `now`,
/// but never `keep` (the one just taken): returns what it deleted.
fn cleanup(dir: &Path, now: Timestamp, days: u32, keep: &Path) -> std::io::Result<Vec<PathBuf>> {
    let before = now - SignedDuration::from_hours(i64::from(days) * 24);
    let mut removed = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path == keep {
            continue;
        }
        if taken_at(&path).is_some_and(|at| at < before) {
            std::fs::remove_file(&path)?;
            removed.push(path);
        }
    }
    removed.sort();
    Ok(removed)
}

/// The newest automatic backup in `dir`, by its name.
fn newest(dir: &Path) -> Option<Timestamp> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.contains("premigrate") && !n.contains("prerestore"))
        })
        .filter_map(|p| taken_at(&p))
        .max()
}

/// Takes a backup whenever one is due (design 7.5), at start and then once
/// a day, while the config enables automatic backups; looks again when the
/// config changes and every `check`, until `stop`.
pub(crate) async fn run(
    store: Store,
    dir: PathBuf,
    mut config: watch::Receiver<Arc<Config>>,
    mut stop: ShutdownSignal,
    check: Duration,
) {
    loop {
        let backup = config.borrow_and_update().backup.clone();
        if backup.enabled {
            let due = newest(&dir).is_none_or(|at| Timestamp::now().duration_since(at) >= INTERVAL);
            if due && let Err(e) = run_once(&store, &dir, Some(backup.retain_days)).await {
                tracing::warn!(error = %e, "automatic backup failed");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(check) => {}
            changed = config.changed() => if changed.is_err() { return },
            () = stop.wait() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backups_are_dated_by_their_names_and_others_are_left_alone() {
        let at = |name: &str| taken_at(Path::new(name));
        let local = |s: &str| {
            s.parse::<DateTime>()
                .unwrap()
                .to_zoned(TimeZone::system())
                .unwrap()
                .timestamp()
        };
        assert_eq!(at("todo-20261103-0915.db"), Some(local("2026-11-03T09:15")));
        assert_eq!(
            at("todo-20261103-0915-2.db"),
            Some(local("2026-11-03T09:15"))
        );
        assert_eq!(
            at("todo-premigrate-v4-20261103-091530.db"),
            Some(local("2026-11-03T09:15:30"))
        );
        assert_eq!(
            at("todo-prerestore-20261103-0915.db"),
            Some(local("2026-11-03T09:15"))
        );
        for other in [
            "todo.db",
            "notes-20261103-0915.db",
            "todo-20261103-0915.db.tmp",
            "todo-20261303-0915.db",
            "todo-premigrate-x-20261103-091530.db",
        ] {
            assert_eq!(at(other), None, "{other}");
        }
    }

    #[test]
    fn cleanup_deletes_only_dated_backups_past_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let names = [
            "todo-20261001-0900.db",
            "todo-premigrate-v3-20261001-090000.db",
            "todo-prerestore-20261020-0900.db",
            "todo-20261101-0900.db",
            "notes.db",
            "todo-20261001-0900.db.tmp",
        ];
        for n in names {
            std::fs::write(dir.path().join(n), "x").unwrap();
        }
        let now = "2026-11-03T09:00"
            .parse::<DateTime>()
            .unwrap()
            .to_zoned(TimeZone::system())
            .unwrap()
            .timestamp();
        let keep = dir.path().join("todo-20261103-0900.db");
        let removed = cleanup(dir.path(), now, 14, &keep).unwrap();
        let removed: Vec<_> = removed
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            removed,
            [
                "todo-20261001-0900.db",
                "todo-premigrate-v3-20261001-090000.db",
                "todo-prerestore-20261020-0900.db"
            ]
        );
        assert!(dir.path().join("notes.db").exists());
        assert!(dir.path().join("todo-20261101-0900.db").exists());
    }
}
