//! SyncService (design 5.11): the sync state, and "sync now".

use todo_proto::v1::sync_service_server::SyncService;
use todo_proto::v1::{
    GetSyncStatusRequest, GetSyncStatusResponse, SyncNowRequest, SyncNowResponse,
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
}
