//! `todo refine` and `todo assistant usage` (design 6, ADR-0010): Claude
//! refines an Item on request, through the person's own Claude Code CLI.
//! What it proposes is a Suggestion to review in the TUI's S pane; nothing
//! changes here.

use std::process::ExitCode;
use std::time::Duration;

use todo_daemon::paths::Paths;
use todo_proto::v1::assistant_service_client::AssistantServiceClient;
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::suggestion_service_client::SuggestionServiceClient;
use todo_proto::v1::{
    self, AssistantJobState, Filter, GetItemRequest, GetJobRequest, ListItemsRequest,
    ListJobsRequest, ListSuggestionsRequest, RefineRequest,
};

use crate::rpc::{call, ready};
use crate::status::failed;
use crate::{print_err, print_out};

/// How long `todo refine` waits for Claude.
const WAIT: Duration = Duration::from_secs(300);

/// `todo refine <id>`: asks, waits, and prints what Claude proposes.
pub(crate) async fn refine(paths: &Paths, id: String) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let Some(item) = resolve(channel.clone(), &id).await? else {
        return Ok(ExitCode::FAILURE);
    };
    let mut ai = AssistantServiceClient::new(channel.clone());
    let job = call(ai.refine(RefineRequest {
        item_ids: vec![item.id.clone()],
    }))
    .await
    .map_err(failed)?
    .job
    .unwrap_or_default();
    print_err(format!("asking Claude to refine {}…", quoted(&item.title)));
    let started = std::time::Instant::now();
    let job = loop {
        let got = call(ai.get_job(GetJobRequest { id: job.id.clone() }))
            .await
            .map_err(failed)?
            .job
            .unwrap_or_default();
        let state = AssistantJobState::try_from(got.state).unwrap_or_default();
        if matches!(state, AssistantJobState::Done | AssistantJobState::Failed) {
            break got;
        }
        if started.elapsed() > WAIT {
            print_err("todo: Claude is still working; its proposal will be in the TUI's S pane");
            return Ok(ExitCode::FAILURE);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    if job.state == AssistantJobState::Failed as i32 {
        print_err(format!("todo: Claude could not refine it: {}", job.error));
        return Ok(ExitCode::FAILURE);
    }
    if job.suggestion_ids.is_empty() {
        print_out("Claude found nothing to change");
        return Ok(ExitCode::SUCCESS);
    }
    let mut pane = SuggestionServiceClient::new(channel);
    let all = call(pane.list_suggestions(ListSuggestionsRequest {
        item_id: Some(item.id.clone()),
    }))
    .await
    .map_err(failed)?
    .suggestions;
    for s in all.iter().filter(|s| job.suggestion_ids.contains(&s.id)) {
        if !s.rationale.is_empty() {
            print_out(&s.rationale);
        }
        for p in &s.patches {
            let field = v1::ItemField::try_from(p.field)
                .map(field_name)
                .unwrap_or("a field");
            print_out(format!(
                "  {field}  {} → {}",
                value_text(p.expected_old.as_ref()),
                value_text(p.new.as_ref())
            ));
        }
    }
    print_out("Review it in the TUI: S, then ctrl-s to apply");
    Ok(ExitCode::SUCCESS)
}

/// `todo assistant usage`: the latest requests and what they cost.
pub(crate) async fn usage(paths: &Paths) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let mut ai = AssistantServiceClient::new(channel);
    let jobs = call(ai.list_jobs(ListJobsRequest { limit: 20 }))
        .await
        .map_err(failed)?
        .jobs;
    if jobs.is_empty() {
        print_out("no requests to Claude yet");
        return Ok(ExitCode::SUCCESS);
    }
    let (mut tokens, mut cost) = (0u64, 0f64);
    for j in &jobs {
        let state = match AssistantJobState::try_from(j.state).unwrap_or_default() {
            AssistantJobState::Done => "done",
            AssistantJobState::Failed => "failed",
            AssistantJobState::Running => "running",
            _ => "queued",
        };
        let when = j
            .created_at
            .as_ref()
            .and_then(|t| jiff::Timestamp::new(t.seconds, t.nanos).ok())
            .map(|t| {
                t.to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%b %d %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        let tokens_of = j.input_tokens + j.output_tokens;
        tokens += tokens_of;
        cost += j.cost_usd;
        print_out(format!(
            "{when}  refine {} item{}  {state}  {tokens_of} tokens ({} cached)  ${:.4}  {:.1}s{}",
            j.item_ids.len(),
            if j.item_ids.len() == 1 { "" } else { "s" },
            j.cached_tokens,
            j.cost_usd,
            j.duration_ms as f64 / 1000.0,
            if j.error.is_empty() {
                String::new()
            } else {
                format!("  ({})", j.error)
            },
        ));
    }
    print_out(format!(
        "{} requests, {tokens} tokens, ${cost:.4}",
        jobs.len()
    ));
    Ok(ExitCode::SUCCESS)
}

/// The Item a full or short (last 8 characters) id names; `None` after
/// saying why there is none, or more than one.
pub(crate) async fn resolve(
    channel: crate::conn::Conn,
    id: &str,
) -> anyhow::Result<Option<v1::Item>> {
    let wanted = id.trim().to_ascii_uppercase();
    let mut items = ItemServiceClient::new(channel);
    if wanted.len() == 26 {
        return match call(items.get_item(GetItemRequest { id: wanted })).await {
            Ok(res) => Ok(res.item),
            Err(status) => Err(failed(status)),
        };
    }
    let crockford = |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
    if !(4..=25).contains(&wanted.len()) || !wanted.chars().all(crockford) {
        print_err(format!(
            "todo: {id} is not an item id (the 8-character short id `todo add` prints, or the full one)"
        ));
        return Ok(None);
    }
    let mut found = Vec::new();
    let mut page_token = String::new();
    loop {
        let page = call(items.list_items(ListItemsRequest {
            filter: Some(Filter {
                include_archived: true,
                include_done: true,
                include_deferred: true,
                ..Filter::default()
            }),
            page_size: 1000,
            page_token: page_token.clone(),
            ..ListItemsRequest::default()
        }))
        .await
        .map_err(failed)?;
        found.extend(page.items.into_iter().filter(|i| i.id.ends_with(&wanted)));
        if page.next_page_token.is_empty() {
            break;
        }
        page_token = page.next_page_token;
    }
    match found.len() {
        0 => {
            print_err(format!("todo: no item has the id {id}"));
            Ok(None)
        }
        1 => Ok(found.pop()),
        n => {
            print_err(format!("todo: {id} names {n} items; use more of the id"));
            Ok(None)
        }
    }
}

fn quoted(title: &str) -> String {
    let t: String = title.chars().take(40).collect();
    format!("\"{t}{}\"", if t.len() < title.len() { "…" } else { "" })
}

fn field_name(f: v1::ItemField) -> &'static str {
    use v1::ItemField as F;
    match f {
        F::Title => "title",
        F::Description => "description",
        F::Priority => "priority",
        F::DueDate => "due",
        F::Labels => "labels",
        F::Waiting => "waiting",
        _ => "a field",
    }
}

/// A patch's value as one short line.
fn value_text(v: Option<&v1::FieldValue>) -> String {
    use v1::field_value::Kind;
    let Some(kind) = v.and_then(|v| v.kind.as_ref()) else {
        return "—".to_owned();
    };
    let one = |s: &str| {
        let line = s.lines().next().unwrap_or_default().trim();
        let cut: String = line.chars().take(50).collect();
        if cut.is_empty() {
            "—".to_owned()
        } else {
            format!("{cut:?}")
        }
    };
    match kind {
        Kind::StringValue(s) | Kind::MarkdownValue(s) => one(s),
        Kind::Priority(p) => match v1::Priority::try_from(*p).unwrap_or_default() {
            v1::Priority::Highest => "+1 highest".into(),
            v1::Priority::High => "+2 high".into(),
            v1::Priority::Medium => "+3 medium".into(),
            v1::Priority::Low => "+4 low".into(),
            v1::Priority::Lowest => "+5 lowest".into(),
            _ => "no priority".into(),
        },
        Kind::DateValue(d) => format!("{:04}-{:02}-{:02}", d.year, d.month, d.day),
        Kind::StringSet(set) if set.values.is_empty() => "no labels".into(),
        Kind::StringSet(set) => set
            .values
            .iter()
            .map(|l| format!("#{l}"))
            .collect::<Vec<_>>()
            .join(" "),
        Kind::Waiting(w) => {
            w.on.as_ref()
                .and_then(|p| match &p.r#ref {
                    Some(v1::person_ref::Ref::FreeText(t)) => Some(format!(
                        "{t}{}",
                        if w.reason.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", w.reason)
                        }
                    )),
                    _ => None,
                })
                .unwrap_or_else(|| "someone".into())
        }
        Kind::Cleared(_) => "—".into(),
        _ => "?".into(),
    }
}
