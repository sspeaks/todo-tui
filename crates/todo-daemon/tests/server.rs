//! In-process daemon tests over a real Unix socket in a temp `TODO_TUI_HOME`.

use std::collections::HashSet;
use std::time::Duration;

use tempfile::TempDir;
use todo_core::new_ulid;
use todo_daemon::paths::{Env, Os, Paths};
use todo_daemon::{Daemon, ShutdownHandle, Started};
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::system_service_client::SystemServiceClient;
use todo_proto::v1::watch_service_client::WatchServiceClient;
use todo_proto::v1::{
    CaptureItemRequest, Clause, ClientKind, CreatedVia, FieldValue, Filter, GetServerInfoRequest,
    Item, ItemField, ListItemsRequest, Operand, Operator, Predicate, ShutdownRequest, SortKey,
    Status, SubscribeRequest, SubscribeResponse, Topic, clause, field_value, operand,
    subscribe_response::Message,
};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tonic::Code;
use tonic::codec::Streaming;
use tonic::transport::Channel;
use tonic_types::StatusExt;

const WAIT: Duration = Duration::from_secs(10);

struct TestDaemon {
    home: TempDir,
    paths: Paths,
    shutdown: ShutdownHandle,
    task: JoinHandle<anyhow::Result<()>>,
}

fn paths_for(home: &TempDir) -> Paths {
    let env = Env {
        os: if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        },
        home: None,
        todo_tui_home: Some(home.path().to_owned()),
        xdg_config_home: None,
        xdg_data_home: None,
        xdg_state_home: None,
        xdg_runtime_dir: None,
        tmpdir: None,
        uid: rustix::process::getuid().as_raw(),
    };
    Paths::resolve(&env).unwrap()
}

async fn start_in(home: TempDir) -> TestDaemon {
    let paths = paths_for(&home);
    let Started::Running(daemon) = Daemon::start(paths.clone()).await.unwrap() else {
        panic!("lock unexpectedly held");
    };
    let shutdown = daemon.shutdown_handle();
    let task = tokio::spawn(daemon.serve());
    TestDaemon {
        home,
        paths,
        shutdown,
        task,
    }
}

async fn start() -> TestDaemon {
    start_in(TempDir::new().unwrap()).await
}

impl TestDaemon {
    async fn channel(&self) -> Channel {
        todo_daemon::client::connect(&self.paths.socket)
            .await
            .unwrap()
    }

    async fn items(&self) -> ItemServiceClient<Channel> {
        ItemServiceClient::new(self.channel().await)
    }

    async fn subscribe(&self, req: SubscribeRequest) -> Streaming<SubscribeResponse> {
        WatchServiceClient::new(self.channel().await)
            .subscribe(req)
            .await
            .unwrap()
            .into_inner()
    }

    async fn stop(self) -> TempDir {
        self.shutdown.trigger();
        timeout(WAIT, self.task).await.unwrap().unwrap().unwrap();
        assert!(!self.paths.socket.exists(), "socket removed on shutdown");
        self.home
    }
}

fn capture_req(text: &str) -> CaptureItemRequest {
    CaptureItemRequest {
        request_id: new_ulid().to_string(),
        text: text.to_owned(),
        created_via: CreatedVia::Cli.into(),
        ..Default::default()
    }
}

async fn capture(items: &mut ItemServiceClient<Channel>, text: &str) -> Item {
    items
        .capture_item(capture_req(text))
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap()
}

async fn next(stream: &mut Streaming<SubscribeResponse>) -> Message {
    timeout(WAIT, stream.message())
        .await
        .expect("stream timed out")
        .unwrap()
        .expect("stream ended")
        .message
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_reports_version_and_api_minor() {
    let d = start().await;
    let info = SystemServiceClient::new(d.channel().await)
        .get_server_info(GetServerInfoRequest {
            client_kind: ClientKind::Oneshot.into(),
            client_version: "test".into(),
            required_api_minor: 0,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.daemon_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.api_minor, todo_proto::API_MINOR);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_then_list() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "first").await;
    let b = capture(&mut items, "  second, verbatim  ").await;
    assert_eq!(a.title, "first");
    assert_eq!(b.title, "  second, verbatim  ");
    assert_eq!(a.created_via, CreatedVia::Cli as i32);

    let page = items
        .list_items(ListItemsRequest::default())
        .await
        .unwrap()
        .into_inner();
    let ids: Vec<_> = page.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, [a.id.as_str(), b.id.as_str()]);
    assert_eq!(page.revision, b.revision);
    assert!(page.next_page_token.is_empty());

    let status_filter = |status: Status| ListItemsRequest {
        filter: Some(Filter {
            all_of: vec![Clause {
                kind: Some(clause::Kind::Predicate(Predicate {
                    target: ItemField::Status.into(),
                    op: Operator::In.into(),
                    operands: vec![Operand {
                        kind: Some(operand::Kind::Value(FieldValue {
                            kind: Some(field_value::Kind::Status(status.into())),
                        })),
                    }],
                })),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let inbox = items
        .list_items(status_filter(Status::Inbox))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(inbox.items.len(), 2);
    let next = items
        .list_items(status_filter(Status::Next))
        .await
        .unwrap()
        .into_inner();
    assert!(next.items.is_empty());

    let sort = |keys: &[(ItemField, bool)]| ListItemsRequest {
        sort: keys
            .iter()
            .map(|&(k, descending)| SortKey {
                key: k.into(),
                descending,
            })
            .collect(),
        ..status_filter(Status::Inbox)
    };
    let sorted = items
        .list_items(sort(&[
            (ItemField::CreatedAt, false),
            (ItemField::Id, false),
        ]))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sorted.items.len(), 2);
    let status = items
        .list_items(sort(&[(ItemField::CreatedAt, true)]))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Unimplemented);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_replay_returns_the_original() {
    let d = start().await;
    let mut items = d.items().await;
    let req = capture_req("once");
    let first = items.capture_item(req.clone()).await.unwrap().into_inner();
    let again = items.capture_item(req).await.unwrap().into_inner();
    assert_eq!(first.item.unwrap().id, again.item.unwrap().id);
    assert_eq!(first.result, again.result);
    let page = items
        .list_items(ListItemsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page.items.len(), 1);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_requests_carry_bad_request_details() {
    let d = start().await;
    let mut items = d.items().await;
    for (req, field) in [
        (
            CaptureItemRequest {
                request_id: "nope".into(),
                ..capture_req("x")
            },
            "request_id",
        ),
        (capture_req("   "), "text"),
        (
            CaptureItemRequest {
                created_via: 0,
                ..capture_req("x")
            },
            "created_via",
        ),
    ] {
        let status = items.capture_item(req).await.unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument);
        let bad = status.get_details_bad_request().expect("BadRequest detail");
        assert_eq!(bad.field_violations[0].field, field);
    }
    let status = items
        .list_items(ListItemsRequest {
            view_id: Some("v".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Unimplemented);
    assert!(status.get_details_error_info().is_some());
    d.stop().await;
}

/// Collects `n` changes, checking the stream's ordering invariants.
async fn collect_changes(
    stream: &mut Streaming<SubscribeResponse>,
    n: usize,
) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    while out.len() < n {
        let m = match timeout(WAIT, stream.message()).await {
            Ok(m) => m.unwrap().expect("stream ended").message.unwrap(),
            Err(_) => panic!(
                "timed out after {} changes, last {:?}",
                out.len(),
                out.last()
            ),
        };
        match m {
            Message::Change(c) => {
                assert_eq!(c.topic, Topic::Item as i32);
                assert!(!c.deleted);
                let todo_proto::v1::change::Resource::Item(item) = c.resource.unwrap();
                assert_eq!(item.id, c.id);
                out.push((c.revision, c.id));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_replays_then_goes_live_without_loss_or_duplicates() {
    const BEFORE: usize = 20;
    const DURING: usize = 60;
    let d = start().await;
    let mut items = d.items().await;
    let mut expected = Vec::new();
    for i in 0..BEFORE {
        expected.push(capture(&mut items, &format!("before {i}")).await.id);
    }

    // Writers race the subscription's replay/live hand-over.
    let writers: Vec<_> = (0..4)
        .map(|w| {
            let mut items = items.clone();
            tokio::spawn(async move {
                let mut ids = Vec::new();
                for i in 0..DURING / 4 {
                    ids.push(capture(&mut items, &format!("w{w} {i}")).await.id);
                }
                ids
            })
        })
        .collect();
    let mut from_zero = d
        .subscribe(SubscribeRequest {
            since_revision: Some(0),
            topics: vec![],
        })
        .await;
    let mut from_now = d.subscribe(SubscribeRequest::default()).await;
    for w in writers {
        expected.extend(w.await.unwrap());
    }
    let total = BEFORE + DURING;

    // Replay, CaughtUp, then live: every revision exactly once, in order.
    let mut changes = Vec::new();
    let mut caught_up = None;
    while changes.len() < total {
        match next(&mut from_zero).await {
            Message::Change(c) => changes.push((c.revision, c.id)),
            Message::CaughtUp(c) => {
                assert!(caught_up.is_none(), "CaughtUp sent twice");
                assert_eq!(c.revision, changes.last().map_or(0, |(r, _)| *r));
                caught_up = Some(c.revision);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(caught_up.unwrap() >= BEFORE as u64);
    let revisions: Vec<u64> = changes.iter().map(|(r, _)| *r).collect();
    assert_eq!(revisions, (1..=total as u64).collect::<Vec<_>>());
    let seen: HashSet<_> = changes.into_iter().map(|(_, id)| id).collect();
    assert_eq!(seen, expected.iter().cloned().collect::<HashSet<_>>());

    // No replay: CaughtUp at the subscribe-time revision, then a gap-free tail.
    let Message::CaughtUp(start) = next(&mut from_now).await else {
        panic!("expected CaughtUp first");
    };
    let tail = collect_changes(&mut from_now, total - start.revision as usize).await;
    let tail: Vec<u64> = tail.into_iter().map(|(r, _)| r).collect();
    assert_eq!(
        tail,
        (start.revision + 1..=total as u64).collect::<Vec<_>>()
    );
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_from_a_revision_replays_only_later_changes() {
    let d = start().await;
    let mut items = d.items().await;
    let first = capture(&mut items, "a").await;
    let second = capture(&mut items, "b").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(first.revision),
            topics: vec![],
        })
        .await;
    assert_eq!(
        collect_changes(&mut stream, 1).await,
        [(second.revision, second.id)]
    );
    assert!(
        matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == second.revision)
    );
    let third = capture(&mut items, "c").await;
    assert_eq!(
        collect_changes(&mut stream, 1).await,
        [(third.revision, third.id)]
    );
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_ahead_of_the_store_requires_resync() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(1000),
            topics: vec![],
        })
        .await;
    assert!(matches!(
        next(&mut stream).await,
        Message::ResyncRequired(r) if r.current_revision == a.revision
    ));
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == a.revision));
    let b = capture(&mut items, "b").await;
    assert_eq!(collect_changes(&mut stream, 1).await, [(b.revision, b.id)]);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_filters_topics() {
    let d = start().await;
    let mut items = d.items().await;
    let mut people = d
        .subscribe(SubscribeRequest {
            since_revision: Some(0),
            topics: vec![Topic::Person.into()],
        })
        .await;
    let mut all = d
        .subscribe(SubscribeRequest {
            since_revision: Some(0),
            topics: vec![Topic::Item.into()],
        })
        .await;
    assert!(matches!(next(&mut people).await, Message::CaughtUp(_)));
    assert!(matches!(next(&mut all).await, Message::CaughtUp(_)));
    let a = capture(&mut items, "a").await;
    assert_eq!(collect_changes(&mut all, 1).await, [(a.revision, a.id)]);
    assert!(
        timeout(Duration::from_millis(200), people.message())
            .await
            .is_err()
    );

    let bad = WatchServiceClient::new(d.channel().await)
        .subscribe(SubscribeRequest {
            since_revision: None,
            topics: vec![0],
        })
        .await
        .unwrap_err();
    assert_eq!(bad.code(), Code::InvalidArgument);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn many_concurrent_writers_stream_in_order() {
    // More than the store's broadcast capacity, written before the client reads.
    const N: usize = 1600;
    let d = start().await;
    let mut stream = d.subscribe(SubscribeRequest::default()).await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == 0));
    let items = d.items().await;
    let writers: Vec<_> = (0..8)
        .map(|w| {
            let mut items = items.clone();
            tokio::spawn(async move {
                for i in 0..N / 8 {
                    capture(&mut items, &format!("{w}-{i}")).await;
                }
            })
        })
        .collect();
    for w in writers {
        w.await.unwrap();
    }
    let revisions: Vec<u64> = collect_changes(&mut stream, N)
        .await
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    assert_eq!(revisions, (1..=N as u64).collect::<Vec<_>>());
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_one_daemon_per_data_dir() {
    let d = start().await;
    let paths = d.paths.clone();
    assert!(matches!(
        Daemon::start(paths.clone()).await.unwrap(),
        Started::AlreadyRunning
    ));
    let pid: u32 = std::fs::read_to_string(&paths.lock)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid, std::process::id());
    let mode = |p: &std::path::Path| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    };
    assert_eq!(mode(&paths.socket), 0o600);
    assert_eq!(mode(&paths.runtime), 0o700);
    let home = d.stop().await;

    // The lock is released on shutdown, and a stale socket file is replaced.
    std::fs::write(&paths.socket, b"stale").unwrap();
    let d = start_in(home).await;
    let mut items = d.items().await;
    capture(&mut items, "after restart").await;
    let page = items
        .list_items(ListItemsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page.items.len(), 1);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_rpc_ends_streams_and_stops_the_server() {
    let d = start().await;
    let mut stream = d.subscribe(SubscribeRequest::default()).await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
    SystemServiceClient::new(d.channel().await)
        .shutdown(ShutdownRequest { restart: false })
        .await
        .unwrap();
    let end = timeout(WAIT, stream.message())
        .await
        .expect("stream should end");
    assert!(matches!(end, Ok(None) | Err(_)));
    timeout(WAIT, d.task).await.unwrap().unwrap().unwrap();
    assert!(!d.paths.socket.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_is_refused_while_other_interactive_clients_are_connected() {
    let d = start().await;
    let hello = |kind: ClientKind| GetServerInfoRequest {
        client_kind: kind.into(),
        ..Default::default()
    };
    let tui = d.channel().await;
    SystemServiceClient::new(tui.clone())
        .get_server_info(hello(ClientKind::Interactive))
        .await
        .unwrap();

    let mut me = SystemServiceClient::new(d.channel().await);
    me.get_server_info(hello(ClientKind::Interactive))
        .await
        .unwrap();
    let refused = me
        .shutdown(ShutdownRequest { restart: true })
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    let info = refused.get_details_error_info().unwrap();
    assert_eq!(info.reason, "INTERACTIVE_CLIENTS_CONNECTED");

    // Once the other session disconnects, the restart goes through.
    drop(tui);
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match me.shutdown(ShutdownRequest { restart: true }).await {
            Ok(_) => break,
            Err(s) if s.code() == Code::FailedPrecondition => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "disconnect never noticed"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(s) => panic!("{s}"),
        }
    }
    timeout(WAIT, d.task).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_dir_is_secured_by_the_daemon_and_checked_by_clients() {
    use std::os::unix::fs::PermissionsExt as _;
    use todo_daemon::client::{check_runtime_dir, dial};

    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    // Missing: no daemon yet, which clients treat as "spawn one".
    let err = check_runtime_dir(&paths.runtime).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

    // A pre-existing, loosely-permissioned dir of ours is tightened on start.
    std::fs::create_dir_all(&paths.runtime).unwrap();
    std::fs::set_permissions(&paths.runtime, std::fs::Permissions::from_mode(0o777)).unwrap();
    let err = check_runtime_dir(&paths.runtime).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    let d = start_in(home).await;
    check_runtime_dir(&d.paths.runtime).unwrap();
    dial(&d.paths.socket).await.unwrap();

    // Clients refuse a socket dir that others can write to, even if it answers.
    std::fs::set_permissions(&d.paths.runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = dial(&d.paths.socket).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        err.to_string().contains("accessible by other users"),
        "{err}"
    );
    std::fs::set_permissions(&d.paths.runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_symlinked_runtime_dir_is_refused() {
    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    let elsewhere = TempDir::new().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), &paths.runtime).unwrap();
    let err = todo_daemon::client::check_runtime_dir(&paths.runtime).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    let Err(err) = Daemon::start(paths.clone()).await else {
        panic!("daemon started in a symlinked runtime dir");
    };
    assert!(format!("{err:#}").contains("not a directory"), "{err:#}");
}

#[test]
fn the_tmp_fallback_parent_is_guarded_too() {
    use std::path::Path;
    use todo_daemon::client::guarded_dirs;

    let parent = format!("/tmp/todo-tui-{}", rustix::process::getuid().as_raw());
    let leaf = format!("{parent}/h-0123abcd");
    assert_eq!(
        guarded_dirs(Path::new(&leaf)),
        [Path::new(&leaf), Path::new(&parent)]
    );
    assert_eq!(guarded_dirs(Path::new(&parent)), [Path::new(&parent)]);
    assert_eq!(guarded_dirs(Path::new("/x/run")), [Path::new("/x/run")]);
}
