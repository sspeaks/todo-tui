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
    let issues = Arc::new(Mutex::new(vec![
        issue_json(1, "Fix the login test", "new", &at(1)),
        issue_json(2, "Ship the release", "indeterminate", &at(2)),
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
                description: "mine".into(),
                ..Item::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["description".into()],
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
