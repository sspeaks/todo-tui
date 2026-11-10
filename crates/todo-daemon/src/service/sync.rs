//! SyncService (design 5.11): the sync state, "sync now", and the outbox.

use todo_proto::convert::{EnumField as _, timestamp_to_proto};
use todo_proto::v1::sync_service_server::SyncService;
use todo_proto::v1::{
    self, GetSyncStatusRequest, GetSyncStatusResponse, ListOutboxRequest, ListOutboxResponse,
    SyncNowRequest, SyncNowResponse,
};
use tonic::{Code, Request, Response, Status};

use super::Ctx;
use crate::error;

pub(crate) struct Sync(pub(crate) Ctx);

#[tonic::async_trait]
impl SyncService for Sync {
    async fn get_sync_status(
        &self,
        _request: Request<GetSyncStatusRequest>,
    ) -> Result<Response<GetSyncStatusResponse>, Status> {
        let instances = self.0.sync_status.borrow().values().cloned().collect();
        Ok(Response::new(GetSyncStatusResponse { instances }))
    }

    async fn sync_now(
        &self,
        request: Request<SyncNowRequest>,
    ) -> Result<Response<SyncNowResponse>, Status> {
        let req = request.into_inner();
        let config = self.0.config.current();
        let known = config
            .instances
            .iter()
            .any(|i| req.instance_id.is_empty() || i.id == req.instance_id);
        if !known {
            return Err(error::error_info(
                Code::FailedPrecondition,
                "NO_INSTANCE",
                if req.instance_id.is_empty() {
                    "no [[instances]] are configured to sync".to_owned()
                } else {
                    format!("no instance {:?} is configured", req.instance_id)
                },
                [],
            ));
        }
        let request = self.0.sync_now.poke();
        Ok(Response::new(SyncNowResponse { request }))
    }

    async fn list_outbox(
        &self,
        request: Request<ListOutboxRequest>,
    ) -> Result<Response<ListOutboxResponse>, Status> {
        let req = request.into_inner();
        let instance = Some(req.instance_id).filter(|id| !id.is_empty());
        let entries = self
            .0
            .store
            .outbox(instance)
            .await
            .map_err(error::store)?
            .into_iter()
            .map(|e| v1::OutboxEntry {
                instance_id: e.instance_id,
                item_id: e.item_id.to_string(),
                remote_key: e.key,
                title: e.title,
                field: e.field.to_i32(),
                new: Some((&e.new).into()),
                state: match e.state.as_str() {
                    "pending" => v1::OutboxState::Pending,
                    "in_flight" => v1::OutboxState::InFlight,
                    "failed" => v1::OutboxState::Failed,
                    _ => v1::OutboxState::Unspecified,
                } as i32,
                attempts: e.attempts,
                next_attempt_at: e.next_attempt_at.map(timestamp_to_proto),
                last_error: e.last_error.unwrap_or_default(),
                updated_at: Some(timestamp_to_proto(e.updated_at)),
            })
            .collect();
        Ok(Response::new(ListOutboxResponse { entries }))
    }
}
