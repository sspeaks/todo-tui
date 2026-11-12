//! The pull loop end to end (design 5.4, 5.5, 5.11): a daemon over a real
//! socket, its instance a fake Jira Data Center on 127.0.0.1.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use todo_daemon::paths::{Env, Os, Paths};
use todo_daemon::{Daemon, DaemonOptions, ShutdownHandle, Started};
use todo_jira::fake::{FakeJira, issue_json};
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::sync_service_client::SyncServiceClient;
use todo_proto::v1::watch_service_client::WatchServiceClient;
use todo_proto::v1::{
    Filter, GetSyncStatusRequest, Item, ListItemsRequest, Status, SubscribeRequest, SyncNowRequest,
    SyncState, SyncStatus, UpdateItemRequest, notice, subscribe_response::Message,
};
use tonic::Code;
use tonic::transport::Channel;
use tonic_types::StatusExt as _;

struct Running {
    _home: TempDir,
    paths: Paths,
    shutdown: ShutdownHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Running {
    async fn channel(&self) -> Channel {
        todo_daemon::client::connect(&self.paths.socket)
            .await
            .unwrap()
    }

    async fn stop(self) {
        self.shutdown.trigger();
        let _ = self.task.await;
    }
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

/// A daemon whose config names `fake` with `token` (a file credential),
/// its scope `scope`.
async fn start(fake: &FakeJira, token: &str) -> Running {
    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    std::fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let tok = home.path().join("jira.token");
    std::fs::write(&tok, token).unwrap();
    std::fs::set_permissions(&tok, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(
        &paths.config,
        format!(
            "[[instances]]\nid = \"work-jira\"\nkind = \"jira-dc\"\nbase_url = \"{}\"\ncredential = \"file:{}\"\n",
            fake.base_url(),
            tok.display()
        ),
    )
    .unwrap();
    let Started::Running(daemon) = Daemon::start_with(paths.clone(), DaemonOptions::default())
        .await
        .unwrap()
    else {
        panic!("lock held");
    };
    let shutdown = daemon.shutdown_handle();
    let task = tokio::spawn(daemon.serve());
    Running {
        _home: home,
        paths,
        shutdown,
        task,
    }
}

/// The fake answers as UTC (its JQL reading), discovery, and `issues`.
fn serve(fake: &FakeJira, issues: Arc<Mutex<Vec<Value>>>) {
    fake.route("/rest/api/2/myself", |_| {
        (200, json!({"key": "JIRAUSER10100", "name": "dana", "displayName": "Dana Example", "timeZone": "UTC"}), vec![])
    });
    fake.serve_discovery();
    fake.serve_search(issues.clone(), 100);
    // One issue by id: from the list, else gone.
    let by_id = issues;
    fake.route("/rest/api/2/issue/*", move |seen| {
        let id = seen
            .target
            .split('?')
            .next()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();
        match by_id
            .lock()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id.as_str())
        {
            Some(v) => (200, v.clone(), vec![]),
            None => (
                404,
                json!({"errorMessages": ["Issue Does Not Exist"]}),
                vec![],
            ),
        }
    });
}

async fn status(r: &Running) -> SyncStatus {
    SyncServiceClient::new(r.channel().await)
        .get_sync_status(GetSyncStatusRequest {})
        .await
        .unwrap()
        .into_inner()
        .instances
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// Waits until the instance's state is `want` after `since` pulls.
async fn settled(r: &Running, want: SyncState) -> SyncStatus {
    for _ in 0..200 {
        let s = status(r).await;
        if s.state == want as i32 {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never {want:?}: {:?}", status(r).await);
}

async fn linked(r: &Running) -> Vec<Item> {
    let mut items = ItemServiceClient::new(r.channel().await);
    let res = items
        .list_items(ListItemsRequest {
            filter: Some(Filter {
                include_archived: true,
                include_done: true,
                ..Filter::default()
            }),
            ..ListItemsRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let mut out: Vec<Item> = res
        .items
        .into_iter()
        .filter(|i| i.remote_link.is_some())
        .collect();
    out.sort_by_key(|i| i.remote_link.as_ref().unwrap().key.clone());
    out
}

/// Pulls now, and waits for the pass that serves the request: one that
/// began after it (served_request), and has ended.
async fn sync_now(r: &Running) -> SyncStatus {
    let request = SyncServiceClient::new(r.channel().await)
        .sync_now(SyncNowRequest::default())
        .await
        .unwrap()
        .into_inner()
        .request;
    assert!(request > 0);
    for _ in 0..200 {
        let s = status(r).await;
        let running = s.state == SyncState::Syncing as i32 || s.state == SyncState::Starting as i32;
        if s.served_request >= request && !running {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no pass finished: {:?}", status(r).await);
}

fn at(h: u32) -> String {
    format!("2026-11-04T{h:02}:00:00.000+0000")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_pass_imports_the_scope_and_later_ones_follow_it() {
    let fake = FakeJira::start("pat").await;
    let mut shipping = issue_json(2, "Ship the release", "indeterminate", &at(2));
    shipping["fields"]["description"] =
        json!("h2. Steps\r\n * tag {{v2}}\r\n * [notes|https://wiki.example.com/x]");
    let issues = Arc::new(Mutex::new(vec![
        issue_json(1, "Fix the login test", "new", &at(1)),
        shipping,
    ]));
    serve(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    let s = settled(&r, SyncState::Ok).await;
    assert_eq!(s.items, 2);
    let items = linked(&r).await;
    assert_eq!(
        items.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
        ["Fix the login test", "Ship the release"]
    );
    // Jira's wiki markup arrives as Markdown (ADR-0009).
    assert_eq!(
        items[1].description,
        "## Steps\n- tag `v2`\n- [notes](https://wiki.example.com/x)"
    );
    assert_eq!(items[0].status, Status::Next as i32);
    assert_eq!(items[1].status, Status::InProgress as i32);
    let link = items[0].remote_link.as_ref().unwrap();
    assert_eq!(link.url, format!("{}/browse/PLAT-1", fake.base_url()));
    // A remote edit arrives on the next pass; no Item is written twice.
    issues.lock().unwrap()[0]["fields"]["summary"] = json!("Fix the flaky login test");
    issues.lock().unwrap()[0]["fields"]["updated"] = json!(at(3));
    sync_now(&r).await;
    assert_eq!(linked(&r).await[0].title, "Fix the flaky login test");
    // The guard holds end to end for what does not push (ADR-0007).
    let mut client = ItemServiceClient::new(r.channel().await);
    let err = client
        .update_item(UpdateItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            item: Some(Item {
                id: items[0].id.clone(),
                item_type: "Story".into(),
                ..Item::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["item_type".into()],
            }),
            ..UpdateItemRequest::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(
        err.get_details_error_info().unwrap().reason,
        "LINKED_READ_ONLY"
    );
    let ok = client
        .update_item(UpdateItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            item: Some(Item {
                id: items[0].id.clone(),
                private_notes: "mine".into(),
                ..Item::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["private_notes".into()],
            }),
            ..UpdateItemRequest::default()
        })
        .await;
    assert!(ok.is_ok(), "{ok:?}");
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconciliation_archives_what_left_and_breaks_what_is_gone() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![
        issue_json(1, "stays", "new", &at(1)),
        issue_json(2, "reassigned", "new", &at(2)),
        issue_json(3, "deleted", "new", &at(3)),
    ]));
    serve(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    // PLAT-2 leaves the scope but still exists; PLAT-3 is gone. The fake's
    // search lists the scope, its by-id route what still exists.
    let reassigned = issues.lock().unwrap()[1].clone();
    issues.lock().unwrap().retain(|v| v["id"] == "1");
    fake.route("/rest/api/2/issue/2", move |_| {
        (200, reassigned.clone(), vec![])
    });
    // A restart reconciles at once.
    let home_paths = r.paths.clone();
    let _ = home_paths;
    let r = restart(r, &fake).await;
    settled(&r, SyncState::Ok).await;
    let items = linked(&r).await;
    let by = |k: &str| {
        items
            .iter()
            .find(|i| i.remote_link.as_ref().unwrap().key == k)
            .unwrap()
    };
    assert!(by("PLAT-1").archived.is_none());
    assert_eq!(
        by("PLAT-2").archived.as_ref().unwrap().reason,
        todo_proto::v1::ArchiveReason::LeftScope as i32
    );
    assert_eq!(
        by("PLAT-3").remote_link.as_ref().unwrap().link_state,
        todo_proto::v1::LinkState::Broken as i32
    );
    r.stop().await;
}

/// The same home again: a new daemon on the old database and config.
async fn restart(r: Running, _fake: &FakeJira) -> Running {
    let Running {
        _home,
        paths,
        shutdown,
        task,
    } = r;
    shutdown.trigger();
    let _ = task.await;
    let Started::Running(daemon) = Daemon::start_with(paths.clone(), DaemonOptions::default())
        .await
        .unwrap()
    else {
        panic!("lock held");
    };
    let shutdown = daemon.shutdown_handle();
    let task = tokio::spawn(daemon.serve());
    Running {
        _home,
        paths,
        shutdown,
        task,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_token_is_an_auth_error_and_status_reaches_subscribers() {
    let fake = FakeJira::start("right").await;
    serve(&fake, Arc::new(Mutex::new(vec![])));
    let r = start(&fake, "wrong").await;
    let s = settled(&r, SyncState::AuthError).await;
    assert!(s.last_error.contains("401"), "{}", s.last_error);
    // A subscriber is told the state after it caught up.
    let mut stream = WatchServiceClient::new(r.channel().await)
        .subscribe(SubscribeRequest::default())
        .await
        .unwrap()
        .into_inner();
    let mut told = None;
    while let Ok(Some(m)) = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .unwrap()
    {
        if let Some(Message::Notice(n)) = m.message
            && let Some(notice::Kind::SyncStatus(s)) = n.kind
        {
            told = Some(s);
            break;
        }
    }
    assert_eq!(told.unwrap().state, SyncState::AuthError as i32);
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_now_without_an_instance_says_so() {
    let home = TempDir::new().unwrap();
    let paths = paths_for(&home);
    let Started::Running(daemon) = Daemon::start_with(paths.clone(), DaemonOptions::default())
        .await
        .unwrap()
    else {
        panic!("lock held");
    };
    let shutdown = daemon.shutdown_handle();
    let task = tokio::spawn(daemon.serve());
    let r = Running {
        _home: home,
        paths,
        shutdown,
        task,
    };
    let err = SyncServiceClient::new(r.channel().await)
        .sync_now(SyncNowRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(err.get_details_error_info().unwrap().reason, "NO_INSTANCE");
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconciliation_imports_what_the_scope_lists_whatever_its_age() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "new", "new", &at(10))]));
    serve(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    // An issue joins the scope with an update time long before the cursor
    // (an edited scope query, a sprint joined): a pull by cursor cannot see
    // it; a reconcile imports it, and a sync now reconciles.
    issues.lock().unwrap().push(issue_json(
        2,
        "old but now mine",
        "new",
        "2026-01-01T09:00:00.000+0000",
    ));
    sync_now(&r).await;
    let items = linked(&r).await;
    assert_eq!(
        items.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
        ["new", "old but now mine"]
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn status_changes_reach_a_subscriber_live() {
    let fake = FakeJira::start("pat").await;
    serve(
        &fake,
        Arc::new(Mutex::new(vec![issue_json(1, "one", "new", &at(1))])),
    );
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let mut stream = WatchServiceClient::new(r.channel().await)
        .subscribe(SubscribeRequest::default())
        .await
        .unwrap()
        .into_inner();
    SyncServiceClient::new(r.channel().await)
        .sync_now(SyncNowRequest::default())
        .await
        .unwrap();
    let mut states = Vec::new();
    while states.last() != Some(&(SyncState::Ok as i32)) || states.len() < 2 {
        let Ok(Some(m)) = tokio::time::timeout(Duration::from_secs(5), stream.message())
            .await
            .unwrap()
        else {
            panic!("stream ended: {states:?}");
        };
        if let Some(Message::Notice(n)) = m.message
            && let Some(notice::Kind::SyncStatus(s)) = n.kind
        {
            states.push(s.state);
        }
    }
    // The snapshot (ok), then the pass: syncing, then ok again.
    assert!(states.contains(&(SyncState::Syncing as i32)), "{states:?}");
    r.stop().await;
}

/// Resolves issue `n` in `issues` (Done, with a resolution), at `updated`.
fn resolve(issues: &Arc<Mutex<Vec<Value>>>, n: &str, updated: &str) {
    let mut all = issues.lock().unwrap();
    let v = all.iter_mut().find(|v| v["id"] == n).unwrap();
    v["fields"]["status"] = json!({"name": "Done", "statusCategory": {"key": "done"}});
    v["fields"]["resolution"] = json!({"name": "Fixed"});
    v["fields"]["updated"] = json!(updated);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resolution_in_jira_shows_on_the_next_sync_now() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "finish it", "new", &at(1))]));
    serve(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    // Resolved: the default scope (resolution = Unresolved) no longer lists
    // it, so only a read by id sees the resolution.
    resolve(&issues, "1", &at(2));
    sync_now(&r).await;
    let items = linked(&r).await;
    assert_eq!(items[0].status, Status::Done as i32);
    assert!(items[0].archived.is_none(), "done, not left scope");
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_item_undeleted_catches_up_with_what_changed_while_deleted() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![
        issue_json(1, "old title", "new", &at(1)),
        issue_json(2, "other", "new", &at(1)),
    ]));
    serve(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    let mut client = ItemServiceClient::new(r.channel().await);
    client
        .delete_item(todo_proto::v1::DeleteItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            id: id.clone(),
            expected_revision: None,
        })
        .await
        .unwrap();
    // Changed in Jira while deleted here: the pull skips it, and its cursor
    // moves past the change.
    issues.lock().unwrap()[0]["fields"]["summary"] = json!("new title");
    issues.lock().unwrap()[0]["fields"]["updated"] = json!(at(2));
    sync_now(&r).await;
    // Later changes take the cursor hours past it.
    issues.lock().unwrap()[1]["fields"]["updated"] = json!(at(6));
    sync_now(&r).await;
    client
        .undelete_item(todo_proto::v1::UndeleteItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            id,
            expected_revision: None,
        })
        .await
        .unwrap();
    sync_now(&r).await;
    assert_eq!(linked(&r).await[0].title, "new title");
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issue_unreadable_for_a_while_heals_when_readable_again() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "restricted", "new", &at(1))]));
    serve(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    // Unreadable (a permission scheme): gone from every search.
    let saved = issues.lock().unwrap().remove(0);
    sync_now(&r).await;
    let items = linked(&r).await;
    assert_eq!(
        items[0].remote_link.as_ref().unwrap().link_state,
        todo_proto::v1::LinkState::Broken as i32
    );
    // Readable again, its update time unchanged.
    issues.lock().unwrap().push(saved);
    sync_now(&r).await;
    let items = linked(&r).await;
    assert_eq!(
        items[0].remote_link.as_ref().unwrap().link_state,
        todo_proto::v1::LinkState::Ok as i32
    );
    assert!(items[0].archived.is_none(), "{:?}", items[0].archived);
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_item_outside_every_scope_follows_jira_by_jiras_own_clock() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "left", "new", &at(1))]));
    serve(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    // Resolved, so out of the scope; done, so tracked outside it, unarchived.
    resolve(&issues, "1", &at(2));
    sync_now(&r).await;
    assert!(linked(&r).await[0].remote_link.is_some());
    // An edit dated before this machine's clock (Jira's clock behind ours):
    // still read, as the outside cursor is the newest update Jira gave.
    issues.lock().unwrap()[0]["fields"]["summary"] = json!("edited outside");
    issues.lock().unwrap()[0]["fields"]["updated"] = json!(at(3));
    sync_now(&r).await;
    assert_eq!(linked(&r).await[0].title, "edited outside");
    r.stop().await;
}

/// serve(), with writes taken as BRME's workflow takes them (ADR-0007).
fn serve_writable(fake: &FakeJira, issues: Arc<Mutex<Vec<Value>>>) {
    serve(fake, issues.clone());
    let n = Arc::new(std::sync::atomic::AtomicU32::new(10));
    fake.serve_writes(issues, move || {
        let h = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("2026-11-04T{h:02}:00:00.000+0000")
    });
}

/// Waits until `check` holds of the linked Items, syncing now meanwhile.
async fn until(r: &Running, what: &str, check: impl Fn(&[Item]) -> bool) -> Vec<Item> {
    for _ in 0..40 {
        let items = linked(r).await;
        if check(&items) {
            return items;
        }
        sync_now(r).await;
    }
    panic!("never {what}: {:?}", linked(r).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_and_a_close_reach_jira_and_read_back() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "old title", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    let mut client = ItemServiceClient::new(r.channel().await);
    client
        .update_item(UpdateItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            item: Some(Item {
                id: id.clone(),
                title: "new title".into(),
                ..Item::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["title".into()],
            }),
            ..UpdateItemRequest::default()
        })
        .await
        .unwrap();
    client
        .set_status(todo_proto::v1::SetStatusRequest {
            request_id: todo_core::new_ulid().to_string(),
            id: id.clone(),
            status: Status::Done as i32,
            ..Default::default()
        })
        .await
        .unwrap();
    // The edit woke the loop; Jira has both, and the pull reads them back.
    let items = until(&r, "pushed", |items| items[0].pending_fields.is_empty()).await;
    let jira = issues.lock().unwrap()[0].clone();
    assert_eq!(jira["fields"]["summary"], "new title");
    assert_eq!(jira["fields"]["status"]["name"], "Closed");
    assert_eq!(jira["fields"]["resolution"]["name"], "Done");
    assert_eq!(
        (items[0].title.as_str(), items[0].status),
        ("new title", Status::Done as i32)
    );
    assert_eq!(status(&r).await.pending_ops, 0);
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_description_reaches_jira_as_wiki_and_keeps_what_did_not_convert() {
    use todo_proto::v1::ListSuggestionsRequest;
    use todo_proto::v1::suggestion_service_client::SuggestionServiceClient;
    let fake = FakeJira::start("pat").await;
    let mut one = issue_json(1, "notes", "new", &at(1));
    one["fields"]["description"] =
        json!("h2. Steps\r\n* tag {{v2}}\r\n\r\n{color:#888}footer{color}\r\n{panel}box{panel}");
    let issues = Arc::new(Mutex::new(vec![one]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let item = linked(&r).await.remove(0);
    let edited = item.description.replace("tag `v2`", "tag `v3` **now**");
    assert_ne!(edited, item.description);
    let mut client = ItemServiceClient::new(r.channel().await);
    client
        .update_item(UpdateItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            item: Some(Item {
                id: item.id.clone(),
                description: edited.clone(),
                ..Item::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["description".into()],
            }),
            ..UpdateItemRequest::default()
        })
        .await
        .unwrap();
    // Sent as wiki, the rest as it was; read back, it settles as it is.
    let items = until(&r, "pushed", |items| items[0].pending_fields.is_empty()).await;
    assert_eq!(
        issues.lock().unwrap()[0]["fields"]["description"],
        "h2. Steps\n* tag {{v3}} *now*\n\n{color:#888}footer{color}\n{panel}box{panel}"
    );
    assert_eq!(items[0].description, edited);
    let mut pane = SuggestionServiceClient::new(r.channel().await);
    let waiting = pane
        .list_suggestions(ListSuggestionsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .suggestions;
    assert!(waiting.is_empty(), "{waiting:?}");
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_jira_refuses_becomes_a_failure_and_jiras_value_stands() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "kept", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    // This Jira refuses any due date.
    fake.route("/rest/api/2/issue/1", |seen| {
        if seen.method == "PUT" {
            (
                400,
                json!({"errorMessages": [], "errors": {"duedate": "You cannot set a due date"}}),
                vec![],
            )
        } else {
            (200, issue_json(1, "kept", "new", &at(1)), vec![])
        }
    });
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    ItemServiceClient::new(r.channel().await)
        .update_item(UpdateItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            item: Some(Item {
                id: id.clone(),
                due_date: Some(todo_proto::google::r#type::Date {
                    year: 2026,
                    month: 12,
                    day: 1,
                }),
                ..Item::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["due_date".into()],
            }),
            ..UpdateItemRequest::default()
        })
        .await
        .unwrap();
    let items = until(&r, "refused", |items| items[0].has_push_failure).await;
    assert_eq!(items[0].due_date, None, "Jira's value stands");
    assert!(items[0].pending_fields.is_empty());
    assert_eq!(status(&r).await.failed_ops, 1);
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_close_whose_answer_was_lost_is_read_back_not_sent_twice() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "once", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    // Jira applies the first close but its answer is lost (a 502 after the
    // write), as a proxy timing out would.
    let closes = Arc::new(std::sync::atomic::AtomicU32::new(0));
    {
        let (issues, closes) = (issues.clone(), closes.clone());
        fake.route("/rest/api/2/issue/1/transitions", move |seen| {
            if seen.method == "POST" {
                closes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut all = issues.lock().unwrap();
                let f = &mut all[0]["fields"];
                f["status"] = json!({"name": "Closed", "statusCategory": {"key": "done"}});
                f["resolution"] = json!({"name": "Done"});
                f["updated"] = json!(at(5));
                return (502, json!({"errorMessages": ["bad gateway"]}), vec![]);
            }
            (200, json!({"transitions": [{"id": "71", "name": "Closed",
                "to": {"name": "Closed", "statusCategory": {"key": "done"}},
                "fields": {"resolution": {"required": true, "allowedValues": [{"id": "10000", "name": "Done"}]}}}]}), vec![])
        });
    }
    ItemServiceClient::new(r.channel().await)
        .set_status(todo_proto::v1::SetStatusRequest {
            request_id: todo_core::new_ulid().to_string(),
            id,
            status: Status::Done as i32,
            ..Default::default()
        })
        .await
        .unwrap();
    // The retry reads first, finds it closed, and settles.
    until(&r, "settled", |items| {
        items[0].pending_fields.is_empty() && items[0].status == Status::Done as i32
    })
    .await;
    assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn someone_elses_issue_takes_a_pushed_edit_only_when_confirmed() {
    let fake = FakeJira::start("pat").await;
    let mut theirs = issue_json(1, "theirs", "new", &at(1));
    theirs["fields"]["assignee"] =
        json!({"key": "JIRAUSER2", "name": "alex", "displayName": "Alex"});
    let issues = Arc::new(Mutex::new(vec![theirs]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    let mut client = ItemServiceClient::new(r.channel().await);
    let edit = |confirm| UpdateItemRequest {
        request_id: todo_core::new_ulid().to_string(),
        item: Some(Item {
            id: id.clone(),
            title: "mine now".into(),
            ..Item::default()
        }),
        update_mask: Some(prost_types::FieldMask {
            paths: vec!["title".into()],
        }),
        confirm_not_mine: confirm,
        ..UpdateItemRequest::default()
    };
    let err = client.update_item(edit(false)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(err.get_details_error_info().unwrap().reason, "NOT_MINE");
    // A repeat rule sets the due date, which pushes: the same guard, and
    // the same confirmation takes it.
    let rule = |confirm| todo_proto::v1::SetRecurrenceRequest {
        request_id: todo_core::new_ulid().to_string(),
        id: id.clone(),
        rule: Some((&todo_core::Rule::Daily).into()),
        expected_revision: None,
        confirm_not_mine: confirm,
    };
    let err = client.set_recurrence(rule(false)).await.unwrap_err();
    assert_eq!(err.get_details_error_info().unwrap().reason, "NOT_MINE");
    client.set_recurrence(rule(true)).await.unwrap();
    // Overlay stays theirs to touch freely; a confirmed edit pushes.
    client.update_item(edit(true)).await.unwrap();
    until(&r, "pushed", |items| {
        items[0].pending_fields.is_empty() && items[0].title == "mine now"
    })
    .await;
    assert_eq!(issues.lock().unwrap()[0]["fields"]["summary"], "mine now");
    r.stop().await;
}

fn edit_title(id: &str, title: &str) -> UpdateItemRequest {
    UpdateItemRequest {
        request_id: todo_core::new_ulid().to_string(),
        item: Some(Item {
            id: id.to_owned(),
            title: title.into(),
            ..Item::default()
        }),
        update_mask: Some(prost_types::FieldMask {
            paths: vec!["title".into()],
        }),
        ..UpdateItemRequest::default()
    }
}

fn close(id: &str) -> todo_proto::v1::SetStatusRequest {
    todo_proto::v1::SetStatusRequest {
        request_id: todo_core::new_ulid().to_string(),
        id: id.to_owned(),
        status: Status::Done as i32,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_close_leaves_a_title_jira_took() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "old", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    // This workflow offers no way to close.
    fake.route("/rest/api/2/issue/1/transitions", |_| {
        (200, json!({"transitions": []}), vec![])
    });
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let before = linked(&r).await[0].clone();
    let id = before.id.clone();
    let mut client = ItemServiceClient::new(r.channel().await);
    client.update_item(edit_title(&id, "new")).await.unwrap();
    client.set_status(close(&id)).await.unwrap();
    let items = until(&r, "refused", |items| items[0].has_push_failure).await;
    assert_eq!(issues.lock().unwrap()[0]["fields"]["summary"], "new");
    assert_eq!(items[0].title, "new");
    assert_eq!(items[0].status, before.status, "Jira's status stands");
    // Only the close failed (one change: its category and kind), not the
    // title.
    let st = status(&r).await;
    assert_eq!((st.failed_ops, st.pending_ops), (1, 0));
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rate_limit_stops_the_batch_and_the_retry_comes_when_due() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![
        issue_json(1, "one", "new", &at(1)),
        issue_json(2, "two", "new", &at(1)),
    ]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    // Jira asks for 2 s on the first PUT, then takes them.
    let limited = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let puts = Arc::new(std::sync::atomic::AtomicU32::new(0));
    {
        let (issues, limited, puts) = (issues.clone(), limited.clone(), puts.clone());
        fake.route("/rest/api/2/issue/*", move |seen| {
            let id = seen
                .target
                .split('?')
                .next()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_owned();
            let mut all = issues.lock().unwrap();
            let Some(issue) = all.iter_mut().find(|v| v["id"] == id.as_str()) else {
                return (
                    404,
                    json!({"errorMessages": ["Issue Does Not Exist"]}),
                    vec![],
                );
            };
            if seen.method != "PUT" {
                return (200, issue.clone(), vec![]);
            }
            if limited.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return (
                    429,
                    json!({"errorMessages": ["slow down"]}),
                    vec![("Retry-After".into(), "2".into())],
                );
            }
            puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body: Value = serde_json::from_str(&seen.body).unwrap_or_default();
            issue["fields"]["summary"] = body["fields"]["summary"].clone();
            issue["fields"]["updated"] = json!(at(9));
            (204, Value::Null, vec![])
        });
    }
    let mut client = ItemServiceClient::new(r.channel().await);
    let items = linked(&r).await;
    for (n, item) in items.iter().enumerate() {
        client
            .update_item(edit_title(&item.id, &format!("pushed {n}")))
            .await
            .unwrap();
    }
    // The 429 ends the pass Offline before the second Item is sent.
    let st = settled(&r, SyncState::Offline).await;
    assert!(st.last_error.contains("rate"), "{st:?}");
    assert_eq!(puts.load(std::sync::atomic::Ordering::SeqCst), 0);
    // Then, without a sync now, both go once the wait is up.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while puts.load(std::sync::atomic::Ordering::SeqCst) < 2 {
        assert!(tokio::time::Instant::now() < deadline, "never retried");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    settled(&r, SyncState::Ok).await;
    // And the hold is over: the loop waits for its poll (a minute), not a
    // pass a second.
    let passes = || {
        fake.seen()
            .iter()
            .filter(|s| s.target.starts_with("/rest/api/2/myself"))
            .count()
    };
    let before = passes();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        passes() - before <= 1,
        "{} passes in 4 s",
        passes() - before
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reload_to_push_false_takes_effect_at_once() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "one", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    let mut client = ItemServiceClient::new(r.channel().await);
    // Hold the next pass in its pull (at most 5 s), so the reload lands
    // mid-pass.
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (gate, held) = (gate.clone(), held.clone());
        fake.route("/rest/api/2/myself", move |_| {
            held.store(true, std::sync::atomic::Ordering::SeqCst);
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while !gate.load(std::sync::atomic::Ordering::SeqCst) && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            (200, json!({"key": "JIRAUSER10100", "name": "dana", "displayName": "Dana Example", "timeZone": "UTC"}), vec![])
        });
    }
    client
        .update_item(edit_title(&id, "pushed?"))
        .await
        .unwrap();
    // The edit wakes a pass, which is held at /myself.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !held.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(tokio::time::Instant::now() < deadline, "no pass began");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The pass is under way; push = false, and reload.
    let config = std::fs::read_to_string(&r.paths.config).unwrap();
    std::fs::write(&r.paths.config, format!("{config}push = false\n")).unwrap();
    todo_proto::v1::system_service_client::SystemServiceClient::new(r.channel().await)
        .reload_config(todo_proto::v1::ReloadConfigRequest {})
        .await
        .unwrap();
    // At once: a new edit of a pushed field is refused as read-only…
    let err = client
        .update_item(edit_title(&id, "again"))
        .await
        .unwrap_err();
    assert_eq!(
        err.get_details_error_info().unwrap().reason,
        "LINKED_READ_ONLY"
    );
    // …and the pass under way sends nothing once it gets there: its
    // read-back of the issue would follow a push, so wait for the pass to
    // end (a search after the gate), then a little.
    gate.store(true, std::sync::atomic::Ordering::SeqCst);
    let before = fake.seen().len();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !fake.seen()[before..]
        .iter()
        .any(|s| s.target.contains("/search"))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pass never pulled"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    settled(&r, SyncState::Ok).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !fake.seen().iter().any(|s| s.method == "PUT"),
        "a PUT after push = false"
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_backs_off_even_with_a_push_waiting() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "one", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    // The push meets a 429 (wait 1 s), and from then on Jira is down (a 503
    // at the first call of a pass): the retry falls due while every pass
    // fails before it pushes.
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (issues, down) = (issues.clone(), down.clone());
        fake.route("/rest/api/2/issue/*", move |seen| {
            if seen.method == "PUT" {
                down.store(true, std::sync::atomic::Ordering::SeqCst);
                return (
                    429,
                    json!({"errorMessages": ["slow down"]}),
                    vec![("Retry-After".into(), "1".into())],
                );
            }
            (200, issues.lock().unwrap()[0].clone(), vec![])
        });
    }
    {
        let down = down.clone();
        fake.route("/rest/api/2/myself", move |_| {
            if down.load(std::sync::atomic::Ordering::SeqCst) {
                return (503, json!({"errorMessages": ["down"]}), vec![]);
            }
            (200, json!({"key": "JIRAUSER10100", "name": "dana", "displayName": "Dana Example", "timeZone": "UTC"}), vec![])
        });
    }
    let mut client = ItemServiceClient::new(r.channel().await);
    let item = linked(&r).await.remove(0);
    client
        .update_item(edit_title(&item.id, "later"))
        .await
        .unwrap();
    // The 429's second over, a pass meets the 503.
    let myself_503 = || {
        fake.seen()
            .iter()
            .filter(|s| s.target.starts_with("/rest/api/2/myself"))
            .count()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !down.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(tokio::time::Instant::now() < deadline, "never pushed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let first = myself_503();
    while myself_503() == first {
        assert!(tokio::time::Instant::now() < deadline, "never retried");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Offline now waits 30 s: in 5 s no further pass, not one a second.
    let before = myself_503();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let passes = myself_503() - before;
    assert!(passes <= 1, "{passes} passes in 5 s");
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_priority_jira_has_no_name_for_settles_as_the_one_it_reads_back_as() {
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "one", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    // Three priorities: High, Medium and Low read back as Highest, Medium
    // and Lowest; local High pushes as "High" (its nearest).
    fake.route("/rest/api/2/priority", |_| {
        (
            200,
            json!([{"name": "High"}, {"name": "Medium"}, {"name": "Low"}]),
            vec![],
        )
    });
    issues.lock().unwrap()[0]["fields"]["priority"] = json!({"name": "Medium"});
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    ItemServiceClient::new(r.channel().await)
        .update_item(UpdateItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            item: Some(Item {
                id: id.clone(),
                priority: todo_proto::v1::Priority::High as i32,
                ..Item::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["priority".into()],
            }),
            ..UpdateItemRequest::default()
        })
        .await
        .unwrap();
    let items = until(&r, "settled", |items| items[0].pending_fields.is_empty()).await;
    assert_eq!(
        issues.lock().unwrap()[0]["fields"]["priority"]["name"],
        "High"
    );
    assert_eq!(items[0].priority, todo_proto::v1::Priority::Highest as i32);
    assert!(!items[0].has_conflict, "no conflict with its own push");
    let st = status(&r).await;
    assert_eq!((st.pending_ops, st.failed_ops), (0, 0));
    r.stop().await;
}

/// Waits until the instance's outbox counts are `want` (pending, failed).
async fn counts(r: &Running, want: (u32, u32)) {
    for _ in 0..200 {
        let s = status(r).await;
        if (s.pending_ops, s.failed_ops) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let s = status(r).await;
    panic!("counts never {want:?}: {:?}", (s.pending_ops, s.failed_ops));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflict_reapplied_through_the_suggestions_pushes_yours() {
    use todo_proto::v1::suggestion_service_client::SuggestionServiceClient;
    use todo_proto::v1::{ApplySuggestionPatchesRequest, ListSuggestionsRequest, SuggestionKind};
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "theirs", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    // Writes refused at first: the edit waits, as offline.
    let refuse = Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let (issues, refuse) = (issues.clone(), refuse.clone());
        let n = Arc::new(std::sync::atomic::AtomicU32::new(10));
        fake.route("/rest/api/2/issue/1", move |seen| {
            let mut all = issues.lock().unwrap();
            match seen.method.as_str() {
                "PUT" if refuse.load(std::sync::atomic::Ordering::SeqCst) => {
                    (503, json!({"errorMessages": ["down"]}), vec![])
                }
                "PUT" => {
                    let body: Value = serde_json::from_str(&seen.body).unwrap();
                    for (k, v) in body["fields"].as_object().unwrap() {
                        all[0]["fields"][k] = v.clone();
                    }
                    let h = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    all[0]["fields"]["updated"] =
                        json!(format!("2026-11-04T{h:02}:00:00.000+0000"));
                    (204, Value::Null, vec![])
                }
                _ => (200, all[0].clone(), vec![]),
            }
        });
    }
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    let mut client = ItemServiceClient::new(r.channel().await);
    client.update_item(edit_title(&id, "mine")).await.unwrap();
    // Counted at once, before any pass.
    counts(&r, (1, 0)).await;
    // Someone renames it in Jira; the next pull lets Jira win, yours kept.
    issues.lock().unwrap()[0]["fields"]["summary"] = json!("theirs, renamed");
    issues.lock().unwrap()[0]["fields"]["updated"] = json!(at(5));
    let items = until(&r, "a conflict", |items| items[0].has_conflict).await;
    assert_eq!(items[0].title, "theirs, renamed");
    counts(&r, (0, 0)).await;
    let mut pane = SuggestionServiceClient::new(r.channel().await);
    let all = pane
        .list_suggestions(ListSuggestionsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .suggestions;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, SuggestionKind::SyncConflict as i32);
    assert!(
        all[0].rationale.contains("the title on PLAT-1"),
        "{:?}",
        all[0]
    );
    // Re-applied (Jira writable now): it pushes, and the pane empties.
    refuse.store(false, std::sync::atomic::Ordering::SeqCst);
    let out = pane
        .apply_suggestion_patches(ApplySuggestionPatchesRequest {
            request_id: todo_core::new_ulid().to_string(),
            patch_ids: vec![all[0].patches[0].id.clone()],
            confirm_not_mine: false,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(out.skipped_patch_ids.is_empty());
    assert_eq!(out.items[0].title, "mine");
    let items = until(&r, "pushed", |items| items[0].pending_fields.is_empty()).await;
    assert_eq!(items[0].title, "mine");
    assert!(!items[0].has_conflict);
    assert_eq!(issues.lock().unwrap()[0]["fields"]["summary"], "mine");
    assert!(
        pane.list_suggestions(ListSuggestionsRequest::default())
            .await
            .unwrap()
            .into_inner()
            .suggestions
            .is_empty()
    );
    // An unknown patch: NOT_FOUND, PATCH_NOT_FOUND.
    // Not a ULID: refused as an argument, not echoed back as a lookup.
    let err = pane
        .apply_suggestion_patches(ApplySuggestionPatchesRequest {
            request_id: todo_core::new_ulid().to_string(),
            patch_ids: vec!["nope".into()],
            confirm_not_mine: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    let err = pane
        .apply_suggestion_patches(ApplySuggestionPatchesRequest {
            request_id: todo_core::new_ulid().to_string(),
            patch_ids: vec![todo_core::new_ulid().to_string()],
            confirm_not_mine: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert_eq!(
        err.get_details_error_info().map(|i| i.reason),
        Some("PATCH_NOT_FOUND".into())
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_outbox_lists_a_refusal_until_its_suggestion_is_rejected() {
    use todo_proto::v1::suggestion_service_client::SuggestionServiceClient;
    use todo_proto::v1::{
        ListOutboxRequest, ListSuggestionsRequest, OutboxState, RejectSuggestionRequest,
    };
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "kept", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    // No way to close.
    fake.route("/rest/api/2/issue/1/transitions", |_| {
        (200, json!({"transitions": []}), vec![])
    });
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let id = linked(&r).await[0].id.clone();
    let mut client = ItemServiceClient::new(r.channel().await);
    client.set_status(close(&id)).await.unwrap();
    until(&r, "refused", |items| items[0].has_push_failure).await;
    counts(&r, (0, 1)).await;
    let mut sync = SyncServiceClient::new(r.channel().await);
    let entries = sync
        .list_outbox(ListOutboxRequest::default())
        .await
        .unwrap()
        .into_inner()
        .entries;
    assert_eq!(entries.len(), 2, "the category and the resolution kind");
    assert!(entries.iter().all(|e| e.state == OutboxState::Failed as i32
        && e.remote_key == "PLAT-1"
        && e.title == "kept"
        && !e.last_error.is_empty()));
    let mut pane = SuggestionServiceClient::new(r.channel().await);
    let all = pane
        .list_suggestions(ListSuggestionsRequest {
            item_id: Some(id.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .suggestions;
    assert!(!all[0].error.is_empty());
    let out = pane
        .reject_suggestion(RejectSuggestionRequest {
            request_id: todo_core::new_ulid().to_string(),
            id: all[0].id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        out.suggestion.unwrap().state,
        todo_proto::v1::SuggestionState::Rejected as i32
    );
    counts(&r, (0, 0)).await;
    assert!(
        sync.list_outbox(ListOutboxRequest::default())
            .await
            .unwrap()
            .into_inner()
            .entries
            .is_empty()
    );
    assert!(!linked(&r).await[0].has_push_failure);
    r.stop().await;
}

/// A local todo, captured.
async fn local(r: &Running, text: &str) -> Item {
    ItemServiceClient::new(r.channel().await)
        .capture_item(todo_proto::v1::CaptureItemRequest {
            request_id: todo_core::new_ulid().to_string(),
            text: text.to_owned(),
            created_via: todo_proto::v1::CreatedVia::Cli.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap()
}

/// The daemon's config with `extra` appended to its instance, reloaded.
async fn reconfigure(r: &Running, extra: &str) {
    let config = std::fs::read_to_string(&r.paths.config).unwrap();
    std::fs::write(&r.paths.config, format!("{config}{extra}")).unwrap();
    todo_proto::v1::system_service_client::SystemServiceClient::new(r.channel().await)
        .reload_config(todo_proto::v1::ReloadConfigRequest {})
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn promote_creates_the_issue_and_links_the_todo_to_it() {
    use todo_proto::v1::PromoteRequest;
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "existing", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let todo = local(&r, "Ship the report +2 #work").await;
    let mut sync = SyncServiceClient::new(r.channel().await);
    // No project and no promote_default: says what to set.
    let err = sync
        .promote(PromoteRequest {
            item_id: todo.id.clone(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.get_details_error_info().unwrap().reason,
        "NO_PROMOTE_TARGET"
    );
    // A project Jira refuses: what is missing, by field.
    let err = sync
        .promote(PromoteRequest {
            item_id: todo.id.clone(),
            project: "OPS".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    let info = err.get_details_error_info().unwrap();
    assert_eq!(
        (info.reason.as_str(), info.metadata["fields"].as_str()),
        ("MISSING_FIELDS", "issuetype")
    );
    reconfigure(
        &r,
        "promote_default = { project = \"PLAT\", issue_type = \"Task\" }\n",
    )
    .await;
    let out = sync
        .promote(PromoteRequest {
            item_id: todo.id.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let item = out.item.unwrap();
    let link = item.remote_link.as_ref().unwrap();
    assert_eq!((link.key.as_str(), link.pinned), ("PLAT-2", true));
    assert_eq!(item.id, todo.id);
    // Jira has it, with the todo's title, priority and labels.
    let jira = issues.lock().unwrap()[1].clone();
    assert_eq!(jira["fields"]["summary"], "Ship the report");
    assert_eq!(jira["fields"]["labels"], json!(["work"]));
    assert_eq!(jira["fields"]["priority"]["name"], "Critical");
    // Once linked, promote again is refused.
    let err = sync
        .promote(PromoteRequest {
            item_id: todo.id.clone(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.get_details_error_info().unwrap().reason,
        "ALREADY_LINKED"
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_links_a_todo_to_an_issue_and_offers_back_what_jira_replaced() {
    use todo_proto::v1::AttachRequest;
    use todo_proto::v1::ListSuggestionsRequest;
    use todo_proto::v1::suggestion_service_client::SuggestionServiceClient;
    let fake = FakeJira::start("pat").await;
    let mut theirs = issue_json(7, "Fix the login test", "new", &at(1));
    theirs["key"] = json!("PLAT-7");
    theirs["fields"]["assignee"] =
        json!({"key": "JIRAUSER2", "name": "alex", "displayName": "Alex Example"});
    // Alex's issue is outside the person's scope: searches list none of it,
    // while the issue reads by key.
    let issues = Arc::new(Mutex::new(vec![theirs]));
    serve_writable(&fake, issues.clone());
    fake.serve_search(Arc::new(Mutex::new(Vec::new())), 100);
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    let todo = local(&r, "look at the login test +1").await;
    let mut sync = SyncServiceClient::new(r.channel().await);
    let err = sync
        .attach(AttachRequest {
            item_id: todo.id.clone(),
            key: "PLAT-99".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let err = sync
        .attach(AttachRequest {
            item_id: todo.id.clone(),
            key: "not a key".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    // A project key may have an underscore: a key, just not one Jira has.
    let err = sync
        .attach(AttachRequest {
            item_id: todo.id.clone(),
            key: "MY_PROJ-12".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    // Someone else's issue links, pinned; Jira's title wins, yours is offered back.
    let out = sync
        .attach(AttachRequest {
            item_id: todo.id.clone(),
            key: "plat-7".into(),
        })
        .await
        .unwrap()
        .into_inner();
    let item = out.item.unwrap();
    assert_eq!(item.title, "Fix the login test");
    assert!(item.remote_link.as_ref().unwrap().pinned);
    assert!(!out.suggestion_id.is_empty());
    let s = SuggestionServiceClient::new(r.channel().await)
        .list_suggestions(ListSuggestionsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .suggestions;
    assert_eq!(s.len(), 1);
    assert_eq!(
        s[0].kind,
        todo_proto::v1::SuggestionKind::LinkConflict as i32
    );
    let fields: Vec<i32> = s[0].patches.iter().map(|p| p.field).collect();
    assert!(
        fields.contains(&(todo_proto::v1::ItemField::Title as i32)),
        "{fields:?}"
    );
    // Pinned: a pass with reconciliation keeps it, outside every scope.
    sync_now(&r).await;
    let kept = linked(&r)
        .await
        .into_iter()
        .find(|i| i.id == todo.id)
        .unwrap();
    assert!(kept.archived.is_none(), "{:?}", kept.archived);
    // Read-only: refused.
    reconfigure(&r, "push = false\n").await;
    let other = local(&r, "another").await;
    let err = sync
        .attach(AttachRequest {
            item_id: other.id,
            key: "PLAT-7".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.get_details_error_info().unwrap().reason, "PUSH_OFF");
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_promote_whose_answer_is_lost_finds_the_issue_rather_than_make_another() {
    use todo_proto::v1::PromoteRequest;
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "existing", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    reconfigure(
        &r,
        "promote_default = { project = \"PLAT\", issue_type = \"Task\" }\n",
    )
    .await;
    // Jira makes the issue, then its answer is a gateway's 502.
    let made = issues.clone();
    fake.route("/rest/api/2/issue", move |seen| {
        let body: Value = serde_json::from_str(&seen.body).unwrap();
        let mut issue = issue_json(
            2,
            body["fields"]["summary"].as_str().unwrap(),
            "new",
            &at(2),
        );
        issue["fields"]["created"] = json!(
            jiff::Timestamp::now()
                .strftime("%Y-%m-%dT%H:%M:%S%.3f+0000")
                .to_string()
        );
        made.lock().unwrap().push(issue);
        (502, json!({"errorMessages": ["Bad Gateway"]}), vec![])
    });
    let todo = local(&r, "Do NOT ship the report").await;
    let out = SyncServiceClient::new(r.channel().await)
        .promote(PromoteRequest {
            item_id: todo.id.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let link = out.item.unwrap().remote_link.unwrap();
    assert_eq!(link.key, "PLAT-2");
    assert_eq!(issues.lock().unwrap().len(), 2, "one issue made, not two");
    let posts = fake
        .seen()
        .into_iter()
        .filter(|s| s.method == "POST" && s.target == "/rest/api/2/issue")
        .count();
    assert_eq!(posts, 1);
    let search = fake
        .seen()
        .into_iter()
        .rev()
        .find(|s| s.target == "/rest/api/2/search" && s.body.contains("currentUser()"))
        .unwrap();
    assert!(
        search.body.contains("do not ship the report"),
        "{}",
        search.body
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_promotes_of_one_todo_make_one_issue() {
    use todo_proto::v1::PromoteRequest;
    let fake = FakeJira::start("pat").await;
    let issues = Arc::new(Mutex::new(vec![issue_json(1, "existing", "new", &at(1))]));
    serve_writable(&fake, issues.clone());
    let r = start(&fake, "pat").await;
    settled(&r, SyncState::Ok).await;
    reconfigure(
        &r,
        "promote_default = { project = \"PLAT\", issue_type = \"Task\" }\n",
    )
    .await;
    let todo = local(&r, "Ship the report").await;
    let promote = || {
        let (channel, id) = (r.channel(), todo.id.clone());
        async move {
            SyncServiceClient::new(channel.await)
                .promote(PromoteRequest {
                    item_id: id,
                    ..Default::default()
                })
                .await
        }
    };
    let (a, b) = tokio::join!(promote(), promote());
    let reasons: Vec<String> = [&a, &b]
        .iter()
        .filter_map(|r| r.as_ref().err())
        .map(|e| e.get_details_error_info().unwrap().reason)
        .collect();
    assert_eq!(reasons, ["ALREADY_LINKED"], "{a:?} {b:?}");
    assert_eq!(issues.lock().unwrap().len(), 2, "one issue made");
    r.stop().await;
}
