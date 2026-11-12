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

/// The runner: each job in turn, while the daemon runs. Restarted after a
/// panic, it takes up the queue where it was (the daemon failed what an
/// earlier daemon left behind before it served any call).
pub(crate) async fn run(
    store: Store,
    config: ConfigHandle,
    run: Run,
    jobs: Jobs,
    mut stop: ShutdownSignal,
) {
    let mut jobs = jobs.lock().await;
    // A job this daemon queued that a panic left running ends as failed;
    // those still queued are in the channel and run below.
    if let Ok(left) = store.jobs(50).await {
        for j in left
            .iter()
            .filter(|j| j.state == todo_store::assistant::JobState::Running)
        {
            let end = JobEnd {
                error: Some("the assistant stopped while it ran; ask again".into()),
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                cost_usd: 0.0,
                duration_ms: 0,
            };
            let _ = store
                .finish_job(j.id.clone(), end, Vec::new(), Timestamp::now())
                .await;
        }
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

/// One job, start to finish. Whatever happens once it starts, it ends:
/// done, or failed with why, never left running.
async fn one(store: &Store, config: &ConfigHandle, run: &Run, id: &str) -> todo_store::Result<()> {
    let Some(job) = store.job(id.to_owned()).await? else {
        return Ok(());
    };
    if job.state != todo_store::assistant::JobState::Queued {
        return Ok(());
    }
    store.start_job(id.to_owned(), Timestamp::now()).await?;
    let mut end = JobEnd {
        error: None,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        cost_usd: 0.0,
        duration_ms: 0,
    };
    let mut refined = Vec::new();
    if let Err(why) = items(store, config, run, &job.item_ids, &mut end, &mut refined).await {
        end.error = Some(why);
    }
    store
        .finish_job(id.to_owned(), end, refined, Timestamp::now())
        .await?;
    Ok(())
}

/// The job's Items, one Claude request each (an answer for one is never
/// applied to another), until one fails. The config is read before each, so
/// a reload that turns the assistant off, or private notes out, holds from
/// the next Item on.
async fn items(
    store: &Store,
    config: &ConfigHandle,
    run: &Run,
    ids: &[ItemId],
    end: &mut JobEnd,
    refined: &mut Vec<Refined>,
) -> Result<(), String> {
    let today = Zoned::now().date();
    for item_id in ids {
        let Some(assistant) = config.current().assistant.clone() else {
            return Err("the assistant was turned off ([assistant] provider)".into());
        };
        let item = match store.get_item(*item_id, today).await {
            Ok(got) if !got.item.is_deleted() => got.item,
            Ok(_) | Err(todo_store::StoreError::NotFound(_)) => continue,
            Err(e) => return Err(format!("reading the item failed: {e}")),
        };
        let found = store
            .refine_context(item.clone(), CANDIDATES)
            .await
            .map_err(|e| format!("reading the context failed: {e}"))?;
        let ctx = refine::Context {
            today: Some(today),
            labels_in_use: found.labels.clone(),
            style_notes: assistant.style_notes.clone(),
            candidates: found.candidates,
            include_private_notes: assistant.include_private_notes,
        };
        let task = refine::task(&item, &ctx);
        let answer = run(assistant.clone(), task)
            .await
            .map_err(|e| e.to_string())?;
        let u = answer.usage;
        end.input_tokens += u.input_tokens;
        end.output_tokens += u.output_tokens;
        end.cached_tokens += u.cached_tokens;
        end.cost_usd += u.cost_usd;
        end.duration_ms += u.duration_ms;
        let (rationale, patches) =
            refine::patches(&item, &answer.json, today, &found.labels, |f| {
                editable(store, &item, f)
            });
        refined.push(Refined {
            item: *item_id,
            rationale,
            patches,
        });
    }
    Ok(())
}

/// Whether applying a patch of `field` on `item` would be taken, as the
/// edit guard has it (ADR-0007, ADR-0009): an unlinked Item takes any of
/// them, a linked one its Overlay (waiting) and the fields it keeps local;
/// of the rest, only what pushes, on a whole link while its instance pushes,
/// on the person's own issue (someone else's would ask, and Claude is not
/// the person), and a description only once it has been read as Markdown.
/// A refused one is not proposed.
fn editable(store: &Store, item: &todo_core::Item, field: ItemField) -> bool {
    let Some(link) = &item.remote_link else {
        return true;
    };
    if field == ItemField::Waiting || store.keeps_local(item, field) {
        return true;
    }
    let read = field != ItemField::Description || link.last_pulled_version.is_some();
    link.link_state == todo_core::LinkState::Ok
        && store.pushes_now(&link.instance_id)
        && item.derived.is_mine
        && todo_core::mutation::pushed(field)
        && read
}

/// Turns ids into Item ids, for Refine.
pub(crate) fn item_ids(ids: &[String]) -> Option<Vec<ItemId>> {
    ids.iter().map(|i| i.parse::<ItemId>().ok()).collect()
}
