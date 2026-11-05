//! `todo sync …` (design 5, ADR-0006). `check` needs no daemon: it reads the
//! config, resolves the credential and asks Jira who the token is, which
//! only reads. `status` and `now` ask the daemon.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use todo_daemon::config::Config;
use todo_daemon::paths::Paths;
use todo_proto::convert::timestamp_from_proto;
use todo_proto::v1::sync_service_client::SyncServiceClient;
use todo_proto::v1::{self, GetSyncStatusRequest, SyncNowRequest, SyncStatus};
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
    (
        format!("{}: {word}{error} · {items} · {last}", s.instance_id),
        ok,
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
