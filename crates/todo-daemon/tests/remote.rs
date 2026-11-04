//! The remote listener (ADR-0004), end to end: gRPC-Web over HTTP/1.1 on
//! loopback TCP, with the bearer token, next to the Unix socket.

use std::fs;
use std::net::SocketAddr;
use std::time::Duration;

use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use tempfile::TempDir;
use todo_core::new_ulid;
use todo_daemon::paths::{Env, Os, Paths};
use todo_daemon::{Daemon, ShutdownHandle, Started, remote};
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::system_service_client::SystemServiceClient;
use todo_proto::v1::watch_service_client::WatchServiceClient;
use todo_proto::v1::{
    CaptureItemRequest, ClientKind, CreatedVia, GetServerInfoRequest, ShutdownRequest,
    SubscribeRequest, Topic, subscribe_response::Message,
};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tonic::Code;
use tonic::body::Body;
use tonic::transport::Channel;
use tonic_web::{GrpcWebCall, GrpcWebClientLayer, GrpcWebClientService};

const WAIT: Duration = Duration::from_secs(10);

type Web = GrpcWebClientService<Client<HttpConnector, GrpcWebCall<Body>>>;

struct TestDaemon {
    _home: TempDir,
    paths: Paths,
    remote: Option<SocketAddr>,
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

/// A daemon whose `config.toml` is `config` (none when `None`).
async fn start(config: Option<&str>) -> TestDaemon {
    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    if let Some(config) = config {
        fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        fs::write(&paths.config, config).unwrap();
    }
    let Started::Running(daemon) = Daemon::start(paths.clone()).await.unwrap() else {
        panic!("lock unexpectedly held");
    };
    let remote = daemon.remote_addr();
    let shutdown = daemon.shutdown_handle();
    let task = tokio::spawn(daemon.serve());
    TestDaemon {
        _home: home,
        paths,
        remote,
        shutdown,
        task,
    }
}

const LOOPBACK: &str = "[remote]\nlisten = \"127.0.0.1:0\"\n";

impl TestDaemon {
    fn web(&self) -> Web {
        let client = Client::builder(TokioExecutor::new()).build_http::<GrpcWebCall<Body>>();
        tower::ServiceBuilder::new()
            .layer(GrpcWebClientLayer::new())
            .service(client)
    }

    fn origin(&self) -> tonic::codegen::http::Uri {
        format!("http://{}", self.remote.expect("remote listener"))
            .parse()
            .unwrap()
    }

    fn system(&self) -> SystemServiceClient<Web> {
        SystemServiceClient::with_origin(self.web(), self.origin())
    }

    fn items(&self) -> ItemServiceClient<Web> {
        ItemServiceClient::with_origin(self.web(), self.origin())
    }

    fn watch(&self) -> WatchServiceClient<Web> {
        WatchServiceClient::with_origin(self.web(), self.origin())
    }

    async fn local_items(&self) -> ItemServiceClient<Channel> {
        ItemServiceClient::new(
            todo_daemon::client::connect(&self.paths.socket)
                .await
                .unwrap(),
        )
    }

    fn pair(&self, name: &str) -> String {
        remote::pair(&self.paths.remote_tokens(), name, jiff::Timestamp::now())
            .unwrap()
            .1
    }

    async fn stop(self) {
        self.shutdown.trigger();
        timeout(WAIT, self.task).await.unwrap().unwrap().unwrap();
    }
}

fn authed<T>(message: T, token: &str) -> tonic::Request<T> {
    let mut req = tonic::Request::new(message);
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    req
}

fn hello() -> GetServerInfoRequest {
    GetServerInfoRequest {
        client_kind: ClientKind::Mobile.into(),
        client_version: "test".to_owned(),
        required_api_minor: 9,
    }
}

fn capture(text: &str, via: CreatedVia) -> CaptureItemRequest {
    CaptureItemRequest {
        request_id: new_ulid().to_string(),
        text: text.to_owned(),
        created_via: via.into(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_listener_without_remote_config() {
    let d = start(None).await;
    assert_eq!(d.remote, None);
    let d2 = start(Some("[other]\nx = 1\n")).await;
    assert_eq!(d2.remote, None);
    d.stop().await;
    d2.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_config_leaves_the_socket_serving() {
    for config in [
        "[remote]\nlisten = \"0.0.0.0:0\"\n",
        "[remote]\nlisten = \"nope\"\n",
        "not toml [",
    ] {
        let d = start(Some(config)).await;
        assert_eq!(d.remote, None, "{config}");
        let item = d
            .local_items()
            .await
            .capture_item(capture("still works", CreatedVia::Cli))
            .await
            .unwrap();
        assert!(item.into_inner().item.is_some());
        d.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_need_a_paired_token() {
    let d = start(Some(LOOPBACK)).await;
    let mut system = d.system();
    let err = system.get_server_info(hello()).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    let err = system
        .get_server_info(authed(hello(), "todo_nope"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");

    let token = d.pair("phone");
    let info = system
        .get_server_info(authed(hello(), &token))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.api_minor, todo_proto::API_MINOR);

    // Revoking takes effect on the next call, without a restart.
    let id = remote::list_devices(&d.paths.remote_tokens()).unwrap()[0]
        .id
        .clone();
    remote::revoke(&d.paths.remote_tokens(), &id).unwrap();
    let err = system
        .get_server_info(authed(hello(), &token))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_is_refused_remotely() {
    let d = start(Some(LOOPBACK)).await;
    let token = d.pair("phone");
    for restart in [false, true] {
        let err = d
            .system()
            .shutdown(authed(ShutdownRequest { restart }, &token))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    }
    // Still serving both.
    d.system()
        .get_server_info(authed(hello(), &token))
        .await
        .unwrap();
    d.local_items()
        .await
        .capture_item(capture("alive", CreatedVia::Cli))
        .await
        .unwrap();
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn captures_and_watches_over_grpc_web() {
    let d = start(Some(LOOPBACK)).await;
    let token = d.pair("phone");
    let mut stream = d
        .watch()
        .subscribe(authed(
            SubscribeRequest {
                since_revision: Some(0),
                topics: vec![Topic::Item.into()],
            },
            &token,
        ))
        .await
        .unwrap()
        .into_inner();

    // A phone capture is a MOBILE item, seen by the laptop's clients.
    let item = d
        .items()
        .capture_item(authed(
            capture("call dentist ^fri", CreatedVia::Mobile),
            &token,
        ))
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap();
    assert_eq!(item.title, "call dentist");
    assert_eq!(item.created_via, i32::from(CreatedVia::Mobile));

    // A laptop capture streams to the phone.
    let local = d
        .local_items()
        .await
        .capture_item(capture("from the laptop", CreatedVia::Cli))
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap();
    let mut seen = Vec::new();
    while !seen.contains(&local.id) {
        let msg = timeout(WAIT, stream.message())
            .await
            .expect("change within WAIT")
            .unwrap()
            .expect("stream open");
        if let Some(Message::Change(change)) = msg.message
            && let Some(todo_proto::v1::change::Resource::Item(i)) = change.resource
        {
            seen.push(i.id);
        }
    }
    assert!(seen.contains(&item.id));
    drop(stream);
    d.stop().await;
}
