//! Daemon startup (lock, socket) and the tonic server.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use jiff::SignedDuration;
use todo_proto::v1::assistant_service_server::AssistantServiceServer;
use todo_proto::v1::item_service_server::ItemServiceServer;
use todo_proto::v1::suggestion_service_server::SuggestionServiceServer;
use todo_proto::v1::sync_service_server::SyncServiceServer;
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
use crate::config::{self, Config, ConfigHandle};
use crate::conn::Clients;
use crate::paths::Paths;
use crate::remote::{self, Auth};
use crate::scheduler::{self, Wake};
use crate::service::{
    Ctx, assistant::Assistants, item::Items, suggestion::Suggestions, sync::Sync, system::System,
    view::Views, watch::Watch,
};
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
#[derive(Clone)]
pub struct DaemonOptions {
    /// How often the scheduler sweeps when no user op wakes it (60 s).
    pub scheduler_interval: Duration,
    /// How long a done or cancelled Item stays out of Archive before the
    /// scheduler archives it `auto_done`. `None` follows the config's
    /// `archive_done_after_days`, read on each sweep; tests set it.
    pub archive_done_after: Option<SignedDuration>,
    /// How often a running daemon looks at the capture spool (30 s).
    pub spool_interval: Duration,
    /// How long a background task that panicked waits before its first
    /// restart (1 s). The wait doubles after each panic that follows, up
    /// to 60 s or this, whichever is longer.
    pub restart_after: Duration,
    /// How often the backup task looks whether a backup is due, between
    /// config changes (1 h).
    pub backup_check: Duration,
    /// How often a quiet Subscribe stream gets a Heartbeat (25 s).
    pub heartbeat: Duration,
    /// Test hook: the scheduler's first this many runs panic at once.
    #[doc(hidden)]
    pub scheduler_panics: u32,
    /// What runs the assistant's tasks: the person's Claude Code CLI, or a
    /// stand-in for tests (never a real Claude call in a test).
    pub assistant: crate::assistant::Run,
}

impl std::fmt::Debug for DaemonOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonOptions")
            .field("scheduler_interval", &self.scheduler_interval)
            .field("archive_done_after", &self.archive_done_after)
            .field("spool_interval", &self.spool_interval)
            .field("restart_after", &self.restart_after)
            .field("backup_check", &self.backup_check)
            .field("heartbeat", &self.heartbeat)
            .finish_non_exhaustive()
    }
}

impl Default for DaemonOptions {
    fn default() -> Self {
        DaemonOptions {
            scheduler_interval: scheduler::INTERVAL,
            archive_done_after: None,
            spool_interval: spool::INTERVAL,
            restart_after: Backoff::default().first,
            backup_check: crate::backup::CHECK,
            heartbeat: crate::service::watch::HEARTBEAT,
            scheduler_panics: 0,
            assistant: crate::assistant::claude_code(),
        }
    }
}

/// A daemon that holds the lock and is bound to its socket, ready to serve.
pub struct Daemon {
    paths: Paths,
    store: Store,
    config: ConfigHandle,
    listener: UnixListener,
    /// The remote listener (ADR-0008), when `[remote]` is configured.
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

        let config_path = paths.config.clone();
        let config =
            tokio::task::spawn_blocking(move || Config::load_or_defaults(&config_path)).await?;
        for warning in &config.warnings {
            tracing::warn!(%warning, path = %config.path.display(), "config");
        }

        let db = paths.database();
        let backups = paths.backups();
        let store = tokio::task::spawn_blocking(move || Store::open_with(&db, Some(&backups)))
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
            config: ConfigHandle::new(config),
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

    /// Reloads the config as SIGHUP does.
    pub fn reload_handle(&self) -> ReloadHandle {
        ReloadHandle {
            config: self.config.clone(),
            store: self.store.clone(),
        }
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
            config,
            listener,
            remote,
            shutdown,
            options,
            _lock,
        } = self;
        let clients = Clients::default();
        // The file may have changed while no daemon ran, and config is not
        // stored: a change at every start makes each resuming client read
        // the config this daemon runs with.
        if let Err(e) = store.record_config_change().await {
            tracing::warn!(error = %e, "recording the config read at start failed");
        }
        // Before any call is served, so the first edit meets the guard the
        // config asks for, not the read-only default.
        crate::config::apply_to_store(&config.current(), &store);
        // Jobs an earlier daemon left queued or running never ran: failed,
        // before a Refine could join one.
        if let Err(e) = store.abandon_jobs(jiff::Timestamp::now()).await {
            tracing::warn!(error = %e, "failing the assistant's abandoned jobs failed");
        }
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
            let config = config.subscribe();
            let mut panics = options.scheduler_panics;
            spawn_supervised("scheduler", shutdown.signal(), backoff, move || {
                let panic = panics > 0;
                panics = panics.saturating_sub(1);
                let run = scheduler::run(
                    store.clone(),
                    wake.clone(),
                    stop.clone(),
                    options.scheduler_interval,
                    match options.archive_done_after {
                        Some(after) => scheduler::After::Fixed(after),
                        None => scheduler::After::Config(config.clone()),
                    },
                );
                async move {
                    assert!(!panic, "scheduler_panics test hook");
                    run.await;
                }
            })
        };
        let backups = {
            let (store, dir, config, stop) = (
                store.clone(),
                paths.backups(),
                config.subscribe(),
                shutdown.signal(),
            );
            spawn_supervised("backup", shutdown.signal(), backoff, move || {
                crate::backup::run(
                    store.clone(),
                    dir.clone(),
                    config.clone(),
                    stop.clone(),
                    options.backup_check,
                )
            })
        };
        let (queue, jobs) = crate::assistant::Queue::new();
        let assistant = {
            let (store, config, run, stop) = (
                store.clone(),
                config.clone(),
                options.assistant.clone(),
                shutdown.signal(),
            );
            spawn_supervised("assistant", shutdown.signal(), backoff, move || {
                crate::assistant::run(
                    store.clone(),
                    config.clone(),
                    run.clone(),
                    jobs.clone(),
                    stop.clone(),
                )
            })
        };
        let (status_tx, sync_status) = crate::sync::status_channel();
        let sync_now = crate::sync::SyncNow::default();
        // The loop pokes the counter when it adds an instance's status.
        let recount = Arc::new(tokio::sync::Notify::new());
        let counter = {
            let (store, status, stop) = (store.clone(), status_tx.clone(), shutdown.signal());
            let recount = recount.clone();
            spawn_supervised("outbox counts", shutdown.signal(), backoff, move || {
                crate::sync::count_outbox(
                    store.clone(),
                    status.clone(),
                    recount.clone(),
                    stop.clone(),
                )
            })
        };
        let sync = {
            let (store, config, clients, now, stop, recount) = (
                store.clone(),
                config.subscribe(),
                clients.clone(),
                sync_now.clone(),
                shutdown.signal(),
                recount.clone(),
            );
            spawn_supervised("sync", shutdown.signal(), backoff, move || {
                crate::sync::run(
                    store.clone(),
                    config.clone(),
                    clients.clone(),
                    now.clone(),
                    status_tx.clone(),
                    recount.clone(),
                    stop.clone(),
                )
            })
        };
        let ctx = Ctx {
            sync_status,
            sync_now,
            backups: paths.backups(),
            store,
            clients: clients.clone(),
            shutdown: shutdown.clone(),
            wake,
            config,
            heartbeat: options.heartbeat,
            assistant: queue,
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
            // An Import message holds a batch of export lines, and one line
            // a whole Item: the cap is the import's own (1 GiB in all).
            .add_service(
                SystemServiceServer::new(System(ctx.clone()))
                    .max_decoding_message_size(crate::service::system::MAX_IMPORT_BYTES),
            )
            .add_service(ItemServiceServer::new(Items(ctx.clone())))
            .add_service(ViewServiceServer::new(Views(ctx.clone())))
            .add_service(SyncServiceServer::new(Sync(ctx.clone())))
            .add_service(SuggestionServiceServer::new(Suggestions(ctx.clone())))
            .add_service(AssistantServiceServer::new(Assistants(ctx.clone())))
            .add_service(WatchServiceServer::new(Watch(ctx.clone())))
            .serve_with_incoming_shutdown(incoming, async move { stop.wait().await });

        // The remote listener: the same services as gRPC-Web over HTTP/1.1,
        // each call checked by `Auth` (ADR-0008).
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
                .add_service(SyncServiceServer::with_interceptor(
                    Sync(ctx.clone()),
                    auth.clone(),
                ))
                .add_service(SuggestionServiceServer::with_interceptor(
                    Suggestions(ctx.clone()),
                    auth.clone(),
                ))
                .add_service(AssistantServiceServer::with_interceptor(
                    Assistants(ctx.clone()),
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
        for task in [spool, scheduler, backups, assistant, sync, counter] {
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
    let reload = daemon.reload_handle();
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    // SIGHUP reloads the config; it never stops the daemon, which outlives
    // its terminal when detached.
    let mut hup = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = term.recv() => break,
                _ = int.recv() => break,
                _ = hup.recv() => {
                    tracing::info!("SIGHUP: reloading the config");
                    reload.reload().await;
                }
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

/// Reloads the config of a running [`Daemon`], as SIGHUP does.
#[derive(Clone)]
pub struct ReloadHandle {
    config: ConfigHandle,
    store: Store,
}

impl ReloadHandle {
    /// Reads the file again; a change is streamed to every subscriber. A
    /// file that is not TOML is logged and the running config kept.
    pub async fn reload(&self) {
        match config::reload(&self.config, &self.store).await {
            Ok((_, false)) => tracing::info!("config unchanged"),
            Ok((_, true)) => {}
            Err(e) => tracing::warn!(error = %e, "config not reloaded"),
        }
    }
}
