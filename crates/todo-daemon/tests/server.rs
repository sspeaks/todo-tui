//! In-process daemon tests over a real Unix socket in a temp `TODO_TUI_HOME`.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::time::Duration;

use jiff::tz::TimeZone;
use prost_types::FieldMask;
use tempfile::TempDir;
use todo_core::new_ulid;
use todo_daemon::paths::{Env, Os, Paths};
use todo_daemon::spool::{self, SpoolEntry};
use todo_daemon::{Daemon, ShutdownHandle, Started};
use todo_proto::convert::{date_to_proto, timestamp_to_proto};
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::system_service_client::SystemServiceClient;
use todo_proto::v1::watch_service_client::WatchServiceClient;
use todo_proto::v1::{
    ArchiveItemRequest, ArchiveReason, BatchMutateRequest, CaptureItemRequest, CaptureTokenKind,
    Clause, ClientKind, CreatedVia, DeleteItemRequest, FieldValue, Filter, GetItemRequest,
    GetServerInfoRequest, Item, ItemField, ListItemsRequest, Mutation, Operand, Operator,
    ParseCaptureRequest, PersonRef, Predicate, Priority, SetStatusRequest, ShutdownRequest,
    SortKey, Status, SubscribeRequest, SubscribeResponse, Topic, UnarchiveItemRequest,
    UndeleteItemRequest, UpdateItemRequest, clause, field_value, mutation, operand, person_ref,
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

fn rid() -> String {
    new_ulid().to_string()
}

fn mask(paths: &[&str]) -> FieldMask {
    FieldMask {
        paths: paths.iter().map(|&p| p.to_owned()).collect(),
    }
}

fn titled(title: &str) -> Item {
    Item {
        title: title.to_owned(),
        ..Default::default()
    }
}

/// Sets the `paths` of `item` on the Item `id`.
fn update_req(id: &str, item: Item, paths: &[&str]) -> UpdateItemRequest {
    UpdateItemRequest {
        request_id: rid(),
        item: Some(Item {
            id: id.to_owned(),
            ..item
        }),
        update_mask: Some(mask(paths)),
        expected_revision: None,
    }
}

fn status_req(id: &str, status: Status) -> SetStatusRequest {
    SetStatusRequest {
        request_id: rid(),
        id: id.to_owned(),
        status: status.into(),
        expected_revision: None,
    }
}

fn archive_req(id: &str) -> ArchiveItemRequest {
    ArchiveItemRequest {
        request_id: rid(),
        id: id.to_owned(),
        expected_revision: None,
    }
}

fn unarchive_req(id: &str) -> UnarchiveItemRequest {
    UnarchiveItemRequest {
        request_id: rid(),
        id: id.to_owned(),
        expected_revision: None,
    }
}

fn delete_req(id: &str) -> DeleteItemRequest {
    DeleteItemRequest {
        request_id: rid(),
        id: id.to_owned(),
        expected_revision: None,
    }
}

fn undelete_req(id: &str) -> UndeleteItemRequest {
    UndeleteItemRequest {
        request_id: rid(),
        id: id.to_owned(),
        expected_revision: None,
    }
}

/// `kind` as a batched mutation, which has no request id of its own.
fn batched(mut kind: mutation::Kind) -> Mutation {
    use mutation::Kind;
    let request_id = match &mut kind {
        Kind::UpdateItem(r) => &mut r.request_id,
        Kind::SetStatus(r) => &mut r.request_id,
        Kind::ArchiveItem(r) => &mut r.request_id,
        Kind::UnarchiveItem(r) => &mut r.request_id,
        Kind::DeleteItem(r) => &mut r.request_id,
        Kind::UndeleteItem(r) => &mut r.request_id,
    };
    request_id.clear();
    Mutation { kind: Some(kind) }
}

fn batch_req(mutations: Vec<Mutation>) -> BatchMutateRequest {
    BatchMutateRequest {
        request_id: rid(),
        mutations,
    }
}

async fn get(items: &mut ItemServiceClient<Channel>, id: &str) -> Item {
    items
        .get_item(GetItemRequest { id: id.to_owned() })
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap()
}

async fn titles(items: &mut ItemServiceClient<Channel>) -> Vec<String> {
    let page = items
        .list_items(ListItemsRequest::default())
        .await
        .unwrap()
        .into_inner();
    page.items.into_iter().map(|i| i.title).collect()
}

/// The single BadRequest violation of an INVALID_ARGUMENT status, as
/// `(field, description)`; the message names the field once.
fn violation(status: &tonic::Status) -> (String, String) {
    assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
    let bad = status.get_details_bad_request().expect("BadRequest detail");
    let [v] = &bad.field_violations[..] else {
        panic!("expected one violation: {bad:?}");
    };
    assert_eq!(status.message(), format!("{}: {}", v.field, v.description));
    assert!(!v.description.starts_with(&v.field), "{status:?}");
    (v.field.clone(), v.description.clone())
}

/// The ErrorInfo metadata of a status, after checking its code and reason.
fn error_info(status: &tonic::Status, code: Code, reason: &str) -> HashMap<String, String> {
    assert_eq!(status.code(), code, "{status:?}");
    let info = status.get_details_error_info().expect("ErrorInfo detail");
    assert_eq!(info.reason, reason, "{status:?}");
    info.metadata
}

fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
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
    assert_eq!(info.api_minor, 1);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_then_list() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "first").await;
    let b = capture(&mut items, "  second,  spaced  ").await;
    assert_eq!(a.title, "first");
    assert_eq!(b.title, "second, spaced");
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
        assert_eq!(violation(&status).0, field);
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

#[tokio::test(flavor = "multi_thread")]
async fn capture_parses_tokens_and_returns_the_draft() {
    use CaptureTokenKind as K;
    let d = start().await;
    let mut items = d.items().await;
    let res = items
        .capture_item(capture_req(
            "call dentist ^2026-10-24 +2 #home ~dana >someday",
        ))
        .await
        .unwrap()
        .into_inner();
    let item = res.item.unwrap();
    let dana = PersonRef {
        r#ref: Some(person_ref::Ref::FreeText("dana".into())),
    };
    let due = date_to_proto(jiff::civil::date(2026, 10, 24));
    assert_eq!(item.title, "call dentist");
    assert_eq!(item.priority, Priority::High as i32);
    assert_eq!(item.due_date, Some(due));
    assert_eq!(item.labels, ["home"]);
    let waiting = item.waiting.as_ref().unwrap();
    assert_eq!(waiting.on, Some(dana.clone()));
    assert_eq!(
        waiting.since,
        Some(date_to_proto(jiff::Zoned::now().date()))
    );
    assert_eq!(item.status, Status::Someday as i32);
    assert_eq!(item.created_via, CreatedVia::Cli as i32);

    let draft = res.draft.unwrap();
    assert_eq!(draft.title, "call dentist");
    assert_eq!(draft.priority, Priority::High as i32);
    assert_eq!(draft.due_date, Some(due));
    assert_eq!(draft.labels, ["home"]);
    assert_eq!(draft.waiting_on, Some(dana));
    assert_eq!(draft.status, Status::Someday as i32);
    let tokens: Vec<_> = draft
        .tokens
        .iter()
        .map(|t| (t.start, t.end, t.kind, t.applied))
        .collect();
    assert_eq!(
        tokens,
        [
            (13, 24, K::DueDate as i32, true),
            (25, 27, K::Priority as i32, true),
            (28, 33, K::Label as i32, true),
            (34, 39, K::Waiting as i32, true),
            (40, 48, K::Status as i32, true),
        ]
    );
    assert!(draft.warnings.is_empty());

    // A token that does not parse stays in the title, with a warning.
    let res = items
        .capture_item(capture_req("fix +9 thing"))
        .await
        .unwrap()
        .into_inner();
    let item = res.item.unwrap();
    assert_eq!(item.title, "fix +9 thing");
    assert_eq!(item.priority, Priority::None as i32);
    let draft = res.draft.unwrap();
    assert!(!draft.tokens[0].applied);
    assert_eq!(draft.warnings, ["+9: priority must be +1 to +5"]);

    // Relative dates resolve against reference_time, the creation time.
    let reference: jiff::Timestamp = "2026-01-15T12:00:00Z".parse().unwrap();
    let req = CaptureItemRequest {
        reference_time: Some(timestamp_to_proto(reference)),
        ..capture_req("renew passport ^tomorrow")
    };
    let item = items
        .capture_item(req)
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap();
    let day = reference.to_zoned(TimeZone::system()).date();
    assert_eq!(item.due_date, Some(date_to_proto(day.tomorrow().unwrap())));
    assert_eq!(item.created_at, Some(timestamp_to_proto(reference)));
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_checks_created_via_and_reference_time() {
    let d = start().await;
    let mut items = d.items().await;
    for via in [CreatedVia::Tui, CreatedVia::Cli, CreatedVia::Mcp] {
        let req = CaptureItemRequest {
            created_via: via.into(),
            ..capture_req("ok")
        };
        let item = items
            .capture_item(req)
            .await
            .unwrap()
            .into_inner()
            .item
            .unwrap();
        assert_eq!(item.created_via, via as i32);
    }
    let reserved = [
        CreatedVia::Unspecified,
        CreatedVia::RemoteImport,
        CreatedVia::Restore,
        CreatedVia::Split,
        CreatedVia::Recurrence,
    ];
    for via in reserved.map(i32::from).into_iter().chain([99]) {
        let req = CaptureItemRequest {
            created_via: via,
            ..capture_req("x")
        };
        let status = items.capture_item(req).await.unwrap_err();
        assert_eq!(violation(&status).0, "created_via", "{via}");
    }

    // The store keeps i64 nanoseconds since the epoch.
    let ts = |seconds, nanos| prost_types::Timestamp { seconds, nanos };
    let outside = [
        ts(-9_223_372_037, 145_224_191),
        ts(9_223_372_036, 854_775_808),
        ts(253_402_300_799, 0),
        ts(0, -1),
    ];
    for t in outside {
        let req = CaptureItemRequest {
            reference_time: Some(t),
            ..capture_req("x")
        };
        let status = items.capture_item(req).await.unwrap_err();
        assert_eq!(violation(&status).0, "reference_time", "{t:?}");
        let req = ParseCaptureRequest {
            text: "x".into(),
            reference_time: Some(t),
        };
        let status = items.parse_capture(req).await.unwrap_err();
        assert_eq!(violation(&status).0, "reference_time", "{t:?}");
    }
    for t in [
        ts(-9_223_372_037, 145_224_192),
        ts(9_223_372_036, 854_775_807),
    ] {
        let req = CaptureItemRequest {
            reference_time: Some(t),
            ..capture_req("at the edge")
        };
        let item = items
            .capture_item(req)
            .await
            .unwrap()
            .into_inner()
            .item
            .unwrap();
        assert_eq!(item.created_at, Some(t));
    }

    let req = CaptureItemRequest {
        spool_id: Some("nope".into()),
        ..capture_req("x")
    };
    let status = items.capture_item(req).await.unwrap_err();
    assert_eq!(violation(&status).0, "spool_id");
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parse_capture_previews_without_writing() {
    let d = start().await;
    let mut items = d.items().await;
    let mut parse = async |text: &str, reference_time| {
        let req = ParseCaptureRequest {
            text: text.to_owned(),
            reference_time,
        };
        items
            .parse_capture(req)
            .await
            .unwrap()
            .into_inner()
            .draft
            .unwrap()
    };
    let draft = parse("buy milk #errands +9", None).await;
    assert_eq!(draft.title, "buy milk +9");
    assert_eq!(draft.labels, ["errands"]);
    assert_eq!(draft.priority, Priority::None as i32);
    assert_eq!(draft.status, Status::Inbox as i32);
    assert_eq!(draft.warnings, ["+9: priority must be +1 to +5"]);

    let reference: jiff::Timestamp = "2026-03-02T08:00:00Z".parse().unwrap();
    let draft = parse("pay rent ^tomorrow", Some(timestamp_to_proto(reference))).await;
    let day = reference.to_zoned(TimeZone::system()).date();
    assert_eq!(draft.due_date, Some(date_to_proto(day.tomorrow().unwrap())));

    // Blank text is an empty draft, not an error.
    let draft = parse("   ", None).await;
    assert_eq!(draft.title, "");
    assert!(draft.tokens.is_empty() && draft.warnings.is_empty());

    let page = items
        .list_items(ListItemsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert!(page.items.is_empty());
    assert_eq!(page.revision, 0);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn get_item_reads_items_and_tombstones() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let got = items
        .get_item(GetItemRequest { id: a.id.clone() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.item.as_ref(), Some(&a));
    assert_eq!(got.revision, a.revision);
    assert_eq!(got.resolved_from, None);

    let missing = rid();
    let status = items
        .get_item(GetItemRequest {
            id: missing.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
        missing
    );
    let status = items
        .get_item(GetItemRequest { id: "nope".into() })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "id");

    let deleted = items
        .delete_item(delete_req(&a.id))
        .await
        .unwrap()
        .into_inner();
    let got = items
        .get_item(GetItemRequest { id: a.id.clone() })
        .await
        .unwrap()
        .into_inner();
    let item = got.item.unwrap();
    assert!(item.deleted_at.is_some());
    assert_eq!(Some(&item), deleted.item.as_ref());
    assert_eq!(got.revision, deleted.result.unwrap().revision);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn update_item_writes_only_the_masked_fields() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "call dentist").await;
    let due = date_to_proto(jiff::civil::date(2026, 11, 2));
    let edit = Item {
        title: "  call the dentist ".into(),
        due_date: Some(due),
        description: "not in the mask".into(),
        ..Default::default()
    };
    let req = UpdateItemRequest {
        expected_revision: Some(a.revision),
        ..update_req(&a.id, edit, &["title", "due_date"])
    };
    let res = items.update_item(req).await.unwrap().into_inner();
    let item = res.item.unwrap();
    assert_eq!(item.title, "call the dentist");
    assert_eq!(item.due_date, Some(due));
    assert_eq!(item.description, "");
    assert_eq!(item.revision, a.revision + 1);
    assert_eq!(res.result.unwrap().revision, item.revision);
    assert_eq!(get(&mut items, &a.id).await, item);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn update_item_errors_name_the_request_field() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let with = |item: Item, paths: &[&str]| update_req(&a.id, item, paths);
    let long_path = "x".repeat(10_000);
    let cases: Vec<(UpdateItemRequest, &str, &str)> = vec![
        (
            UpdateItemRequest {
                request_id: "nope".into(),
                ..with(titled("b"), &["title"])
            },
            "request_id",
            "must be a ULID",
        ),
        (
            UpdateItemRequest {
                item: None,
                ..with(titled("b"), &["title"])
            },
            "item",
            "required field is missing",
        ),
        (
            update_req("nope", titled("b"), &["title"]),
            "item.id",
            "invalid ULID \"nope\"",
        ),
        (
            UpdateItemRequest {
                update_mask: None,
                ..with(titled("b"), &["title"])
            },
            "update_mask",
            "required field is missing",
        ),
        (with(titled("b"), &[]), "update_mask", "must not be empty"),
        (
            with(titled("b"), &["bogus"]),
            "update_mask",
            "unknown field path \"bogus\"",
        ),
        (
            with(titled("b"), &[&long_path]),
            "update_mask",
            "unknown field path \"xxx",
        ),
        (
            with(titled("b"), &["title", "title"]),
            "update_mask",
            "\"title\" repeats or overlaps an earlier path",
        ),
        (
            with(titled("b"), &["waiting", "waiting.on"]),
            "update_mask",
            "\"waiting.on\" repeats or overlaps an earlier path",
        ),
        (
            with(titled("  "), &["title"]),
            "item.title",
            "must not be empty",
        ),
        (
            with(titled("b"), &["created_at"]),
            "item.created_at",
            "is read-only",
        ),
        (
            with(titled("b"), &["status"]),
            "item.status",
            "changes only through SetStatus",
        ),
        (
            with(titled("b"), &["archived"]),
            "item.archived",
            "changes only through ArchiveItem and UnarchiveItem",
        ),
        (
            with(titled("b"), &["waiting.follow_up_on"]),
            "item.waiting.follow_up_on",
            "cannot be set while waiting is unset",
        ),
        (
            with(
                Item {
                    focus_week: Some("soon".into()),
                    ..Default::default()
                },
                &["focus_week"],
            ),
            "item.focus_week",
            "\"soon\" is not an ISO week",
        ),
        (
            with(
                Item {
                    local_priority_override: Some(Priority::None.into()),
                    ..Default::default()
                },
                &["local_priority_override"],
            ),
            "item.local_priority_override",
            "remove the override with cleared",
        ),
    ];
    for (req, field, description) in cases {
        let status = items.update_item(req).await.unwrap_err();
        let (got, got_description) = violation(&status);
        assert_eq!(got, field, "{status:?}");
        assert!(
            got_description.starts_with(description),
            "{field}: {got_description}"
        );
        // Echoed input is capped.
        assert!(got_description.chars().count() <= 201, "{field}");
        assert!(status.message().len() < 300, "{field}");
    }
    assert_eq!(get(&mut items, &a.id).await, a);

    let missing = rid();
    let status = items
        .update_item(update_req(&missing, titled("b"), &["title"]))
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
        missing
    );

    let stale = UpdateItemRequest {
        expected_revision: Some(a.revision + 7),
        ..with(titled("b"), &["title"])
    };
    let status = items.update_item(stale).await.unwrap_err();
    let meta = error_info(&status, Code::Aborted, "REVISION_MISMATCH");
    assert_eq!(meta["item_id"], a.id);
    assert_eq!(meta["expected_revision"], (a.revision + 7).to_string());
    assert_eq!(meta["actual_revision"], a.revision.to_string());

    items.delete_item(delete_req(&a.id)).await.unwrap();
    let status = items
        .update_item(with(titled("b"), &["title"]))
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
        a.id
    );
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn set_status_archive_and_delete() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let done = items
        .set_status(SetStatusRequest {
            expected_revision: Some(a.revision),
            ..status_req(&a.id, Status::Done)
        })
        .await
        .unwrap()
        .into_inner();
    let item = done.item.unwrap();
    assert_eq!(item.status, Status::Done as i32);
    assert_eq!(item.revision, done.result.unwrap().revision);
    for status in [0, 99] {
        let req = SetStatusRequest {
            status,
            ..status_req(&a.id, Status::Done)
        };
        let status = items.set_status(req).await.unwrap_err();
        assert_eq!(violation(&status).0, "status");
    }

    let archived = items
        .archive_item(archive_req(&a.id))
        .await
        .unwrap()
        .into_inner();
    let item = archived.item.clone().unwrap();
    assert_eq!(
        item.archived.as_ref().unwrap().reason,
        ArchiveReason::User as i32
    );
    // Archiving again changes nothing, so it allocates no revision.
    let again = items
        .archive_item(archive_req(&a.id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(again.item, archived.item);
    assert_eq!(
        again.result.unwrap().revision,
        archived.result.unwrap().revision
    );
    let unarchived = items
        .unarchive_item(unarchive_req(&a.id))
        .await
        .unwrap()
        .into_inner();
    let item = unarchived.item.unwrap();
    assert_eq!(item.archived, None);

    items
        .set_status(status_req(&a.id, Status::Next))
        .await
        .unwrap();
    assert_eq!(titles(&mut items).await, ["a"]);
    let deleted = items
        .delete_item(delete_req(&a.id))
        .await
        .unwrap()
        .into_inner();
    assert!(deleted.item.unwrap().deleted_at.is_some());
    assert!(titles(&mut items).await.is_empty());
    let with_deleted = ListItemsRequest {
        filter: Some(Filter {
            include_deleted: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let page = items.list_items(with_deleted).await.unwrap().into_inner();
    assert_eq!(page.items.len(), 1);
    for status in [
        items
            .set_status(status_req(&a.id, Status::Done))
            .await
            .unwrap_err(),
        items.archive_item(archive_req(&a.id)).await.unwrap_err(),
    ] {
        assert_eq!(
            error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
            a.id
        );
    }
    let undeleted = items
        .undelete_item(undelete_req(&a.id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(undeleted.item.unwrap().deleted_at, None);
    assert_eq!(titles(&mut items).await, ["a"]);

    let missing = rid();
    let status = items
        .set_status(status_req(&missing, Status::Done))
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
        missing
    );
    let bad = "nope";
    for status in [
        items
            .set_status(status_req(bad, Status::Done))
            .await
            .unwrap_err(),
        items.archive_item(archive_req(bad)).await.unwrap_err(),
        items.unarchive_item(unarchive_req(bad)).await.unwrap_err(),
        items.delete_item(delete_req(bad)).await.unwrap_err(),
        items.undelete_item(undelete_req(bad)).await.unwrap_err(),
    ] {
        assert_eq!(violation(&status).0, "id");
    }
    let status = items
        .archive_item(ArchiveItemRequest {
            request_id: String::new(),
            ..archive_req(&a.id)
        })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "request_id");
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_id_replays_its_own_request_only() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let req = SetStatusRequest {
        expected_revision: Some(a.revision),
        ..status_req(&a.id, Status::Done)
    };
    let first = items.set_status(req.clone()).await.unwrap().into_inner();
    items
        .set_status(status_req(&a.id, Status::Next))
        .await
        .unwrap();
    // The original result with the current state, though expected_revision
    // is stale by now.
    let replay = items.set_status(req.clone()).await.unwrap().into_inner();
    assert_eq!(replay.result, first.result);
    assert_eq!(replay.item.unwrap().status, Status::Next as i32);

    let reused = [
        items
            .archive_item(ArchiveItemRequest {
                request_id: req.request_id.clone(),
                ..archive_req(&a.id)
            })
            .await
            .unwrap_err(),
        items
            .capture_item(CaptureItemRequest {
                request_id: req.request_id.clone(),
                ..capture_req("b")
            })
            .await
            .unwrap_err(),
        items
            .batch_mutate(BatchMutateRequest {
                request_id: req.request_id.clone(),
                mutations: vec![batched(mutation::Kind::SetStatus(req.clone()))],
            })
            .await
            .unwrap_err(),
    ];
    for status in reused {
        assert_eq!(
            violation(&status),
            (
                "request_id".to_owned(),
                "was already used for a different request".to_owned()
            )
        );
    }
    assert_eq!(get(&mut items, &a.id).await.archived, None);
    assert_eq!(titles(&mut items).await, ["a"]);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_mutate_applies_all_or_nothing() {
    use mutation::Kind;
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let b = capture(&mut items, "b").await;
    let req = batch_req(vec![
        batched(Kind::SetStatus(status_req(&a.id, Status::Done))),
        batched(Kind::UpdateItem(update_req(
            &b.id,
            titled("b2"),
            &["title"],
        ))),
        batched(Kind::ArchiveItem(archive_req(&a.id))),
    ]);
    let res = items.batch_mutate(req.clone()).await.unwrap().into_inner();
    assert_eq!(res.items.len(), 3);
    assert_eq!(res.items[0], res.items[2]);
    assert_eq!(res.items[0].status, Status::Done as i32);
    assert!(res.items[0].archived.is_some());
    assert_eq!(res.items[1].title, "b2");
    // One revision per touched Item, in first-touch order.
    assert_eq!(res.items[0].revision, b.revision + 1);
    assert_eq!(res.items[1].revision, b.revision + 2);
    let result = res.result.unwrap();
    assert_eq!(result.revision, b.revision + 2);
    let replay = items.batch_mutate(req).await.unwrap().into_inner();
    assert_eq!(replay.result, Some(result));

    // A bad mutation rejects the whole batch.
    let req = batch_req(vec![
        batched(Kind::UpdateItem(update_req(
            &a.id,
            titled("never"),
            &["title"],
        ))),
        batched(Kind::SetStatus(status_req(&b.id, Status::Someday))),
        batched(Kind::UpdateItem(update_req(&b.id, titled(" "), &["title"]))),
    ]);
    let status = items.batch_mutate(req).await.unwrap_err();
    assert_eq!(
        violation(&status),
        (
            "mutations[2].update_item.item.title".to_owned(),
            "must not be empty".to_owned()
        )
    );
    assert_eq!(get(&mut items, &a.id).await.title, "a");
    assert_eq!(get(&mut items, &b.id).await.status, Status::Inbox as i32);

    let done = || batched(Kind::SetStatus(status_req(&a.id, Status::Done)));
    let cases: Vec<(Vec<Mutation>, &str)> = vec![
        (vec![], "mutations"),
        (vec![done(); 1001], "mutations"),
        (
            vec![batched(Kind::SetStatus(status_req(
                &a.id,
                Status::Unspecified,
            )))],
            "mutations[0].set_status.status",
        ),
        (
            vec![
                done(),
                Mutation {
                    kind: Some(Kind::SetStatus(status_req(&b.id, Status::Next))),
                },
            ],
            "mutations[1].set_status.request_id",
        ),
        (vec![done(), Mutation { kind: None }], "mutations[1].kind"),
        (
            vec![batched(Kind::UpdateItem(update_req(
                &a.id,
                titled("x"),
                &["created_at"],
            )))],
            "mutations[0].update_item.item.created_at",
        ),
        (
            vec![batched(Kind::DeleteItem(delete_req("nope")))],
            "mutations[0].delete_item.id",
        ),
    ];
    for (mutations, field) in cases {
        let status = items.batch_mutate(batch_req(mutations)).await.unwrap_err();
        assert_eq!(violation(&status).0, field);
    }
    let req = BatchMutateRequest {
        request_id: "nope".into(),
        mutations: vec![done()],
    };
    let status = items.batch_mutate(req).await.unwrap_err();
    assert_eq!(violation(&status).0, "request_id");
    let res = items
        .batch_mutate(batch_req(vec![done(); 1000]))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.items.len(), 1000);

    let stale = SetStatusRequest {
        expected_revision: Some(1),
        ..status_req(&b.id, Status::Next)
    };
    let req = batch_req(vec![done(), batched(Kind::SetStatus(stale))]);
    let status = items.batch_mutate(req).await.unwrap_err();
    let meta = error_info(&status, Code::Aborted, "REVISION_MISMATCH");
    assert_eq!(meta["item_id"], b.id);
    assert_eq!(meta["expected_revision"], "1");

    items.delete_item(delete_req(&b.id)).await.unwrap();
    let req = batch_req(vec![
        batched(Kind::UnarchiveItem(unarchive_req(&a.id))),
        batched(Kind::SetStatus(status_req(&b.id, Status::Next))),
    ]);
    let status = items.batch_mutate(req).await.unwrap_err();
    assert_eq!(
        error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
        b.id
    );
    assert!(get(&mut items, &a.id).await.archived.is_some());
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_sees_updates_and_soft_deletes() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(a.revision),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == a.revision));

    items
        .update_item(update_req(&a.id, titled("a2"), &["title"]))
        .await
        .unwrap();
    items.delete_item(delete_req(&a.id)).await.unwrap();
    items.undelete_item(undelete_req(&a.id)).await.unwrap();
    let mut seen = Vec::new();
    for _ in 0..3 {
        let Message::Change(c) = next(&mut stream).await else {
            panic!("expected a change");
        };
        // A soft delete is an ordinary change: the tombstone is sent.
        assert!(!c.deleted);
        assert_eq!(c.id, a.id);
        let todo_proto::v1::change::Resource::Item(item) = c.resource.unwrap();
        seen.push((c.revision, item.title, item.deleted_at.is_some()));
    }
    let r = a.revision;
    assert_eq!(
        seen,
        [
            (r + 1, "a2".to_owned(), false),
            (r + 2, "a2".to_owned(), true),
            (r + 3, "a2".to_owned(), false),
        ]
    );
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

/// Waits until the daemon has imported and truncated the spool.
async fn wait_for_spool_import(paths: &Paths) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while std::fs::metadata(paths.spool()).unwrap().len() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "spool never imported"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn spool_entry(spool_id: &str, captured_at: &str, text: &str) -> SpoolEntry {
    SpoolEntry {
        spool_id: spool_id.to_owned(),
        captured_at: captured_at.to_owned(),
        text: text.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn spooled_captures_are_imported_at_start() {
    // A first run records request ids that spool entries then reuse.
    let d = start().await;
    let paths = d.paths.clone();
    let mut items = d.items().await;
    let delivered = capture_req("delivered before");
    let delivered_item = items
        .capture_item(delivered.clone())
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap();
    let next = status_req(&delivered_item.id, Status::Next);
    items.set_status(next.clone()).await.unwrap();
    let home = d.stop().await;

    let at = "2026-01-15T10:00:00-05:00";
    // Late on the 16th in +09:00, so "tomorrow" is the 17th.
    let passport = spool_entry(
        &rid(),
        "2026-01-16T00:30:00+09:00",
        "renew passport ^tomorrow #admin",
    );
    spool::append(&paths, &passport).unwrap();
    // The CLI spooled a capture whose RPC did reach the daemon.
    spool::append(
        &paths,
        &spool_entry(&delivered.request_id, at, "delivered before"),
    )
    .unwrap();
    spool::append(&paths, &passport).unwrap();
    let json = |e: SpoolEntry| serde_json::to_string(&e).unwrap();
    let malformed = [
        "not json".to_owned(),
        json(spool_entry("nope", at, "bad id")),
        json(spool_entry(&rid(), "2026-01-15 10:00", "bad time")),
        json(spool_entry(&rid(), at, "  ")),
        // Valid, but the store refuses it: the id names a SetStatus.
        json(spool_entry(&next.request_id, at, "reused request id")),
        // A torn last line, without its newline.
        r#"{"spool_id":"01"#.to_owned(),
    ];
    let (torn, whole) = malformed.split_last().unwrap();
    let raw = format!("{}\n\n{torn}", whole.join("\n"));
    std::fs::OpenOptions::new()
        .append(true)
        .open(paths.spool())
        .unwrap()
        .write_all(raw.as_bytes())
        .unwrap();
    assert_eq!(mode(&paths.spool()), 0o600);

    let d = start_in(home).await;
    wait_for_spool_import(&paths).await;
    let mut items = d.items().await;
    let page = items
        .list_items(ListItemsRequest::default())
        .await
        .unwrap()
        .into_inner();
    let titles: Vec<_> = page.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["renew passport", "delivered before"]);
    let item = &page.items[0];
    assert_eq!(item.created_via, CreatedVia::Cli as i32);
    let captured: jiff::Timestamp = "2026-01-15T15:30:00Z".parse().unwrap();
    assert_eq!(item.created_at, Some(timestamp_to_proto(captured)));
    assert_eq!(
        item.due_date,
        Some(date_to_proto(jiff::civil::date(2026, 1, 17)))
    );
    assert_eq!(item.labels, ["admin"]);

    let rejected = std::fs::read_to_string(paths.spool_rejected()).unwrap();
    assert_eq!(rejected, malformed.join("\n") + "\n");
    assert_eq!(mode(&paths.spool_rejected()), 0o600);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spool_entry_is_imported_once_across_restarts() {
    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    let entry = SpoolEntry::now(new_ulid(), "only once ^tomorrow");
    spool::append(&paths, &entry).unwrap();
    assert_eq!(mode(&paths.data), 0o700);
    assert_eq!(mode(&paths.spool()), 0o600);

    let d = start_in(home).await;
    wait_for_spool_import(&paths).await;
    let mut items = d.items().await;
    assert_eq!(titles(&mut items).await, ["only once"]);
    let home = d.stop().await;

    // As if the daemon had stopped before truncating the spool.
    spool::append(&paths, &entry).unwrap();
    let d = start_in(home).await;
    wait_for_spool_import(&paths).await;
    let mut items = d.items().await;
    assert_eq!(titles(&mut items).await, ["only once"]);
    assert!(!paths.spool_rejected().exists());
    d.stop().await;
}
