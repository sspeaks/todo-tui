//! The capture spool (design 2.1). When `todo add` cannot reach the daemon
//! it appends the capture to `spool.jsonl`; the daemon imports the entries
//! when it starts and, while it runs, whenever the spool is non-empty.
//!
//! Appends and imports take an exclusive lock on the spool, and an import
//! holds it from reading the file until it truncates it, so an entry
//! appended meanwhile waits for the next import instead of being lost.
//! Imports are idempotent per spool id: a crash before the truncate only
//! repeats them.

use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, FileExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jiff::civil::Date;
use jiff::fmt::temporal::Pieces;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};
use todo_core::{CreatedVia, parse_capture};
use todo_store::{CaptureInput, Store, StoreError};
use ulid::Ulid;

use crate::paths::Paths;
use crate::service::item::{canonical_ulid, storable};
use crate::shutdown::ShutdownSignal;

/// How often a running daemon looks for new entries.
pub(crate) const INTERVAL: Duration = Duration::from_secs(30);

/// How long [`append`] waits for the spool's lock, which an import holds
/// while it runs, before it gives up.
const LOCK_WAIT: Duration = Duration::from_secs(10);
/// How often [`append`] tries the lock meanwhile.
const LOCK_RETRY: Duration = Duration::from_millis(20);

/// One line of the spool: `{"spool_id":…,"captured_at":…,"text":…}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolEntry {
    /// A ULID. The CLI uses the request id of the capture that failed, so a
    /// capture that did reach the daemon is not imported a second time.
    pub spool_id: String,
    /// When the text was captured: RFC 3339 local time with its offset.
    /// Relative dates in `text` resolve against its date in that offset.
    pub captured_at: String,
    pub text: String,
}

impl SpoolEntry {
    /// An entry captured now, stamped with the system time zone's offset.
    pub fn now(spool_id: Ulid, text: impl Into<String>) -> SpoolEntry {
        let now = Zoned::now();
        SpoolEntry {
            spool_id: spool_id.to_string(),
            captured_at: now
                .timestamp()
                .display_with_offset(now.offset())
                .to_string(),
            text: text.into(),
        }
    }
}

/// Appends `entry` as one line and syncs it, holding the spool's exclusive
/// lock (so this waits while an import runs, for up to 10s; then it fails
/// with `TimedOut`). Creates the data directory (mode 0700) and the spool
/// (mode 0600) as needed.
pub fn append(paths: &Paths, entry: &SpoolEntry) -> io::Result<()> {
    append_within(paths, entry, LOCK_WAIT)
}

/// [`append`], waiting at most `wait` for the lock.
fn append_within(paths: &Paths, entry: &SpoolEntry, wait: Duration) -> io::Result<()> {
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&paths.data)?;
    let mut file = open_append(&paths.spool())?;
    lock_within(&file, wait)?;
    // A writer that died mid-line left it torn: end it first, or this entry
    // would join it and be rejected with it.
    if !ends_a_line(&file)? {
        line.insert(0, b'\n');
    }
    file.write_all(&line)?;
    file.sync_data()?;
    file.unlock()
}

/// Takes `file`'s exclusive lock, retrying every [`LOCK_RETRY`] for up to
/// `wait`, so a wedged importer cannot hang a capture.
fn lock_within(file: &File, wait: Duration) -> io::Result<()> {
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_RETRY);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("the spool is still locked after {wait:?}"),
                ));
            }
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// Opens `path` to append to it (and read its tail), creating it with mode
/// 0600.
fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// Whether `file` is empty or ends with a newline.
fn ends_a_line(file: &File) -> io::Result<bool> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(true);
    }
    let mut last = [0];
    file.read_exact_at(&mut last, len - 1)?;
    Ok(last == *b"\n")
}

/// What one import did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Imported {
    /// Entries captured (or found already imported).
    pub(crate) captured: usize,
    /// Lines moved to `spool.rejected.jsonl`.
    pub(crate) rejected: usize,
}

/// Imports every entry and truncates the spool, holding its lock
/// throughout. Each entry is captured from the CLI with its spool id as
/// both request id and spool id, at its `captured_at`. A line that is not
/// a valid entry, or whose capture the store refuses, is appended verbatim
/// to `spool.rejected.jsonl` and logged. Any other store error stops the
/// import and leaves both files as they are.
pub(crate) async fn import(paths: &Paths, store: &Store) -> anyhow::Result<Imported> {
    let spool = paths.spool();
    let read = tokio::task::spawn_blocking(move || lock_and_read(&spool)).await?;
    let Some((file, content)) =
        read.with_context(|| format!("reading {}", paths.spool().display()))?
    else {
        return Ok(Imported::default());
    };

    let today = Zoned::now().date();
    let mut done = Imported::default();
    let mut rejected = Vec::new();
    for (i, line) in content.split(|&b| b == b'\n').enumerate() {
        if line.trim_ascii().is_empty() {
            continue;
        }
        let reason = match entry_input(line, today) {
            Err(reason) => reason,
            Ok(input) => match store.capture(input).await {
                Ok(_) => {
                    done.captured += 1;
                    continue;
                }
                Err(e @ (StoreError::Mutation { .. } | StoreError::RequestIdReused)) => {
                    e.to_string()
                }
                Err(e) => {
                    return Err(e).context("importing the spool; it is kept for the next try");
                }
            },
        };
        tracing::warn!(line = i + 1, %reason, "rejecting a spool line");
        rejected.extend_from_slice(line);
        rejected.push(b'\n');
        done.rejected += 1;
    }

    let rejected_path = paths.spool_rejected();
    let database = paths.database();
    let finish = tokio::task::spawn_blocking(move || {
        if done.captured > 0 {
            sync_wal(&database)?;
        }
        if !rejected.is_empty() {
            let mut out = open_append(&rejected_path)?;
            out.write_all(&rejected)?;
            out.sync_data()?;
        }
        file.set_len(0)?;
        file.sync_data()?;
        file.unlock()
    });
    finish.await?.context("truncating the spool")?;
    Ok(done)
}

/// Makes the store's commits durable before the spool lines behind them are
/// dropped. The store runs in WAL mode with `synchronous=NORMAL`, which
/// syncs its log only at checkpoints, so a power loss could otherwise undo
/// captures whose lines are already gone.
fn sync_wal(database: &Path) -> io::Result<()> {
    let mut wal = database.as_os_str().to_owned();
    wal.push("-wal");
    match File::open(wal) {
        Ok(wal) => wal.sync_all(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Opens, locks and reads the spool; `None` when there is none.
fn lock_and_read(path: &Path) -> io::Result<Option<(File, Vec<u8>)>> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    file.lock()?;
    let mut content = Vec::new();
    file.read_to_end(&mut content)?;
    Ok(Some((file, content)))
}

/// The capture a spool line asks for, or why the line is malformed.
fn entry_input(line: &[u8], today: Date) -> Result<CaptureInput, String> {
    let entry: SpoolEntry =
        serde_json::from_slice(line).map_err(|e| format!("not a spool entry: {e}"))?;
    let Some(spool_id) = canonical_ulid(&entry.spool_id) else {
        return Err("spool_id is not a ULID".to_owned());
    };
    let Some((at, day)) = captured_at(&entry.captured_at) else {
        return Err("captured_at is not a storable RFC 3339 time with an offset".to_owned());
    };
    if entry.text.trim().is_empty() {
        return Err("text is blank".to_owned());
    }
    Ok(CaptureInput {
        request_id: spool_id.clone(),
        draft: parse_capture(&entry.text, day),
        created_via: CreatedVia::Cli,
        parent_id: None,
        spool_id: Some(spool_id),
        now: at,
        today,
    })
}

/// The instant `s` names and its date in the offset `s` was written with.
fn captured_at(s: &str) -> Option<(Timestamp, Date)> {
    let at: Timestamp = s.parse().ok()?;
    let offset = Pieces::parse(s).ok()?.to_numeric_offset()?;
    storable(at).then(|| (at, at.to_zoned(TimeZone::fixed(offset)).date()))
}

/// Imports the spool now, then every `interval` while it is non-empty,
/// until `stop`. A failed import is logged and retried at the next interval.
pub(crate) async fn run(paths: Paths, store: Store, mut stop: ShutdownSignal, interval: Duration) {
    let spool = paths.spool();
    let mut due = true;
    loop {
        if due {
            match import(&paths, &store).await {
                Ok(Imported {
                    captured: 0,
                    rejected: 0,
                }) => {}
                Ok(done) => tracing::info!(
                    captured = done.captured,
                    rejected = done.rejected,
                    "imported the spool"
                ),
                Err(e) => tracing::error!(error = %format_args!("{e:#}"), "spool import failed"),
            }
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = stop.wait() => return,
        }
        // Only the metadata, so an idle daemon never takes the lock.
        due = tokio::fs::metadata(&spool).await.is_ok_and(|m| m.len() > 0);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use todo_core::new_ulid;
    use todo_store::ListQuery;

    use super::*;
    use crate::ShutdownHandle;
    use crate::paths::{Env, Os};

    fn paths_in(home: &Path) -> Paths {
        Paths::resolve(&Env {
            os: Os::Linux,
            home: None,
            todo_tui_home: Some(home.to_owned()),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_state_home: None,
            xdg_runtime_dir: None,
            tmpdir: None,
            uid: rustix::process::getuid().as_raw(),
        })
        .unwrap()
    }

    /// A store in a fresh `TODO_TUI_HOME`, whose data dir the spool shares.
    fn setup() -> (tempfile::TempDir, Paths, Store) {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        std::fs::create_dir_all(&paths.data).unwrap();
        let store = Store::open(&paths.database()).unwrap();
        (home, paths, store)
    }

    /// How many Items have each title.
    async fn titles(store: &Store) -> HashMap<String, usize> {
        let query = ListQuery {
            page_size: 1000,
            ..ListQuery::default()
        };
        let mut counts = HashMap::new();
        for item in store.list_items(query).await.unwrap().items {
            *counts.entry(item.core.title).or_default() += 1;
        }
        counts
    }

    #[test]
    fn an_entry_is_one_json_line_in_field_order() {
        let id = new_ulid();
        let entry = SpoolEntry::now(id, "call \"dentist\"");
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            json,
            format!(
                r#"{{"spool_id":"{id}","captured_at":"{}","text":"call \"dentist\""}}"#,
                entry.captured_at
            )
        );
        let (at, day) = captured_at(&entry.captured_at).unwrap();
        let local = at.to_zoned(TimeZone::system());
        assert_eq!(day, local.date());
        let offset = Pieces::parse(&entry.captured_at)
            .unwrap()
            .to_numeric_offset();
        assert_eq!(offset, Some(local.offset()));
    }

    #[test]
    fn captured_at_resolves_in_its_own_offset() {
        use jiff::civil::date;
        let late_evening = "2026-01-15T23:30:00-05:00";
        let (at, day) = captured_at(late_evening).unwrap();
        assert_eq!(at, "2026-01-16T04:30:00Z".parse().unwrap());
        assert_eq!(day, date(2026, 1, 15));
        assert_eq!(
            captured_at("2026-01-16T04:30:00.5Z").unwrap().1,
            date(2026, 1, 16)
        );
        for bad in [
            "2026-01-15T23:30:00",
            "2026-01-15",
            "yesterday",
            "",
            "1600-01-01T00:00:00Z",
            "2300-01-01T00:00:00+01:00",
        ] {
            assert_eq!(captured_at(bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn an_append_after_a_torn_line_starts_its_own_line() {
        let (_home, paths, store) = setup();
        let torn = r#"{"spool_id":"01"#;
        std::fs::write(paths.spool(), torn).unwrap();
        append(&paths, &SpoolEntry::now(new_ulid(), "after the tear")).unwrap();
        let done = import(&paths, &store).await.unwrap();
        assert_eq!(
            done,
            Imported {
                captured: 1,
                rejected: 1
            }
        );
        assert_eq!(titles(&store).await.get("after the tear"), Some(&1));
        let rejected = std::fs::read_to_string(paths.spool_rejected()).unwrap();
        assert_eq!(rejected, format!("{torn}\n"));
    }

    #[test]
    fn an_append_gives_up_on_a_lock_held_too_long() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        append(&paths, &SpoolEntry::now(new_ulid(), "first")).unwrap();
        let before = std::fs::read(paths.spool()).unwrap();
        // As an import holds it.
        let holder = File::open(paths.spool()).unwrap();
        holder.lock().unwrap();
        let wait = Duration::from_millis(200);
        let start = Instant::now();
        let entry = SpoolEntry::now(new_ulid(), "second");
        let err = append_within(&paths, &entry, wait).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert_eq!(err.to_string(), "the spool is still locked after 200ms");
        assert!(start.elapsed() >= wait, "{:?}", start.elapsed());
        assert_eq!(std::fs::read(paths.spool()).unwrap(), before);

        // Released within the wait, the append goes through.
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            holder.unlock().unwrap();
        });
        append_within(&paths, &entry, Duration::from_secs(10)).unwrap();
        release.join().unwrap();
        let content = std::fs::read_to_string(paths.spool()).unwrap();
        assert_eq!(content.lines().count(), 2);
    }

    #[tokio::test]
    async fn a_spool_id_is_imported_once_in_any_spelling() {
        let (_home, paths, store) = setup();
        let entry = SpoolEntry::now(new_ulid(), "once");
        append(&paths, &entry).unwrap();
        import(&paths, &store).await.unwrap();
        let lower = SpoolEntry {
            spool_id: entry.spool_id.to_ascii_lowercase(),
            ..entry
        };
        append(&paths, &lower).unwrap();
        let done = import(&paths, &store).await.unwrap();
        assert_eq!(
            done,
            Imported {
                captured: 1,
                rejected: 0
            }
        );
        assert_eq!(titles(&store).await.get("once"), Some(&1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn appends_racing_an_import_are_kept_for_the_next_one() {
        const BEFORE: usize = 200;
        const WRITERS: usize = 4;
        const EACH: usize = 50;
        let (_home, paths, store) = setup();
        for i in 0..BEFORE {
            append(&paths, &SpoolEntry::now(new_ulid(), format!("before {i}"))).unwrap();
        }
        let writers: Vec<_> = (0..WRITERS)
            .map(|w| {
                let paths = paths.clone();
                std::thread::spawn(move || {
                    for i in 0..EACH {
                        let entry = SpoolEntry::now(new_ulid(), format!("w{w} {i}"));
                        append(&paths, &entry).unwrap();
                        std::thread::sleep(Duration::from_micros(500));
                    }
                })
            })
            .collect();
        let mut imported = 0;
        while writers.iter().any(|w| !w.is_finished()) {
            imported += import(&paths, &store).await.unwrap().captured;
            // A daemon pauses between imports. Appends poll for the lock, so
            // a pause longer than their retry gives each waiting one a turn.
            tokio::time::sleep(LOCK_RETRY + Duration::from_millis(5)).await;
        }
        for w in writers {
            w.join().unwrap();
        }
        imported += import(&paths, &store).await.unwrap().captured;

        let total = BEFORE + WRITERS * EACH;
        assert_eq!(imported, total);
        let titles = titles(&store).await;
        assert_eq!(titles.len(), total);
        assert!(titles.values().all(|&n| n == 1));
        assert_eq!(std::fs::metadata(paths.spool()).unwrap().len(), 0);
        assert!(!paths.spool_rejected().exists());
    }

    #[tokio::test]
    async fn run_imports_at_start_then_whenever_the_spool_fills() {
        let (_home, paths, store) = setup();
        append(&paths, &SpoolEntry::now(new_ulid(), "at start")).unwrap();
        let shutdown = ShutdownHandle::new();
        let task = tokio::spawn(run(
            paths.clone(),
            store.clone(),
            shutdown.signal(),
            Duration::from_millis(20),
        ));
        let wait_for = async |title: &str| {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while !titles(&store).await.contains_key(title) {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "{title} never imported"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        wait_for("at start").await;
        // Off the runtime: the import may still hold the lock, and it needs
        // this test's only runtime thread to get to its truncate.
        let spool = paths.clone();
        tokio::task::spawn_blocking(move || {
            append(&spool, &SpoolEntry::now(new_ulid(), "while running"))
        })
        .await
        .unwrap()
        .unwrap();
        wait_for("while running").await;
        shutdown.trigger();
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(titles(&store).await.len(), 2);
    }
}
