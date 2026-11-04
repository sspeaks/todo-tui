//! Daemon startup (lock, socket) and the tonic server.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use jiff::SignedDuration;
use todo_proto::v1::item_service_server::ItemServiceServer;
use todo_proto::v1::system_service_server::SystemServiceServer;
use todo_proto::v1::view_service_server::ViewServiceServer;
use todo_proto::v1::watch_service_server::WatchServiceServer;
use todo_store::Store;
use tokio::net::{TcpListener, UnixListener};
use tokio::signal::unix::{SignalKind, signal};
use tonic::transport::Server;
use tonic_web::GrpcWebLayer;

use crate::ShutdownHandle;
use crate::client;
use crate::conn::Clients;
use crate::paths::Paths;
use crate::remote::{self, Auth};
use crate::scheduler::{self, Wake};
use crate::service::{Ctx, item::Items, system::System, view::Views, watch::Watch};
use crate::spool;
use crate::supervise::{Backoff, spawn_supervised};

/// How long in-flight work may take once shutdown starts (design 4.2).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

pub enum Started {
    Running(Box<Daemon>),
    /// Another process holds `data/daemon.lock`.
    AlreadyRunning,
}

/// How a [`Daemon`] runs its background tasks. Tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonOptions {
    /// How often the scheduler sweeps when no user op wakes it (60 s).
    pub scheduler_interval: Duration,
    /// How long a done or cancelled Item stays out of Archive before the
    /// scheduler archives it `auto_done`
    /// ([`ARCHIVE_DONE_AFTER_DAYS`](crate::ARCHIVE_DONE_AFTER_DAYS) days).
    pub archive_done_after: SignedDuration,
    /// How often a running daemon looks at the capture spool (30 s).
    pub spool_interval: Duration,
    /// How long a background task that panicked waits before its first
    /// restart (1 s). The wait doubles after each panic that follows, up
    /// to 60 s or this, whichever is longer.
    pub restart_after: Duration,
    /// Test hook: the scheduler's first this many runs panic at once.
    #[doc(hidden)]
    pub scheduler_panics: u32,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        DaemonOptions {
            scheduler_interval: scheduler::INTERVAL,
            archive_done_after: scheduler::archive_done_after(),
            spool_interval: spool::INTERVAL,
            restart_after: Backoff::default().first,
            scheduler_panics: 0,
        }
    }
}

/// A daemon that holds the lock and is bound to its socket, ready to serve.
pub struct Daemon {
    paths: Paths,
    store: Store,
    listener: UnixListener,
    /// The remote listener (ADR-0004), when `[remote]` is configured.
    remote: Option<TcpListener>,
    shutdown: ShutdownHandle,
    options: DaemonOptions,
    _lock: File,
}

impl Daemon {
    /// [`Daemon::start_with`] the default options.
    pub async fn start(paths: Paths) -> anyhow::Result<Started> {
        Daemon::start_with(paths, DaemonOptions::default()).await
    }

    /// Takes the single-daemon lock (D6/D7), records the pid in it, opens the
    /// store and binds the socket (mode 0600). Must run inside a tokio runtime.
    pub async fn start_with(paths: Paths, options: DaemonOptions) -> anyhow::Result<Started> {
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
        let remote = bind_remote(&paths).await;

        Ok(Started::Running(Box::new(Daemon {
            paths,
            store,
            listener,
            remote,
            shutdown: ShutdownHandle::new(),
            options,
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

    /// The remote listener's bound address, when it is listening.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.remote.as_ref().and_then(|l| l.local_addr().ok())
    }

    /// Imports the capture spool (design 2.1), then serves until shutdown is
    /// triggered (by a handle or the `Shutdown` RPC), then drains for up to
    /// 10s, removes the socket and releases the lock. Meanwhile imports the
    /// spool again whenever it fills, and runs the scheduler. The spooled
    /// captures are undo stack entries, so they are imported before any call
    /// is served: the first Undo finds them on the stack, and none lands on
    /// top of a later op. Both background tasks are restarted if they panic.
    pub async fn serve(self) -> anyhow::Result<()> {
        let Daemon {
            paths,
            store,
            listener,
            remote,
            shutdown,
            options,
            _lock,
        } = self;
        let clients = Clients::default();
        spool::import_logged(&paths, &store).await;
        let backoff = Backoff {
            first: options.restart_after,
            max: options.restart_after.max(Backoff::default().max),
            ..Backoff::default()
        };
        let spool = {
            let (paths, store, stop) = (paths.clone(), store.clone(), shutdown.signal());
            spawn_supervised("spool", shutdown.signal(), backoff, move || {
                spool::run(
                    paths.clone(),
                    store.clone(),
                    stop.clone(),
                    options.spool_interval,
                )
            })
        };
        let wake = Wake::default();
        let scheduler = {
            let (store, wake, stop) = (store.clone(), wake.clone(), shutdown.signal());
            let mut panics = options.scheduler_panics;
            spawn_supervised("scheduler", shutdown.signal(), backoff, move || {
                let panic = panics > 0;
                panics = panics.saturating_sub(1);
                let run = scheduler::run(
                    store.clone(),
                    wake.clone(),
                    stop.clone(),
                    options.scheduler_interval,
                    options.archive_done_after,
                );
                async move {
                    assert!(!panic, "scheduler_panics test hook");
                    run.await;
                }
            })
        };
        let ctx = Ctx {
            store,
            clients: clients.clone(),
            shutdown: shutdown.clone(),
            wake,
        };
        let remote_clients = clients.clone();

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
            .add_service(ViewServiceServer::new(Views(ctx.clone())))
            .add_service(WatchServiceServer::new(Watch(ctx.clone())))
            .serve_with_incoming_shutdown(incoming, async move { stop.wait().await });

        // The remote listener: the same services as gRPC-Web over HTTP/1.1,
        // each call checked by `Auth` (ADR-0004).
        let remote_server = remote.map(|remote| {
            let clients = remote_clients;
            let auth = Auth::new(paths.remote_tokens());
            let incoming = async_stream::stream! {
                loop {
                    match remote.accept().await {
                        Ok((io, _)) => yield Ok::<_, io::Error>(clients.accept(io)),
                        Err(e) => {
                            tracing::warn!(error = %e, "remote accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            };
            let mut stop = shutdown.signal();
            Server::builder()
                .accept_http1(true)
                .layer(GrpcWebLayer::new())
                .add_service(SystemServiceServer::with_interceptor(
                    System(ctx.clone()),
                    auth.clone(),
                ))
                .add_service(ItemServiceServer::with_interceptor(
                    Items(ctx.clone()),
                    auth.clone(),
                ))
                .add_service(ViewServiceServer::with_interceptor(
                    Views(ctx.clone()),
                    auth.clone(),
                ))
                .add_service(WatchServiceServer::with_interceptor(Watch(ctx), auth))
                .serve_with_incoming_shutdown(incoming, async move { stop.wait().await })
        });
        let servers = async {
            match remote_server {
                Some(remote_server) => {
                    let (local, remote) = tokio::join!(server, remote_server);
                    local
                        .context("server failed")
                        .and(remote.context("remote server failed"))
                }
                None => server.await.context("server failed"),
            }
        };

        tracing::info!(socket = %paths.socket.display(), pid = std::process::id(), "daemon listening");
        let mut stop = shutdown.signal();
        let result = tokio::select! {
            r = servers => r,
            () = async { stop.wait().await; tokio::time::sleep(SHUTDOWN_GRACE).await } => {
                tracing::warn!("in-flight work did not finish within {SHUTDOWN_GRACE:?}");
                Ok(())
            }
        };
        // Both tasks have seen the shutdown and end at their next pause.
        // An interrupted import is safe: what it captured is recorded by
        // spool id, and the spool is only truncated once it completes. An
        // interrupted sweep is too: each of its ops commits or not at all.
        // Each abort returns once its task, and the Store clone it holds,
        // has been dropped, so the last clone (dropped here) joins the
        // writer thread before the lock is released: a daemon that takes
        // the lock next sees every commit.
        for task in [spool, scheduler] {
            task.abort().await;
        }
        let _ = fs::remove_file(&paths.socket);
        tracing::info!("daemon stopped");
        result
    }
}

/// Binds the remote listener when `[remote]` is configured. A config or
/// bind problem is logged and leaves the daemon serving the socket alone:
/// the local clients must not lose the daemon over the phone's listener.
async fn bind_remote(paths: &Paths) -> Option<TcpListener> {
    let config = match remote::read_config(&paths.config) {
        Ok(c) => c?,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "remote listener disabled");
            return None;
        }
    };
    match TcpListener::bind(config.listen).await {
        Ok(l) => {
            tracing::info!(addr = %l.local_addr().map_or(config.listen, |a| a), "remote listener bound");
            Some(l)
        }
        Err(e) => {
            tracing::error!(addr = %config.listen, error = %e, "remote listener disabled: bind failed");
            None
        }
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
