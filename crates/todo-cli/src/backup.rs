//! `todo backup now|list` and `todo restore PATH` (design 7.5).

use std::fs::{self, OpenOptions};
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use jiff::Zoned;
use todo_daemon::paths::Paths;
use todo_proto::v1::system_service_client::SystemServiceClient;
use todo_proto::v1::{BackupRequest, ShutdownRequest};
use tonic::Code;
use tonic_types::StatusExt as _;

use crate::rpc::{call, ready};
use crate::{print_err, print_out, spawn};

/// `todo backup now`: has the daemon (started if needed) take one.
pub(crate) async fn now(paths: &Paths) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let res = call(SystemServiceClient::new(channel).backup(BackupRequest {}))
        .await
        .map_err(|s| anyhow::anyhow!("backup failed: {}", s.message()))?;
    print_out(format!("{}  {}", res.path, size(res.bytes)));
    for removed in res.removed {
        print_out(format!("removed {removed}"));
    }
    Ok(ExitCode::SUCCESS)
}

/// `todo backup list`: the files in the backups directory, oldest first.
/// Needs no daemon.
pub(crate) fn list(paths: &Paths) -> anyhow::Result<ExitCode> {
    let dir = paths.backups();
    let mut files: Vec<(PathBuf, u64)> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "db"))
            .map(|p| {
                let len = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                (p, len)
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    if files.is_empty() {
        print_out(format!("no backups in {}", dir.display()));
        return Ok(ExitCode::SUCCESS);
    }
    files.sort();
    for (path, len) in files {
        print_out(format!("{}  {}", path.display(), size(len)));
    }
    Ok(ExitCode::SUCCESS)
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MiB", b as f64 / f64::from(1 << 20)),
        b if b >= 1 << 10 => format!("{:.1} KiB", b as f64 / f64::from(1 << 10)),
        b => format!("{b} B"),
    }
}

/// `todo restore PATH`: puts a backup in place of the database. It checks
/// the file, asks (unless `yes`), stops the daemon, backs up the current
/// database as `todo-prerestore-…`, swaps the file in while holding the
/// daemon's lock (so none starts meanwhile), moves its revision past both
/// so every client resyncs, and starts the daemon again.
pub(crate) async fn restore(paths: &Paths, from: &Path, yes: bool) -> anyhow::Result<ExitCode> {
    let from = from
        .canonicalize()
        .with_context(|| format!("reading {}", from.display()))?;
    let db = paths.database();
    if db.canonicalize().is_ok_and(|current| current == from) {
        bail!("{} is the database itself", from.display());
    }
    let info = todo_store::inspect(&from).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("restore replaces the database: run it in a terminal to confirm, or pass --yes");
        }
        print!(
            "Restore {} ({} items, schema v{})? The current database is backed up first. y/N ",
            from.display(),
            info.items,
            info.schema_version
        );
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            print_out("not restored");
            return Ok(ExitCode::FAILURE);
        }
    }

    stop_daemon(paths).await?;
    // The daemon's own lock, held for the swap: a client that auto-spawns
    // one meanwhile finds it taken and waits.
    fs::create_dir_all(&paths.data)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&paths.lock)
        .with_context(|| format!("opening {}", paths.lock.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => bail!("a daemon started meanwhile; try again"),
        Err(fs::TryLockError::Error(e)) => return Err(e).context("locking the database"),
    }

    // The current database is backed up, and the backup copied (its WAL
    // included, mode 0600) and made ready for the clients of the database
    // it replaces, all beside it: until the rename the database is
    // untouched, and a step that fails leaves it so.
    let previous = if db.exists() {
        let stamp = Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
        let to = todo_store::free_backup_path(&paths.backups(), "todo-prerestore", &stamp);
        todo_store::backup_file(&db, &to)
            .map_err(|e| anyhow::anyhow!("backing up the current database: {e}"))?;
        Some((
            to,
            todo_store::inspect(&db).map(|i| i.revision).unwrap_or(0),
        ))
    } else {
        None
    };
    let tmp = db.with_extension("db.restore");
    let tmp_files = || {
        ["", "-wal", "-shm", "-journal"]
            .map(|side| PathBuf::from(format!("{}{side}", tmp.display())))
    };
    for leftover in tmp_files() {
        let _ = fs::remove_file(leftover);
    }
    let put = (|| -> anyhow::Result<()> {
        todo_store::backup_file(&from, &tmp)
            .map_err(|e| anyhow::anyhow!("copying {}: {e}", from.display()))?;
        let old_revision = previous.as_ref().map_or(0, |(_, r)| *r);
        todo_store::prepare_restored(&tmp, old_revision).map_err(|e| anyhow::anyhow!("{e}"))?;
        fs::File::open(&tmp)?.sync_all()?;
        for stale in ["db-wal", "db-shm"] {
            match fs::remove_file(db.with_extension(stale)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        fs::rename(&tmp, &db).context("putting the backup in place")
    })();
    for leftover in tmp_files() {
        let _ = fs::remove_file(leftover);
    }
    put?;
    drop(lock);

    // A service's daemon (TODO_NO_SPAWN) is started by the service.
    if !spawn::no_spawn() {
        spawn::spawn_and_wait(paths).await?;
    }
    print_out(format!(
        "restored {} ({} items)",
        from.display(),
        info.items
    ));
    if let Some((to, _)) = previous {
        print_out(format!(
            "the previous database is saved as {}",
            to.display()
        ));
    }
    Ok(ExitCode::SUCCESS)
}

/// Stops a running daemon and waits for it to exit. Refused while other
/// TUIs are open: they would lose the database under them.
async fn stop_daemon(paths: &Paths) -> anyhow::Result<()> {
    if !spawn::is_listening(&paths.socket).await {
        return Ok(());
    }
    let pid = spawn::daemon_pid(paths);
    let channel = todo_daemon::client::connect(&paths.socket).await?;
    match call(SystemServiceClient::new(channel).shutdown(ShutdownRequest { restart: true })).await
    {
        Ok(_) => {}
        Err(s)
            if s.code() == Code::FailedPrecondition
                && s.get_details_error_info()
                    .is_some_and(|i| i.reason == "INTERACTIVE_CLIENTS_CONNECTED") =>
        {
            bail!("close the other todo sessions first: {}", s.message());
        }
        Err(s) => print_err(format!("todo restore: shutdown: {}", s.message())),
    }
    if let Some(pid) = pid {
        spawn::wait_for_exit(pid).await?;
    }
    Ok(())
}
