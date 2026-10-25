//! Daemon startup (lock, socket) and the tonic server.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use todo_proto::v1::item_service_server::ItemServiceServer;
use todo_proto::v1::system_service_server::SystemServiceServer;
use todo_proto::v1::watch_service_server::WatchServiceServer;
use todo_store::Store;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tonic::transport::Server;

use crate::ShutdownHandle;
use crate::client;
use crate::conn::Clients;
use crate::paths::Paths;
use crate::service::{Ctx, item::Items, system::System, watch::Watch};
use crate::spool;

/// How long in-flight work may take once shutdown starts (design 4.2).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

pub enum Started {
    Running(Box<Daemon>),
    /// Another process holds `data/daemon.lock`.
    AlreadyRunning,
}

/// A daemon that holds the lock and is bound to its socket, ready to serve.
pub struct Daemon {
    paths: Paths,
    store: Store,
    listener: UnixListener,
    shutdown: ShutdownHandle,
    _lock: File,
}

impl Daemon {
    /// Takes the single-daemon lock (D6/D7), records the pid in it, opens the
    /// store and binds the socket (mode 0600). Must run inside a tokio runtime.
    pub async fn start(paths: Paths) -> anyhow::Result<Started> {
        create_dir(&paths.data)?;
        create_dir(&paths.logs)?;
        create_runtime_dir(&paths.runtime)?;

        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&paths.lock)
            .with_context(|| format!("opening {}", paths.lock.display()))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(Started::AlreadyRunning),
            Err(fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking {}", paths.lock.display()));
            }
        }
        lock.set_len(0)?;
        writeln!(lock, "{}", std::process::id())?;

        let db = paths.database();
        let store = tokio::task::spawn_blocking(move || Store::open(&db))
            .await?
            .with_context(|| format!("opening {}", paths.database().display()))?;

        // We hold the lock, so any socket file left behind is stale.
        match fs::remove_file(&paths.socket) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("removing {}", paths.socket.display()));
            }
            _ => {}
        }
        let listener = UnixListener::bind(&paths.socket)
            .with_context(|| format!("binding {}", paths.socket.display()))?;
        fs::set_permissions(&paths.socket, Permissions::from_mode(0o600))?;

        Ok(Started::Running(Box::new(Daemon {
            paths,
            store,
            listener,
            shutdown: ShutdownHandle::new(),
            _lock: lock,
        })))
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn shutdown_handle(&self) -> ShutdownHandle {
        self.shutdown.clone()
    }

    /// Imports the capture spool (design 2.1), then serves until shutdown is
    /// triggered (by a handle or the `Shutdown` RPC), then drains for up to
    /// 10s, removes the socket and releases the lock. Meanwhile imports the
    /// spool again whenever it fills. The spooled captures are undo stack
    /// entries, so they are imported before any call is served: the first
    /// Undo finds them on the stack, and none lands on top of a later op.
    pub async fn serve(self) -> anyhow::Result<()> {
        let Daemon {
            paths,
            store,
            listener,
            shutdown,
            _lock,
        } = self;
        let clients = Clients::default();
        spool::import_logged(&paths, &store).await;
        let spool = tokio::spawn(spool::run(
            paths.clone(),
            store.clone(),
            shutdown.signal(),
            spool::INTERVAL,
        ));
        let ctx = Ctx {
            store,
            clients: clients.clone(),
            shutdown: shutdown.clone(),
        };

        let incoming = async_stream::stream! {
            loop {
                match listener.accept().await {
                    Ok((io, _)) => yield Ok::<_, io::Error>(clients.accept(io)),
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        };

        let mut stop = shutdown.signal();
        let server = Server::builder()
            .http2_keepalive_interval(Some(Duration::from_secs(30)))
            .http2_keepalive_timeout(Some(Duration::from_secs(10)))
            .add_service(SystemServiceServer::new(System(ctx.clone())))
            .add_service(ItemServiceServer::new(Items(ctx.clone())))
            .add_service(WatchServiceServer::new(Watch(ctx)))
            .serve_with_incoming_shutdown(incoming, async move { stop.wait().await });

        tracing::info!(socket = %paths.socket.display(), pid = std::process::id(), "daemon listening");
        let mut stop = shutdown.signal();
        let result = tokio::select! {
            r = server => r.context("server failed"),
            () = async { stop.wait().await; tokio::time::sleep(SHUTDOWN_GRACE).await } => {
                tracing::warn!("in-flight work did not finish within {SHUTDOWN_GRACE:?}");
                Ok(())
            }
        };
        // An interrupted import is safe: what it captured is recorded by
        // spool id, and the spool is only truncated once it completes.
        spool.abort();
        let _ = spool.await;
        let _ = fs::remove_file(&paths.socket);
        tracing::info!("daemon stopped");
        result
    }
}

/// `todo daemon`: logs to the logs dir, serves until SIGTERM/SIGINT or the
/// `Shutdown` RPC. Returns [`Exit::AlreadyRunning`] without serving when
/// another daemon holds the lock.
pub async fn run(paths: Paths) -> anyhow::Result<Exit> {
    crate::logging::init(&paths.daemon_log())?;
    let daemon = match Daemon::start(paths).await? {
        Started::Running(d) => d,
        Started::AlreadyRunning => {
            tracing::info!("another daemon holds the lock; exiting");
            return Ok(Exit::AlreadyRunning);
        }
    };

    let shutdown = daemon.shutdown_handle();
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    // Detached daemons outlive their terminal.
    let mut hup = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = term.recv() => break,
                _ = int.recv() => break,
                _ = hup.recv() => tracing::info!("ignoring SIGHUP"),
            }
        }
        tracing::info!("signal received, shutting down");
        shutdown.trigger();
    });

    daemon.serve().await?;
    Ok(Exit::Stopped)
}

/// How [`run`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Stopped,
    AlreadyRunning,
}

/// Creates `dir` and missing parents with mode 0700.
fn create_dir(dir: &Path) -> anyhow::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

/// Like [`create_dir`], then makes sure every guarded runtime directory
/// (the runtime dir, and `/tmp/todo-tui-$UID` under the D8 fallback) is a
/// real directory we own with mode 0700: it may live in a shared location.
fn create_runtime_dir(dir: &Path) -> anyhow::Result<()> {
    create_dir(dir)?;
    for d in client::guarded_dirs(dir) {
        let meta = client::owned_dir_meta(d)?;
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(d, Permissions::from_mode(0o700))
                .with_context(|| format!("securing {}", d.display()))?;
        }
    }
    client::check_runtime_dir(dir)?;
    Ok(())
}
