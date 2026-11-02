//! The scheduler (design 3.7, 4.1): archives done and cancelled Items once
//! they have been closed for `archive_done_after_days`, and unarchives the
//! ones it archived that are open again. It sweeps when it starts, every
//! interval, and whenever a user op wakes it, so the healing after an undo
//! of a completion is immediate.

use std::sync::Arc;
use std::time::Duration;

use jiff::{SignedDuration, Zoned};
use todo_store::{AutoArchiveInput, Store};
use tokio::sync::{Notify, watch};

use crate::config::Config;
use crate::shutdown::ShutdownSignal;

/// How often the scheduler sweeps without being woken.
pub(crate) const INTERVAL: Duration = Duration::from_secs(60);

/// The most auto-archive ops one sweep runs (each changes at most 1000
/// Items); a backlog beyond that waits for the next sweep.
const MAX_OPS_PER_SWEEP: usize = 100;

/// Wakes the scheduler. Cheap to clone; wakes coalesce, so a burst of user
/// ops costs at most one extra sweep.
#[derive(Clone, Debug, Default)]
pub(crate) struct Wake(Arc<Notify>);

impl Wake {
    /// Asks for a sweep soon. One that is already running sweeps again
    /// after it ends, so no op is missed.
    pub(crate) fn poke(&self) {
        self.0.notify_one();
    }

    async fn woken(&self) {
        self.0.notified().await;
    }
}

/// How long the scheduler waits before it archives a closed Item.
#[derive(Debug, Clone)]
pub(crate) enum After {
    /// The config's `archive_done_after_days`, as of each sweep.
    Config(watch::Receiver<std::sync::Arc<Config>>),
    /// A fixed wait (tests).
    Fixed(SignedDuration),
}

impl After {
    fn current(&self) -> SignedDuration {
        match self {
            After::Config(config) => config.borrow().archive_done_after(),
            After::Fixed(after) => *after,
        }
    }
}

/// Sweeps at start, then every `interval` or when `wake` is poked, until
/// `stop`. Items are archived after `after`, by the wall clock, which the
/// auto-archive's stickiness compares with the user's own archive events.
pub(crate) async fn run(
    store: Store,
    wake: Wake,
    mut stop: ShutdownSignal,
    interval: Duration,
    after: After,
) {
    loop {
        sweep(&store, after.current()).await;
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = wake.woken() => {}
            () = stop.wait() => return,
        }
    }
}

/// Auto-archives until nothing more is due, yielding between ops so the
/// writer is shared with the user's calls. Logs what it changed; a failure
/// is logged and ends the sweep, and the next one tries again.
async fn sweep(store: &Store, after: SignedDuration) {
    for _ in 0..MAX_OPS_PER_SWEEP {
        let now = Zoned::now();
        let input = AutoArchiveInput {
            now: now.timestamp(),
            after,
            today: now.date(),
        };
        let out = match store.auto_archive(input).await {
            Ok(out) => out,
            Err(e) => {
                tracing::warn!(error = %e, "auto-archive failed");
                return;
            }
        };
        if !out.archived.is_empty() || !out.healed.is_empty() {
            tracing::info!(
                archived = out.archived.len(),
                unarchived = out.healed.len(),
                "auto-archived done items"
            );
        }
        if !out.more {
            return;
        }
        tokio::task::yield_now().await;
    }
    tracing::info!("auto-archive backlog left for the next sweep");
}
