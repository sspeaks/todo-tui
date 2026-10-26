use todo_proto::v1::view_service_server::ViewService;
use todo_proto::v1::{self, ListViewsRequest, ListViewsResponse};
use tonic::{Request, Response, Status};

use super::Ctx;
use crate::error;

pub(crate) struct Views(pub(crate) Ctx);

#[tonic::async_trait]
impl ViewService for Views {
    async fn list_views(
        &self,
        _request: Request<ListViewsRequest>,
    ) -> Result<Response<ListViewsResponse>, Status> {
        let (views, revision) = self.0.store.list_views().await.map_err(error::store)?;
        Ok(Response::new(ListViewsResponse {
            views: views.iter().map(v1::View::from).collect(),
            revision,
        }))
    }
}
