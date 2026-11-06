//! The pull loop (design 5.4, 5.5, 5.11): one supervised task that keeps
//! the configured instance's issues in step, read-only (ADR-0006).
//!
//! Each pass discovers the instance, pulls each scope from its cursor (a
//! page committed at a time, the cursor with it), refreshes the tracked
//! Items outside every scope, and reconciles the ones no scope lists any
//! more (at start, then hourly). Its state is a watch channel the
//! SyncService and Subscribe read.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::{Timestamp, Zoned};
use todo_jira::{Adapter, Client, Fetched, JiraError, PullError};
use todo_proto::convert::timestamp_to_proto;
use todo_proto::v1;
use todo_store::{PullPage, Reconcile, Store};
use todo_sync::Instance;
use todo_sync::credential::{SystemSecrets, resolve};
use tokio::sync::{Notify, watch};

use crate::config::Config;
use crate::conn::Clients;
use crate::shutdown::ShutdownSignal;

/// How often the scopes are reconciled (design 5.5).
pub(crate) const RECONCILE_EVERY: Duration = Duration::from_secs(60 * 60);

/// The longest an offline instance waits before it tries again.
const OFFLINE_MAX: Duration = Duration::from_secs(10 * 60);

/// The sync state, by instance, for the SyncService and Subscribe.
pub(crate) type Status = watch::Receiver<BTreeMap<String, v1::SyncStatus>>;

/// Wakes the loop for a pull now ("sync now", design 5.4), numbering
/// each request so that its caller can wait for the pass that serves it.
#[derive(Clone, Default)]
pub(crate) struct SyncNow(Arc<Notify>, Arc<AtomicU64>);

impl SyncNow {
    /// Asks for a pass; this request's number.
    pub(crate) fn poke(&self) -> u64 {
        let n = self.1.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.notify_one();
        n
    }

    /// The newest request so far: a pass that starts now serves it.
    fn requested(&self) -> u64 {
        self.1.load(Ordering::SeqCst)
    }
}

struct State {
    tx: watch::Sender<BTreeMap<String, v1::SyncStatus>>,
}

impl State {
    fn set(&self, id: &str, f: impl FnOnce(&mut v1::SyncStatus)) {
        self.tx.send_modify(|all| {
            let s = all.entry(id.to_owned()).or_insert_with(|| v1::SyncStatus {
                instance_id: id.to_owned(),
                state: v1::SyncState::Starting as i32,
                ..v1::SyncStatus::default()
            });
            f(s);
        });
    }
}

/// The channel the loop reports on, and its first value (nothing configured).
pub(crate) fn status_channel() -> (watch::Sender<BTreeMap<String, v1::SyncStatus>>, Status) {
    watch::channel(BTreeMap::new())
}

/// The loop. It runs while the daemon does; a config change re-reads the
/// instance (and starts over with it).
pub(crate) async fn run(
    store: Store,
    mut config: watch::Receiver<Arc<Config>>,
    clients: Clients,
    now: SyncNow,
    status: watch::Sender<BTreeMap<String, v1::SyncStatus>>,
    mut stop: ShutdownSignal,
) {
    let state = State { tx: status };
    let queued = store.pushes_queued();
    let mut last_reconcile: Option<Timestamp> = None;
    let mut offline_wait = Duration::from_secs(15);
    loop {
        let instance = config.borrow_and_update().instances.first().cloned();
        // A person may edit a linked Item's pushed fields only while its
        // instance pushes (ADR-0007).
        store.set_pushes(
            instance
                .as_ref()
                .filter(|i| i.push.enabled)
                .map(|i| i.id.as_str()),
        );
        // Only the configured instances have a status.
        state
            .tx
            .send_modify(|all| all.retain(|id, _| instance.as_ref().is_some_and(|i| &i.id == id)));
        let wait = match &instance {
            None => None,
            Some(i) => {
                // A status from the start, before the credential is read
                // (a keychain prompt can take a while).
                state.set(&i.id, |_| {});
                let served = now.requested();
                let reconcile = last_reconcile.is_none_or(|at| {
                    Timestamp::now().duration_since(at).unsigned_abs() >= RECONCILE_EVERY
                });
                let outcome = pass(&store, i, &state, reconcile).await;
                // Only once the pass is over: a caller waiting on its request
                // reads served_request with the state the pass ended in.
                state.set(&i.id, |st| st.served_request = served);
                match outcome {
                    Outcome::Ok => {
                        offline_wait = Duration::from_secs(15);
                        if reconcile {
                            last_reconcile = Some(Timestamp::now());
                        }
                        Some(if clients.other_interactive(None) > 0 {
                            i.poll_interactive
                        } else {
                            i.poll_idle
                        })
                    }
                    Outcome::Offline => {
                        let w = offline_wait;
                        offline_wait = (offline_wait * 2).min(OFFLINE_MAX);
                        Some(w)
                    }
                    // Waits for the config (or a sync now) to change.
                    Outcome::Auth => None,
                    Outcome::Degraded => Some(i.poll_idle),
                }
            }
        };
        let sleep = async {
            match wait {
                Some(w) => tokio::time::sleep(w).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = sleep => {}
            // A sync now reconciles too: a resolution in the default scope
            // (resolution = Unresolved) shows only there (5.5).
            () = now.0.notified() => last_reconcile = None,
            // A TUI arriving moves to its cadence now (design 5.4).
            () = clients.interactive_arrived() => {}
            // An edit queued a push: a pass now (pull first, then push),
            // with no reconciliation.
            () = queued.notified() => {}
            changed = config.changed() => {
                if changed.is_err() {
                    return;
                }
                last_reconcile = None;
            }
            () = stop.wait() => return,
        }
    }
}

enum Outcome {
    Ok,
    Offline,
    Auth,
    Degraded,
}

fn fail(state: &State, id: &str, e: &JiraError) -> Outcome {
    let (s, outcome) = match e {
        JiraError::Auth => (v1::SyncState::AuthError, Outcome::Auth),
        e if e.is_transient() => (v1::SyncState::Offline, Outcome::Offline),
        _ => (v1::SyncState::Degraded, Outcome::Degraded),
    };
    tracing::warn!(instance = id, error = %e, "sync failed");
    state.set(id, |st| {
        st.state = s as i32;
        st.last_error = e.to_string();
    });
    outcome
}

/// One pass of an instance.
async fn pass(store: &Store, i: &Instance, state: &State, reconcile: bool) -> Outcome {
    let token = match resolve(&i.credential, &SystemSecrets) {
        Ok(t) => t,
        Err(e) => {
            state.set(&i.id, |st| {
                st.state = v1::SyncState::AuthError as i32;
                st.last_error = format!("credential {e}");
            });
            return Outcome::Auth;
        }
    };
    state.set(&i.id, |st| {
        st.state = v1::SyncState::Syncing as i32;
        st.pulled = 0;
    });
    let adapter = match Client::new(&i.base_url, token) {
        Ok(c) => match Adapter::connect(c, &i.id, i.cancelled_resolutions.clone(), i.push.clone())
            .await
        {
            Ok(a) => a,
            Err(e) => return fail(state, &i.id, &e),
        },
        Err(e) => return fail(state, &i.id, &e),
    };
    let mut pulled = 0u32;
    // What the scope pulls returned, so the rest can be read by id.
    let mut seen: HashSet<String> = HashSet::new();
    // Each scope from its cursor.
    for scope in &i.scopes {
        let cursor = match store.sync_cursor(i.id.clone(), scope.name.clone()).await {
            Ok(c) => c,
            Err(e) => return store_fail(state, &i.id, &e),
        };
        let result = adapter
            .pull_scope(&scope.query, cursor, |issues, cursor| {
                pulled += u32::try_from(issues.len()).unwrap_or(u32::MAX);
                let n = pulled;
                state.set(&i.id, |st| st.pulled = n);
                seen.extend(issues.iter().map(|is| is.remote_id.clone()));
                let page = PullPage {
                    instance_id: i.id.clone(),
                    issues: issues
                        .into_iter()
                        .map(|is| (is, vec![scope.name.clone()]))
                        .collect(),
                    cursor: cursor.map(|c| (scope.name.clone(), c)),
                    replace_scopes: false,
                    now: Timestamp::now(),
                    today: Zoned::now().date(),
                };
                let store = store.clone();
                async move { store.apply_pull(page).await.map(|_| ()) }
            })
            .await;
        match result {
            Ok(_) => {}
            Err(PullError::Jira(e)) => return fail(state, &i.id, &e),
            Err(PullError::Page(e)) => return store_fail(state, &i.id, &e),
        }
    }
    if let Err(o) = tracked_and_reconcile(store, i, &adapter, state, reconcile, &seen).await {
        return o;
    }
    // Then the outbox, on data just pulled (design 5.11: pull first).
    if let Err(o) = push(store, i, &adapter, state).await {
        return o;
    }
    let items = store
        .tracked(i.id.clone())
        .await
        .map(|t| t.len())
        .unwrap_or(0);
    let (pending, failed) = store.outbox_counts(i.id.clone()).await.unwrap_or((0, 0));
    state.set(&i.id, |st| {
        st.pending_ops = pending;
        st.failed_ops = failed;
        st.state = v1::SyncState::Ok as i32;
        st.last_error.clear();
        st.last_successful_pull_at = Some(timestamp_to_proto(Timestamp::now()));
        st.items = u32::try_from(items).unwrap_or(u32::MAX);
    });
    Outcome::Ok
}

/// The cursor of the tracked Items outside every scope: the newest update
/// among them a pass has read (Jira's clock, as the scopes' cursors are).
const OUTSIDE: &str = "\u{0}outside";

fn store_fail(state: &State, id: &str, e: &todo_store::StoreError) -> Outcome {
    tracing::error!(instance = id, error = %e, "a sync store write failed");
    state.set(id, |st| {
        st.state = v1::SyncState::Degraded as i32;
        st.last_error = format!("store: {e}");
    });
    Outcome::Degraded
}

fn page(
    i: &Instance,
    issues: Vec<(todo_core::remote::RemoteIssue, Vec<String>)>,
    cursor: Option<(String, Timestamp)>,
    replace_scopes: bool,
) -> PullPage {
    PullPage {
        instance_id: i.id.clone(),
        issues,
        cursor,
        replace_scopes,
        now: Timestamp::now(),
        today: Zoned::now().date(),
    }
}

/// The tracked Items outside every scope (design 5.4 step 2b), those to be
/// read again by id, and, when due, the reconciliation (5.5). `seen` is
/// what this pass's scope pulls returned.
async fn tracked_and_reconcile(
    store: &Store,
    i: &Instance,
    adapter: &Adapter,
    state: &State,
    reconcile: bool,
    seen: &HashSet<String>,
) -> Result<(), Outcome> {
    let sfail = |e: todo_store::StoreError| store_fail(state, &i.id, &e);
    let jfail = |e: JiraError| fail(state, &i.id, &e);
    let tracked = store.tracked(i.id.clone()).await.map_err(sfail)?;
    let outside: Vec<String> = tracked
        .iter()
        .filter(|t| t.scopes.is_empty() && !t.stale)
        .map(|t| t.remote_id.clone())
        .collect();
    // Stale ones are read below, from no cursor.
    let all_outside: Vec<String> = tracked
        .iter()
        .filter(|t| t.scopes.is_empty())
        .map(|t| t.remote_id.clone())
        .collect();
    if !outside.is_empty() {
        let since = store
            .sync_cursor(i.id.clone(), OUTSIDE.to_owned())
            .await
            .map_err(sfail)?;
        let issues = adapter.pull_ids(&outside, since).await.map_err(jfail)?;
        let newest = issues.iter().map(|is| is.updated_at).max();
        let issues = issues.into_iter().map(|is| (is, Vec::new())).collect();
        store
            .apply_pull(page(
                i,
                issues,
                newest.map(|at| (OUTSIDE.to_owned(), at)),
                false,
            ))
            .await
            .map_err(sfail)?;
    }
    // Read again by id, whatever their update time: what a pull could not
    // bring up to date (deleted meanwhile, or unreadable), and, on a
    // reconciling pass, every Item in a scope its pulls did not return, so
    // a change that took it out of the scope's query (a resolution) shows
    // at once. Reconciliation below sorts out what is gone.
    let again: Vec<&todo_store::Tracked> = tracked
        .iter()
        .filter(|t| t.stale || (reconcile && !t.scopes.is_empty() && !seen.contains(&t.remote_id)))
        .collect();
    if !again.is_empty() {
        let ids: Vec<String> = again.iter().map(|t| t.remote_id.clone()).collect();
        let issues = adapter.pull_ids(&ids, None).await.map_err(jfail)?;
        let issues = issues
            .into_iter()
            .map(|is| {
                let scopes = again
                    .iter()
                    .find(|t| t.remote_id == is.remote_id)
                    .map(|t| t.scopes.clone())
                    .unwrap_or_default();
                (is, scopes)
            })
            .collect();
        store
            .apply_pull(page(i, issues, None, true))
            .await
            .map_err(sfail)?;
    }
    if !reconcile {
        return Ok(());
    }
    // R, by id: the scopes that list each now.
    let mut listed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &i.scopes {
        for id in adapter.scope_ids(&s.query).await.map_err(jfail)? {
            listed.entry(id).or_default().push(s.name.clone());
        }
    }
    let known: HashSet<&str> = tracked.iter().map(|t| t.remote_id.as_str()).collect();
    // L - R (design 5.5 step 2): in a scope here, in none there. And the
    // outside Items that no longer exist (step 1): a by-id listing of them
    // leaves out what is gone or forbidden.
    let still: HashSet<String> = adapter
        .existing_ids(&all_outside)
        .await
        .map_err(jfail)?
        .into_iter()
        .collect();
    let mut out = Vec::new();
    for t in &tracked {
        let (id, remote, scopes) = (&t.id, &t.remote_id, &t.scopes);
        let gone_outside = scopes.is_empty() && !still.contains(remote);
        let left = !scopes.is_empty() && !listed.contains_key(remote);
        if !(gone_outside || left) {
            continue;
        }
        let fetched = match adapter.fetch(remote).await.map_err(jfail)? {
            Fetched::Found(issue) => Some(*issue),
            Fetched::NotFound | Fetched::Forbidden => None,
        };
        // Outside and still there: nothing to reconcile.
        if gone_outside && fetched.is_some() {
            continue;
        }
        out.push((*id, fetched));
    }
    if !out.is_empty() {
        store
            .reconcile(Reconcile {
                instance_id: i.id.clone(),
                out_of_scope: out,
                now: Timestamp::now(),
                today: Zoned::now().date(),
            })
            .await
            .map_err(sfail)?;
    }
    // R - L (step 3): listed but not in that scope here, whatever their
    // update time (a scope query edited, a sprint joined): imported, or
    // their membership (and a left_scope archive) back.
    let in_scope: BTreeMap<&str, &Vec<String>> = tracked
        .iter()
        .map(|t| (t.remote_id.as_str(), &t.scopes))
        .collect();
    let missing: Vec<String> = listed
        .iter()
        .filter(|(id, scopes)| match in_scope.get(id.as_str()) {
            None => !known.contains(id.as_str()),
            Some(have) => scopes.iter().any(|s| !have.contains(s)),
        })
        .map(|(id, _)| id.clone())
        .collect();
    if !missing.is_empty() {
        let issues = adapter.pull_ids(&missing, None).await.map_err(jfail)?;
        let issues = issues
            .into_iter()
            .map(|is| {
                let scopes = listed.get(&is.remote_id).cloned().unwrap_or_default();
                (is, scopes)
            })
            .collect();
        store
            .apply_pull(page(i, issues, None, true))
            .await
            .map_err(sfail)?;
    }
    Ok(())
}

/// Items whose pushes one pass sends at most: more wait for the next one.
const PUSH_BATCH: usize = 50;

/// The first retry after a transient failure; it doubles, to [`OFFLINE_MAX`].
const PUSH_RETRY: Duration = Duration::from_secs(15);

/// Sends what the outbox has due (design 5.6, ADR-0007), after the pull, so
/// the conflict rule ran on fresh data. Per Item: read the issue again and
/// run the rule on what Jira has now (a lost answer, or a change since the
/// pull); push what still waits, fields then the status; then read it back
/// and apply it, which settles the rows. A transient failure retries with
/// backoff (a 429's wait honoured); a refusal becomes a push_failed
/// Suggestion and Jira's values stand; a 401 stops the pass as auth_error.
async fn push(
    store: &Store,
    i: &Instance,
    adapter: &Adapter,
    state: &State,
) -> Result<(), Outcome> {
    use todo_jira::{FieldPatch, PushError, Target};
    use todo_store::outbox::PushResult;
    if !i.push.enabled {
        return Ok(());
    }
    let sfail = |e: todo_store::StoreError| store_fail(state, &i.id, &e);
    let due = store
        .due_pushes(i.id.clone(), Timestamp::now(), PUSH_BATCH)
        .await
        .map_err(sfail)?;
    for d in due {
        // Fresh first: the rule on what Jira has now (5.6).
        let issue = match adapter.fetch(&d.remote_id).await {
            Ok(Fetched::Found(issue)) => *issue,
            // Gone or forbidden: the reconciliation breaks the link; the rows
            // wait for it to heal.
            Ok(Fetched::NotFound | Fetched::Forbidden) => continue,
            Err(JiraError::Auth) => return Err(fail(state, &i.id, &JiraError::Auth)),
            Err(e) if e.is_transient() => {
                let at = retry_at(&e, d.attempts);
                store
                    .record_push(
                        d.item_id,
                        d.key.clone(),
                        d.rows,
                        PushResult::Retry {
                            at,
                            error: e.to_string(),
                        },
                        Timestamp::now(),
                    )
                    .await
                    .map_err(sfail)?;
                continue;
            }
            Err(e) => return Err(fail(state, &i.id, &e)),
        };
        store
            .apply_pull(page(i, vec![(issue, Vec::new())], None, false))
            .await
            .map_err(sfail)?;
        // What still waits after the rule: Jira at the base.
        let due = store
            .due_pushes(i.id.clone(), Timestamp::now(), PUSH_BATCH)
            .await
            .map_err(sfail)?;
        let Some(d) = due.into_iter().find(|x| x.item_id == d.item_id) else {
            continue;
        };
        let mut fields = Vec::new();
        let mut target = None;
        let (mut category, mut kind) = (None, None);
        for w in &d.rows {
            use todo_core::{FieldValue as V, ItemField as F};
            match (w.field, &w.new) {
                (F::Title, V::String(t)) => fields.push(FieldPatch::Title(t.clone())),
                (F::Priority, V::Priority(p)) => fields.push(FieldPatch::Priority(*p)),
                (F::Labels, V::StringSet(l)) => fields.push(FieldPatch::Labels(l.clone())),
                (F::DueDate, V::Date(day)) => fields.push(FieldPatch::DueDate(Some(*day))),
                (F::DueDate, V::Cleared) => fields.push(FieldPatch::DueDate(None)),
                (F::StatusCategory, V::StatusCategory(c)) => category = Some(*c),
                (F::ResolutionKind, V::ResolutionKind(k)) => kind = Some(*k),
                _ => {}
            }
        }
        if category.is_some() || kind.is_some() {
            // A kind alone (done to cancelled) is a close again, into Done.
            target = Some(match category.unwrap_or(todo_core::StatusCategory::Done) {
                todo_core::StatusCategory::Todo => Target::Todo,
                todo_core::StatusCategory::InProgress => Target::InProgress,
                todo_core::StatusCategory::Done => {
                    Target::Done(kind.unwrap_or(todo_core::ResolutionKind::Done))
                }
            });
        }
        let sent = async {
            adapter.update_fields(&d.remote_id, &fields).await?;
            if let Some(t) = target {
                adapter.transition(&d.remote_id, t).await?;
            }
            Ok::<(), PushError>(())
        }
        .await;
        let result = match sent {
            Ok(()) => PushResult::Sent,
            Err(PushError::Jira(JiraError::Auth)) => {
                return Err(fail(state, &i.id, &JiraError::Auth));
            }
            Err(PushError::Jira(e)) if e.is_transient() => PushResult::Retry {
                at: retry_at(&e, d.attempts),
                error: e.to_string(),
            },
            Err(e) => PushResult::Refused {
                error: e.to_string(),
                jira: d.rows.iter().map(|w| (w.field, w.base.clone())).collect(),
            },
        };
        let sent_ok = result == PushResult::Sent;
        let refused = matches!(result, PushResult::Refused { .. });
        store
            .record_push(
                d.item_id,
                d.key.clone(),
                d.rows.clone(),
                result,
                Timestamp::now(),
            )
            .await
            .map_err(sfail)?;
        // Read back what Jira has now: a sent push settles (Jira at new);
        // a refused one puts Jira's values back on the Item.
        if (sent_ok || refused)
            && let Ok(Fetched::Found(issue)) = adapter.fetch(&d.remote_id).await
        {
            store
                .apply_pull(page(i, vec![(*issue, Vec::new())], None, false))
                .await
                .map_err(sfail)?;
        }
    }
    Ok(())
}

/// When to try again after `e`: a 429's wait, else 15 s doubling with
/// each attempt, up to ten minutes.
fn retry_at(e: &JiraError, attempts: u32) -> Timestamp {
    let wait = match e {
        JiraError::RateLimited(d) => *d,
        _ => PUSH_RETRY
            .saturating_mul(1u32 << attempts.min(6))
            .min(OFFLINE_MAX),
    };
    Timestamp::now()
        + jiff::SignedDuration::try_from(wait).unwrap_or(jiff::SignedDuration::from_secs(600))
}
