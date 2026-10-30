//! Restarting the daemon's background tasks when they panic (design 4.5).

use std::any::Any;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::shutdown::ShutdownSignal;

/// How long [`spawn_supervised`] waits before restarting a panicked task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Backoff {
    /// The wait after the first panic, doubled after each one that follows.
    pub(crate) first: Duration,
    /// The longest wait.
    pub(crate) max: Duration,
    /// A run at least this long resets the wait to `first`.
    pub(crate) healthy: Duration,
}

impl Default for Backoff {
    /// 1 s doubling to 60 s, reset by a minute without a panic.
    fn default() -> Self {
        Backoff {
            first: Duration::from_secs(1),
            max: Duration::from_secs(60),
            healthy: Duration::from_secs(60),
        }
    }
}

/// Runs `make_task()` as a task named `name`, and runs it again after the
/// `backoff` wait each time it panics, until `stop`. A task that returns
/// is not restarted. A running task is left to see `stop` itself, so it
/// can finish what it is doing; once `stop` is signalled a panic is not
/// restarted, nor is a restart that is still waiting.
/// [`Supervised::abort`] ends it sooner.
pub(crate) fn spawn_supervised<F, Fut>(
    name: &'static str,
    mut stop: ShutdownSignal,
    backoff: Backoff,
    mut make_task: F,
) -> Supervised
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (cancel, mut cancelled) = oneshot::channel::<()>();
    let supervisor = tokio::spawn(async move {
        let mut wait = backoff.first;
        loop {
            let started = Instant::now();
            let mut task = tokio::spawn(make_task());
            let result = tokio::select! {
                r = &mut task => r,
                _ = &mut cancelled => {
                    // An aborted task's handle resolves only once its
                    // future, and all it holds, has been dropped.
                    task.abort();
                    let _ = task.await;
                    return;
                }
            };
            let panic = match result {
                Ok(()) => return,
                Err(e) if e.is_panic() => e.into_panic(),
                // Cancelled: only this supervisor aborts it.
                Err(_) => return,
            };
            let panic = panic_message(panic.as_ref());
            if stop.is_triggered() {
                tracing::error!(
                    task = name,
                    panic,
                    "background task panicked during shutdown"
                );
                return;
            }
            if started.elapsed() >= backoff.healthy {
                wait = backoff.first;
            }
            tracing::error!(
                task = name,
                panic,
                restart_in = ?wait,
                "background task panicked; restarting it"
            );
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = stop.wait() => return,
                _ = &mut cancelled => return,
            }
            wait = (wait * 2).min(backoff.max);
        }
    });
    Supervised {
        _cancel: cancel,
        supervisor,
    }
}

/// A supervised task. Dropping it aborts the task, without waiting.
pub(crate) struct Supervised {
    /// Dropped, which closes the channel, to cancel the supervisor.
    _cancel: oneshot::Sender<()>,
    supervisor: JoinHandle<()>,
}

impl Supervised {
    /// Aborts the task, and returns once it has been dropped, so nothing
    /// it held (such as a [`Store`](todo_store::Store) clone, whose last
    /// drop joins the writer thread) outlives the call.
    pub(crate) async fn abort(self) {
        let Supervised {
            _cancel: cancel,
            supervisor,
        } = self;
        drop(cancel);
        let _ = supervisor.await;
    }

    /// Waits for the supervisor to end on its own.
    #[cfg(test)]
    async fn join(&mut self) -> Result<(), tokio::task::JoinError> {
        (&mut self.supervisor).await
    }
}

/// The message a panic was raised with, if it is text.
fn panic_message(panic: &(dyn Any + Send)) -> &str {
    if let Some(s) = panic.downcast_ref::<&str>() {
        s
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s
    } else {
        "(not text)"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::time::timeout;

    use super::*;
    use crate::ShutdownHandle;

    const WAIT: Duration = Duration::from_secs(10);

    fn short() -> Backoff {
        Backoff {
            first: Duration::from_millis(10),
            max: Duration::from_millis(40),
            healthy: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    async fn a_panicked_task_restarts_and_one_that_returns_does_not() {
        let shutdown = ShutdownHandle::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let started = Instant::now();
        let mut supervisor = spawn_supervised("test", shutdown.signal(), short(), move || {
            let run = counted.fetch_add(1, Ordering::SeqCst);
            async move {
                if run == 0 {
                    panic!("first run fails");
                }
            }
        });
        // The second run returns, which ends the supervisor.
        timeout(WAIT, supervisor.join()).await.unwrap().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert!(
            started.elapsed() >= short().first,
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn the_wait_doubles_to_its_cap() {
        let shutdown = ShutdownHandle::new();
        let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&starts);
        let mut supervisor = spawn_supervised("test", shutdown.signal(), short(), move || {
            let mut starts = recorded.lock().unwrap();
            starts.push(Instant::now());
            let run = starts.len();
            async move {
                if run <= 5 {
                    panic!("run {run} fails");
                }
            }
        });
        timeout(WAIT, supervisor.join()).await.unwrap().unwrap();
        let starts = starts.lock().unwrap();
        assert_eq!(starts.len(), 6);
        let gaps: Vec<Duration> = starts.windows(2).map(|w| w[1] - w[0]).collect();
        let ms = Duration::from_millis;
        for (gap, at_least) in gaps.iter().zip([ms(10), ms(20), ms(40), ms(40), ms(40)]) {
            assert!(*gap >= at_least, "{gaps:?}");
        }
    }

    #[tokio::test]
    async fn shutdown_stops_the_restarts() {
        // A task that panics once it sees shutdown is not restarted.
        let shutdown = ShutdownHandle::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let stop = shutdown.signal();
        let mut supervisor = spawn_supervised("test", shutdown.signal(), short(), move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut stop = stop.clone();
            async move {
                stop.wait().await;
                panic!("fails while stopping");
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        shutdown.trigger();
        timeout(WAIT, supervisor.join()).await.unwrap().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // Shutdown during the wait before a restart: no second run.
        let shutdown = ShutdownHandle::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let slow = Backoff {
            first: Duration::from_secs(3600),
            ..short()
        };
        let mut supervisor = spawn_supervised("test", shutdown.signal(), slow, move || {
            counted.fetch_add(1, Ordering::SeqCst);
            async { panic!("always fails") }
        });
        while runs.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        shutdown.trigger();
        timeout(WAIT, supervisor.join()).await.unwrap().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropping_the_handle_aborts_the_task() {
        let shutdown = ShutdownHandle::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let mut tx = Some(tx);
        let supervisor = spawn_supervised("test", shutdown.signal(), short(), move || {
            // Dropped, closing the channel, only when the task is.
            let tx = tx.take();
            async move {
                let _tx = tx;
                std::future::pending::<()>().await;
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(supervisor);
        assert!(timeout(WAIT, rx).await.unwrap().is_err());
    }

    /// Sets its flag once it has been dropped, slowly, as the last
    /// `Store` clone is when it joins the writer thread.
    struct SlowDrop(Arc<AtomicBool>);

    impl Drop for SlowDrop {
        fn drop(&mut self) {
            std::thread::sleep(Duration::from_millis(50));
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn abort_returns_once_the_task_has_been_dropped() {
        for wait_first in [false, true] {
            let shutdown = ShutdownHandle::new();
            let dropped = Arc::new(AtomicBool::new(false));
            let runs = Arc::new(AtomicUsize::new(0));
            let (flag, counted) = (Arc::clone(&dropped), Arc::clone(&runs));
            let slow = Backoff {
                first: Duration::from_secs(3600),
                ..short()
            };
            let supervisor = spawn_supervised("test", shutdown.signal(), slow, move || {
                let guard = SlowDrop(Arc::clone(&flag));
                let run = counted.fetch_add(1, Ordering::SeqCst);
                async move {
                    let _guard = guard;
                    if run == 0 && wait_first {
                        panic!("fails, then waits for a restart");
                    }
                    std::future::pending::<()>().await;
                }
            });
            while runs.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            if !wait_first {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            timeout(WAIT, supervisor.abort()).await.unwrap();
            assert!(dropped.load(Ordering::SeqCst), "wait_first: {wait_first}");
            assert_eq!(runs.load(Ordering::SeqCst), 1);
        }
    }
}
