use todo_proto::v1::view_service_server::ViewService;
use todo_proto::v1::{ListViewsRequest, ListViewsResponse};
use tonic::{Request, Response, Status};

use super::not_implemented;

pub(crate) struct Views;

#[tonic::async_trait]
impl ViewService for Views {
    async fn list_views(
        &self,
        _request: Request<ListViewsRequest>,
    ) -> Result<Response<ListViewsResponse>, Status> {
        Err(not_implemented("ListViews"))
    }
}
