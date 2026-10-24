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
    self, CaughtUp, ResyncRequired, SubscribeRequest, SubscribeResponse, change,
    subscribe_response::Message,
};
use todo_store::{ChangeRow, ChangesSinceError, Resource, Store};
use tokio::sync::broadcast::error::RecvError;
use tonic::{Request, Response, Status};

use super::Ctx;
use crate::error;

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
        let mut shutdown = self.0.shutdown.signal();
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
                                yield Ok(change_message(row));
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

            loop {
                let next = tokio::select! {
                    next = live.recv() => next,
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
                                        yield Ok(change_message(row));
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
                    yield Ok(change_message(row));
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

fn message(m: Message) -> SubscribeResponse {
    SubscribeResponse { message: Some(m) }
}

fn change_message(row: ChangeRow) -> SubscribeResponse {
    let resource = row.resource.map(|r| match r {
        Resource::Item(item) => change::Resource::Item(Box::new(v1::Item::from(&*item))),
    });
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
    use futures_util::StreamExt as _;
    use todo_core::CreatedVia;
    use todo_store::CaptureInput;

    use super::*;
    use crate::ShutdownHandle;
    use crate::conn::Clients;

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
        let watch = Watch(Ctx {
            store: store.clone(),
            clients: Clients::default(),
            shutdown: ShutdownHandle::new(),
        });
        let mut stream = watch
            .subscribe(Request::new(SubscribeRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert!(matches!(next(&mut stream).await, Message::CaughtUp(c) if c.revision == 0));

        capture(&store, N as usize).await;
        let mut revisions = Vec::new();
        while revisions.len() < N as usize {
            match next(&mut stream).await {
                Message::Change(c) => revisions.push(c.revision),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(revisions, (1..=N).collect::<Vec<_>>());
    }
}
