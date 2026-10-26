//! `todo status` (design 2.1): the inbox and due counts and the count of
//! each view that shows one, and `todo status --short` for a shell prompt.

use std::process::ExitCode;
use std::time::Duration;

use todo_daemon::paths::{Env, Paths};
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::view_service_client::ViewServiceClient;
use todo_proto::v1::{GetStatusCountsRequest, ListViewsRequest, StatusCounts, View, ViewCount};
use tonic::Status;

use crate::rpc::{call, describe, is_transport, ready};
use crate::{print_out, short_id};

/// How long `todo status --short` may take to connect and get its answer,
/// all told: a prompt must never wait on the daemon.
const SHORT_DEADLINE: Duration = Duration::from_millis(40);

/// `todo status --short`: one line such as `📥3 ⏰1`, from a single
/// GetStatusCounts call under [`SHORT_DEADLINE`], made without starting,
/// handshaking with or restarting the daemon. Zero counts are left out.
/// Prints nothing when every count is zero or anything goes wrong (no
/// daemon, no answer in time, an older daemon without the RPC), and
/// always succeeds.
pub(crate) fn short() -> ExitCode {
    // One call needs no more than the current thread.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    if let Ok(runtime) = runtime
        && let Some(line) = runtime.block_on(short_line())
    {
        print_out(line);
    }
    ExitCode::SUCCESS
}

async fn short_line() -> Option<String> {
    let paths = Paths::resolve(&Env::from_process()).ok()?;
    let ask = async {
        let channel = todo_daemon::client::connect(&paths.socket).await.ok()?;
        let request = GetStatusCountsRequest::default();
        let reply = ItemServiceClient::new(channel)
            .get_status_counts(request)
            .await
            .ok()?;
        reply.into_inner().counts
    };
    let counts = tokio::time::timeout(SHORT_DEADLINE, ask).await.ok()??;
    prompt(&counts)
}

/// The `--short` line for `counts`; `None` when every count is zero.
fn prompt(counts: &StatusCounts) -> Option<String> {
    let parts: Vec<String> = [("📥", counts.inbox), ("⏰", counts.due)]
        .into_iter()
        .filter(|&(_, n)| n > 0)
        .map(|(glyph, n)| format!("{glyph}{n}"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// `todo status`: the counts, then each counted view's name and count in
/// tab order, from the daemon, which is started or restarted as for
/// `todo add`.
pub(crate) async fn status(paths: &Paths) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let request = GetStatusCountsRequest {
        include_views: true,
        reference_time: None,
    };
    let reply = call(ItemServiceClient::new(channel.clone()).get_status_counts(request))
        .await
        .map_err(failed)?;
    let views = call(ViewServiceClient::new(channel).list_views(ListViewsRequest {}))
        .await
        .map_err(failed)?;
    let counts = reply.counts.unwrap_or_default();
    for line in report(&counts, &reply.views, &views.views) {
        print_out(line);
    }
    Ok(ExitCode::SUCCESS)
}

fn failed(status: Status) -> anyhow::Error {
    if is_transport(&status) {
        anyhow::anyhow!("daemon unavailable ({})", describe(&status))
    } else {
        anyhow::anyhow!("{}", status.message())
    }
}

/// The lines `todo status` prints: the inbox and due counts, then a line
/// per counted view, named from `views`.
fn report(counts: &StatusCounts, counted: &[ViewCount], views: &[View]) -> Vec<String> {
    let mut lines = vec![
        format!("📥 Inbox: {}", counts.inbox),
        format!("⏰ Due: {} ({} overdue)", counts.due, counts.overdue),
    ];
    if !counted.is_empty() {
        lines.push("Views:".to_owned());
    }
    for count in counted {
        // A view removed since the counts were read keeps its short id.
        let name = views
            .iter()
            .find(|v| v.id == count.view_id)
            .map_or_else(|| short_id(&count.view_id), |v| v.name.as_str());
        lines.push(format!("  {name}: {}", count.count));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(inbox: u32, due: u32, overdue: u32) -> StatusCounts {
        StatusCounts {
            inbox,
            due,
            overdue,
        }
    }

    #[test]
    fn the_prompt_line_leaves_out_zero_counts() {
        assert_eq!(prompt(&counts(3, 1, 1)).as_deref(), Some("📥3 ⏰1"));
        assert_eq!(prompt(&counts(3, 0, 0)).as_deref(), Some("📥3"));
        assert_eq!(prompt(&counts(0, 12, 0)).as_deref(), Some("⏰12"));
        // Overdue is part of due, which the line already counts.
        assert_eq!(prompt(&counts(0, 0, 0)), None);
    }

    #[test]
    fn the_report_names_each_counted_view() {
        let view = |id: &str, name: &str| View {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        };
        let count = |id: &str, count| ViewCount {
            view_id: id.into(),
            count,
        };
        let views = [view("A", "Inbox"), view("B", "Today"), view("C", "Someday")];
        let gone = "01JB0000000000000000ABCDEF";
        let lines = report(
            &counts(2, 3, 1),
            &[count("A", 2), count("B", 4), count(gone, 0)],
            &views,
        );
        assert_eq!(
            lines,
            [
                "📥 Inbox: 2",
                "⏰ Due: 3 (1 overdue)",
                "Views:",
                "  Inbox: 2",
                "  Today: 4",
                "  00ABCDEF: 0",
            ]
        );
        // No views counted, no views heading.
        assert_eq!(
            report(&counts(0, 0, 0), &[], &views),
            ["📥 Inbox: 0", "⏰ Due: 0 (0 overdue)"]
        );
    }
}
