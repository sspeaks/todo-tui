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
    /// Jira asked to wait (a 429's Retry-After, design 5.12): no pass
    /// before then, whatever wakes the loop.
    hold: std::sync::Mutex<Option<Timestamp>>,
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

/// Keeps each instance's outbox counts current (design 5.11), the only
/// writer of them: after every committed change of an Item or a
/// Suggestion, so an edit made offline shows as waiting at once and a
/// reject takes a failure off, whatever the pull is doing; after a config
/// change, which is what an import records when it replaces the outbox;
/// and when the loop adds an instance's status (`recount`). Counts that
/// did not change send nothing.
pub(crate) async fn count_outbox(
    store: Store,
    status: watch::Sender<BTreeMap<String, v1::SyncStatus>>,
    recount: Arc<Notify>,
    mut stop: ShutdownSignal,
) {
    let mut changes = store.subscribe();
    loop {
        tokio::select! {
            got = changes.recv() => match got {
                Ok(row) if !matches!(
                    row.topic,
                    todo_core::Topic::Item | todo_core::Topic::Suggestion | todo_core::Topic::Config
                ) => continue,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
            () = recount.notified() => {}
            () = stop.wait() => return,
        }
        // One count for a burst: a pull's page is many changes.
        while changes.try_recv().is_ok() {}
        let ids: Vec<String> = status.borrow().keys().cloned().collect();
        for id in ids {
            let Ok((pending, failed)) = store.outbox_counts(id.clone()).await else {
                continue;
            };
            status.send_if_modified(|all| match all.get_mut(&id) {
                Some(st) if (st.pending_ops, st.failed_ops) != (pending, failed) => {
                    (st.pending_ops, st.failed_ops) = (pending, failed);
                    true
                }
                _ => false,
            });
        }
    }
}

/// The loop. It runs while the daemon does; a config change re-reads the
/// instance (and starts over with it).
pub(crate) async fn run(
    store: Store,
    mut config: watch::Receiver<Arc<Config>>,
    clients: Clients,
    now: SyncNow,
    status: watch::Sender<BTreeMap<String, v1::SyncStatus>>,
    recount: Arc<Notify>,
    mut stop: ShutdownSignal,
) {
    let state = State {
        tx: status,
        hold: std::sync::Mutex::new(None),
    };
    let queued = store.pushes_queued();
    let mut last_reconcile: Option<Timestamp> = None;
    let mut offline_wait = Duration::from_secs(15);
    let mut retry_wakes = true;
    loop {
        let current = Arc::clone(&config.borrow_and_update());
        let instance = current.instances.first().cloned();
        // A person may edit a linked Item's pushed fields only while its
        // instance pushes; a local field is theirs either way (ADR-0007).
        // A reload sets these at once too, mid-pass.
        crate::config::apply_to_store(&current, &store);
        // Only the configured instances have a status.
        state
            .tx
            .send_modify(|all| all.retain(|id, _| instance.as_ref().is_some_and(|i| &i.id == id)));
        let wait = match &instance {
            None => None,
            Some(i) => {
                // A status from the start, before the credential is read
                // (a keychain prompt can take a while); count_outbox fills
                // in what waits.
                state.set(&i.id, |_| {});
                recount.notify_one();
                let served = now.requested();
                let reconcile = last_reconcile.is_none_or(|at| {
                    Timestamp::now().duration_since(at).unsigned_abs() >= RECONCILE_EVERY
                });
                let held = state
                    .hold
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .filter(|at| *at > Timestamp::now());
                let outcome = match held {
                    // Jira asked to wait: this wake-up waits too.
                    Some(_) => Outcome::Held,
                    None => {
                        // Over: the next wait is the poll's again.
                        *state.hold.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        pass(&store, i, &state, reconcile).await
                    }
                };
                // A push's retry may wake the loop sooner than the poll only
                // after a pass that got as far as pushing: an offline, a
                // refused token or a failing pull waits its own time.
                retry_wakes = matches!(outcome, Outcome::Ok | Outcome::Held);
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
                    // Until the hold ends (the retry's wake-up below).
                    Outcome::Held => Some(i.poll_idle),
                    Outcome::Degraded => Some(i.poll_idle),
                }
            }
        };
        // A push waiting on a retry wakes the loop when it is due, sooner
        // than the poll (a 429's few seconds); never sooner than a second.
        let retry = match &instance {
            Some(i) if retry_wakes && store.pushes_now(&i.id) => {
                store.next_push_at(i.id.clone()).await.ok().flatten()
            }
            _ => None,
        }
        .map(|at| {
            // Already due (a negative wait): in a second.
            Duration::try_from(at.duration_since(Timestamp::now()))
                .unwrap_or_default()
                .max(Duration::from_secs(1))
        });
        let held = state
            .hold
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .filter(|at| *at > Timestamp::now())
            .map(|at| {
                Duration::try_from(at.duration_since(Timestamp::now()))
                    .unwrap_or_default()
                    .max(Duration::from_secs(1))
            });
        let retry = match (retry, held) {
            (Some(r), Some(h)) => Some(r.max(h)),
            (r, h) => r.or(h),
        };
        let wait = match (wait, retry) {
            (Some(w), Some(r)) => Some(w.min(r)),
            (w, r) => w.or(r),
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
    /// Skipped: Jira's Retry-After has not passed.
    Held,
}

fn fail(state: &State, id: &str, e: &JiraError) -> Outcome {
    let (s, outcome) = match e {
        JiraError::Auth => (v1::SyncState::AuthError, Outcome::Auth),
        e if e.is_transient() => (v1::SyncState::Offline, Outcome::Offline),
        _ => (v1::SyncState::Degraded, Outcome::Degraded),
    };
    tracing::warn!(instance = id, error = %e, "sync failed");
    if let JiraError::RateLimited(wait) = e {
        let until = Timestamp::now()
            + jiff::SignedDuration::try_from(*wait).unwrap_or(jiff::SignedDuration::from_secs(60));
        *state.hold.lock().unwrap_or_else(|e| e.into_inner()) = Some(until);
    }
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
    state.set(&i.id, |st| {
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
/// pull); mark what still waits in flight with the value it sends; push the
/// fields and the status as two units, each settled or refused on its own;
/// then read the issue back, which settles what landed. A transient failure
/// ends the batch (the pass ends Offline, and the rows retry with backoff,
/// a 429's wait honoured); a refusal becomes a push_failed Suggestion and
/// Jira's values stand; a 401 stops the pass as auth_error.
async fn push(
    store: &Store,
    i: &Instance,
    adapter: &Adapter,
    state: &State,
) -> Result<(), Outcome> {
    use todo_core::{FieldValue as V, ItemField as F};
    use todo_jira::{FieldPatch, PushError, Target};
    use todo_store::outbox::{PushResult, Waiting};
    // The store's flag, not this pass's config: a reload's push = false
    // stops the pass between Items.
    if !store.pushes_now(&i.id) {
        return Ok(());
    }
    let sfail = |e: todo_store::StoreError| store_fail(state, &i.id, &e);
    let due = store
        .due_pushes(i.id.clone(), Timestamp::now(), PUSH_BATCH)
        .await
        .map_err(sfail)?;
    for d in due {
        if !store.pushes_now(&i.id) {
            return Ok(());
        }
        // Fresh first: the rule on what Jira has now (5.6).
        let issue = match adapter.fetch(&d.remote_id).await {
            Ok(Fetched::Found(issue)) => *issue,
            // Gone or forbidden: the reconciliation breaks the link; the rows
            // wait for it to heal.
            Ok(Fetched::NotFound | Fetched::Forbidden) => continue,
            Err(JiraError::Auth) => return Err(fail(state, &i.id, &JiraError::Auth)),
            Err(e) if e.is_transient() => {
                return later(store, i, state, d.rows.clone(), &d, e).await;
            }
            Err(e) => return Err(fail(state, &i.id, &e)),
        };
        // Not the issue the pull just linked (moved since, or another Jira
        // answering under this id): nothing is sent to it this pass. A move
        // is picked up by the next pull, and the rows go then. (A base_url
        // changed under the same instance id is not caught here once a
        // pull has read the other Jira's issue: that wants the links'
        // origin recorded.)
        if issue.key != d.key {
            tracing::warn!(
                instance = %i.id,
                linked = %d.key,
                found = %issue.key,
                "the issue read back is not the one linked; its pushes wait"
            );
            continue;
        }
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
        // In flight, each with what it sends: a priority as the level Jira
        // reads its name back as (one with no name of its own takes a
        // neighbour's), so the read-back settles it.
        let sends: Vec<(Waiting, V)> = d
            .rows
            .iter()
            .map(|w| {
                let send = match &w.new {
                    V::Priority(p) => V::Priority(adapter.priority_read_back(*p).unwrap_or(*p)),
                    v => v.clone(),
                };
                (w.clone(), send)
            })
            .collect();
        let begun = store
            .begin_push(sends.clone(), Timestamp::now())
            .await
            .map_err(sfail)?;
        // A row left out (edited since, or kept local and so cancelled)
        // changes the Item's pending fields: the read-back shows that too.
        let left_out = begun.len() < sends.len();
        let rows: Vec<Waiting> = sends
            .into_iter()
            .filter(|(w, _)| begun.contains(&w.id))
            .map(|(mut w, send)| {
                w.sent = Some(send.clone());
                w.new = send;
                w
            })
            .collect();
        let status = |w: &Waiting| matches!(w.field, F::StatusCategory | F::ResolutionKind);
        let (moves, edits): (Vec<Waiting>, Vec<Waiting>) = rows.into_iter().partition(status);
        let mut fields = Vec::new();
        for w in &edits {
            match (w.field, &w.new) {
                (F::Title, V::String(t)) => fields.push(FieldPatch::Title(t.clone())),
                (F::Priority, V::Priority(p)) => fields.push(FieldPatch::Priority(*p)),
                (F::Labels, V::StringSet(l)) => fields.push(FieldPatch::Labels(l.clone())),
                (F::DueDate, V::Date(day)) => fields.push(FieldPatch::DueDate(Some(*day))),
                (F::DueDate, V::Cleared) => fields.push(FieldPatch::DueDate(None)),
                _ => {}
            }
        }
        let (mut category, mut kind) = (None, None);
        for w in &moves {
            match &w.new {
                V::StatusCategory(c) => category = Some(*c),
                V::ResolutionKind(k) => kind = Some(*k),
                _ => {}
            }
        }
        // A kind alone (done to cancelled) is a close again, into Done.
        let target = (category.is_some() || kind.is_some()).then(|| {
            match category.unwrap_or(todo_core::StatusCategory::Done) {
                todo_core::StatusCategory::Todo => Target::Todo,
                todo_core::StatusCategory::InProgress => Target::InProgress,
                todo_core::StatusCategory::Done => {
                    Target::Done(kind.unwrap_or(todo_core::ResolutionKind::Done))
                }
            }
        });
        // A reopen goes first: a closed issue may take no edits.
        let reopen = matches!(target, Some(Target::Todo | Target::InProgress));
        let mut units: Vec<(Vec<Waiting>, Option<Target>)> = Vec::new();
        if reopen {
            units.push((moves, target));
            units.push((edits, None));
        } else {
            units.push((edits, None));
            units.push((moves, target));
        }
        let mut read_back = left_out;
        let mut units = units.into_iter();
        while let Some((unit, target)) = units.next() {
            if unit.is_empty() {
                continue;
            }
            let sent = match target {
                Some(t) => adapter.transition(&d.remote_id, t).await,
                None => adapter.update_fields(&d.remote_id, &fields).await,
            };
            let result = match sent {
                Ok(()) => PushResult::Sent,
                Err(PushError::Jira(JiraError::Auth)) => {
                    return Err(fail(state, &i.id, &JiraError::Auth));
                }
                // This unit and the ones not sent behind it wait for the
                // retry, so none reads as sent.
                Err(PushError::Jira(e)) if e.is_transient() => {
                    let rest = unit.into_iter().chain(units.flat_map(|(u, _)| u)).collect();
                    return later(store, i, state, rest, &d, e).await;
                }
                Err(e) => PushResult::Refused {
                    error: e.to_string(),
                    jira: unit.iter().map(|w| (w.field, w.base.clone())).collect(),
                },
            };
            store
                .record_push(d.item_id, d.key.clone(), unit, result, Timestamp::now())
                .await
                .map_err(sfail)?;
            read_back = true;
        }
        // Read back what Jira has now: a sent push settles (Jira at what was
        // sent); a refused one puts Jira's values back on the Item. If this
        // read fails, the Item is read again by id next pass.
        if read_back {
            match adapter.fetch(&d.remote_id).await {
                Ok(Fetched::Found(issue)) => {
                    store
                        .apply_pull(page(i, vec![(*issue, Vec::new())], None, false))
                        .await
                        .map_err(sfail)?;
                }
                Ok(_) => {}
                Err(e) => return Err(fail(state, &i.id, &e)),
            }
        }
    }
    Ok(())
}

/// A transient failure of `d`'s push: `rows` wait for a retry (a 429's
/// wait honoured), and the pass ends Offline, sending nothing more.
async fn later(
    store: &Store,
    i: &Instance,
    state: &State,
    rows: Vec<todo_store::outbox::Waiting>,
    d: &todo_store::outbox::DuePush,
    e: JiraError,
) -> Result<(), Outcome> {
    store
        .record_push(
            d.item_id,
            d.key.clone(),
            rows,
            todo_store::outbox::PushResult::Retry {
                at: retry_at(&e, d.attempts),
                error: e.to_string(),
            },
            Timestamp::now(),
        )
        .await
        .map_err(|e| store_fail(state, &i.id, &e))?;
    Err(fail(state, &i.id, &e))
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
