use jiff::Zoned;
use todo_core::view::{View, ViewId};
use todo_core::{MAX_QUERY_BYTES, parse_query};
use todo_proto::convert::{ConvertError, filter_from_proto, sort_from_proto};
use todo_proto::v1::view_service_server::ViewService;
use todo_proto::v1::{
    self, CreateViewRequest, CreateViewResponse, DeleteViewRequest, DeleteViewResponse,
    ListViewsRequest, ListViewsResponse, MoveViewRequest, MoveViewResponse, ParseQueryRequest,
    ParseQueryResponse, QueryError, UpdateViewRequest, UpdateViewResponse,
};
use todo_store::{
    CreateViewInput, MoveViewInput, UpdateViewInput, ViewOutput, ViewUpdate, check_view_filter,
    check_view_name,
};
use tonic::{Request, Response, Status};

use super::Ctx;
use super::item::{check_request_id, reference_time};
use crate::error;

/// A view's filter and sort, read as a request's are: an invalid filter,
/// then the sort, then a filter not built yet. Either is `None` to leave it
/// unread (an UpdateView mask that does not name it). Paths start with
/// `prefix` (`view.` for UpdateView, whose filter is `view.filter`).
#[allow(clippy::type_complexity)]
fn filter_and_sort(
    prefix: &str,
    filter: Option<Option<&v1::Filter>>,
    sort: Option<&[v1::SortKey]>,
) -> Result<(Option<todo_core::Filter>, Option<Vec<todo_core::SortKey>>), Status> {
    let within = |e: ConvertError| {
        let e = if prefix.is_empty() {
            e
        } else {
            e.within(prefix)
        };
        error::convert(e)
    };
    let no_filter = v1::Filter::default();
    let filter = filter.map(|f| match filter_from_proto(f.unwrap_or(&no_filter), None) {
        Err(e) if !matches!(e, ConvertError::Unimplemented(..)) => Err(within(e)),
        filter => Ok(filter),
    });
    let filter = filter.transpose()?;
    let sort = sort.map(sort_from_proto).transpose().map_err(within)?;
    let filter = filter.transpose().map_err(within)?;
    if let Some(f) = &filter {
        check_view_filter(f).map_err(|e| within(ConvertError::from(e).within("filter")))?;
    }
    Ok((filter, sort))
}

fn view_id(field: &str, id: &str) -> Result<ViewId, Status> {
    id.parse()
        .map_err(|_| error::bad_request(field, "must be a ULID"))
}

fn moved(out: &ViewOutput) -> Vec<v1::View> {
    out.moved.iter().map(v1::View::from).collect()
}

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

    async fn create_view(
        &self,
        request: Request<CreateViewRequest>,
    ) -> Result<Response<CreateViewResponse>, Status> {
        let req = request.into_inner();
        let request_id = check_request_id(&req.request_id)?;
        let (filter, sort) = filter_and_sort("", Some(req.filter.as_ref()), Some(&req.sort))?;
        let (filter, sort) = (filter.unwrap_or_default(), sort.unwrap_or_default());
        let view = View {
            id: None,
            name: req.name,
            filter,
            sort,
            show_count: req.show_count,
            position: 0,
            seed: None,
            revision: 0,
            created_at: None,
            updated_at: None,
        };
        let out = self
            .0
            .store
            .create_view(CreateViewInput {
                request_id,
                view,
                position: req.position,
            })
            .await
            .map_err(error::store)?;
        Ok(Response::new(CreateViewResponse {
            view: out.view.as_ref().map(v1::View::from),
            moved: moved(&out),
            result: Some(out.result.into()),
        }))
    }

    async fn update_view(
        &self,
        request: Request<UpdateViewRequest>,
    ) -> Result<Response<UpdateViewResponse>, Status> {
        let req = request.into_inner();
        let request_id = check_request_id(&req.request_id)?;
        let view = req
            .view
            .ok_or_else(|| error::bad_request("view", "is required"))?;
        let id = view_id("view.id", &view.id)?;
        let paths = req.update_mask.map(|m| m.paths).unwrap_or_default();
        if paths.is_empty() {
            return Err(error::bad_request("update_mask", "names no field"));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, path) in paths.iter().enumerate() {
            let at = format!("update_mask.paths[{i}]");
            if !matches!(path.as_str(), "name" | "filter" | "sort" | "show_count") {
                return Err(error::bad_request(
                    &at,
                    format!("{path:?} cannot be updated: use name, filter, sort or show_count"),
                ));
            }
            if !seen.insert(path.as_str()) {
                return Err(error::bad_request(&at, format!("{path} is named twice")));
            }
        }
        // Only what the mask names is read (AIP-134).
        let (filter, sort) = filter_and_sort(
            "view",
            seen.contains("filter").then_some(view.filter.as_ref()),
            seen.contains("sort").then_some(&view.sort[..]),
        )?;
        let name = match seen.contains("name") {
            true => Some(
                check_view_name(&view.name).map_err(|why| error::bad_request("view.name", why))?,
            ),
            false => None,
        };
        let update = ViewUpdate {
            name,
            filter,
            sort,
            show_count: seen.contains("show_count").then_some(view.show_count),
        };
        let out = self
            .0
            .store
            .update_view(UpdateViewInput {
                request_id,
                id,
                update,
                expected_revision: req.expected_revision,
            })
            .await
            .map_err(error::store)?;
        Ok(Response::new(UpdateViewResponse {
            view: out.view.as_ref().map(v1::View::from),
            result: Some(out.result.into()),
        }))
    }

    async fn delete_view(
        &self,
        request: Request<DeleteViewRequest>,
    ) -> Result<Response<DeleteViewResponse>, Status> {
        let req = request.into_inner();
        let request_id = check_request_id(&req.request_id)?;
        let id = view_id("id", &req.id)?;
        let out = self
            .0
            .store
            .delete_view(request_id, id)
            .await
            .map_err(error::store)?;
        Ok(Response::new(DeleteViewResponse {
            moved: moved(&out),
            result: Some(out.result.into()),
        }))
    }

    async fn move_view(
        &self,
        request: Request<MoveViewRequest>,
    ) -> Result<Response<MoveViewResponse>, Status> {
        let req = request.into_inner();
        let request_id = check_request_id(&req.request_id)?;
        let id = view_id("id", &req.id)?;
        let out = self
            .0
            .store
            .move_view(MoveViewInput {
                request_id,
                id,
                position: req.position,
            })
            .await
            .map_err(error::store)?;
        Ok(Response::new(MoveViewResponse {
            view: out.view.as_ref().map(v1::View::from),
            moved: moved(&out),
            result: Some(out.result.into()),
        }))
    }
}
