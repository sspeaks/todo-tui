//! `WatchService.Subscribe` (design 4.4): subscribe to the live broadcast
//! first, replay from the database, send `CaughtUp`, then stream live rows,
//! skipping any revision already sent.

use std::collections::HashSet;
use std::pin::Pin;

use futures_util::Stream;
use todo_core::{Revision, Topic};
use todo_proto::convert::EnumField;
use todo_proto::v1::watch_service_server::WatchService;
use todo_proto::v1::{
    self, CaughtUp, Heartbeat, ResyncRequired, SubscribeRequest, SubscribeResponse, change,
    subscribe_response::Message,
};
use todo_store::{ChangeRow, ChangesSinceError, Resource, Store};
use tokio::sync::broadcast::error::RecvError;
use tonic::{Request, Response, Status};

use super::Ctx;
use crate::config::ConfigHandle;
use crate::error;

/// How often a quiet stream gets a Heartbeat: well inside the 5 minutes
/// a fetch body may sit idle in Node, so the remote listener's streams are
/// not dropped for quiet, and their clients notice one that died.
pub(crate) const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(25);

pub(crate) struct Watch(pub(crate) Ctx);

type SubscribeStream = Pin<Box<dyn Stream<Item = Result<SubscribeResponse, Status>> + Send>>;

#[tonic::async_trait]
impl WatchService for Watch {
    type SubscribeStream = SubscribeStream;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<SubscribeStream>, Status> {
        let req = request.into_inner();
        let topics = req
            .topics
            .iter()
            .map(|&t| Topic::from_i32("topics", t))
            .collect::<Result<HashSet<_>, _>>()
            .map_err(error::convert)?;
        let wants = move |row: &ChangeRow| topics.is_empty() || topics.contains(&row.topic);

        let store = self.0.store.clone();
        let config = self.0.config.clone();
        let mut sync = self.0.sync_status.clone();
        let mut shutdown = self.0.shutdown.signal();
        let every = self.0.heartbeat;
        let stream = async_stream::stream! {
            let mut live = store.subscribe();
            let mut last = match req.since_revision {
                None => store.current_revision(),
                Some(since) => match replay(&store, since).await {
                    Ok(Replay::Rows(rows)) => {
                        let mut last = since;
                        for row in rows {
                            last = row.revision;
                            if wants(&row) {
                                yield Ok(change_message(row, &config));
                            }
                        }
                        last
                    }
                    Ok(Replay::Resync(current)) => {
                        yield Ok(message(Message::ResyncRequired(ResyncRequired { current_revision: current })));
                        current
                    }
                    Err(status) => {
                        yield Err(status);
                        return;
                    }
                },
            };
            yield Ok(message(Message::CaughtUp(CaughtUp { revision: last })));
            // The sync state as it is, then each change of it (design 5.11).
            let mut sent: std::collections::BTreeMap<String, v1::SyncStatus> =
                sync.borrow_and_update().clone();
            for status in sent.values() {
                yield Ok(sync_notice(status.clone()));
            }
            let mut sync_closed = false;
            let mut beat = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
            beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                let next = tokio::select! {
                    _ = beat.tick() => {
                        yield Ok(message(Message::Heartbeat(Heartbeat {})));
                        continue;
                    }
                    next = live.recv() => next,
                    changed = sync.changed(), if !sync_closed => {
                        if changed.is_err() {
                            // The sync task is gone with the daemon.
                            sync_closed = true;
                            continue;
                        }
                        let now = sync.borrow_and_update().clone();
                        for (id, status) in &now {
                            if sent.get(id) != Some(status) {
                                yield Ok(sync_notice(status.clone()));
                            }
                        }
                        // An instance no longer configured: its last word.
                        for id in sent.keys().filter(|id| !now.contains_key(*id)) {
                            yield Ok(sync_notice(v1::SyncStatus {
                                instance_id: id.clone(),
                                state: v1::SyncState::Unspecified as i32,
                                last_error: "no longer configured".to_owned(),
                                ..v1::SyncStatus::default()
                            }));
                        }
                        sent = now;
                        continue;
                    }
                    () = shutdown.wait() => break,
                };
                let row = match next {
                    Ok(row) if row.revision <= last => continue,
                    Ok(row) if row.revision == last + 1 => row,
                    // A gap or a lagged receiver: fill in from the database.
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        match replay(&store, last).await {
                            Ok(Replay::Rows(rows)) => {
                                for row in rows {
                                    last = row.revision;
                                    if wants(&row) {
                                        yield Ok(change_message(row, &config));
                                    }
                                }
                            }
                            Ok(Replay::Resync(current)) => {
                                last = current;
                                yield Ok(message(Message::ResyncRequired(ResyncRequired { current_revision: current })));
                            }
                            Err(status) => {
                                yield Err(status);
                                return;
                            }
                        }
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                };
                last = row.revision;
                if wants(&row) {
                    yield Ok(change_message(row, &config));
                }
            }
        };
        Ok(Response::new(Box::pin(stream)))
    }
}

enum Replay {
    Rows(Vec<ChangeRow>),
    /// `since` is outside the replay window; carries the current revision.
    Resync(Revision),
}

async fn replay(store: &Store, since: Revision) -> Result<Replay, Status> {
    match store.changes_since(since).await {
        Ok(rows) => Ok(Replay::Rows(rows)),
        Err(ChangesSinceError::Expired(_) | ChangesSinceError::Ahead { .. }) => {
            Ok(Replay::Resync(store.current_revision()))
        }
        Err(ChangesSinceError::Store(e)) => Err(error::store(e)),
    }
}

fn sync_notice(status: v1::SyncStatus) -> SubscribeResponse {
    message(Message::Notice(v1::Notice {
        kind: Some(v1::notice::Kind::SyncStatus(status)),
    }))
}

fn message(m: Message) -> SubscribeResponse {
    SubscribeResponse { message: Some(m) }
}

/// A change as sent. Config is not stored, so a config change, live or
/// replayed, carries the config current now.
fn change_message(row: ChangeRow, config: &ConfigHandle) -> SubscribeResponse {
    let resource = match row.resource {
        Some(Resource::Item(item)) => {
            Some(change::Resource::Item(Box::new(v1::Item::from(&*item))))
        }
        Some(Resource::Suggestion(s)) => {
            Some(change::Resource::Suggestion(v1::Suggestion::from(&*s)))
        }
        Some(Resource::Person(p)) => Some(change::Resource::Person(v1::Person::from(&*p))),
        Some(Resource::View(view)) => {
            Some(change::Resource::View(Box::new(v1::View::from(&*view))))
        }
        None if row.topic == Topic::Config && !row.deleted => Some(change::Resource::Config(
            Box::new(v1::ConfigSnapshot::from(&*config.current())),
        )),
        None => None,
    };
    message(Message::Change(v1::Change {
        revision: row.revision,
        topic: row.topic.to_i32(),
        id: row.id,
        deleted: row.deleted,
        resource,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures_util::StreamExt as _;
    use todo_core::CreatedVia;
    use todo_store::CaptureInput;

    use super::*;
    use crate::ShutdownHandle;
    use crate::config::Config;
    use crate::conn::Clients;
    use crate::scheduler::Wake;

    async fn capture(store: &Store, n: usize) {
        for i in 0..n {
            store
                .capture(CaptureInput {
                    request_id: todo_core::new_ulid().to_string(),
                    draft: todo_core::CaptureDraft::from_title(format!("item {i}")),
                    created_via: CreatedVia::Cli,
                    parent_id: None,
                    spool_id: None,
                    now: jiff::Timestamp::now(),
                    today: jiff::Zoned::now().date(),
                })
                .await
                .unwrap();
        }
    }

    fn ctx(store: &Store, heartbeat: std::time::Duration) -> Ctx {
        Ctx {
            store: store.clone(),
            clients: Clients::default(),
            shutdown: ShutdownHandle::new(),
            wake: Wake::default(),
            config: ConfigHandle::new(Config::defaults("/c/config.toml".into())),
            backups: "/c/backups".into(),
            sync_status: crate::sync::status_channel().1,
            sync_now: crate::sync::SyncNow::default(),
            heartbeat,
            assistant: crate::assistant::Queue::new().0,
            links: crate::sync::Links::default(),
        }
    }

    /// A quiet stream gets a Heartbeat every interval, and a change still
    /// comes through between them.
    #[tokio::test]
    async fn a_quiet_stream_gets_heartbeats() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("todo.db")).unwrap();
        let every = std::time::Duration::from_millis(200);
        let watch = Watch(ctx(&store, every));
        let mut stream = watch
            .subscribe(Request::new(SubscribeRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert!(matches!(next(&mut stream).await, Message::CaughtUp(_)));
        let start = std::time::Instant::now();
        assert!(matches!(next(&mut stream).await, Message::Heartbeat(_)));
        assert!(start.elapsed() >= every);
        capture(&store, 1).await;
        assert!(matches!(next(&mut stream).await, Message::Change(_)));
        assert!(matches!(next(&mut stream).await, Message::Heartbeat(_)));
        assert!(start.elapsed() >= every * 2);
    }

    async fn next(stream: &mut SubscribeStream) -> Message {
        stream.next().await.unwrap().unwrap().message.unwrap()
    }

    /// The stream is lazy, so writes made while it is not polled overflow
    /// the broadcast channel; the subscriber must refill from the database.
    #[tokio::test]
    async fn lagged_subscriber_refills_from_the_database() {
        const N: u64 = 3000;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("todo.db")).unwrap();
        let watch = Watch(ctx(&store, HEARTBEAT));
        let mut stream = watch
            .subscribe(Request::new(SubscribeRequest::default()))
            .await
            .unwrap()
            .into_inner();
        // The store starts at the revision of its seeded views.
        let seeded = store.current_revision();
        assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == seeded));

        capture(&store, N as usize).await;
        let mut revisions = Vec::new();
        while revisions.len() < N as usize {
            match next(&mut stream).await {
                Message::Change(c) => revisions.push(c.revision),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(revisions, (seeded + 1..=seeded + N).collect::<Vec<_>>());
    }

    /// A view change reaches a live subscriber, and a later replay, with
    /// the whole View.
    #[tokio::test]
    async fn view_changes_stream_and_replay_with_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("todo.db")).unwrap();
        let watch = Watch(ctx(&store, HEARTBEAT));
        let subscribe = async |since_revision| {
            let req = SubscribeRequest {
                since_revision,
                topics: vec![Topic::View.to_i32()],
            };
            watch
                .subscribe(Request::new(req))
                .await
                .unwrap()
                .into_inner()
        };
        let mut live = subscribe(None).await;
        let seeded = store.current_revision();
        assert!(matches!(next(&mut live).await, Message::CaughtUp(c) if c.revision == seeded));

        // Renames the Today view, as editing a view will (M6).
        let (views, _) = store.list_views().await.unwrap();
        let mut view = views[1].clone();
        let renamed = store
            .write(move |tx| {
                let id = view.id.unwrap().to_string();
                view.name = "Now".into();
                view.revision = tx.next_revision();
                tx.conn().execute(
                    &format!(
                        "UPDATE views SET name = 'Now', revision = {} WHERE id = '{id}'",
                        view.revision
                    ),
                    [],
                )?;
                let resource = Resource::View(Arc::new(view.clone()));
                tx.record_change(Topic::View, &id, false, Some(resource))?;
                Ok(view)
            })
            .await
            .unwrap();
        let Message::Change(streamed) = next(&mut live).await else {
            panic!("expected a change");
        };
        let resource = change::Resource::View(Box::new(v1::View::from(&renamed)));
        assert_eq!(streamed.revision, seeded + 1);
        assert_eq!(streamed.resource, Some(resource));
        let mut replay = subscribe(Some(seeded)).await;
        assert_eq!(next(&mut replay).await, Message::Change(streamed));
    }
}
