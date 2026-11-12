//! The assistant end to end (design 6, ADR-0010): `Refine` queues a job, the
//! runner asks a stand-in for Claude (never the real one), and the answer
//! lands as a `refine` Suggestion, applied or rejected like any other.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use todo_assistant::{Answer, ProviderError, Task, Usage};
use todo_core::new_ulid;
use todo_daemon::assistant::Run;
use todo_daemon::paths::{Env, Os, Paths};
use todo_daemon::{Daemon, DaemonOptions, ShutdownHandle, Started};
use todo_proto::v1::assistant_service_client::AssistantServiceClient;
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::suggestion_service_client::SuggestionServiceClient;
use todo_proto::v1::{
    ApplySuggestionPatchesRequest, AssistantJobState, CaptureItemRequest, CreatedVia,
    GetItemRequest, GetJobRequest, Item, ListJobsRequest, ListSuggestionsRequest, Priority,
    RefineRequest, SuggestionKind, SuggestionSource, UpdateItemRequest,
};
use tokio::task::JoinHandle;
use tonic::Code;
use tonic::transport::Channel;
use tonic_types::StatusExt;

const WAIT: Duration = Duration::from_secs(10);

struct Running {
    _home: TempDir,
    paths: Paths,
    shutdown: ShutdownHandle,
    task: JoinHandle<anyhow::Result<()>>,
    /// The prompts the stand-in was given.
    asked: Arc<Mutex<Vec<String>>>,
}

impl Running {
    async fn channel(&self) -> Channel {
        todo_daemon::client::connect(&self.paths.socket)
            .await
            .unwrap()
    }

    async fn stop(self) {
        self.shutdown.trigger();
        let _ = tokio::time::timeout(WAIT, self.task).await;
    }
}

/// A daemon with `config` and a stand-in answering every task with
/// `answer` (or its error).
async fn start(config: &str, answer: Result<serde_json::Value, ProviderError>) -> Running {
    let home = TempDir::new().unwrap();
    std::fs::write(home.path().join("config.toml"), config).unwrap();
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
    let paths = Paths::resolve(&env).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let run: Run = Arc::new(move |_a, task: Task| {
        seen.lock().unwrap().push(task.prompt);
        let answer = answer.clone();
        Box::pin(async move {
            answer.map(|json| Answer {
                json,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    cached_tokens: 5,
                    cost_usd: 0.001,
                    duration_ms: 900,
                },
            })
        })
    });
    let options = DaemonOptions {
        assistant: run,
        ..DaemonOptions::default()
    };
    let Started::Running(daemon) = Daemon::start_with(paths.clone(), options).await.unwrap() else {
        panic!("lock held");
    };
    let shutdown = daemon.shutdown_handle();
    let task = tokio::spawn(daemon.serve());
    Running {
        _home: home,
        paths,
        shutdown,
        task,
        asked,
    }
}

const ON: &str = "[assistant]\nprovider = \"claude_code\"\nstyle_notes = \"titles imperative\"\n";

async fn capture(items: &mut ItemServiceClient<Channel>, text: &str) -> Item {
    items
        .capture_item(CaptureItemRequest {
            request_id: new_ulid().to_string(),
            text: text.to_owned(),
            created_via: CreatedVia::Cli.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap()
}

/// Waits for the job to end.
async fn done(c: &mut AssistantServiceClient<Channel>, id: &str) -> todo_proto::v1::AssistantJob {
    for _ in 0..200 {
        let job = c
            .get_job(GetJobRequest { id: id.to_owned() })
            .await
            .unwrap()
            .into_inner()
            .job
            .unwrap();
        if job.state == AssistantJobState::Done as i32
            || job.state == AssistantJobState::Failed as i32
        {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the job never ended");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refine_becomes_a_claude_suggestion_and_applies_as_one_op() {
    let answer = json!({
        "rationale": "A follow-up with Dana, due Friday.",
        "title": "Follow up with Dana on the flaky login test",
        "priority": "medium",
        "due_date": "2099-01-02",
        "waiting_on": "Dana",
        "waiting_reason": "the fix"
    });
    let d = start(ON, Ok(answer)).await;
    let mut items = ItemServiceClient::new(d.channel().await);
    let item = capture(&mut items, "ping dana fri re flaky login test").await;
    capture(&mut items, "flaky login test timeouts").await;
    let mut ai = AssistantServiceClient::new(d.channel().await);
    let job = ai
        .refine(RefineRequest {
            item_ids: vec![item.id.clone()],
        })
        .await
        .unwrap()
        .into_inner()
        .job
        .unwrap();
    let job = done(&mut ai, &job.id).await;
    assert_eq!(job.state, AssistantJobState::Done as i32, "{}", job.error);
    assert_eq!(
        (job.input_tokens, job.output_tokens, job.cached_tokens),
        (100, 20, 5)
    );
    assert_eq!(job.suggestion_ids.len(), 1);
    // The prompt held the style notes and the likely duplicate.
    let prompt = d.asked.lock().unwrap()[0].clone();
    assert!(prompt.contains("titles imperative"), "{prompt}");
    assert!(prompt.contains("flaky login test timeouts"), "{prompt}");
    // In the pane: Claude's, a patch per field, the title's as Claude saw it.
    let mut pane = SuggestionServiceClient::new(d.channel().await);
    let all = pane
        .list_suggestions(ListSuggestionsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .suggestions;
    assert_eq!(all.len(), 1);
    let s = &all[0];
    assert_eq!(
        (s.kind, s.source),
        (
            SuggestionKind::Refine as i32,
            SuggestionSource::Claude as i32
        )
    );
    assert_eq!(s.rationale, "A follow-up with Dana, due Friday.");
    assert_eq!(s.patches.len(), 4);
    // Applying them all is one op the person can undo.
    pane.apply_suggestion_patches(ApplySuggestionPatchesRequest {
        request_id: new_ulid().to_string(),
        patch_ids: s.patches.iter().map(|p| p.id.clone()).collect(),
        ..Default::default()
    })
    .await
    .unwrap();
    let got = items
        .get_item(GetItemRequest {
            id: item.id.clone(),
        })
        .await
        .unwrap()
        .into_inner()
        .item
        .unwrap();
    assert_eq!(got.title, "Follow up with Dana on the flaky login test");
    assert_eq!(got.priority, Priority::Medium as i32);
    assert!(got.waiting.is_some());
    let jobs = ai
        .list_jobs(ListJobsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .jobs;
    assert_eq!(jobs.len(), 1);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_meanwhile_makes_its_patch_stale() {
    let d = start(ON, Ok(json!({"rationale": "r", "title": "Clean title"}))).await;
    let mut items = ItemServiceClient::new(d.channel().await);
    let item = capture(&mut items, "messy title").await;
    let mut ai = AssistantServiceClient::new(d.channel().await);
    let job = ai
        .refine(RefineRequest {
            item_ids: vec![item.id.clone()],
        })
        .await
        .unwrap()
        .into_inner()
        .job
        .unwrap();
    done(&mut ai, &job.id).await;
    items
        .update_item(UpdateItemRequest {
            request_id: new_ulid().to_string(),
            item: Some(Item {
                id: item.id.clone(),
                title: "my own".into(),
                ..Item::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["title".into()],
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut pane = SuggestionServiceClient::new(d.channel().await);
    let s = pane
        .list_suggestions(ListSuggestionsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .suggestions
        .remove(0);
    let out = pane
        .apply_suggestion_patches(ApplySuggestionPatchesRequest {
            request_id: new_ulid().to_string(),
            patch_ids: vec![s.patches[0].id.clone()],
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(out.skipped_patch_ids, vec![s.patches[0].id.clone()]);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn off_refuses_a_failing_claude_fails_the_job_and_nothing_to_change_makes_none() {
    let off = start("", Ok(json!({"rationale": ""}))).await;
    let mut items = ItemServiceClient::new(off.channel().await);
    let item = capture(&mut items, "x").await;
    let err = AssistantServiceClient::new(off.channel().await)
        .refine(RefineRequest {
            item_ids: vec![item.id],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(
        err.get_details_error_info().unwrap().reason,
        "ASSISTANT_OFF"
    );
    off.stop().await;

    let failing = start(ON, Err(ProviderError::NotLoggedIn)).await;
    let mut items = ItemServiceClient::new(failing.channel().await);
    let item = capture(&mut items, "x").await;
    let mut ai = AssistantServiceClient::new(failing.channel().await);
    let job = ai
        .refine(RefineRequest {
            item_ids: vec![item.id],
        })
        .await
        .unwrap()
        .into_inner()
        .job
        .unwrap();
    let job = done(&mut ai, &job.id).await;
    assert_eq!(job.state, AssistantJobState::Failed as i32);
    assert!(job.error.contains("not logged in"), "{}", job.error);
    failing.stop().await;

    let quiet = start(
        ON,
        Ok(json!({"rationale": "Already clear.", "title": "Buy milk"})),
    )
    .await;
    let mut items = ItemServiceClient::new(quiet.channel().await);
    let item = capture(&mut items, "Buy milk").await;
    let mut ai = AssistantServiceClient::new(quiet.channel().await);
    let job = ai
        .refine(RefineRequest {
            item_ids: vec![item.id],
        })
        .await
        .unwrap()
        .into_inner()
        .job
        .unwrap();
    let job = done(&mut ai, &job.id).await;
    assert_eq!(job.state, AssistantJobState::Done as i32);
    assert!(job.suggestion_ids.is_empty());
    let mut pane = SuggestionServiceClient::new(quiet.channel().await);
    assert!(
        pane.list_suggestions(ListSuggestionsRequest::default())
            .await
            .unwrap()
            .into_inner()
            .suggestions
            .is_empty()
    );
    // Bad ids are refused.
    let err = ai
        .refine(RefineRequest {
            item_ids: vec!["nope".into()],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    quiet.stop().await;
}
