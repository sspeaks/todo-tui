use jiff::Zoned;
use todo_core::{MAX_QUERY_BYTES, parse_query};
use todo_proto::v1::view_service_server::ViewService;
use todo_proto::v1::{
    self, ListViewsRequest, ListViewsResponse, ParseQueryRequest, ParseQueryResponse, QueryError,
};
use tonic::{Request, Response, Status};

use super::Ctx;
use super::item::reference_time;
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

    async fn parse_query(
        &self,
        request: Request<ParseQueryRequest>,
    ) -> Result<Response<ParseQueryResponse>, Status> {
        let req = request.into_inner();
        if req.text.len() > MAX_QUERY_BYTES {
            return Err(error::bad_request(
                "text",
                format!("is longer than {MAX_QUERY_BYTES} bytes"),
            ));
        }
        let today = reference_time(req.reference_time.as_ref(), &Zoned::now())?.date();
        let parsed = parse_query(&req.text);
        let errors: Vec<QueryError> = parsed
            .errors
            .into_iter()
            .map(|e| QueryError {
                start: u32::try_from(e.start).unwrap_or(u32::MAX),
                end: u32::try_from(e.end).unwrap_or(u32::MAX),
                message: e.message,
            })
            .collect();
        let query = parsed.query;
        if !errors.is_empty() {
            return Ok(Response::new(ParseQueryResponse {
                filter: None,
                sort: Vec::new(),
                show_count: query.show_count,
                errors,
                count: None,
            }));
        }
        // A relative date out of range for today is the one error left; it
        // has no span, so it is the whole text's.
        let resolved = match query.filter.resolve(today) {
            Ok(resolved) => resolved,
            Err(e) => {
                return Ok(Response::new(ParseQueryResponse {
                    filter: None,
                    sort: Vec::new(),
                    show_count: query.show_count,
                    errors: vec![QueryError {
                        start: 0,
                        end: u32::try_from(req.text.chars().count()).unwrap_or(u32::MAX),
                        message: e.reason,
                    }],
                    count: None,
                }));
            }
        };
        let count = if req.count {
            Some(
                self.0
                    .store
                    .count_items(resolved)
                    .await
                    .map_err(error::store)?,
            )
        } else {
            None
        };
        Ok(Response::new(ParseQueryResponse {
            filter: Some((&query.filter).into()),
            sort: query.sort.into_iter().map(Into::into).collect(),
            show_count: query.show_count,
            errors,
            count,
        }))
    }
}
