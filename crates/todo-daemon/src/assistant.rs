//! The assistant's job runner (design 6.3, ADR-0010): refines asked for by a
//! person, one at a time, through the person's own Claude Code CLI (or, in
//! tests, a stand-in). A job reads the Items, builds the context, asks
//! Claude, keeps what is valid, and leaves a `refine` Suggestion per Item.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use jiff::{Timestamp, Zoned};
use todo_assistant::{Answer, ClaudeCode, Provider, ProviderError, Task, refine};
use todo_core::{ItemField, ItemId};
use todo_store::Store;
use todo_store::assistant::{JobEnd, Refined};
use tokio::sync::mpsc;

use crate::config::{Assistant, ConfigHandle};
use crate::shutdown::ShutdownSignal;

/// Runs a task: the configured CLI, or a stand-in for tests.
pub type Run = Arc<
    dyn Fn(Assistant, Task) -> Pin<Box<dyn Future<Output = Result<Answer, ProviderError>> + Send>>
        + Send
        + Sync,
>;

/// The person's Claude Code CLI, as `[assistant]` names it.
pub fn claude_code() -> Run {
    Arc::new(|a: Assistant, task: Task| {
        Box::pin(async move {
            let cc = ClaudeCode {
                bin: a.claude_bin,
                model: a.model,
                timeout: a.timeout,
            };
            cc.run(task).await
        })
    })
}

/// Where Refine puts a job for the runner.
#[derive(Clone)]
pub(crate) struct Queue(mpsc::UnboundedSender<String>);

/// The runner's end: shared, so a runner restarted after a panic takes it up.
pub(crate) type Jobs = Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>>;

impl Queue {
    pub(crate) fn new() -> (Self, Jobs) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Queue(tx), Arc::new(tokio::sync::Mutex::new(rx)))
    }

    pub(crate) fn push(&self, job: String) {
        let _ = self.0.send(job);
    }
}

/// Candidates shown for duplicate detection (design 6.3).
const CANDIDATES: usize = 20;

/// The runner: each job in turn, while the daemon runs. A job left queued
/// or running by a daemon that stopped is failed at start.
pub(crate) async fn run(
    store: Store,
    config: ConfigHandle,
    run: Run,
    jobs: Jobs,
    mut stop: ShutdownSignal,
) {
    let mut jobs = jobs.lock().await;
    if let Err(e) = store.abandon_jobs(Timestamp::now()).await {
        tracing::warn!(error = %e, "failing the assistant's abandoned jobs failed");
    }
    loop {
        let id = tokio::select! {
            got = jobs.recv() => match got {
                Some(id) => id,
                None => return,
            },
            () = stop.wait() => return,
        };
        if let Err(e) = one(&store, &config, &run, &id).await {
            tracing::warn!(job = %id, error = %e, "an assistant job failed to record");
        }
    }
}

/// One job, start to finish.
async fn one(store: &Store, config: &ConfigHandle, run: &Run, id: &str) -> todo_store::Result<()> {
    let Some(job) = store.job(id.to_owned()).await? else {
        return Ok(());
    };
    if job.state != todo_store::assistant::JobState::Queued {
        return Ok(());
    }
    let fail = |error: String| JobEnd {
        error: Some(error),
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        cost_usd: 0.0,
        duration_ms: 0,
    };
    let Some(assistant) = config.current().assistant.clone() else {
        let end = fail("the assistant was turned off ([assistant] provider)".into());
        store
            .finish_job(id.to_owned(), end, Vec::new(), Timestamp::now())
            .await?;
        return Ok(());
    };
    store.start_job(id.to_owned(), Timestamp::now()).await?;
    let today = Zoned::now().date();
    let mut refined = Vec::new();
    let mut end = JobEnd {
        error: None,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        cost_usd: 0.0,
        duration_ms: 0,
    };
    // One request per Item: an answer for one is never applied to another.
    for item_id in &job.item_ids {
        let item = match store.get_item(*item_id, today).await {
            Ok(got) if !got.item.is_deleted() => got.item,
            Ok(_) | Err(todo_store::StoreError::NotFound(_)) => continue,
            Err(e) => return Err(e),
        };
        let found = store.refine_context(item.clone(), CANDIDATES).await?;
        let ctx = refine::Context {
            today: Some(today),
            labels_in_use: found.labels,
            style_notes: assistant.style_notes.clone(),
            candidates: found.candidates,
            include_private_notes: assistant.include_private_notes,
        };
        let task = refine::task(&item, &ctx);
        match run(assistant.clone(), task).await {
            Ok(answer) => {
                let u = answer.usage;
                end.input_tokens += u.input_tokens;
                end.output_tokens += u.output_tokens;
                end.cached_tokens += u.cached_tokens;
                end.cost_usd += u.cost_usd;
                end.duration_ms += u.duration_ms;
                let (rationale, patches) =
                    refine::patches(&item, &answer.json, today, |f| editable(store, &item, f));
                refined.push(Refined {
                    item: *item_id,
                    rationale,
                    patches,
                });
            }
            Err(e) => {
                end.error = Some(e.to_string());
                break;
            }
        }
    }
    store
        .finish_job(id.to_owned(), end, refined, Timestamp::now())
        .await?;
    Ok(())
}

/// Whether applying a patch of `field` on `item` would be taken: an unlinked
/// Item takes any of them; a linked one only what pushes (title, description,
/// priority, labels, due date) while its instance pushes, and its Overlay
/// (waiting). A refused one is not proposed.
fn editable(store: &Store, item: &todo_core::Item, field: ItemField) -> bool {
    if !item.is_linked() {
        return true;
    }
    if field == ItemField::Waiting {
        return true;
    }
    let pushing = item
        .remote_link
        .as_ref()
        .is_some_and(|l| store.pushes_now(&l.instance_id));
    pushing && todo_core::mutation::pushed(field) && !store.keeps_local(item, field)
}

/// Turns ids into Item ids, for Refine.
pub(crate) fn item_ids(ids: &[String]) -> Option<Vec<ItemId>> {
    ids.iter().map(|i| i.parse::<ItemId>().ok()).collect()
}
