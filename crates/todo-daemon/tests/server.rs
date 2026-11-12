//! In-process daemon tests over a real Unix socket in a temp `TODO_TUI_HOME`.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::time::Duration;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use prost_types::FieldMask;
use tempfile::TempDir;
use todo_core::{MAX_CAPTURE_BYTES, new_ulid};
use todo_daemon::paths::{Env, Os, Paths};
use todo_daemon::spool::{self, SpoolEntry};
use todo_daemon::{Daemon, DaemonOptions, ShutdownHandle, Started};
use todo_proto::convert::{date_to_proto, timestamp_to_proto};
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::system_service_client::SystemServiceClient;
use todo_proto::v1::view_service_client::ViewServiceClient;
use todo_proto::v1::watch_service_client::WatchServiceClient;
use todo_proto::v1::{
    AddRelationRequest, ArchiveItemRequest, ArchiveReason, BackupRequest, BatchMutateRequest,
    BuildMeetingLinkRequest, CaptureItemRequest, CaptureTokenKind, Clause, ClauseGroup, ClientKind,
    ConfigSnapshot, CreateViewRequest, CreatedVia, DeleteItemRequest, DeleteViewRequest,
    ExportRequest, FieldValue, Filter, GetActivityReportRequest, GetConfigRequest, GetItemRequest,
    GetServerInfoRequest, GetStatusCountsRequest, GetUndoStateRequest, ImportRequest, Item,
    ItemField, ListItemsRequest, ListViewsRequest, MoveViewRequest, Mutation, MutationResult,
    OpKind, Operand, Operator, ParseCaptureRequest, ParseDateRequest, ParseQueryRequest,
    ParseRecurrenceRequest, PersonRef, Predicate, Priority, RecurrenceRule, RedoRequest,
    RedoResponse, Relation, RelationKind, ReloadConfigRequest, RemoveRelationRequest,
    SetRecurrenceRequest, SetStatusRequest, ShutdownRequest, SortKey, Status, SubscribeRequest,
    SubscribeResponse, Topic, UnarchiveItemRequest, UndeleteItemRequest, UndoRequest, UndoResponse,
    UndoSkip, UndoSkipReason, UpdateItemRequest, UpdateViewRequest, View, Weekday, clause,
    field_value, mutation, operand, person_ref, recurrence_rule, subscribe_response::Message,
};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tonic::Code;
use tonic::codec::Streaming;
use tonic::transport::Channel;
use tonic_types::StatusExt;

const WAIT: Duration = Duration::from_secs(10);

/// A new store's revision: one per default view, seeded when it opens.
const SEEDED: u64 = 6;

/// A new daemon's revision: the seeded views, then the config change it
/// records at start.
const STARTED: u64 = SEEDED + 1;

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
        confirm_not_mine: false,
    }
}

fn status_req(id: &str, status: Status) -> SetStatusRequest {
    SetStatusRequest {
        request_id: rid(),
        id: id.to_owned(),
        status: status.into(),
        expected_revision: None,
        confirm_not_mine: false,
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
        Kind::SetRecurrence(r) => &mut r.request_id,
    };
    request_id.clear();
    Mutation { kind: Some(kind) }
}

fn batch_req(mutations: Vec<Mutation>) -> BatchMutateRequest {
    BatchMutateRequest {
        request_id: rid(),
        mutations,
        confirm_not_mine: false,
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
    assert_eq!(info.api_minor, 26);
    d.stop().await;
}

fn daily() -> RecurrenceRule {
    RecurrenceRule {
        kind: Some(recurrence_rule::Kind::Daily(recurrence_rule::Daily {})),
    }
}

fn weekly(days: &[Weekday]) -> RecurrenceRule {
    let days = days.iter().map(|&d| d as i32).collect();
    RecurrenceRule {
        kind: Some(recurrence_rule::Kind::Weekly(recurrence_rule::Weekly {
            days,
        })),
    }
}

fn monthly(day: u32) -> RecurrenceRule {
    RecurrenceRule {
        kind: Some(recurrence_rule::Kind::Monthly(recurrence_rule::Monthly {
            day,
        })),
    }
}

fn after_completion(days: u32) -> RecurrenceRule {
    RecurrenceRule {
        kind: Some(recurrence_rule::Kind::AfterCompletion(
            recurrence_rule::AfterCompletion { days },
        )),
    }
}

fn recurrence_req(id: &str, rule: Option<RecurrenceRule>) -> SetRecurrenceRequest {
    SetRecurrenceRequest {
        request_id: rid(),
        id: id.to_owned(),
        rule,
        expected_revision: None,
        confirm_not_mine: false,
    }
}

fn today() -> jiff::civil::Date {
    jiff::Zoned::now().date()
}

/// Waits out local midnight when it is under 30 s away, so that a test
/// counting dates from `today()` sees the day the daemon's own clock does
/// on every request it makes.
async fn away_from_midnight() {
    let now = jiff::Zoned::now();
    let midnight = now
        .date()
        .tomorrow()
        .unwrap()
        .to_zoned(now.time_zone().clone())
        .unwrap();
    let left = now.duration_until(&midnight);
    if left < SignedDuration::from_secs(30) {
        tokio::time::sleep((left + SignedDuration::from_millis(50)).unsigned_abs()).await;
    }
}

fn day_after(date: jiff::civil::Date, days: i64) -> jiff::civil::Date {
    date.checked_add(jiff::Span::new().days(days)).unwrap()
}

/// `Mon Nov 2` as an op summary names the next instance's date.
fn summary_day(date: jiff::civil::Date) -> String {
    if date.year() == today().year() {
        date.strftime("%a %b %-d").to_string()
    } else {
        date.strftime("%a %b %-d %Y").to_string()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn set_recurrence_sets_replaces_and_clears_a_rule() {
    away_from_midnight().await;
    let d = start().await;
    let mut items = d.items().await;
    let x = capture(&mut items, "water plants").await;
    let set = SetRecurrenceRequest {
        expected_revision: Some(x.revision),
        ..recurrence_req(&x.id, Some(daily()))
    };
    let res = items
        .set_recurrence(set.clone())
        .await
        .unwrap()
        .into_inner();
    let item = res.item.unwrap();
    let result = res.result.unwrap();
    assert_eq!(item.revision, result.revision);
    let recurrence = item.recurrence.clone().unwrap();
    assert_eq!(recurrence.rule, Some(daily()));
    assert!(!recurrence.series_id.is_empty());
    // Without a due date the first occurrence is today, which becomes it.
    let on_today = Some(date_to_proto(today()));
    assert_eq!(recurrence.occurrence_date, on_today);
    assert_eq!(item.due_date, on_today);
    let (undo_top, _) = undo_state(&mut items).await;
    assert_eq!(undo_top, Some(result.op_id.clone()));

    // A replay returns the original op and writes nothing, though its
    // expected_revision is stale by now.
    let replay = items
        .set_recurrence(set.clone())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(replay.result, Some(result.clone()));
    assert_eq!(replay.item, Some(item.clone()));
    let other = capture(&mut items, "other").await;
    let reused = SetRecurrenceRequest {
        id: other.id.clone(),
        ..set.clone()
    };
    let status = items.set_recurrence(reused).await.unwrap_err();
    assert_eq!(violation(&status).0, "request_id");

    // A new rule keeps the series and the due date, and moves the slot.
    use Weekday::{Monday, Thursday};
    let res = items
        .set_recurrence(recurrence_req(&x.id, Some(weekly(&[Thursday, Monday]))))
        .await
        .unwrap()
        .into_inner();
    let item = res.item.unwrap();
    let changed = item.recurrence.clone().unwrap();
    assert_eq!(changed.rule, Some(weekly(&[Monday, Thursday])));
    assert_eq!(changed.series_id, recurrence.series_id);
    let slot = (0..7)
        .map(|n| day_after(today(), n))
        .find(|d| {
            use jiff::civil::Weekday as W;
            matches!(d.weekday(), W::Monday | W::Thursday)
        })
        .unwrap();
    assert_eq!(changed.occurrence_date, Some(date_to_proto(slot)));
    assert_eq!(item.due_date, on_today);

    // A stale expected_revision is refused.
    let stale = SetRecurrenceRequest {
        expected_revision: Some(x.revision),
        ..recurrence_req(&x.id, None)
    };
    let status = items.set_recurrence(stale).await.unwrap_err();
    error_info(&status, Code::Aborted, "REVISION_MISMATCH");

    // No rule clears it; the Item stays, due as it was.
    let res = items
        .set_recurrence(recurrence_req(&x.id, None))
        .await
        .unwrap()
        .into_inner();
    let item = res.item.unwrap();
    assert_eq!(item.recurrence, None);
    assert_eq!(item.due_date, on_today);
    // And an undo brings the rule back.
    let undone = undo(&mut items).await;
    assert_eq!(undone.items[0].recurrence, Some(changed));
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn set_recurrence_errors_name_the_request_field() {
    let d = start().await;
    let mut items = d.items().await;
    let x = capture(&mut items, "x").await;
    let bad = |rule: RecurrenceRule| recurrence_req(&x.id, Some(rule));
    let rows = [
        (bad(RecurrenceRule { kind: None }), "rule.kind"),
        (bad(weekly(&[])), "rule.weekly.days"),
        (bad(weekly(&[Weekday::Unspecified])), "rule.weekly.days"),
        (
            bad(RecurrenceRule {
                kind: Some(recurrence_rule::Kind::Weekly(recurrence_rule::Weekly {
                    days: vec![99],
                })),
            }),
            "rule.weekly.days",
        ),
        (bad(monthly(0)), "rule.monthly.day"),
        (bad(monthly(32)), "rule.monthly.day"),
        (bad(after_completion(0)), "rule.after_completion.days"),
        (bad(after_completion(3651)), "rule.after_completion.days"),
        (recurrence_req("nope", Some(daily())), "id"),
        (
            SetRecurrenceRequest {
                request_id: "nope".into(),
                ..recurrence_req(&x.id, Some(daily()))
            },
            "request_id",
        ),
    ];
    for (req, field) in rows {
        let status = items.set_recurrence(req.clone()).await.unwrap_err();
        assert_eq!(violation(&status).0, field, "{req:?}");
    }
    let status = items.set_recurrence(bad(monthly(32))).await.unwrap_err();
    assert_eq!(violation(&status).1, "the day of the month must be 1 to 31");
    // In a batch, under the mutation's oneof case.
    let batch = batch_req(vec![batched(mutation::Kind::SetRecurrence(bad(weekly(
        &[],
    ))))]);
    let status = items.batch_mutate(batch).await.unwrap_err();
    assert_eq!(
        violation(&status).0,
        "mutations[0].set_recurrence.rule.weekly.days"
    );

    let missing = new_ulid().to_string();
    let status = items
        .set_recurrence(recurrence_req(&missing, Some(daily())))
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
        missing
    );
    let gone = capture(&mut items, "gone").await;
    items.delete_item(delete_req(&gone.id)).await.unwrap();
    let status = items
        .set_recurrence(recurrence_req(&gone.id, Some(daily())))
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
        gone.id
    );

    // A done or cancelled Item takes no new rule, but may lose its rule.
    for closed in [Status::Done, Status::Cancelled] {
        let y = capture(&mut items, "y").await;
        items.set_status(status_req(&y.id, closed)).await.unwrap();
        let status = items
            .set_recurrence(recurrence_req(&y.id, Some(daily())))
            .await
            .unwrap_err();
        let meta = error_info(&status, Code::FailedPrecondition, "ITEM_CLOSED");
        assert_eq!(meta["item_id"], y.id);
        assert_eq!(
            status.message(),
            format!("item {} is done or cancelled: reopen it first", y.id)
        );
        let batch = batch_req(vec![batched(mutation::Kind::SetRecurrence(
            recurrence_req(&y.id, Some(daily())),
        ))]);
        let status = items.batch_mutate(batch).await.unwrap_err();
        error_info(&status, Code::FailedPrecondition, "ITEM_CLOSED");
        assert_eq!(
            status.message(),
            format!(
                "mutations[0].set_recurrence.id: item {} is done or cancelled: reopen it first",
                y.id
            )
        );
    }
    let z = capture(&mut items, "z @daily").await;
    let done = items
        .set_status(status_req(&z.id, Status::Done))
        .await
        .unwrap()
        .into_inner();
    assert!(done.generated.is_some());
    let cleared = items
        .set_recurrence(recurrence_req(&z.id, None))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(cleared.item.unwrap().recurrence, None);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parse_recurrence_resolves_the_rule_grammar() {
    let d = start().await;
    let mut items = d.items().await;
    // A Monday.
    let day = jiff::civil::date(2030, 6, 3);
    let noon = day.at(12, 0, 0, 0).to_zoned(TimeZone::system()).unwrap();
    let parse = async |items: &mut ItemServiceClient<Channel>,
                       text: &str,
                       anchor: Option<jiff::civil::Date>| {
        let req = ParseRecurrenceRequest {
            text: text.to_owned(),
            reference_time: Some(timestamp_to_proto(noon.timestamp())),
            anchor: anchor.map(date_to_proto),
        };
        let res = items.parse_recurrence(req).await.unwrap().into_inner();
        (res.rule, res.first, res.warning)
    };
    let on = |rule: RecurrenceRule, first: Option<jiff::civil::Date>| {
        (Some(rule), first.map(date_to_proto), String::new())
    };
    let not = |warning: &str| (None, None, warning.to_owned());
    use Weekday::{Friday, Monday, Thursday};
    let wednesday = jiff::civil::date(2030, 6, 5);
    let friday = jiff::civil::date(2030, 6, 7);
    let rows = [
        ("daily", None, on(daily(), Some(day))),
        ("@daily", Some(friday), on(daily(), Some(friday))),
        ("  @DAILY ", None, on(daily(), Some(day))),
        // The first slot on or after the anchor, else today.
        (
            "@mon,thu",
            Some(wednesday),
            on(weekly(&[Monday, Thursday]), Some(day_after(wednesday, 1))),
        ),
        ("thu,mon", None, on(weekly(&[Monday, Thursday]), Some(day))),
        // weekly and monthly take the anchor's weekday and day.
        ("weekly", Some(friday), on(weekly(&[Friday]), Some(friday))),
        ("weekly", None, on(weekly(&[Monday]), Some(day))),
        (
            "monthly",
            Some(jiff::civil::date(2030, 1, 31)),
            on(monthly(31), Some(jiff::civil::date(2030, 1, 31))),
        ),
        ("monthly", None, on(monthly(3), Some(day))),
        (
            "15th",
            None,
            on(monthly(15), Some(jiff::civil::date(2030, 6, 15))),
        ),
        (
            "1st",
            None,
            on(monthly(1), Some(jiff::civil::date(2030, 7, 1))),
        ),
        // After completion: the first is the anchor if any.
        ("3d", None, on(after_completion(3), None)),
        ("@2w", Some(friday), on(after_completion(14), Some(friday))),
        // Warnings, worded as ParseCapture words an @ token.
        ("home", None, not("home: not a repeat rule")),
        ("@home", Some(friday), not("@home: not a repeat rule")),
        ("22th", None, not("22th: write 22nd")),
        (
            "0d",
            None,
            not("0d: after completion must be 1 to 3650 days"),
        ),
        ("mon,xyz", None, not("mon,xyz: xyz is not a weekday")),
        ("", None, not("empty repeat rule")),
        ("@", None, not("@: empty repeat rule")),
    ];
    for (text, anchor, want) in rows {
        assert_eq!(parse(&mut items, text, anchor).await, want, "{text:?}");
    }
    // The capture grammar says the same of the token.
    let draft = items
        .parse_capture(ParseCaptureRequest {
            text: "x @home".into(),
            reference_time: None,
        })
        .await
        .unwrap()
        .into_inner()
        .draft
        .unwrap();
    assert_eq!(draft.warnings, ["@home: not a repeat rule"]);

    let bad_anchor = ParseRecurrenceRequest {
        text: "daily".into(),
        reference_time: None,
        anchor: Some(todo_proto::google::r#type::Date {
            year: 2030,
            month: 2,
            day: 30,
        }),
    };
    let status = items.parse_recurrence(bad_anchor).await.unwrap_err();
    assert_eq!(violation(&status).0, "anchor");
    let bad_time = ParseRecurrenceRequest {
        text: "daily".into(),
        reference_time: Some(prost_types::Timestamp {
            seconds: i64::MAX / 2,
            nanos: 0,
        }),
        anchor: None,
    };
    let status = items.parse_recurrence(bad_time).await.unwrap_err();
    assert_eq!(violation(&status).0, "reference_time");
    let long = ParseRecurrenceRequest {
        text: "x".repeat(MAX_CAPTURE_BYTES + 1),
        ..Default::default()
    };
    let status = items.parse_recurrence(long).await.unwrap_err();
    assert_eq!(violation(&status).0, "text");
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_capture_with_a_repeat_rule_starts_a_series() {
    away_from_midnight().await;
    let d = start().await;
    let mut items = d.items().await;
    let res = items
        .capture_item(capture_req("feed cat @daily"))
        .await
        .unwrap()
        .into_inner();
    let item = res.item.unwrap();
    let draft = res.draft.unwrap();
    assert_eq!(item.title, "feed cat");
    assert_eq!(draft.recurrence, Some(daily()));
    let on_today = Some(date_to_proto(today()));
    assert_eq!(draft.recurrence_first, on_today);
    assert_eq!(draft.tokens[0].kind, CaptureTokenKind::Recurrence as i32);
    let recurrence = item.recurrence.unwrap();
    assert_eq!(recurrence.rule, Some(daily()));
    assert!(!recurrence.series_id.is_empty());
    assert_eq!(recurrence.occurrence_date, on_today);
    assert_eq!(item.due_date, on_today);

    // A ^ date is kept and anchors the rule, in either order.
    let friday = jiff::civil::date(2031, 1, 3);
    for text in ["pay @weekly ^2031-01-03", "pay ^2031-01-03 @weekly"] {
        let item = capture(&mut items, text).await;
        let recurrence = item.recurrence.unwrap();
        assert_eq!(recurrence.rule, Some(weekly(&[Weekday::Friday])), "{text}");
        assert_eq!(recurrence.occurrence_date, Some(date_to_proto(friday)));
        assert_eq!(item.due_date, Some(date_to_proto(friday)));
    }
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn completing_an_instance_returns_and_streams_the_next() {
    away_from_midnight().await;
    let d = start().await;
    let mut items = d.items().await;
    let x = capture(&mut items, "water plants @daily").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(x.revision),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));

    let req = status_req(&x.id, Status::Done);
    let res = items.set_status(req.clone()).await.unwrap().into_inner();
    let done = res.item.unwrap();
    let result = res.result.unwrap();
    let generated = res.generated.unwrap();
    let tomorrow = Some(date_to_proto(day_after(today(), 1)));
    assert_eq!(done.status, Status::Done as i32);
    assert_ne!(generated.id, x.id);
    assert_eq!(generated.title, "water plants");
    assert_eq!(generated.created_via, CreatedVia::Recurrence as i32);
    assert_eq!(generated.status, Status::Next as i32);
    assert_eq!(generated.due_date, tomorrow);
    assert_eq!(generated.defer_until, tomorrow);
    let recurrence = generated.recurrence.clone().unwrap();
    let series = x.recurrence.as_ref().unwrap();
    assert_eq!(recurrence.series_id, series.series_id);
    assert_eq!(recurrence.rule, Some(daily()));
    assert_eq!(recurrence.occurrence_date, tomorrow);
    // One op: the completed Item, then the new one.
    assert_eq!(done.revision + 1, generated.revision);
    assert_eq!(result.revision, generated.revision);
    let changed = changed_items(&mut stream, 2).await;
    assert_eq!(changed, [done.clone(), generated.clone()]);

    let state = items
        .get_undo_state(GetUndoStateRequest {})
        .await
        .unwrap()
        .into_inner();
    let entry = state.undo.unwrap();
    assert_eq!(entry.op_id, result.op_id);
    assert_eq!(entry.kind, OpKind::SetStatus as i32);
    assert_eq!(entry.item_ids, [x.id.clone(), generated.id.clone()]);
    assert_eq!(
        entry.description,
        format!(
            "mark done \"water plants\" · next {}",
            summary_day(day_after(today(), 1))
        )
    );

    // A replay names the same instance and creates no other.
    let replay = items.set_status(req).await.unwrap().into_inner();
    assert_eq!(replay.result, Some(result));
    assert_eq!(replay.generated, Some(generated.clone()));
    // Completing an Item that does not recur generates nothing.
    let plain = capture(&mut items, "plain").await;
    let res = items
        .set_status(status_req(&plain.id, Status::Done))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.generated, None);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_mutate_reports_the_generated_instances_in_order() {
    use mutation::Kind;
    away_from_midnight().await;
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a @daily").await;
    let b = capture(&mut items, "b @3d").await;
    let c = capture(&mut items, "c").await;
    let req = batch_req(vec![
        batched(Kind::SetStatus(status_req(&b.id, Status::Done))),
        batched(Kind::SetStatus(status_req(&c.id, Status::Done))),
        batched(Kind::SetStatus(status_req(&a.id, Status::Cancelled))),
    ]);
    let res = items.batch_mutate(req.clone()).await.unwrap().into_inner();
    let series = |item: &Item| item.recurrence.as_ref().unwrap().series_id.clone();
    let generated: Vec<(String, String)> = res
        .generated
        .iter()
        .map(|g| (g.title.clone(), series(g)))
        .collect();
    assert_eq!(
        generated,
        [("b".to_owned(), series(&b)), ("a".to_owned(), series(&a))]
    );
    assert_eq!(
        res.generated[0].due_date,
        Some(date_to_proto(day_after(today(), 3)))
    );
    let state = items
        .get_undo_state(GetUndoStateRequest {})
        .await
        .unwrap()
        .into_inner();
    // Done and cancelled are two statuses, so the batch reads as a change.
    assert_eq!(state.undo.unwrap().description, "change 3 items · 2 next");
    let replay = items.batch_mutate(req).await.unwrap().into_inner();
    assert_eq!(replay.generated, res.generated);

    // A bulk SetRecurrence gives each Item its own series.
    let e = capture(&mut items, "e").await;
    let f = capture(&mut items, "f").await;
    let req = batch_req(vec![
        batched(Kind::SetRecurrence(recurrence_req(&e.id, Some(daily())))),
        batched(Kind::SetRecurrence(recurrence_req(&f.id, Some(daily())))),
    ]);
    let res = items.batch_mutate(req).await.unwrap().into_inner();
    assert!(res.generated.is_empty());
    assert_ne!(series(&res.items[0]), series(&res.items[1]));
    d.stop().await;
}

async fn start_with(options: DaemonOptions) -> TestDaemon {
    start_with_in(TempDir::new().unwrap(), options).await
}

async fn start_with_in(home: TempDir, options: DaemonOptions) -> TestDaemon {
    let paths = paths_for(&home);
    let Started::Running(daemon) = Daemon::start_with(paths.clone(), options).await.unwrap() else {
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

/// The next change on `stream` to the Item `id`, skipping others.
async fn next_change_to(stream: &mut Streaming<SubscribeResponse>, id: &str) -> Item {
    loop {
        let [item] = &changed_items(stream, 1).await[..] else {
            unreachable!();
        };
        if item.id == id {
            return item.clone();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_scheduler_archives_done_items_and_a_user_unarchive_sticks() {
    let d = start_with(DaemonOptions {
        scheduler_interval: Duration::from_millis(20),
        archive_done_after: Some(SignedDuration::ZERO),
        ..DaemonOptions::default()
    })
    .await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let open = capture(&mut items, "open").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(open.revision),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
    let done = items
        .set_status(status_req(&a.id, Status::Done))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(next_change_to(&mut stream, &a.id).await.archived, None);
    let archived = next_change_to(&mut stream, &a.id).await;
    let reason = archived.archived.as_ref().unwrap().reason;
    assert_eq!(reason, ArchiveReason::AutoDone as i32);
    assert_eq!(archived.status, Status::Done as i32);
    // A system op: the user's completion is still the top undo entry.
    let (undo_top, _) = undo_state(&mut items).await;
    assert_eq!(undo_top, Some(done.result.unwrap().op_id));

    // A z after the close sticks, through many sweeps.
    let res = items
        .unarchive_item(unarchive_req(&a.id))
        .await
        .unwrap()
        .into_inner();
    let unarchived = res.item.unwrap();
    assert_eq!(unarchived.archived, None);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(get(&mut items, &a.id).await, unarchived);
    // An open Item is never archived.
    assert_eq!(get(&mut items, &open.id).await.archived, None);

    // Closing it again restarts the clock.
    items
        .set_status(status_req(&a.id, Status::Cancelled))
        .await
        .unwrap();
    loop {
        let item = next_change_to(&mut stream, &a.id).await;
        if let Some(archived) = item.archived {
            assert_eq!(archived.reason, ArchiveReason::AutoDone as i32);
            assert_eq!(item.status, Status::Cancelled as i32);
            break;
        }
    }
    d.stop().await;
}

/// serve() runs the scheduler under its supervisor: it panics twice at
/// start, and the restarts still archive.
#[tokio::test(flavor = "multi_thread")]
async fn a_panicked_scheduler_is_restarted() {
    let d = start_with(DaemonOptions {
        scheduler_interval: Duration::from_millis(20),
        archive_done_after: Some(SignedDuration::ZERO),
        restart_after: Duration::from_millis(10),
        scheduler_panics: 2,
        ..DaemonOptions::default()
    })
    .await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(a.revision),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
    items
        .set_status(status_req(&a.id, Status::Done))
        .await
        .unwrap();
    assert_eq!(next_change_to(&mut stream, &a.id).await.archived, None);
    let archived = next_change_to(&mut stream, &a.id).await;
    let reason = archived.archived.as_ref().unwrap().reason;
    assert_eq!(reason, ArchiveReason::AutoDone as i32);
    d.stop().await;
}

/// No user op after the close: the sweep it wakes is too early, so only
/// a sweep on the interval archives it.
#[tokio::test(flavor = "multi_thread")]
async fn the_scheduler_sweeps_every_interval_unwoken() {
    let d = start_with(DaemonOptions {
        scheduler_interval: Duration::from_millis(20),
        archive_done_after: Some(SignedDuration::from_millis(300)),
        ..DaemonOptions::default()
    })
    .await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(a.revision),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
    items
        .set_status(status_req(&a.id, Status::Done))
        .await
        .unwrap();
    assert_eq!(next_change_to(&mut stream, &a.id).await.archived, None);
    let archived = next_change_to(&mut stream, &a.id).await;
    let reason = archived.archived.as_ref().unwrap().reason;
    assert_eq!(reason, ArchiveReason::AutoDone as i32);
    d.stop().await;
}

/// A restart over an Item already due archives it with no user op: the
/// interval is an hour, and GetItem wakes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn the_scheduler_sweeps_when_it_starts() {
    let hour = DaemonOptions {
        scheduler_interval: Duration::from_secs(3600),
        archive_done_after: Some(SignedDuration::from_hours(1)),
        ..DaemonOptions::default()
    };
    let d = start_with(hour.clone()).await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    items
        .set_status(status_req(&a.id, Status::Done))
        .await
        .unwrap();
    assert_eq!(get(&mut items, &a.id).await.archived, None);
    let home = d.stop().await;

    let d = start_with_in(
        home,
        DaemonOptions {
            archive_done_after: Some(SignedDuration::ZERO),
            ..hour
        },
    )
    .await;
    let mut items = d.items().await;
    let archived = timeout(WAIT, async {
        loop {
            if let Some(archived) = get(&mut items, &a.id).await.archived {
                return archived;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("never archived without a user op");
    assert_eq!(archived.reason, ArchiveReason::AutoDone as i32);
    d.stop().await;
}

/// The scheduler is woken by the undo itself: its interval is an hour.
#[tokio::test(flavor = "multi_thread")]
async fn an_undone_completion_is_unarchived_at_once() {
    let d = start_with(DaemonOptions {
        scheduler_interval: Duration::from_secs(3600),
        archive_done_after: Some(SignedDuration::ZERO),
        ..DaemonOptions::default()
    })
    .await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(a.revision),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
    items
        .set_status(status_req(&a.id, Status::Done))
        .await
        .unwrap();
    next_change_to(&mut stream, &a.id).await;
    let archived = next_change_to(&mut stream, &a.id).await;
    let reason = archived.archived.as_ref().unwrap().reason;
    assert_eq!(reason, ArchiveReason::AutoDone as i32);

    // The undo reopens it, still archived; the sweep it wakes heals it.
    let undone = undo(&mut items).await;
    assert_eq!(undone.items[0].status, Status::Inbox as i32);
    assert!(undone.items[0].archived.is_some());
    assert!(next_change_to(&mut stream, &a.id).await.archived.is_some());
    let healed = next_change_to(&mut stream, &a.id).await;
    assert_eq!(healed.archived, None);
    assert_eq!(healed.status, Status::Inbox as i32);
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
    // Any sortable key, in either direction.
    let newest = items
        .list_items(sort(&[(ItemField::CreatedAt, true)]))
        .await
        .unwrap()
        .into_inner();
    let ids: Vec<_> = newest.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, [b.id.as_str(), a.id.as_str()]);
    let status = items
        .list_items(sort(&[(ItemField::Labels, false)]))
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "sort[0].key");
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

    // The draft is the original parse: relative dates resolve against the
    // original reference time, not the replay's.
    let reference: jiff::Timestamp = "2026-01-15T12:00:00Z".parse().unwrap();
    let req = CaptureItemRequest {
        reference_time: Some(timestamp_to_proto(reference)),
        ..capture_req("renew passport ^tomorrow ~dana")
    };
    let first = items.capture_item(req.clone()).await.unwrap().into_inner();
    let day = reference.to_zoned(TimeZone::system()).date();
    let due = date_to_proto(day.tomorrow().unwrap());
    assert_eq!(first.draft.as_ref().unwrap().due_date, Some(due));
    let later: jiff::Timestamp = "2026-03-01T12:00:00Z".parse().unwrap();
    for reference_time in [None, Some(timestamp_to_proto(later))] {
        let replay = CaptureItemRequest {
            reference_time,
            ..req.clone()
        };
        let again = items.capture_item(replay).await.unwrap().into_inner();
        assert_eq!(again.result, first.result);
        assert_eq!(again.draft, first.draft);
        assert_eq!(again.item.unwrap().due_date, Some(due));
    }
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
    assert_eq!(
        violation(&status),
        ("view_id".to_owned(), "must be a ULID".to_owned())
    );
    let unknown = rid();
    let status = items
        .list_items(ListItemsRequest {
            view_id: Some(unknown.clone()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::NotFound, "VIEW_NOT_FOUND")["view_id"],
        unknown
    );
    // Filter errors name their path in the request.
    let labels_eq = ListItemsRequest {
        filter: Some(Filter {
            any_of: vec![Clause {
                kind: Some(clause::Kind::Group(ClauseGroup {
                    all_of: vec![Clause {
                        kind: Some(clause::Kind::Predicate(Predicate {
                            target: ItemField::Labels.into(),
                            op: Operator::Eq.into(),
                            operands: vec![Operand {
                                kind: Some(operand::Kind::Value(FieldValue {
                                    kind: Some(field_value::Kind::StringValue("x".into())),
                                })),
                            }],
                        })),
                    }],
                    any_of: vec![],
                })),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let status = items.list_items(labels_eq).await.unwrap_err();
    assert_eq!(
        violation(&status).0,
        "filter.any_of[0].group.all_of[0].predicate.op"
    );
    let status = items
        .list_items(ListItemsRequest {
            page_token: "p2.0000000000000000.1.01JB8X4Z5Q6R7S8T9V0W1X2Y3Z".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "page_token");
    let status = items
        .list_items(ListItemsRequest {
            reference_time: Some(prost_types::Timestamp {
                seconds: 9_223_372_037,
                nanos: 0,
            }),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "reference_time");
    d.stop().await;
}

/// A new store's views replay from revision 0 on TOPIC_VIEW, and ListItems
/// lists through one, narrowed by the request.
#[tokio::test(flavor = "multi_thread")]
async fn the_seeded_views_replay_and_list_items_through_them() {
    let d = start().await;
    let mut items = d.items().await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(0),
            topics: vec![Topic::View.into()],
        })
        .await;
    let mut views = Vec::new();
    for revision in 1..=SEEDED {
        let Message::Change(c) = next(&mut stream).await else {
            panic!("expected a change");
        };
        assert_eq!(
            (c.revision, c.topic, c.deleted),
            (revision, Topic::View as i32, false)
        );
        let Some(todo_proto::v1::change::Resource::View(view)) = c.resource else {
            panic!("expected a View");
        };
        assert_eq!(view.id, c.id);
        views.push(*view);
    }
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == STARTED));
    let seeds: Vec<_> = views.iter().map(|v| v.seed.as_deref().unwrap()).collect();
    assert_eq!(
        seeds,
        ["inbox", "today", "next", "waiting", "someday", "archive"]
    );

    // On a fixed day the Today view lists what is due by then, overdue
    // first.
    let day = jiff::civil::date(2030, 6, 3);
    let due = |title: &'static str, on: jiff::civil::Date| {
        let mut items = items.clone();
        async move {
            let item = capture(&mut items, title).await;
            let due = Item {
                due_date: Some(date_to_proto(on)),
                ..Default::default()
            };
            items
                .update_item(update_req(&item.id, due, &["due_date"]))
                .await
                .unwrap();
            item.id
        }
    };
    let today = due("today", day).await;
    let later = due("later", day.tomorrow().unwrap()).await;
    let overdue = due("overdue", day.yesterday().unwrap()).await;
    let noon = day.at(12, 0, 0, 0).to_zoned(TimeZone::system()).unwrap();
    let in_view = |filter: Option<Filter>| ListItemsRequest {
        filter,
        view_id: Some(views[1].id.clone()),
        reference_time: Some(timestamp_to_proto(noon.timestamp())),
        ..Default::default()
    };
    let listed = items.list_items(in_view(None)).await.unwrap().into_inner();
    let ids: Vec<_> = listed.items.iter().map(|i| i.id.clone()).collect();
    assert_eq!(ids, [overdue.clone(), today]);
    assert!(listed.items[0].is_overdue && !listed.items[1].is_overdue);
    assert!(!ids.contains(&later));
    // The request's clauses narrow the view.
    let titled_overdue = Filter {
        all_of: vec![Clause {
            kind: Some(clause::Kind::Predicate(Predicate {
                target: ItemField::Title.into(),
                op: Operator::Eq.into(),
                operands: vec![Operand {
                    kind: Some(operand::Kind::Value(FieldValue {
                        kind: Some(field_value::Kind::StringValue("overdue".into())),
                    })),
                }],
            })),
        }],
        ..Default::default()
    };
    let narrowed = items
        .list_items(in_view(Some(titled_overdue)))
        .await
        .unwrap()
        .into_inner();
    let ids: Vec<_> = narrowed.items.iter().map(|i| i.id.clone()).collect();
    assert_eq!(ids, [overdue]);
    d.stop().await;
}

/// ListItems with a general filter and sort pages with `p3` tokens bound to
/// them. A filter that is valid but not built yet is UNIMPLEMENTED, unless
/// the request is also invalid.
#[tokio::test(flavor = "multi_thread")]
async fn list_items_filters_sorts_and_pages() {
    let d = start().await;
    let mut items = d.items().await;
    for title in ["a", "b", "c", "d", "e"] {
        capture(&mut items, title).await;
    }
    let predicate = |target: ItemField, op: Operator, operands: Vec<operand::Kind>| Clause {
        kind: Some(clause::Kind::Predicate(Predicate {
            target: target.into(),
            op: op.into(),
            operands: operands
                .into_iter()
                .map(|kind| Operand { kind: Some(kind) })
                .collect(),
        })),
    };
    let title = |t: &str| {
        operand::Kind::Value(FieldValue {
            kind: Some(field_value::Kind::StringValue(t.into())),
        })
    };
    let sort = |key: ItemField, descending| SortKey {
        key: key.into(),
        descending,
    };
    let middle = ListItemsRequest {
        filter: Some(Filter {
            all_of: vec![predicate(
                ItemField::Title,
                Operator::In,
                vec![title("b"), title("c"), title("d")],
            )],
            ..Default::default()
        }),
        sort: vec![sort(ItemField::Title, true)],
        page_size: 2,
        ..Default::default()
    };
    let first = items.list_items(middle.clone()).await.unwrap().into_inner();
    assert!(first.next_page_token.starts_with("p3."));
    let next = ListItemsRequest {
        page_token: first.next_page_token.clone(),
        ..middle.clone()
    };
    let second = items.list_items(next.clone()).await.unwrap().into_inner();
    assert!(second.next_page_token.is_empty());
    let titles: Vec<_> = (first.items.iter().chain(&second.items))
        .map(|i| i.title.as_str())
        .collect();
    assert_eq!(titles, ["d", "c", "b"]);
    // The token is bound to the sort as well as the filter.
    let resorted = ListItemsRequest {
        sort: vec![sort(ItemField::Title, false)],
        ..next
    };
    let status = items.list_items(resorted).await.unwrap_err();
    assert_eq!(violation(&status).0, "page_token");

    // Relative dates on instants are not built yet.
    let unbuilt = Filter {
        all_of: vec![predicate(
            ItemField::CreatedAt,
            Operator::Ge,
            vec![operand::Kind::RelativeDate(todo_proto::v1::RelativeDate {
                anchor: todo_proto::v1::relative_date::Anchor::Today.into(),
                offset_days: 0,
            })],
        )],
        ..Default::default()
    };
    let status = items
        .list_items(ListItemsRequest {
            filter: Some(unbuilt.clone()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::Unimplemented, "NOT_IMPLEMENTED")["field"],
        "filter.all_of[0].predicate.operands[0]"
    );
    // An invalid sort wins over that.
    let status = items
        .list_items(ListItemsRequest {
            filter: Some(unbuilt),
            sort: vec![sort(ItemField::Labels, false)],
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "sort[0].key");
    d.stop().await;
}

/// ListItems takes today from reference_time, in the daemon's zone, for the
/// defer rule.
#[tokio::test(flavor = "multi_thread")]
async fn list_items_today_is_the_reference_time() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "deferred").await;
    let day = jiff::civil::date(2030, 6, 1);
    let defer = Item {
        defer_until: Some(date_to_proto(day)),
        ..Default::default()
    };
    items
        .update_item(update_req(&a.id, defer, &["defer_until"]))
        .await
        .unwrap();
    let listed = async |items: &mut ItemServiceClient<Channel>, now: jiff::Timestamp| {
        let req = ListItemsRequest {
            reference_time: Some(timestamp_to_proto(now)),
            ..Default::default()
        };
        let page = items.list_items(req).await.unwrap().into_inner();
        page.items.len()
    };
    // The daemon runs in this process, in its zone: the Item shows from
    // that zone's midnight.
    let midnight = day.to_zoned(TimeZone::system()).unwrap().timestamp();
    let before = midnight - jiff::SignedDuration::from_nanos(1);
    assert_eq!(listed(&mut items, before).await, 0);
    assert_eq!(listed(&mut items, midnight).await, 1);
    d.stop().await;
}

/// ListViews returns the seeded defaults in tab order, each as the change
/// stream carries it.
#[tokio::test(flavor = "multi_thread")]
async fn list_views_returns_the_seeded_views_in_tab_order() {
    let d = start().await;
    let listed = ViewServiceClient::new(d.channel().await)
        .list_views(ListViewsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.revision, STARTED);
    let defaults = todo_core::view::defaults();
    assert_eq!(listed.views.len(), defaults.len());
    for (i, (view, default)) in listed.views.iter().zip(&defaults).enumerate() {
        // The default as stored: with an id, a revision and timestamps.
        let stored = View {
            id: view.id.clone(),
            revision: view.revision,
            created_at: view.created_at,
            updated_at: view.updated_at,
            ..View::from(default)
        };
        assert_eq!(*view, stored);
        assert!(view.id.parse::<ulid::Ulid>().is_ok(), "{}", view.id);
        assert_eq!((view.position, view.revision), (i as u32, i as u64 + 1));
        assert!(view.created_at.is_some() && view.created_at == view.updated_at);
    }
    let tabs: Vec<_> = (listed.views.iter())
        .map(|v| (v.name.as_str(), v.show_count))
        .collect();
    assert_eq!(
        tabs,
        [
            ("Inbox", true),
            ("Today", true),
            ("Next", true),
            ("Waiting", true),
            ("Someday", false),
            ("Archive", false),
        ]
    );
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(0),
            topics: Vec::new(),
        })
        .await;
    for view in &listed.views {
        let Message::Change(c) = next(&mut stream).await else {
            panic!("expected a change");
        };
        let resource = todo_proto::v1::change::Resource::View(Box::new(view.clone()));
        assert_eq!((c.revision, c.resource), (view.revision, Some(resource)));
    }
    d.stop().await;
}

/// GetStatusCounts on a fixed day: the status counts pass the implicit
/// rules, and each counted view's count is the length of its listing.
#[tokio::test(flavor = "multi_thread")]
async fn status_counts_match_the_listings() {
    use jiff::ToSpan as _;
    let d = start().await;
    let mut items = d.items().await;
    let day = jiff::civil::date(2030, 6, 3);
    let noon = day.at(12, 0, 0, 0).to_zoned(TimeZone::system()).unwrap();
    let reference_time = Some(timestamp_to_proto(noon.timestamp()));
    // A ^ token for the day `n` days from the reference day.
    let due = |n: i64| format!("^{}", day.checked_add(n.days()).unwrap());
    for text in [
        "plain".to_owned(),
        format!("due today {}", due(0)),
        format!("overdue {}", due(-1)),
        format!("later {}", due(1)),
        format!("next overdue >next {}", due(-2)),
        "waiting ~bob".to_owned(),
        "someday >someday".to_owned(),
    ] {
        capture(&mut items, &text).await;
    }
    let started = capture(&mut items, "started").await;
    (items.set_status(status_req(&started.id, Status::InProgress)))
        .await
        .unwrap();
    // Due, but each hidden by an implicit rule.
    let done = capture(&mut items, &format!("done {}", due(-1))).await;
    (items.set_status(status_req(&done.id, Status::Done)))
        .await
        .unwrap();
    let archived = capture(&mut items, &format!("archived {}", due(-1))).await;
    items.archive_item(archive_req(&archived.id)).await.unwrap();
    let deferred = capture(&mut items, &format!("deferred {}", due(-1))).await;
    let defer = Item {
        defer_until: Some(date_to_proto(day.tomorrow().unwrap())),
        ..Default::default()
    };
    (items.update_item(update_req(&deferred.id, defer, &["defer_until"])))
        .await
        .unwrap();
    let deleted = capture(&mut items, &format!("deleted {}", due(-1))).await;
    items.delete_item(delete_req(&deleted.id)).await.unwrap();

    let req = |include_views| GetStatusCountsRequest {
        include_views,
        reference_time,
    };
    let got = items
        .get_status_counts(req(true))
        .await
        .unwrap()
        .into_inner();
    let counts = got.counts.unwrap();
    assert_eq!((counts.inbox, counts.due, counts.overdue), (5, 3, 2));
    assert_eq!(got.today, Some(date_to_proto(day)));
    // The counted views, in tab order.
    let views = ViewServiceClient::new(d.channel().await)
        .list_views(ListViewsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .views;
    let counted: Vec<_> = (views.iter().filter(|v| v.show_count))
        .map(|v| v.id.clone())
        .collect();
    let ids: Vec<_> = got.views.iter().map(|v| v.view_id.clone()).collect();
    assert_eq!(ids, counted);
    let mut listed = Vec::new();
    for v in &got.views {
        let req = ListItemsRequest {
            view_id: Some(v.view_id.clone()),
            reference_time,
            page_size: 1000,
            ..Default::default()
        };
        let page = items.list_items(req).await.unwrap().into_inner();
        assert_eq!(page.revision, got.revision);
        assert_eq!(v.count as usize, page.items.len(), "{}", v.view_id);
        listed.push(page.items.into_iter().map(|i| i.title).collect::<Vec<_>>());
    }
    assert_eq!(
        listed,
        [
            vec!["plain", "due today", "overdue", "later", "waiting"],
            vec!["next overdue", "overdue", "due today", "started"],
            vec!["next overdue", "started"],
            vec!["waiting"],
        ]
    );
    // Without include_views only the status counts.
    let plain = items
        .get_status_counts(req(false))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(plain.counts, Some(counts));
    assert!(plain.views.is_empty());
    assert_eq!(plain.revision, got.revision);

    // Each call reads the store as it is now, on the daemon's today.
    capture(&mut items, &format!("due now {}", due(0))).await;
    let before = jiff::Zoned::now().date();
    let now = (items.get_status_counts(GetStatusCountsRequest::default()))
        .await
        .unwrap()
        .into_inner();
    let after = jiff::Zoned::now().date();
    assert_eq!(now.revision, got.revision + 1);
    assert!(now.views.is_empty());
    let today = now.today.unwrap();
    assert!([date_to_proto(before), date_to_proto(after)].contains(&today));
    assert_eq!(now.counts.unwrap().inbox, 6);

    let status = items
        .get_status_counts(GetStatusCountsRequest {
            include_views: true,
            reference_time: Some(prost_types::Timestamp {
                seconds: 9_223_372_037,
                nanos: 0,
            }),
        })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "reference_time");
    d.stop().await;
}

/// ParseDate resolves the date grammar on the reference day, with or
/// without a leading ^, and words what does not parse as ParseCapture
/// words a ^ token.
#[tokio::test(flavor = "multi_thread")]
async fn parse_date_resolves_the_date_grammar() {
    use jiff::ToSpan as _;
    let d = start().await;
    let mut items = d.items().await;
    let day = jiff::civil::date(2030, 6, 3);
    let noon = day.at(12, 0, 0, 0).to_zoned(TimeZone::system()).unwrap();
    let parse_req = |text: &str| ParseDateRequest {
        text: text.to_owned(),
        reference_time: Some(timestamp_to_proto(noon.timestamp())),
    };
    let mut parse = async |text: &str| {
        let res = items.parse_date(parse_req(text)).await.unwrap();
        let res = res.into_inner();
        (res.date, res.warning)
    };
    let on = |date: jiff::civil::Date| (Some(date_to_proto(date)), String::new());
    let not = |warning: &str| (None, warning.to_owned());
    let friday = day.nth_weekday(1, jiff::civil::Weekday::Friday).unwrap();
    let tomorrow = day.tomorrow().unwrap();
    assert_eq!(parse("fri").await, on(friday));
    assert_eq!(parse("^fri").await, on(friday));
    assert_eq!(parse("+3d").await, on(day.checked_add(3.days()).unwrap()));
    assert_eq!(parse("^tomorrow").await, on(tomorrow));
    assert_eq!(parse(" ^TOM\n").await, on(tomorrow));
    assert_eq!(
        parse("^2026-11-02").await,
        on(jiff::civil::date(2026, 11, 2))
    );
    assert_eq!(parse("someday").await, not("someday: not a date"));
    // Only one ^ is stripped.
    assert_eq!(parse("^^fri").await, not("^^fri: not a date"));
    // Empty text is a warning, not an error.
    assert_eq!(parse("").await, not("empty date"));
    assert_eq!(parse(" ^ ").await, not("^: empty date"));

    let mut warnings = Vec::new();
    for typed in ["^someday", "^feb30", "^+3x", "^nov"] {
        let (date, warning) = parse(typed).await;
        assert_eq!(date, None, "{typed}");
        warnings.push(warning);
    }
    for (typed, warning) in ["^someday", "^feb30", "^+3x", "^nov"].iter().zip(warnings) {
        let req = ParseCaptureRequest {
            text: format!("x {typed}"),
            reference_time: None,
        };
        let draft = items.parse_capture(req).await.unwrap().into_inner();
        assert_eq!(draft.draft.unwrap().warnings, [warning]);
    }

    // Without a reference time, today is the daemon's.
    let before = jiff::Zoned::now().date();
    let req = ParseDateRequest {
        text: "today".into(),
        reference_time: None,
    };
    let today = items.parse_date(req).await.unwrap().into_inner().date;
    let after = jiff::Zoned::now().date();
    assert!(
        [before, after]
            .map(|d| Some(date_to_proto(d)))
            .contains(&today)
    );

    // Text over the capture limit is refused without being parsed.
    let over = "x".repeat(MAX_CAPTURE_BYTES + 1);
    let status = items.parse_date(parse_req(&over)).await.unwrap_err();
    assert_eq!(
        violation(&status),
        ("text".to_owned(), "is longer than 64 KiB".to_owned())
    );
    let status = items
        .parse_date(ParseDateRequest {
            text: "fri".into(),
            reference_time: Some(prost_types::Timestamp {
                seconds: -9_223_372_037,
                nanos: 0,
            }),
        })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "reference_time");
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
    for via in [
        CreatedVia::Tui,
        CreatedVia::Cli,
        CreatedVia::Mcp,
        CreatedVia::Mobile,
    ] {
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
async fn capture_under_a_missing_or_deleted_parent_names_parent_id() {
    let d = start().await;
    let mut items = d.items().await;
    let parent = capture(&mut items, "parent").await;
    let child = items
        .capture_item(CaptureItemRequest {
            parent_id: Some(parent.id.clone()),
            ..capture_req("child")
        })
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap();
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));

    let other = capture(&mut items, "other").await;
    items.delete_item(delete_req(&parent.id)).await.unwrap();
    // As UpdateItem reports the same parent: the request's argument is bad,
    // no Item it targets is missing.
    for (parent_id, reason) in [
        (parent.id.clone(), "the parent is deleted"),
        (rid(), "no such Item"),
    ] {
        let req = CaptureItemRequest {
            parent_id: Some(parent_id.clone()),
            ..capture_req("orphan")
        };
        let status = items.capture_item(req).await.unwrap_err();
        assert_eq!(
            violation(&status),
            ("parent_id".to_owned(), reason.to_owned())
        );
        let update = update_req(
            &other.id,
            Item {
                parent_id: Some(parent_id),
                ..Default::default()
            },
            &["parent_id"],
        );
        let status = items.update_item(update).await.unwrap_err();
        assert_eq!(
            violation(&status),
            ("item.parent_id".to_owned(), reason.to_owned())
        );
    }
    // Deleting the parent took its child with it (design 3.7).
    assert_eq!(titles(&mut items).await, ["other"]);
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
    assert_eq!(page.revision, STARTED);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_text_and_titles_are_limited_to_64_kib() {
    use mutation::Kind;
    let d = start().await;
    let mut items = d.items().await;
    // 64 KiB of UTF-8 in two-byte characters, then one byte more.
    let longest = "é".repeat(MAX_CAPTURE_BYTES / 2);
    let over = format!("{longest}x");
    let too_long = |field: &str| (field.to_owned(), "is longer than 64 KiB".to_owned());

    assert_eq!(capture(&mut items, &longest).await.title, longest);
    let status = items.capture_item(capture_req(&over)).await.unwrap_err();
    assert_eq!(violation(&status), too_long("text"));

    let parse = |text: &str| ParseCaptureRequest {
        text: text.to_owned(),
        reference_time: None,
    };
    let draft = items.parse_capture(parse(&longest)).await.unwrap();
    assert_eq!(draft.into_inner().draft.unwrap().title, longest);
    let status = items.parse_capture(parse(&over)).await.unwrap_err();
    assert_eq!(violation(&status), too_long("text"));

    let b = capture(&mut items, "b").await;
    let status = items
        .update_item(update_req(&b.id, titled(&over), &["title"]))
        .await
        .unwrap_err();
    assert_eq!(violation(&status), too_long("item.title"));
    let req = batch_req(vec![batched(Kind::UpdateItem(update_req(
        &b.id,
        titled(&over),
        &["title"],
    )))]);
    let status = items.batch_mutate(req).await.unwrap_err();
    assert_eq!(
        violation(&status),
        too_long("mutations[0].update_item.item.title")
    );
    let res = items
        .update_item(update_req(&b.id, titled(&longest), &["title"]))
        .await
        .unwrap();
    assert_eq!(res.into_inner().item.unwrap().title, longest);
    assert_eq!(titles(&mut items).await, [longest.clone(), longest]);
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
            with(titled("b"), &["relations"]),
            "item.relations",
            "changes only through AddRelation and RemoveRelation",
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
        (
            with(
                Item {
                    energy: 9,
                    ..Default::default()
                },
                &["energy"],
            ),
            "item.energy",
            "unknown enum value 9",
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

/// The store keeps durations as i64 nanoseconds (about 292 years); a longer
/// one is the client's error, as are control characters in a label.
#[tokio::test(flavor = "multi_thread")]
async fn values_the_store_cannot_keep_are_invalid_arguments() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let estimate = |seconds| Item {
        original_estimate: Some(prost_types::Duration { seconds, nanos: 0 }),
        remaining_estimate: Some(prost_types::Duration { seconds, nanos: 0 }),
        ..Default::default()
    };
    let three_centuries = 300 * 31_557_600;
    let req = update_req(&a.id, estimate(three_centuries), &["original_estimate"]);
    let status = items.update_item(req).await.unwrap_err();
    let (field, description) = violation(&status);
    assert_eq!(field, "item.original_estimate");
    assert_eq!(description, "must be shorter than about 292 years");
    let update = update_req(&a.id, estimate(three_centuries), &["remaining_estimate"]);
    let req = batch_req(vec![batched(mutation::Kind::UpdateItem(update))]);
    let status = items.batch_mutate(req).await.unwrap_err();
    assert_eq!(
        violation(&status).0,
        "mutations[0].update_item.item.remaining_estimate"
    );
    let labels = Item {
        labels: vec!["ok".into(), "bell\u{7}".into()],
        ..Default::default()
    };
    let status = items
        .update_item(update_req(&a.id, labels, &["labels"]))
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "item.labels");
    assert_eq!(get(&mut items, &a.id).await, a);

    // The longest that fits is kept.
    let req = update_req(&a.id, estimate(9_223_372_036), &["original_estimate"]);
    let item = items
        .update_item(req)
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap();
    assert_eq!(item.original_estimate.unwrap().seconds, 9_223_372_036);
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
    assert!(deleted.item.as_ref().unwrap().deleted_at.is_some());
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
    // Every mutation of a deleted Item fails except UndeleteItem, and
    // DeleteItem, which changes nothing.
    for status in [
        items
            .update_item(update_req(&a.id, titled("zombie"), &["title"]))
            .await
            .unwrap_err(),
        items
            .set_status(status_req(&a.id, Status::Done))
            .await
            .unwrap_err(),
        items.archive_item(archive_req(&a.id)).await.unwrap_err(),
        items
            .unarchive_item(unarchive_req(&a.id))
            .await
            .unwrap_err(),
    ] {
        assert_eq!(
            error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
            a.id
        );
    }
    let again = items
        .delete_item(delete_req(&a.id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(again.item, deleted.item);
    assert_eq!(
        again.result.unwrap().revision,
        deleted.result.unwrap().revision
    );
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

    let b = capture(&mut items, "b").await;
    let reused = [
        items
            .set_status(SetStatusRequest {
                id: b.id.clone(),
                ..req.clone()
            })
            .await
            .unwrap_err(),
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
                confirm_not_mine: false,
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
    assert_eq!(get(&mut items, &b.id).await.status, Status::Inbox as i32);
    assert_eq!(titles(&mut items).await, ["a", "b"]);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idempotency_keys_match_in_any_ulid_spelling() {
    let d = start().await;
    let mut items = d.items().await;
    let req = capture_req("once");
    let first = items.capture_item(req.clone()).await.unwrap().into_inner();
    let lower = CaptureItemRequest {
        request_id: req.request_id.to_ascii_lowercase(),
        ..req
    };
    let again = items.capture_item(lower).await.unwrap().into_inner();
    assert_eq!(again.result, first.result);

    let spool_id = rid();
    let spooled = CaptureItemRequest {
        spool_id: Some(spool_id.clone()),
        ..capture_req("spooled")
    };
    let first_spooled = items.capture_item(spooled).await.unwrap().into_inner();
    let respooled = CaptureItemRequest {
        spool_id: Some(spool_id.to_ascii_lowercase()),
        ..capture_req("spooled")
    };
    let again = items.capture_item(respooled).await.unwrap().into_inner();
    assert_eq!(again.result, first_spooled.result);
    assert_eq!(titles(&mut items).await, ["once", "spooled"]);

    let id = first.item.unwrap().id;
    let done = status_req(&id, Status::Done);
    let first = items.set_status(done.clone()).await.unwrap().into_inner();
    let lower = SetStatusRequest {
        request_id: done.request_id.to_ascii_lowercase(),
        ..done.clone()
    };
    let again = items.set_status(lower).await.unwrap().into_inner();
    assert_eq!(again.result, first.result);
    let batch = batch_req(vec![batched(mutation::Kind::SetStatus(done))]);
    let first = items
        .batch_mutate(batch.clone())
        .await
        .unwrap()
        .into_inner();
    let lower = BatchMutateRequest {
        request_id: batch.request_id.to_ascii_lowercase(),
        ..batch
    };
    let again = items.batch_mutate(lower).await.unwrap().into_inner();
    assert_eq!(again.result, first.result);
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
        confirm_not_mine: false,
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
        let Some(todo_proto::v1::change::Resource::Item(item)) = c.resource else {
            panic!("expected an Item");
        };
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
                let Some(todo_proto::v1::change::Resource::Item(item)) = c.resource else {
                    panic!("expected an Item");
                };
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
    // From the first Item change: the seeded views come before it.
    let mut from_start = d
        .subscribe(SubscribeRequest {
            since_revision: Some(STARTED),
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
        match next(&mut from_start).await {
            Message::Change(c) => changes.push((c.revision, c.id)),
            Message::CaughtUp(c) => {
                assert!(caught_up.is_none(), "CaughtUp sent twice");
                assert_eq!(c.revision, changes.last().map_or(STARTED, |(r, _)| *r));
                caught_up = Some(c.revision);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(caught_up.unwrap() >= STARTED + BEFORE as u64);
    let revisions: Vec<u64> = changes.iter().map(|(r, _)| *r).collect();
    let total = total as u64;
    assert_eq!(
        revisions,
        (STARTED + 1..=STARTED + total).collect::<Vec<_>>()
    );
    let seen: HashSet<_> = changes.into_iter().map(|(_, id)| id).collect();
    assert_eq!(seen, expected.iter().cloned().collect::<HashSet<_>>());

    // No replay: CaughtUp at the subscribe-time revision, then a gap-free tail.
    let Message::CaughtUp(start) = next(&mut from_now).await else {
        panic!("expected CaughtUp first");
    };
    let tail = collect_changes(&mut from_now, (STARTED + total - start.revision) as usize).await;
    let tail: Vec<u64> = tail.into_iter().map(|(r, _)| r).collect();
    assert_eq!(
        tail,
        (start.revision + 1..=STARTED + total).collect::<Vec<_>>()
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
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == STARTED));
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
    assert_eq!(
        revisions,
        (STARTED + 1..=STARTED + N as u64).collect::<Vec<_>>()
    );
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

/// Spooled captures are undo stack entries, there before any call is
/// served: the first Undo after the daemon starts undoes the last of them,
/// and they are all imported.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_undo_finds_the_spooled_captures_on_the_stack() {
    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    for n in 1..=30 {
        let entry = SpoolEntry::now(new_ulid(), format!("spooled {n}"));
        spool::append(&paths, &entry).unwrap();
    }
    let d = start_in(home).await;
    let mut items = d.items().await;
    let last = "capture \"spooled 30\"";
    assert_eq!(undo(&mut items).await.entry.unwrap().description, last);
    assert_eq!(redo(&mut items).await.entry.unwrap().description, last);
    assert_eq!(titles(&mut items).await.len(), 30);
    d.stop().await;
}

// ---- M2: undo and redo, relations, parent rules ----------------------------

fn undo_req(expected_op_id: Option<&str>) -> UndoRequest {
    UndoRequest {
        request_id: rid(),
        expected_op_id: expected_op_id.map(str::to_owned),
    }
}

fn redo_req(expected_op_id: Option<&str>) -> RedoRequest {
    RedoRequest {
        request_id: rid(),
        expected_op_id: expected_op_id.map(str::to_owned),
    }
}

async fn undo(items: &mut ItemServiceClient<Channel>) -> UndoResponse {
    items.undo(undo_req(None)).await.unwrap().into_inner()
}

async fn redo(items: &mut ItemServiceClient<Channel>) -> RedoResponse {
    items.redo(redo_req(None)).await.unwrap().into_inner()
}

/// The op ids of the entries Undo and Redo would act on.
async fn undo_state(items: &mut ItemServiceClient<Channel>) -> (Option<String>, Option<String>) {
    let state = items
        .get_undo_state(GetUndoStateRequest {})
        .await
        .unwrap()
        .into_inner();
    (state.undo.map(|e| e.op_id), state.redo.map(|e| e.op_id))
}

fn op_id(result: Option<MutationResult>) -> String {
    result.unwrap().op_id
}

fn relation(from: &Item, to: &Item, kind: RelationKind) -> Relation {
    Relation {
        from_id: from.id.clone(),
        to_id: to.id.clone(),
        kind: kind as i32,
    }
}

fn add_req(relation: Relation) -> AddRelationRequest {
    AddRelationRequest {
        request_id: rid(),
        relation: Some(relation),
    }
}

fn remove_req(relation: Relation) -> RemoveRelationRequest {
    RemoveRelationRequest {
        request_id: rid(),
        relation: Some(relation),
    }
}

async fn capture_under(items: &mut ItemServiceClient<Channel>, text: &str, parent: &Item) -> Item {
    let req = CaptureItemRequest {
        parent_id: Some(parent.id.clone()),
        ..capture_req(text)
    };
    items
        .capture_item(req)
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap()
}

/// The Items of the next `n` changes on `stream`.
async fn changed_items(stream: &mut Streaming<SubscribeResponse>, n: usize) -> Vec<Item> {
    let mut out = Vec::new();
    while out.len() < n {
        let Message::Change(c) = next(stream).await else {
            panic!("expected a change");
        };
        assert!(!c.deleted);
        let Some(todo_proto::v1::change::Resource::Item(item)) = c.resource else {
            panic!("expected an Item");
        };
        assert_eq!(item.id, c.id);
        out.push(*item);
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_and_redo_move_the_top_entry_between_the_stacks() {
    let d = start().await;
    let mut items = d.items().await;
    let res = items
        .capture_item(capture_req("a"))
        .await
        .unwrap()
        .into_inner();
    let a = res.item.unwrap();
    let captured = op_id(res.result);
    let state = items
        .get_undo_state(GetUndoStateRequest {})
        .await
        .unwrap()
        .into_inner();
    let entry = state.undo.unwrap();
    assert_eq!(
        (entry.op_id.as_str(), entry.kind, entry.item_ids.as_slice()),
        (
            captured.as_str(),
            OpKind::Capture as i32,
            [a.id.clone()].as_slice()
        )
    );
    assert_eq!(entry.description, "capture \"a\"");
    assert!(entry.at.is_some());
    assert_eq!(state.redo, None);

    let edit = items
        .update_item(update_req(&a.id, titled("a2"), &["title"]))
        .await
        .unwrap()
        .into_inner();
    let edited = op_id(edit.result);
    assert_eq!(undo_state(&mut items).await, (Some(edited.clone()), None));

    // Guarded by the entry the client saw, in any spelling of its id.
    let req = undo_req(Some(&edited.to_lowercase()));
    let undone = items.undo(req.clone()).await.unwrap().into_inner();
    let entry = undone.entry.clone().unwrap();
    assert_eq!(
        (entry.op_id.as_str(), entry.kind),
        (edited.as_str(), OpKind::Update as i32)
    );
    assert_eq!(entry.description, "edit \"a2\"");
    assert_eq!(undone.skipped, []);
    let [item] = &undone.items[..] else {
        panic!("expected one Item: {:?}", undone.items);
    };
    assert_eq!(
        (item.id.as_str(), item.title.as_str()),
        (a.id.as_str(), "a")
    );
    let result = undone.result.clone().unwrap();
    assert_ne!(result.op_id, edited);
    assert_eq!(result.revision, item.revision);
    assert_eq!(get(&mut items, &a.id).await.title, "a");
    assert_eq!(
        undo_state(&mut items).await,
        (Some(captured.clone()), Some(edited.clone()))
    );
    // A replay is the original response.
    assert_eq!(items.undo(req).await.unwrap().into_inner(), undone);
    assert_eq!(get(&mut items, &a.id).await.title, "a");

    let req = redo_req(Some(&edited));
    let redone = items.redo(req.clone()).await.unwrap().into_inner();
    assert_eq!(redone.entry, undone.entry);
    assert_eq!(redone.skipped, []);
    assert_eq!(redone.items[0].title, "a2");
    assert_eq!(items.redo(req).await.unwrap().into_inner(), redone);
    assert_eq!(undo_state(&mut items).await, (Some(edited), None));

    // Undoing the capture soft-deletes the Item; redo brings it back.
    undo(&mut items).await;
    let out = undo(&mut items).await;
    assert_eq!(out.entry.unwrap().op_id, captured);
    assert!(out.items[0].deleted_at.is_some());
    assert!(get(&mut items, &a.id).await.deleted_at.is_some());
    assert_eq!(titles(&mut items).await, Vec::<String>::new());
    let out = redo(&mut items).await;
    assert_eq!(out.items[0].deleted_at, None);
    assert_eq!(titles(&mut items).await, ["a"]);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_on_an_empty_stack_succeeds_and_keeps_its_request_id() {
    let d = start().await;
    let mut items = d.items().await;
    let req = undo_req(None);
    let empty = items.undo(req.clone()).await.unwrap().into_inner();
    assert_eq!(empty.entry, None);
    assert_eq!((empty.items.len(), empty.skipped.len()), (0, 0));
    let result = empty.result.clone().unwrap();
    assert!(result.op_id.parse::<ulid::Ulid>().is_ok());
    assert_eq!(result.revision, STARTED);
    let out = redo(&mut items).await;
    assert_eq!(out.entry, None);
    assert_eq!(undo_state(&mut items).await, (None, None));

    // A late retry does not undo an op made since.
    let a = capture(&mut items, "a").await;
    assert_eq!(items.undo(req).await.unwrap().into_inner(), empty);
    assert_eq!(get(&mut items, &a.id).await.deleted_at, None);
    assert!(undo_state(&mut items).await.0.is_some());
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_errors_carry_their_details() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let top = undo_state(&mut items).await.0.unwrap();

    for (request_id, expected_op_id, field) in [
        ("nope".to_owned(), None, "request_id"),
        (String::new(), None, "request_id"),
        (rid(), Some("nope"), "expected_op_id"),
        (rid(), Some(""), "expected_op_id"),
    ] {
        let expected_op_id = expected_op_id.map(str::to_owned);
        let status = items
            .undo(UndoRequest {
                request_id: request_id.clone(),
                expected_op_id: expected_op_id.clone(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            violation(&status),
            (field.to_owned(), "must be a ULID".to_owned())
        );
        let status = items
            .redo(RedoRequest {
                request_id,
                expected_op_id,
            })
            .await
            .unwrap_err();
        assert_eq!(violation(&status).0, field);
    }

    // Another entry on top, or none: ABORTED, and nothing changes.
    let other = new_ulid().to_string();
    let status = items.undo(undo_req(Some(&other))).await.unwrap_err();
    let meta = error_info(&status, Code::Aborted, "UNDO_STACK_CHANGED");
    assert_eq!(meta["expected_op_id"], other);
    assert_eq!(meta["top_op_id"], top);
    assert_eq!(get(&mut items, &a.id).await.deleted_at, None);
    let status = items
        .redo(redo_req(Some(&other.to_lowercase())))
        .await
        .unwrap_err();
    let meta = error_info(&status, Code::Aborted, "UNDO_STACK_CHANGED");
    assert_eq!(meta["expected_op_id"], other);
    assert_eq!(meta["top_op_id"], "");
    assert_eq!(undo_state(&mut items).await, (Some(top), None));

    // An undo's request id is no redo's.
    let req = undo_req(None);
    items.undo(req.clone()).await.unwrap();
    let status = items
        .redo(RedoRequest {
            request_id: req.request_id,
            expected_op_id: None,
        })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "request_id");
    assert!(get(&mut items, &a.id).await.deleted_at.is_some());
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_reports_the_changes_it_leaves_alone() {
    use todo_core::{Actor, Mutation as CoreMutation, MutationOp};
    use todo_store::{CaptureInput, MutateInput, OpKind as StoreOp, RelationInput, Store};

    // Changes the daemon's RPCs cannot make: a system edit (no undo stack
    // entry) and a relation removed behind the store's back.
    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    std::fs::create_dir_all(&paths.data).unwrap();
    let store = Store::open(&paths.database()).unwrap();
    let now = jiff::Timestamp::now();
    let today = now.to_zoned(TimeZone::system()).date();
    let capture_one = |title: &str| CaptureInput {
        request_id: new_ulid().to_string(),
        draft: todo_core::CaptureDraft::from_title(title),
        created_via: todo_core::CreatedVia::Cli,
        parent_id: None,
        spool_id: None,
        now,
        today,
    };
    let (x, _) = store.capture(capture_one("x")).await.unwrap();
    let (y, _) = store.capture(capture_one("y")).await.unwrap();
    let set_status = |actor, status| MutateInput {
        request_id: new_ulid().to_string(),
        actor,
        kind: StoreOp::SetStatus,
        mutations: vec![CoreMutation {
            id: x.id,
            expected_revision: None,
            op: MutationOp::SetStatus(status),
        }],
        now,
        today,
        confirm_not_mine: false,
    };
    let done = store
        .mutate(set_status(Actor::User, todo_core::Status::Done))
        .await
        .unwrap();
    store
        .mutate(set_status(Actor::System, todo_core::Status::Next))
        .await
        .unwrap();
    let blocks = todo_core::Relation::new(x.id, y.id, todo_core::RelationKind::Blocks).unwrap();
    let added = store
        .add_relation(RelationInput {
            request_id: new_ulid().to_string(),
            relation: blocks,
            now,
            today,
        })
        .await
        .unwrap();
    store
        .write(|tx| {
            tx.conn().execute("DELETE FROM relations", [])?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);

    let d = start_in(home).await;
    let mut items = d.items().await;
    let x_id = x.id.to_string();
    // The current revision: the config change this daemon recorded at start.
    let revision = SystemServiceClient::new(d.channel().await)
        .get_config(GetConfigRequest {})
        .await
        .unwrap()
        .into_inner()
        .revision;
    assert_eq!(
        revision,
        get(&mut items, &y.id.to_string()).await.revision + 1
    );
    let out = undo(&mut items).await;
    let entry = out.entry.unwrap();
    assert_eq!(entry.op_id, added.result.op_id.to_string());
    assert_eq!(entry.kind, OpKind::AddRelation as i32);
    assert_eq!(entry.item_ids, [x.id.to_string(), y.id.to_string()]);
    assert_eq!(
        out.skipped,
        [UndoSkip {
            item_id: x.id.to_string(),
            field: ItemField::Unspecified as i32,
            reason: UndoSkipReason::ChangedSince as i32,
            message: "the relation has changed since".into(),
            relation: Some(Relation {
                from_id: x.id.to_string(),
                to_id: y.id.to_string(),
                kind: RelationKind::Blocks as i32,
            }),
        }]
    );
    // Nothing restored: the entry is dropped and nothing is written.
    assert_eq!(out.items, []);
    assert_eq!(out.result.unwrap().revision, revision);

    let out = undo(&mut items).await;
    assert_eq!(out.entry.unwrap().op_id, done.result.op_id.to_string());
    // The status triple is one unit, reported as STATUS.
    assert_eq!(
        out.skipped,
        [UndoSkip {
            item_id: x.id.to_string(),
            field: ItemField::Status as i32,
            reason: UndoSkipReason::ChangedSince as i32,
            message: "the field has changed since".into(),
            relation: None,
        }]
    );
    assert_eq!(get(&mut items, &x_id).await.status, Status::Next as i32);
    let (undo_top, redo_top) = undo_state(&mut items).await;
    assert!(undo_top.is_some());
    assert_eq!(redo_top, None);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_cascades_and_undelete_needs_a_live_parent() {
    let d = start().await;
    let mut items = d.items().await;
    let p = capture(&mut items, "p").await;
    let c = capture_under(&mut items, "c", &p).await;
    let g = capture_under(&mut items, "g", &c).await;

    // Only the requested Item is in the response.
    let res = items
        .delete_item(delete_req(&p.id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.item.unwrap().id, p.id);
    for id in [&p.id, &c.id, &g.id] {
        assert!(get(&mut items, id).await.deleted_at.is_some());
    }
    let state = items
        .get_undo_state(GetUndoStateRequest {})
        .await
        .unwrap()
        .into_inner();
    let entry = state.undo.unwrap();
    assert_eq!(entry.kind, OpKind::Delete as i32);
    assert_eq!(entry.description, "delete \"p\" and 2 subtasks");
    assert_eq!(entry.item_ids.len(), 3);

    let status = items.undelete_item(undelete_req(&c.id)).await.unwrap_err();
    let meta = error_info(&status, Code::FailedPrecondition, "PARENT_DELETED");
    assert_eq!(
        (meta["item_id"].as_str(), meta["parent_id"].as_str()),
        (c.id.as_str(), p.id.as_str())
    );
    assert!(
        status.message().starts_with(&format!("item {}", c.id)),
        "{status:?}"
    );

    // In a batch the parent is checked once the batch is done, and the
    // message names the mutation.
    use mutation::Kind;
    let undelete = |id: &str| batched(Kind::UndeleteItem(undelete_req(id)));
    let status = items
        .batch_mutate(batch_req(vec![undelete(&g.id), undelete(&c.id)]))
        .await
        .unwrap_err();
    let meta = error_info(&status, Code::FailedPrecondition, "PARENT_DELETED");
    assert_eq!(
        (meta["item_id"].as_str(), meta["parent_id"].as_str()),
        (c.id.as_str(), p.id.as_str())
    );
    assert!(
        status
            .message()
            .starts_with("mutations[1].undelete_item.id: "),
        "{status:?}"
    );
    let res = items
        .batch_mutate(batch_req(vec![undelete(&c.id), undelete(&p.id)]))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.items.len(), 2);
    for id in [&p.id, &c.id, &g.id] {
        assert_eq!(get(&mut items, id).await.deleted_at, None);
    }

    // ITEM_DELETED in a batch names its mutation the same way.
    items.delete_item(delete_req(&g.id)).await.unwrap();
    let status = items
        .batch_mutate(batch_req(vec![
            batched(Kind::SetStatus(status_req(&c.id, Status::Next))),
            batched(Kind::SetStatus(status_req(&g.id, Status::Next))),
        ]))
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
        g.id
    );
    assert_eq!(
        status.message(),
        format!("mutations[1].set_status.id: item {} is deleted", g.id)
    );
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn relations_change_through_their_rpcs() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let b = capture(&mut items, "b").await;
    let c = capture(&mut items, "c").await;
    let blocks = relation(&a, &b, RelationKind::Blocks);

    let req = add_req(blocks.clone());
    let added = items.add_relation(req.clone()).await.unwrap().into_inner();
    let (from, to) = (
        added.from_item.clone().unwrap(),
        added.to_item.clone().unwrap(),
    );
    assert_eq!(
        (from.id.as_str(), to.id.as_str()),
        (a.id.as_str(), b.id.as_str())
    );
    assert_eq!(
        (from.relations.as_slice(), to.relations.as_slice()),
        ([blocks.clone()].as_slice(), [blocks.clone()].as_slice())
    );
    let result = added.result.clone().unwrap();
    assert_eq!(
        (from.revision, to.revision),
        (c.revision + 1, c.revision + 2)
    );
    assert_eq!(result.revision, c.revision + 2);
    assert_eq!(
        get(&mut items, &b.id).await.relations,
        std::slice::from_ref(&blocks)
    );
    let listed = items
        .list_items(ListItemsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.items[0].relations, std::slice::from_ref(&blocks));
    assert_eq!(
        items.add_relation(req.clone()).await.unwrap().into_inner(),
        added
    );

    // Adding it again changes nothing.
    let again = items
        .add_relation(add_req(blocks.clone()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(again.result.unwrap().revision, result.revision);

    // RELATES_TO is stored from the lower id, and named either way round.
    assert!(a.id < c.id);
    let relates = relation(&c, &a, RelationKind::RelatesTo);
    let out = items
        .add_relation(add_req(relates))
        .await
        .unwrap()
        .into_inner();
    let stored = relation(&a, &c, RelationKind::RelatesTo);
    assert_eq!(out.from_item.unwrap().id, a.id);
    assert_eq!(
        out.to_item.unwrap().relations,
        std::slice::from_ref(&stored)
    );

    let req = remove_req(blocks.clone());
    let removed = items
        .remove_relation(req.clone())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        removed.from_item.clone().unwrap().relations,
        std::slice::from_ref(&stored)
    );
    assert_eq!(removed.to_item.clone().unwrap().relations, []);
    assert_eq!(
        items.remove_relation(req).await.unwrap().into_inner(),
        removed
    );
    let state = items
        .get_undo_state(GetUndoStateRequest {})
        .await
        .unwrap()
        .into_inner();
    let entry = state.undo.unwrap();
    assert_eq!(entry.op_id, op_id(removed.result));
    assert_eq!(entry.kind, OpKind::RemoveRelation as i32);
    assert_eq!(entry.item_ids, [a.id.clone(), b.id.clone()]);
    assert_eq!(entry.description, "unrelate \"a\" blocks \"b\"");
    let out = undo(&mut items).await;
    assert_eq!(
        out.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        [a.id.as_str(), b.id.as_str()]
    );
    assert_eq!(out.items[1].relations, std::slice::from_ref(&blocks));

    // A request id names one relation request.
    let req = add_req(relation(&b, &c, RelationKind::Duplicates));
    items.add_relation(req.clone()).await.unwrap();
    let reused = [
        items
            .add_relation(AddRelationRequest {
                relation: Some(relation(&b, &a, RelationKind::Duplicates)),
                ..req.clone()
            })
            .await
            .unwrap_err(),
        items
            .add_relation(AddRelationRequest {
                relation: Some(relation(&b, &c, RelationKind::Blocks)),
                ..req.clone()
            })
            .await
            .unwrap_err(),
        items
            .remove_relation(RemoveRelationRequest {
                request_id: req.request_id.clone(),
                relation: req.relation.clone(),
            })
            .await
            .unwrap_err(),
    ];
    for status in reused {
        assert_eq!(violation(&status).0, "request_id");
    }
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn relation_errors_name_the_ends_as_the_request_does() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    let b = capture(&mut items, "b").await;
    let blocks = relation(&a, &b, RelationKind::Blocks);
    let with = |f: fn(&mut Relation)| {
        let mut r = blocks.clone();
        f(&mut r);
        r
    };
    let invalid = [
        (None, "relation", "required field is missing"),
        (
            Some(with(|r| r.from_id = "nope".into())),
            "relation.from_id",
            "invalid ULID \"nope\"",
        ),
        (
            Some(with(|r| r.to_id = String::new())),
            "relation.to_id",
            "invalid ULID \"\"",
        ),
        (
            Some(with(|r| r.kind = 0)),
            "relation.kind",
            "value must not be UNSPECIFIED",
        ),
        (
            Some(with(|r| r.to_id = r.from_id.to_lowercase())),
            "relation.to_id",
            "must differ from from_id",
        ),
    ];
    for (relation, field, description) in invalid {
        let want = (field.to_owned(), description.to_owned());
        let status = items
            .add_relation(AddRelationRequest {
                request_id: rid(),
                relation: relation.clone(),
            })
            .await
            .unwrap_err();
        assert_eq!(violation(&status), want);
        let status = items
            .remove_relation(RemoveRelationRequest {
                request_id: rid(),
                relation,
            })
            .await
            .unwrap_err();
        assert_eq!(violation(&status), want);
    }
    let status = items
        .add_relation(AddRelationRequest {
            request_id: "nope".into(),
            relation: Some(blocks.clone()),
        })
        .await
        .unwrap_err();
    assert_eq!(violation(&status).0, "request_id");

    // A missing from_id is NOT_FOUND, a missing to_id an argument.
    let missing = new_ulid().to_string();
    let status = items
        .add_relation(add_req(Relation {
            from_id: missing.clone(),
            ..blocks.clone()
        }))
        .await
        .unwrap_err();
    assert_eq!(
        error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
        missing
    );
    let status = items
        .remove_relation(remove_req(Relation {
            to_id: missing,
            ..blocks.clone()
        }))
        .await
        .unwrap_err();
    assert_eq!(
        violation(&status),
        ("relation.to_id".to_owned(), "no such Item".to_owned())
    );

    // A deleted end, by which end of the request it is, also when the
    // stored RELATES_TO has its ends the other way round.
    items.add_relation(add_req(blocks.clone())).await.unwrap();
    items.delete_item(delete_req(&b.id)).await.unwrap();
    for kind in [RelationKind::Duplicates, RelationKind::RelatesTo] {
        let status = items
            .add_relation(add_req(relation(&b, &a, kind)))
            .await
            .unwrap_err();
        assert_eq!(
            error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
            b.id
        );
        let status = items
            .add_relation(add_req(relation(&a, &b, kind)))
            .await
            .unwrap_err();
        assert_eq!(
            violation(&status),
            (
                "relation.to_id".to_owned(),
                "the Item is deleted".to_owned()
            )
        );
    }
    // Removing works with a deleted end.
    let out = items
        .remove_relation(remove_req(blocks))
        .await
        .unwrap()
        .into_inner();
    assert!(out.to_item.unwrap().deleted_at.is_some());
    assert_eq!(get(&mut items, &a.id).await.relations, []);

    // With both ends wrong, the request's from_id is named, whichever way
    // round a RELATES_TO is stored.
    let mut gone = [new_ulid().to_string(), new_ulid().to_string()];
    gone.sort();
    let mut dead = [
        capture(&mut items, "c").await,
        capture(&mut items, "d").await,
    ];
    dead.sort_by(|x, y| x.id.cmp(&y.id));
    for item in &dead {
        items.delete_item(delete_req(&item.id)).await.unwrap();
    }
    for kind in [RelationKind::Blocks, RelationKind::RelatesTo] {
        for (from, to) in [(&gone[0], &gone[1]), (&gone[1], &gone[0])] {
            let wrong = Relation {
                from_id: from.clone(),
                to_id: to.clone(),
                kind: kind as i32,
            };
            let status = items
                .add_relation(add_req(wrong.clone()))
                .await
                .unwrap_err();
            assert_eq!(
                error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
                *from,
                "{kind:?}"
            );
            let status = items.remove_relation(remove_req(wrong)).await.unwrap_err();
            assert_eq!(
                error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
                *from,
                "{kind:?}"
            );
        }
        for (from, to) in [(&dead[0], &dead[1]), (&dead[1], &dead[0])] {
            let status = items
                .add_relation(add_req(relation(from, to, kind)))
                .await
                .unwrap_err();
            assert_eq!(
                error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
                from.id,
                "{kind:?}"
            );
        }

        // An Item related to itself has a wrong to_id; when its from_id is
        // wrong too, the from_id is named.
        let itself = |id: &str| Relation {
            from_id: id.to_owned(),
            to_id: id.to_lowercase(),
            kind: kind as i32,
        };
        let status = items
            .add_relation(add_req(itself(&gone[0])))
            .await
            .unwrap_err();
        assert_eq!(
            error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
            gone[0],
            "{kind:?}"
        );
        let status = items
            .remove_relation(remove_req(itself(&gone[0])))
            .await
            .unwrap_err();
        assert_eq!(
            error_info(&status, Code::NotFound, "ITEM_NOT_FOUND")["item_id"],
            gone[0],
            "{kind:?}"
        );
        let status = items
            .add_relation(add_req(itself(&dead[0].id)))
            .await
            .unwrap_err();
        assert_eq!(
            error_info(&status, Code::FailedPrecondition, "ITEM_DELETED")["item_id"],
            dead[0].id,
            "{kind:?}"
        );
        // A remove takes a deleted from_id, so only to_id is wrong.
        let same = (
            "relation.to_id".to_owned(),
            "must differ from from_id".to_owned(),
        );
        for id in [&dead[0].id, &a.id] {
            let status = items
                .remove_relation(remove_req(itself(id)))
                .await
                .unwrap_err();
            assert_eq!(violation(&status), same, "{kind:?}");
        }
        let status = items
            .add_relation(add_req(itself(&a.id)))
            .await
            .unwrap_err();
        assert_eq!(violation(&status), same, "{kind:?}");
    }
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_sees_undo_relations_and_cascades() {
    let d = start().await;
    let mut items = d.items().await;
    let p = capture(&mut items, "p").await;
    let c = capture_under(&mut items, "c", &p).await;
    let x = capture(&mut items, "x").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(x.revision),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
    let ids = |seen: &[Item]| {
        let mut ids: Vec<String> = seen.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        ids
    };
    let sorted = |of: &[&Item]| {
        let mut ids: Vec<String> = of.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        ids
    };

    // The cascaded descendant arrives with the deleted Item.
    items.delete_item(delete_req(&p.id)).await.unwrap();
    let seen = changed_items(&mut stream, 2).await;
    assert_eq!(ids(&seen), sorted(&[&p, &c]));
    assert!(seen.iter().all(|i| i.deleted_at.is_some()));

    // Undoing the delete brings both back.
    let out = undo(&mut items).await;
    let seen = changed_items(&mut stream, 2).await;
    assert_eq!(ids(&seen), sorted(&[&p, &c]));
    assert!(seen.iter().all(|i| i.deleted_at.is_none()));
    assert_eq!(ids(&out.items), sorted(&[&p, &c]));

    // Both ends of a relation change.
    let blocks = relation(&x, &c, RelationKind::Blocks);
    items.add_relation(add_req(blocks.clone())).await.unwrap();
    let seen = changed_items(&mut stream, 2).await;
    assert_eq!(ids(&seen), sorted(&[&x, &c]));
    assert!(seen.iter().all(|i| i.relations == [blocks.clone()]));

    // And of its undo and redo.
    let out = undo(&mut items).await;
    let seen = changed_items(&mut stream, 2).await;
    assert_eq!(ids(&seen), sorted(&[&x, &c]));
    assert!(seen.iter().all(|i| i.relations.is_empty()));
    assert_eq!(seen.last().unwrap().revision, out.result.unwrap().revision);
    redo(&mut items).await;
    let seen = changed_items(&mut stream, 2).await;
    assert!(seen.iter().all(|i| i.relations == [blocks.clone()]));
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_re_emits_parents_and_subtasks() {
    let d = start().await;
    let mut items = d.items().await;
    let p = capture(&mut items, "p").await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(p.revision),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));

    // A capture under p re-emits p, then arrives itself.
    let a = capture_under(&mut items, "a", &p).await;
    assert_eq!(a.parent_title.as_deref(), Some("p"));
    let seen = changed_items(&mut stream, 2).await;
    assert_eq!(
        (seen[0].id.as_str(), seen[0].subtasks_total),
        (p.id.as_str(), 1)
    );
    assert_eq!(seen[1].id, a.id);

    // Ticking it re-emits p with its count; p's updated_at stays.
    let res = items
        .set_status(status_req(&a.id, Status::Done))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.item.unwrap().id, a.id);
    let seen = changed_items(&mut stream, 2).await;
    let parent = seen.iter().find(|i| i.id == p.id).unwrap();
    assert_eq!((parent.subtasks_done, parent.subtasks_total), (1, 1));
    assert_eq!(parent.updated_at, p.updated_at);

    // Retitling p re-emits a with the new title.
    items
        .update_item(update_req(&p.id, titled("parent"), &["title"]))
        .await
        .unwrap();
    let seen = changed_items(&mut stream, 2).await;
    let child = seen.iter().find(|i| i.id == a.id).unwrap();
    assert_eq!(child.parent_title.as_deref(), Some("parent"));
    d.stop().await;
}

fn write_config(d: &TestDaemon, text: &str) {
    std::fs::write(&d.paths.config, text).unwrap();
}

fn config_change(message: Message) -> (u64, ConfigSnapshot) {
    let Message::Change(c) = message else {
        panic!("not a change: {message:?}");
    };
    assert_eq!(
        (c.topic, c.id.as_str(), c.deleted),
        (Topic::Config as i32, "config", false)
    );
    let Some(todo_proto::v1::change::Resource::Config(config)) = c.resource else {
        panic!("a config change without its config");
    };
    (c.revision, *config)
}

#[tokio::test(flavor = "multi_thread")]
async fn get_config_serves_the_defaults_without_a_file() {
    let d = start().await;
    let res = SystemServiceClient::new(d.channel().await)
        .get_config(GetConfigRequest {})
        .await
        .unwrap()
        .into_inner();
    let config = res.config.unwrap();
    assert_eq!(config.path, d.paths.config.display().to_string());
    assert!(!config.exists);
    assert_eq!(config.config_version, 1);
    let ui = config.ui.unwrap();
    assert_eq!(
        (ui.review_day, ui.archive_done_after_days),
        (Weekday::Monday as i32, 7)
    );
    let backup = config.backup.unwrap();
    assert_eq!(
        (backup.enabled, backup.interval.as_str(), backup.retain_days),
        (true, "daily", 14)
    );
    assert!(config.keymap.is_empty() && config.warnings.is_empty());
    assert_eq!(res.revision, STARTED);
    d.stop().await;
}

/// A reload that changes the config streams it; one that does not streams
/// nothing; a file that is not TOML is refused and the config kept.
#[tokio::test(flavor = "multi_thread")]
async fn reload_config_streams_a_change_and_keeps_the_config_on_bad_toml() {
    let d = start().await;
    let mut system = SystemServiceClient::new(d.channel().await);
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(STARTED),
            topics: vec![Topic::Config.into()],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == STARTED));

    let res = system
        .reload_config(ReloadConfigRequest {})
        .await
        .unwrap()
        .into_inner();
    assert!(!res.changed);

    write_config(
        &d,
        "[ui]\narchive_done_after_days = 3\nweird = 1\n[keymap]\n\"K\" = \"move_up\"\n",
    );
    let res = system
        .reload_config(ReloadConfigRequest {})
        .await
        .unwrap()
        .into_inner();
    assert!(res.changed);
    let (revision, streamed) = config_change(next(&mut stream).await);
    assert_eq!(revision, STARTED + 1);
    assert_eq!(streamed.ui.unwrap().archive_done_after_days, 3);
    assert_eq!(
        streamed.keymap.get("K").map(String::as_str),
        Some("move_up")
    );
    assert_eq!(streamed.warnings.len(), 1);
    assert_eq!(streamed.warnings[0].key, "ui.weird");
    assert_eq!(res.config.unwrap().keymap, streamed.keymap);

    write_config(&d, "[ui\n");
    let err = system
        .reload_config(ReloadConfigRequest {})
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(err.message().contains("line 1"), "{}", err.message());
    let kept = system
        .get_config(GetConfigRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(kept.config.unwrap().ui.unwrap().archive_done_after_days, 3);
    assert_eq!(kept.revision, STARTED + 1);
    d.stop().await;
}

/// Config is not stored: a replayed config change carries the config
/// current when it is sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_config_change_carries_the_current_config() {
    let d = start().await;
    let mut system = SystemServiceClient::new(d.channel().await);
    for days in [2, 5] {
        write_config(&d, &format!("[ui]\narchive_done_after_days = {days}\n"));
        assert!(
            system
                .reload_config(ReloadConfigRequest {})
                .await
                .unwrap()
                .into_inner()
                .changed
        );
    }
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(STARTED),
            topics: vec![],
        })
        .await;
    for revision in [STARTED + 1, STARTED + 2] {
        let (r, config) = config_change(next(&mut stream).await);
        assert_eq!(r, revision);
        assert_eq!(config.ui.unwrap().archive_done_after_days, 5);
    }
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == STARTED + 2));
    d.stop().await;
}

/// The config read at start, a bad value warned about and defaulted.
#[tokio::test(flavor = "multi_thread")]
async fn the_config_is_read_at_start() {
    let home = TempDir::new().unwrap();
    std::fs::write(
        paths_for(&home).config,
        "[ui]\narchive_done_after_days = 0\n[backup]\nretain_days = 30\n",
    )
    .unwrap();
    let d = start_in(home).await;
    let config = SystemServiceClient::new(d.channel().await)
        .get_config(GetConfigRequest {})
        .await
        .unwrap()
        .into_inner()
        .config
        .unwrap();
    assert!(config.exists);
    assert_eq!(config.ui.unwrap().archive_done_after_days, 7);
    assert_eq!(config.backup.unwrap().retain_days, 30);
    let keys: Vec<_> = config.warnings.iter().map(|w| w.key.as_str()).collect();
    assert_eq!(keys, ["ui.archive_done_after_days"]);
    d.stop().await;
}

/// With no fixed wait in the options, the scheduler follows the config:
/// a reload from 7 days to 1 archives an Item closed a day and a half ago.
#[tokio::test(flavor = "multi_thread")]
async fn the_scheduler_follows_archive_done_after_days() {
    let d = start_with(DaemonOptions {
        scheduler_interval: Duration::from_millis(20),
        ..DaemonOptions::default()
    })
    .await;
    let mut items = d.items().await;
    let a = capture(&mut items, "a").await;
    items
        .set_status(status_req(&a.id, Status::Done))
        .await
        .unwrap();
    // Closed a day and a half ago, as the scheduler sees it.
    let at = jiff::Timestamp::now() - SignedDuration::from_hours(36);
    rusqlite::Connection::open(d.paths.database())
        .unwrap()
        .execute(
            "UPDATE items SET status_entered_at = ?1 WHERE id = ?2",
            rusqlite::params![at.as_nanosecond() as i64, a.id],
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        get(&mut items, &a.id).await.archived,
        None,
        "7 days have not passed"
    );

    write_config(&d, "[ui]\narchive_done_after_days = 1\n");
    SystemServiceClient::new(d.channel().await)
        .reload_config(ReloadConfigRequest {})
        .await
        .unwrap();
    // The reload is not a user op, so it is the interval sweep that sees it.
    let archived = timeout(WAIT, async {
        loop {
            if let Some(archived) = get(&mut items, &a.id).await.archived {
                return archived;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("never archived after the reload");
    assert_eq!(archived.reason, ArchiveReason::AutoDone as i32);
    d.stop().await;
}

/// A file edited while no daemon ran reaches a client that resumes from
/// before the restart: each start records a config change.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_streams_the_config_it_read() {
    let d = start().await;
    let home = d.stop().await;
    std::fs::write(
        paths_for(&home).config,
        "[ui]\narchive_done_after_days = 4\n",
    )
    .unwrap();
    let d = start_in(home).await;
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(STARTED),
            topics: vec![],
        })
        .await;
    let (revision, config) = config_change(next(&mut stream).await);
    assert_eq!(revision, STARTED + 1);
    assert_eq!(config.ui.unwrap().archive_done_after_days, 4);
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == STARTED + 1));
    d.stop().await;
}

async fn parse_query(
    d: &TestDaemon,
    text: &str,
    count: bool,
) -> todo_proto::v1::ParseQueryResponse {
    ViewServiceClient::new(d.channel().await)
        .parse_query(ParseQueryRequest {
            text: text.to_owned(),
            reference_time: None,
            count,
        })
        .await
        .unwrap()
        .into_inner()
}

#[tokio::test(flavor = "multi_thread")]
async fn parse_query_reads_text_and_counts_what_it_lists() {
    let d = start().await;
    let mut items = d.items().await;
    for text in ["a #work >next", "b #work", "c >next"] {
        capture(&mut items, text).await;
    }
    let res = parse_query(&d, "is:next #work sort:-created_at count:off", true).await;
    assert_eq!(res.errors, vec![]);
    assert_eq!(res.count, Some(1));
    assert!(!res.show_count);
    assert_eq!(res.sort.len(), 1);
    assert_eq!(res.filter.as_ref().unwrap().all_of.len(), 2);
    // The filter it answers lists what it counted.
    let listed = items
        .list_items(ListItemsRequest {
            filter: res.filter,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner()
        .items;
    let titles: Vec<_> = listed.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["a"]);
    assert_eq!(parse_query(&d, "#work", false).await.count, None);

    // A problem is an error with its span, in code points; nothing else.
    let res = parse_query(&d, "ü dew:1", true).await;
    assert_eq!(res.filter, None);
    assert_eq!(res.count, None);
    let spans: Vec<_> = res
        .errors
        .iter()
        .map(|e| (e.start, e.end, e.message.as_str()))
        .collect();
    assert_eq!(spans, [(2, 7, "unknown field \"dew\"")]);

    let err = ViewServiceClient::new(d.channel().await)
        .parse_query(ParseQueryRequest {
            text: "a".repeat(4097),
            reference_time: None,
            count: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    d.stop().await;
}

/// Each view carries its query text, which ParseQuery reads back to it.
#[tokio::test(flavor = "multi_thread")]
async fn views_carry_their_query_text() {
    let d = start().await;
    let views = ViewServiceClient::new(d.channel().await)
        .list_views(ListViewsRequest {})
        .await
        .unwrap()
        .into_inner()
        .views;
    assert_eq!(views[0].query, "is:inbox sort:created_at");
    assert_eq!(
        views[5].query,
        "any:(has:archived is:done,cancelled) include:archived,done,deferred sort:-updated_at count:off"
    );
    for view in &views {
        let res = parse_query(&d, &view.query, false).await;
        assert_eq!(res.errors, vec![], "{}", view.name);
        assert_eq!(res.filter, view.filter, "{}", view.name);
        assert_eq!(res.sort, view.sort, "{}", view.name);
        assert_eq!(res.show_count, view.show_count, "{}", view.name);
    }
    d.stop().await;
}

fn create_view_req(name: &str, text: &str) -> CreateViewRequest {
    let parsed = todo_core::parse_query(text);
    assert_eq!(parsed.errors, vec![], "{text}");
    CreateViewRequest {
        request_id: new_ulid().to_string(),
        name: name.to_owned(),
        filter: Some((&parsed.query.filter).into()),
        sort: parsed.query.sort.into_iter().map(Into::into).collect(),
        show_count: parsed.query.show_count,
        position: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn view_edits_stream_on_the_view_topic_and_map_their_errors() {
    let d = start().await;
    let mut views = ViewServiceClient::new(d.channel().await);
    let mut stream = d
        .subscribe(SubscribeRequest {
            since_revision: Some(STARTED),
            topics: vec![Topic::View.into()],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));

    let created = views
        .create_view(CreateViewRequest {
            position: Some(0),
            ..create_view_req("Work", "#work sort:-priority")
        })
        .await
        .unwrap()
        .into_inner();
    let work = created.view.unwrap();
    assert_eq!(
        (work.position, work.query.as_str()),
        (0, "#work sort:-priority")
    );
    assert_eq!(created.moved.len(), 6);
    // The six shifted views, then Work, each a change.
    let mut ids = HashSet::new();
    for _ in 0..7 {
        let Message::Change(c) = next(&mut stream).await else {
            panic!("expected a change");
        };
        ids.insert(c.id);
    }
    assert!(ids.contains(&work.id));

    // A name taken in another case.
    let err = views
        .create_view(create_view_req("work", "is:next"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::AlreadyExists);
    assert_eq!(
        err.get_details_error_info().unwrap().reason,
        "VIEW_NAME_TAKEN"
    );
    let err = views
        .create_view(create_view_req("  ", "is:next"))
        .await
        .unwrap_err();
    assert_eq!(
        err.get_details_bad_request().unwrap().field_violations[0].field,
        "name"
    );
    // A bad filter names its path.
    let mut bad = create_view_req("Bad", "is:next");
    bad.filter.as_mut().unwrap().all_of[0] = Clause {
        kind: Some(clause::Kind::Predicate(Predicate {
            target: ItemField::Status as i32,
            op: Operator::Lt as i32,
            operands: vec![],
        })),
    };
    let err = views.create_view(bad).await.unwrap_err();
    assert_eq!(
        err.get_details_bad_request().unwrap().field_violations[0].field,
        "filter.all_of[0].predicate.op"
    );

    // Update: the masked fields only, with expected_revision.
    let update = |mask: &[&str], view: View, expected: Option<u64>| UpdateViewRequest {
        request_id: new_ulid().to_string(),
        view: Some(view),
        update_mask: Some(FieldMask {
            paths: mask.iter().map(|p| (*p).to_owned()).collect(),
        }),
        expected_revision: expected,
    };
    let renamed = View {
        name: "Deep work".into(),
        show_count: false,
        ..work.clone()
    };
    let out = views
        .update_view(update(&["name"], renamed.clone(), Some(work.revision)))
        .await
        .unwrap()
        .into_inner()
        .view
        .unwrap();
    assert_eq!((out.name.as_str(), out.show_count), ("Deep work", true));
    let err = views
        .update_view(update(
            &["show_count"],
            renamed.clone(),
            Some(work.revision),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Aborted);
    let err = views
        .update_view(update(&["seed"], renamed.clone(), None))
        .await
        .unwrap_err();
    assert_eq!(
        err.get_details_bad_request().unwrap().field_violations[0].field,
        "update_mask.paths[0]"
    );
    let mut bad_filter = renamed.clone();
    bad_filter.filter = Some(Filter {
        all_of: vec![Clause { kind: None }],
        ..Filter::default()
    });
    let err = views
        .update_view(update(&["filter"], bad_filter, None))
        .await
        .unwrap_err();
    assert_eq!(
        err.get_details_bad_request().unwrap().field_violations[0].field,
        "view.filter.all_of[0]"
    );

    // Move, then delete.
    let moved = views
        .move_view(MoveViewRequest {
            request_id: new_ulid().to_string(),
            id: work.id.clone(),
            position: 99,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(moved.view.unwrap().position, 6);
    assert_eq!(moved.moved.len(), 7);
    let deleted = views
        .delete_view(DeleteViewRequest {
            request_id: new_ulid().to_string(),
            id: work.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(deleted.moved.is_empty());
    let err = views
        .delete_view(DeleteViewRequest {
            request_id: new_ulid().to_string(),
            id: work.id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let err = views
        .delete_view(DeleteViewRequest {
            request_id: "nope".into(),
            id: work.id,
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.get_details_bad_request().unwrap().field_violations[0].field,
        "request_id"
    );
    // None of it reached the undo stack.
    let (top, _) = undo_state(&mut d.items().await).await;
    assert_eq!(top, None);
    d.stop().await;
}

fn backups_in(d: &TestDaemon) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(d.paths.backups())
        .map(|dir| {
            dir.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A new daemon takes its first automatic backup at start; Backup takes one
/// now, cleaning up old ones while automatic backups are on.
#[tokio::test(flavor = "multi_thread")]
async fn backups_at_start_on_request_and_cleaned_up() {
    let d = start().await;
    let first = timeout(WAIT, async {
        loop {
            // Finished backups only: one being written is a .db.tmp.
            let names: Vec<_> = backups_in(&d)
                .into_iter()
                .filter(|n| n.ends_with(".db"))
                .collect();
            if !names.is_empty() {
                return names;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("no backup at start");
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(
        first[0].starts_with("todo-") && first[0].ends_with(".db"),
        "{first:?}"
    );

    // Old ones by the name's date, and only backups, go.
    let dir = d.paths.backups();
    for name in [
        "todo-20200101-0000.db",
        "todo-premigrate-v1-20200101-000000.db",
        "keep-me.db",
    ] {
        std::fs::write(dir.join(name), "x").unwrap();
    }
    let mut items = d.items().await;
    capture(&mut items, "after the first").await;
    let res = SystemServiceClient::new(d.channel().await)
        .backup(BackupRequest {})
        .await
        .unwrap()
        .into_inner();
    assert!(res.bytes > 0);
    let removed: Vec<_> = res
        .removed
        .iter()
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        removed,
        [
            "todo-20200101-0000.db",
            "todo-premigrate-v1-20200101-000000.db"
        ]
    );
    let names = backups_in(&d);
    assert!(names.contains(&"keep-me.db".to_owned()));
    // Two this minute: the second has a suffix, and holds the new Item.
    let latest = std::path::Path::new(&res.path);
    assert_eq!(todo_store::inspect(latest).unwrap().items, 1);
    d.stop().await;
}

/// With automatic backups off, none is taken at start, and Backup still
/// works but deletes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn backups_off_take_none_and_keep_old_ones() {
    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    std::fs::write(&paths.config, "[backup]\nenabled = false\n").unwrap();
    std::fs::create_dir_all(paths.backups()).unwrap();
    std::fs::write(paths.backups().join("todo-20200101-0000.db"), "x").unwrap();
    let d = start_in(home).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(backups_in(&d), ["todo-20200101-0000.db"]);
    let res = SystemServiceClient::new(d.channel().await)
        .backup(BackupRequest {})
        .await
        .unwrap()
        .into_inner();
    assert!(res.removed.is_empty());
    assert_eq!(backups_in(&d).len(), 2);
    d.stop().await;
}

async fn export_lines(d: &TestDaemon, no_history: bool) -> Vec<String> {
    let mut stream = SystemServiceClient::new(d.channel().await)
        .max_decoding_message_size(usize::MAX)
        .export(ExportRequest { no_history })
        .await
        .unwrap()
        .into_inner();
    let mut lines = Vec::new();
    while let Some(batch) = stream.message().await.unwrap() {
        lines.extend(batch.lines);
    }
    lines
}

async fn import_lines(
    d: &TestDaemon,
    lines: Vec<String>,
) -> Result<todo_proto::v1::ImportResponse, tonic::Status> {
    let batches: Vec<ImportRequest> = lines
        .chunks(7)
        .map(|c| ImportRequest { lines: c.to_vec() })
        .collect();
    SystemServiceClient::new(d.channel().await)
        .import(futures_util::stream::iter(batches))
        .await
        .map(|r| r.into_inner())
}

/// A database with one of everything: subtasks, relations, a recurring
/// Item completed (so its next instance), undo and redo, a deleted Item,
/// an edited, moved and created view, a deleted default.
async fn rich(d: &TestDaemon) {
    let mut items = d.items().await;
    let parent = capture(&mut items, "parent #work +2 ^2026-11-20").await;
    let child = items
        .capture_item(CaptureItemRequest {
            parent_id: Some(parent.id.clone()),
            ..capture_req("child ~dana")
        })
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap();
    let water = capture(&mut items, "water plants %low @mon,thu").await;
    let gone = capture(&mut items, "gone").await;
    items
        .add_relation(AddRelationRequest {
            request_id: new_ulid().to_string(),
            relation: Some(Relation {
                from_id: parent.id.clone(),
                to_id: child.id.clone(),
                kind: RelationKind::Blocks as i32,
            }),
        })
        .await
        .unwrap();
    items
        .set_status(status_req(&water.id, Status::Done))
        .await
        .unwrap();
    items
        .update_item(update_req(&child.id, titled("child task"), &["title"]))
        .await
        .unwrap();
    undo(&mut items).await;
    items
        .delete_item(DeleteItemRequest {
            request_id: new_ulid().to_string(),
            id: gone.id.clone(),
            expected_revision: None,
        })
        .await
        .unwrap();
    let mut views = ViewServiceClient::new(d.channel().await);
    let work = views
        .create_view(create_view_req("Work", "#work sort:-priority"))
        .await
        .unwrap()
        .into_inner()
        .view
        .unwrap();
    views
        .move_view(MoveViewRequest {
            request_id: new_ulid().to_string(),
            id: work.id,
            position: 0,
        })
        .await
        .unwrap();
    let listed = views
        .list_views(ListViewsRequest {})
        .await
        .unwrap()
        .into_inner()
        .views;
    let someday = listed.iter().find(|v| v.name == "Someday").unwrap();
    views
        .delete_view(DeleteViewRequest {
            request_id: new_ulid().to_string(),
            id: someday.id.clone(),
        })
        .await
        .unwrap();
}

/// design 8.3's round trip: export, import into a fresh daemon, export
/// again: every record line the same.
#[tokio::test(flavor = "multi_thread")]
async fn export_import_export_round_trips() {
    let a = start().await;
    rich(&a).await;
    for no_history in [false, true] {
        let first = export_lines(&a, no_history).await;
        let header: serde_json::Value = serde_json::from_str(&first[0]).unwrap();
        assert_eq!(header["format"], "todo-tui-export");
        assert_eq!(header["formatVersion"], 1);
        let b = start().await;
        let out = import_lines(&b, first.clone()).await.unwrap();
        assert_eq!(out.records as usize, first.len() - 1);
        let second = export_lines(&b, no_history).await;
        assert_eq!(first[1..], second[1..], "no_history {no_history}");
        // Energy travels, as proto3 JSON's enum name.
        assert!(
            first
                .iter()
                .any(|l| l.contains("\"energy\":\"ENERGY_LOW\"")),
            "no_history {no_history}"
        );
        // What the import holds works: the views, search, and undo.
        let views = ViewServiceClient::new(b.channel().await)
            .list_views(ListViewsRequest {})
            .await
            .unwrap()
            .into_inner()
            .views;
        let names: Vec<_> = views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            ["Work", "Inbox", "Today", "Next", "Waiting", "Archive"]
        );
        let mut items = b.items().await;
        let found = items
            .list_items(ListItemsRequest {
                filter: Some(Filter {
                    all_of: vec![Clause {
                        kind: Some(clause::Kind::Predicate(Predicate {
                            target: ItemField::Title as i32,
                            op: Operator::Matches as i32,
                            operands: vec![Operand {
                                kind: Some(operand::Kind::Text("plants".into())),
                            }],
                        })),
                    }],
                    // Done, and the next instance deferred to its day.
                    include_done: true,
                    include_deferred: true,
                    ..Filter::default()
                }),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner()
            .items;
        assert_eq!(
            found.len(),
            2,
            "water plants and its next instance, found by the index"
        );
        if !no_history {
            let (top, _) = undo_state(&mut items).await;
            assert!(top.is_some(), "the imported undo stack");
            undo(&mut items).await;
        }
        b.stop().await;
    }
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn import_refuses_a_database_in_use_and_a_bad_line() {
    let a = start().await;
    rich(&a).await;
    let lines = export_lines(&a, false).await;
    // Into a database with Items.
    let err = import_lines(&a, lines.clone()).await.unwrap_err();
    assert_eq!(
        (
            err.code(),
            err.get_details_error_info().unwrap().reason.as_str()
        ),
        (Code::FailedPrecondition, "DATABASE_NOT_EMPTY")
    );

    let field = |e: &tonic::Status| {
        e.get_details_bad_request().unwrap().field_violations[0]
            .field
            .clone()
    };
    for (bad, line) in [
        (vec!["{}".to_owned()], "line 1"),
        (
            vec![
                lines[0].clone(),
                "{\"type\":\"frob\",\"data\":{}}".to_owned(),
            ],
            "line 2",
        ),
        (
            vec![lines[0].clone(), lines[1].clone(), "not json".to_owned()],
            "line 3",
        ),
    ] {
        let b = start().await;
        let err = import_lines(&b, bad).await.unwrap_err();
        assert_eq!(
            (err.code(), field(&err).as_str()),
            (Code::InvalidArgument, line)
        );
        // Nothing went in.
        // The header, and the six seeded views with their seeds.
        assert_eq!(export_lines(&b, false).await.len(), 13);
        b.stop().await;
    }
    a.stop().await;
}

/// Large Items: an import batch over tonic's 4 MiB default still goes in.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_export_imports_whole() {
    let a = start().await;
    let mut items = a.items().await;
    let long = "x".repeat(60_000);
    for i in 0..80 {
        capture(&mut items, &format!("{i} {long}")).await;
    }
    let lines = export_lines(&a, false).await;
    let b = start().await;
    let batches = vec![ImportRequest {
        lines: lines.clone(),
    }];
    let out = SystemServiceClient::new(b.channel().await)
        .max_encoding_message_size(usize::MAX)
        .import(futures_util::stream::iter(batches))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(out.items, 80);
    a.stop().await;
    b.stop().await;
}

/// A subscriber connected through an import is sent to resync, and the next
/// write numbers after the import.
#[tokio::test(flavor = "multi_thread")]
async fn an_import_resyncs_live_subscribers() {
    let a = start().await;
    rich(&a).await;
    let lines = export_lines(&a, false).await;
    let b = start().await;
    let mut stream = b
        .subscribe(SubscribeRequest {
            since_revision: Some(STARTED),
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
    let out = import_lines(&b, lines).await.unwrap();
    loop {
        match next(&mut stream).await {
            Message::ResyncRequired(r) => {
                assert_eq!(r.current_revision, out.revision);
                break;
            }
            Message::Change(c) => panic!("a change from before the import: {c:?}"),
            _ => {}
        }
    }
    let mut items = b.items().await;
    let after = capture(&mut items, "after").await;
    assert_eq!(after.revision, out.revision + 1);
    a.stop().await;
    b.stop().await;
}

/// Even an export older than the database it goes into leaves a gap: the
/// target restarted more often than the source ever wrote.
#[tokio::test(flavor = "multi_thread")]
async fn an_import_resyncs_a_subscriber_ahead_of_the_export() {
    let a = start().await;
    let lines = export_lines(&a, false).await;
    a.stop().await;
    let mut home = TempDir::new().unwrap();
    for _ in 0..3 {
        home = start_in(home).await.stop().await;
    }
    let b = start_in(home).await;
    let mut stream = b
        .subscribe(SubscribeRequest {
            since_revision: None,
            topics: vec![],
        })
        .await;
    assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
    let out = import_lines(&b, lines).await.unwrap();
    loop {
        match next(&mut stream).await {
            Message::ResyncRequired(r) => {
                assert_eq!(r.current_revision, out.revision);
                break;
            }
            Message::Change(c) => panic!("taken as an ordinary change: {c:?}"),
            _ => {}
        }
    }
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parse_query_refuses_an_offset_a_view_would() {
    let d = start().await;
    let res = parse_query(&d, "due_date<=today+40000d", true).await;
    assert_eq!(res.filter, None);
    assert_eq!(res.errors.len(), 1);
    assert!(
        res.errors[0].message.contains("36525"),
        "{}",
        res.errors[0].message
    );
    d.stop().await;
}

/// Monday 2026-11-02, noon in the test daemon's zone.
fn monday_noon() -> prost_types::Timestamp {
    let t = jiff::civil::date(2026, 11, 2)
        .at(12, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::system())
        .unwrap()
        .timestamp();
    todo_proto::convert::timestamp_to_proto(t)
}

#[tokio::test(flavor = "multi_thread")]
async fn activity_report_covers_the_working_day_window() {
    let d = start().await;
    let mut items = d.items().await;
    let done = capture(&mut items, "ship the release").await;
    items
        .set_status(status_req(&done.id, Status::Done))
        .await
        .unwrap();
    capture(&mut items, "only captured").await;
    let start_req = |since: &str| GetActivityReportRequest {
        reference_time: Some(monday_noon()),
        since: since.into(),
    };
    let res = items
        .get_activity_report(start_req(""))
        .await
        .unwrap()
        .into_inner();
    // Monday's window: Thursday back, Wednesday on; today's work is in it.
    let day = |d: &Option<todo_proto::google::r#type::Date>| d.as_ref().map(|d| (d.month, d.day));
    assert_eq!(
        (day(&res.since), day(&res.until), day(&res.today)),
        (Some((10, 29)), Some((11, 4)), Some((11, 2)))
    );
    assert_eq!(
        res.last.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
        ["ship the release"]
    );
    assert_eq!(res.last[0].item_id, done.id);
    assert!(
        res.text.starts_with("L48:\n- ship the release\nN48:\n"),
        "{}",
        res.text
    );
    // [l48] widens the window; a since after today is refused.
    write_config(&d, "[l48]\nwindow_days = 3\n");
    SystemServiceClient::new(d.channel().await)
        .reload_config(ReloadConfigRequest {})
        .await
        .unwrap();
    let wide = items
        .get_activity_report(start_req(""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(day(&wide.since), Some((10, 28)));
    // since counts back: thu is last Thursday.
    let back = items
        .get_activity_report(start_req("thu"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(day(&back.since), Some((10, 29)));
    // After today, or before the store's range, is refused.
    for bad in ["2026-11-03", "tomorrow", "1600-01-01"] {
        let err = items.get_activity_report(start_req(bad)).await.unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument, "{bad}");
    }
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn build_meeting_link_prefills_teams_and_writes_nothing() {
    let d = start().await;
    let mut items = d.items().await;
    let a = capture(&mut items, "plan the offsite ~dana@example.com").await;
    let b = capture(&mut items, "budget").await;
    let before = items
        .get_undo_state(GetUndoStateRequest {})
        .await
        .unwrap()
        .into_inner();
    let res = items
        .build_meeting_link(BuildMeetingLinkRequest {
            ids: vec![a.id.clone(), b.id.clone()],
            subject: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.subject, "plan the offsite + 1 more");
    assert_eq!(res.attendees, ["dana@example.com"]);
    assert!(
        res.url.starts_with(
            "https://teams.microsoft.com/l/meeting/new?subject=plan%20the%20offsite%20%2B%201%20more&"
        ),
        "{}",
        res.url
    );
    assert!(
        res.url.ends_with("&attendees=dana%40example.com"),
        "{}",
        res.url
    );
    let after = items
        .get_undo_state(GetUndoStateRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(before, after, "nothing is written");
    // A deleted Item, no ids, and meetings turned off.
    items.delete_item(delete_req(&b.id)).await.unwrap();
    let err = items
        .build_meeting_link(BuildMeetingLinkRequest {
            ids: vec![b.id.clone()],
            subject: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let err = items
        .build_meeting_link(BuildMeetingLinkRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    write_config(&d, "[meetings]\nprovider = \"none\"\n");
    SystemServiceClient::new(d.channel().await)
        .reload_config(ReloadConfigRequest {})
        .await
        .unwrap();
    let err = items
        .build_meeting_link(BuildMeetingLinkRequest {
            ids: vec![a.id.clone()],
            subject: Some("x".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(err.get_details_error_info().unwrap().reason, "MEETINGS_OFF");
    d.stop().await;
}
