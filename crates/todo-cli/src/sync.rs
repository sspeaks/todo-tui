//! `todo sync …` (design 5, ADR-0006, ADR-0007). `check` needs no daemon:
//! it reads the config, resolves the credential and asks Jira who the token
//! is, which only reads. `status`, `now` and `outbox` ask the daemon.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use todo_daemon::config::Config;
use todo_daemon::paths::Paths;
use todo_proto::convert::timestamp_from_proto;
use todo_proto::v1::sync_service_client::SyncServiceClient;
use todo_proto::v1::{self, GetSyncStatusRequest, ListOutboxRequest, SyncNowRequest, SyncStatus};
use todo_sync::credential::{SystemSecrets, resolve};

use crate::rpc::{call, ready};
use crate::status::failed;
use crate::{print_err, print_out};

/// `todo sync check`: for each instance, `ok as <name> (<key>)` or why not.
pub(crate) async fn check(paths: &Paths) -> anyhow::Result<ExitCode> {
    let config = Config::load_or_defaults(&paths.config);
    for w in config
        .warnings
        .iter()
        // The file as a whole (not TOML) and the instances.
        .filter(|w| w.key.is_empty() || w.key.starts_with("instances"))
    {
        print_err(format!("{}: {w}", paths.config.display()));
    }
    if config.instances.is_empty() {
        print_err(format!(
            "{}: no [[instances]] to sync; see the README's Jira section",
            paths.config.display()
        ));
        return Ok(ExitCode::FAILURE);
    }
    let mut failed = false;
    for instance in &config.instances {
        let line = match resolve(&instance.credential, &SystemSecrets) {
            Err(e) => {
                failed = true;
                format!("{}: credential {e}", instance.id)
            }
            Ok(token) => match todo_jira::Client::new(&instance.base_url, token) {
                Err(e) => {
                    failed = true;
                    format!("{}: {e}", instance.id)
                }
                Ok(client) => match client.myself().await {
                    Ok(me) => {
                        let tz = if me.time_zone.is_empty() {
                            "unknown".to_owned()
                        } else {
                            me.time_zone
                        };
                        format!(
                            "{}: ok as {} ({}), time zone {tz}",
                            instance.id,
                            if me.display_name.is_empty() {
                                &me.name
                            } else {
                                &me.display_name
                            },
                            me.key
                        )
                    }
                    Err(e) => {
                        failed = true;
                        format!("{}: {} — {e}", instance.id, instance.base_url)
                    }
                },
            },
        };
        print_out(line);
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `todo sync status`: each instance's line, from the daemon. Fails when
/// one needs attention (a refused token, a failed pull).
pub(crate) async fn status(paths: &Paths) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let mut sync = SyncServiceClient::new(channel);
    let all = call(sync.get_sync_status(GetSyncStatusRequest {}))
        .await
        .map_err(failed)?
        .instances;
    if all.is_empty() {
        print_err(format!(
            "{}: no [[instances]] sync; see the README's Jira section",
            paths.config.display()
        ));
        return Ok(ExitCode::FAILURE);
    }
    Ok(print_statuses(&all, Timestamp::now()))
}

/// `todo sync outbox`: each change that waits to reach Jira or that Jira
/// refused, oldest first, one line each (`PLAT-12  title → "new title"
/// waiting`); a status's two rows read as one. Fails while one is refused.
pub(crate) async fn outbox(paths: &Paths) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let mut sync = SyncServiceClient::new(channel);
    let entries = call(sync.list_outbox(ListOutboxRequest::default()))
        .await
        .map_err(failed)?
        .entries;
    if entries.is_empty() {
        print_out("nothing waits to reach Jira");
        return Ok(ExitCode::SUCCESS);
    }
    let now = Timestamp::now();
    let mut refused = false;
    for line in outbox_lines(&entries, now) {
        refused |= line.1;
        print_out(line.0);
    }
    // Refused: it needs the person (apply it again, or reject it).
    Ok(if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The lines of `todo sync outbox`, each with whether it was refused.
fn outbox_lines(entries: &[v1::OutboxEntry], now: Timestamp) -> Vec<(String, bool)> {
    use todo_core::{FieldValue as V, ItemField as F, ResolutionKind, StatusCategory};
    let field = |e: &v1::OutboxEntry| F::from_number(e.field as u32);
    let value = |e: &v1::OutboxEntry| {
        e.new
            .as_ref()
            .and_then(|v| todo_proto::convert::field_value_from_proto("new", v).ok())
    };
    // The resolution kind waiting beside `e`'s category, if any.
    let kind_beside = |e: &v1::OutboxEntry| {
        entries.iter().find(|o| {
            o.item_id == e.item_id && o.state == e.state && field(o) == Some(F::ResolutionKind)
        })
    };
    let mut out = Vec::new();
    for e in entries {
        // A status is one change: the category's line, its kind joined in.
        let category = entries.iter().any(|o| {
            o.item_id == e.item_id && o.state == e.state && field(o) == Some(F::StatusCategory)
        });
        if field(e) == Some(F::ResolutionKind) && category {
            continue;
        }
        let (what, to) = match (field(e), value(e)) {
            (Some(F::StatusCategory), Some(V::StatusCategory(c))) => {
                let cancelled = kind_beside(e).and_then(value)
                    == Some(V::ResolutionKind(ResolutionKind::Cancelled));
                let word = match c {
                    StatusCategory::Todo => "to do",
                    StatusCategory::InProgress => "in progress",
                    StatusCategory::Done if cancelled => "cancelled",
                    StatusCategory::Done => "done",
                };
                ("status", word.to_owned())
            }
            // Done to cancelled: the kind alone.
            (Some(F::ResolutionKind), Some(V::ResolutionKind(k))) => (
                "status",
                if k == ResolutionKind::Cancelled {
                    "cancelled"
                } else {
                    "done"
                }
                .to_owned(),
            ),
            (Some(f), Some(v)) => (todo_proto::convert::item_field_path(f), value_text(&v)),
            _ => ("a field", "?".to_owned()),
        };
        let retry = e
            .next_attempt_at
            .as_ref()
            .and_then(|t| timestamp_from_proto("next_attempt_at", t).ok())
            .filter(|at| *at > now);
        let (word, refused) = match v1::OutboxState::try_from(e.state) {
            Ok(v1::OutboxState::Pending) => match retry {
                Some(at) => (format!("waiting, retry in {}", until(at, now)), false),
                None => ("waiting".to_owned(), false),
            },
            Ok(v1::OutboxState::InFlight) => ("sent, not read back yet".to_owned(), false),
            Ok(v1::OutboxState::Failed) => ("refused".to_owned(), true),
            _ => ("?".to_owned(), false),
        };
        let error = if e.last_error.is_empty() {
            String::new()
        } else {
            format!(" ({})", e.last_error)
        };
        out.push((
            format!(
                "{}  {what} → {to}  {word}{error}  · {}",
                e.remote_key,
                truncate(&e.title, 50)
            ),
            refused,
        ));
    }
    out
}

/// A pushed value as a person reads it.
fn value_text(v: &todo_core::FieldValue) -> String {
    use todo_core::{FieldValue as V, Priority as P};
    match v {
        V::String(s) => format!("{s:?}"),
        V::Priority(p) => match p {
            P::None => "no priority".to_owned(),
            P::Highest => "+1 highest".to_owned(),
            P::High => "+2 high".to_owned(),
            P::Medium => "+3 medium".to_owned(),
            P::Low => "+4 low".to_owned(),
            P::Lowest => "+5 lowest".to_owned(),
        },
        V::StringSet(labels) if labels.is_empty() => "no labels".to_owned(),
        V::StringSet(labels) => labels
            .iter()
            .map(|l| format!("#{l}"))
            .collect::<Vec<_>>()
            .join(" "),
        V::Date(d) => d.to_string(),
        V::Cleared => "none".to_owned(),
        other => format!("{other:?}"),
    }
}

/// `s` cut to `n` characters, with an ellipsis.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let cut: String = s.chars().take(n - 1).collect();
    format!("{cut}…")
}

/// "12s", "3m": how long until `at`.
fn until(at: Timestamp, now: Timestamp) -> String {
    let s = at.as_second().saturating_sub(now.as_second()).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// How long `todo sync now` waits for its pull.
const NOW_WAIT: Duration = Duration::from_secs(300);

/// `todo sync now [instance]`: asks for a pull and waits for the pass that
/// serves the request (one started after it), then prints the status as
/// `todo sync status` does.
pub(crate) async fn now(paths: &Paths, instance: Option<String>) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let mut sync = SyncServiceClient::new(channel);
    let id = instance.unwrap_or_default();
    let request = call(sync.sync_now(SyncNowRequest {
        instance_id: id.clone(),
    }))
    .await
    .map_err(failed)?
    .request;
    let start = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let mine: Vec<SyncStatus> = call(sync.get_sync_status(GetSyncStatusRequest {}))
            .await
            .map_err(failed)?
            .instances
            .into_iter()
            .filter(|s| id.is_empty() || s.instance_id == id)
            .collect();
        if served(&mine, request) {
            return Ok(print_statuses(&mine, Timestamp::now()));
        }
        if start.elapsed() >= NOW_WAIT {
            print_err("todo: the pull is still running; see todo sync status");
            return Ok(ExitCode::FAILURE);
        }
    }
}

/// Whether every instance in `mine` (at least one) has finished a pass
/// that began after `request`.
fn served(mine: &[SyncStatus], request: u64) -> bool {
    !mine.is_empty()
        && mine.iter().all(|s| {
            s.served_request >= request
                && !matches!(
                    v1::SyncState::try_from(s.state),
                    Ok(v1::SyncState::Syncing | v1::SyncState::Starting)
                )
        })
}

/// Prints one line per instance; a failure when any is not ok.
fn print_statuses(all: &[SyncStatus], now: Timestamp) -> ExitCode {
    let mut bad = false;
    for s in all {
        let (line, ok) = status_line(s, now);
        bad |= !ok;
        print_out(line);
    }
    if bad {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `work-jira: ok · 12 items · last pull 3m ago`, and whether it is fine.
fn status_line(s: &SyncStatus, now: Timestamp) -> (String, bool) {
    use v1::SyncState as S;
    let state = S::try_from(s.state).unwrap_or(S::Unspecified);
    let (word, ok) = match state {
        S::Ok => ("ok", true),
        S::Syncing => ("syncing", true),
        S::Starting => ("starting", true),
        S::Offline => ("offline, retrying", false),
        S::AuthError => (
            "the token was refused: store a new one, then todo sync now",
            false,
        ),
        S::Degraded => ("a pull failed", false),
        S::Unspecified => ("not configured", false),
    };
    let error = if s.last_error.is_empty() || state == S::Ok {
        String::new()
    } else {
        format!(" ({})", s.last_error)
    };
    let last = match s
        .last_successful_pull_at
        .as_ref()
        .and_then(|t| timestamp_from_proto("last_successful_pull_at", t).ok())
    {
        Some(at) => format!("last pull {}", ago(at, now)),
        None => "not pulled yet".to_owned(),
    };
    let items = format!("{} item{}", s.items, if s.items == 1 { "" } else { "s" });
    // What waits and what was refused, when there is any (design 5.11).
    let outbox = match (s.pending_ops, s.failed_ops) {
        (0, 0) => String::new(),
        (p, 0) => format!(" · {p} to push"),
        (0, f) => format!(" · {f} refused (todo sync outbox)"),
        (p, f) => format!(" · {p} to push, {f} refused (todo sync outbox)"),
    };
    (
        format!(
            "{}: {word}{error} · {items}{outbox} · {last}",
            s.instance_id
        ),
        ok && s.failed_ops == 0,
    )
}

/// "just now", "3m ago", "2h ago", "4d ago".
fn ago(at: Timestamp, now: Timestamp) -> String {
    let s = now.as_second().saturating_sub(at.as_second()).max(0);
    match s {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(state: v1::SyncState, last: Option<i64>) -> SyncStatus {
        SyncStatus {
            instance_id: "work-jira".into(),
            state: state as i32,
            last_successful_pull_at: last.map(|s| {
                todo_proto::convert::timestamp_to_proto(Timestamp::from_second(s).unwrap())
            }),
            last_error: "401 Unauthorized".into(),
            items: 12,
            pulled: 0,
            served_request: 0,
            pending_ops: 0,
            failed_ops: 0,
        }
    }

    #[test]
    fn a_status_line_says_how_it_stands() {
        let now = Timestamp::from_second(10_000).unwrap();
        assert_eq!(
            status_line(&st(v1::SyncState::Ok, Some(10_000 - 180)), now),
            (
                "work-jira: ok · 12 items · last pull 3m ago".to_owned(),
                true
            )
        );
        assert_eq!(
            status_line(&st(v1::SyncState::AuthError, None), now),
            (
                "work-jira: the token was refused: store a new one, then todo sync now (401 Unauthorized) · 12 items · not pulled yet"
                    .to_owned(),
                false
            )
        );
        assert_eq!(
            ago(Timestamp::from_second(10_000 - 7200).unwrap(), now),
            "2h ago"
        );
        assert_eq!(ago(now, now), "just now");
        let waiting = SyncStatus {
            pending_ops: 2,
            failed_ops: 1,
            ..st(v1::SyncState::Ok, Some(10_000 - 180))
        };
        assert_eq!(
            status_line(&waiting, now),
            (
                "work-jira: ok · 12 items · 2 to push, 1 refused (todo sync outbox) · last pull 3m ago"
                    .to_owned(),
                false
            )
        );
    }

    #[test]
    fn the_outbox_reads_a_status_as_one_line() {
        use todo_core::{FieldValue, ItemField, ResolutionKind, StatusCategory};
        let now = Timestamp::from_second(10_000).unwrap();
        let entry = |field: ItemField, new: FieldValue, state: v1::OutboxState| v1::OutboxEntry {
            instance_id: "work-jira".into(),
            item_id: "01HZ".into(),
            remote_key: "PLAT-1".into(),
            title: "Fix the login".into(),
            field: field.number() as i32,
            new: Some((&new).into()),
            state: state as i32,
            ..v1::OutboxEntry::default()
        };
        let lines = outbox_lines(
            &[
                entry(
                    ItemField::StatusCategory,
                    FieldValue::StatusCategory(StatusCategory::Done),
                    v1::OutboxState::Failed,
                ),
                entry(
                    ItemField::ResolutionKind,
                    FieldValue::ResolutionKind(ResolutionKind::Cancelled),
                    v1::OutboxState::Failed,
                ),
                v1::OutboxEntry {
                    next_attempt_at: Some(todo_proto::convert::timestamp_to_proto(
                        Timestamp::from_second(10_090).unwrap(),
                    )),
                    last_error: "rate limited".into(),
                    ..entry(
                        ItemField::Title,
                        FieldValue::String("Fix it".into()),
                        v1::OutboxState::Pending,
                    )
                },
            ],
            now,
        );
        assert_eq!(
            lines,
            [
                ("PLAT-1  status → cancelled  refused  · Fix the login".to_owned(), true),
                (
                    "PLAT-1  title → \"Fix it\"  waiting, retry in 1m (rate limited)  · Fix the login"
                        .to_owned(),
                    false
                ),
            ]
        );
    }

    #[test]
    fn now_waits_for_a_pass_that_began_after_its_request() {
        let at = |served_request, state: v1::SyncState| SyncStatus {
            served_request,
            ..st(state, None)
        };
        // No status yet (the credential is still being read): not served.
        assert!(!served(&[], 1));
        // The pass already running when it asked serves an older request.
        assert!(!served(&[at(0, v1::SyncState::Ok)], 1));
        assert!(!served(&[at(1, v1::SyncState::Syncing)], 1));
        assert!(served(&[at(1, v1::SyncState::Ok)], 1));
        assert!(served(&[at(2, v1::SyncState::AuthError)], 1));
    }
}
