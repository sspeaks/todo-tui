use jiff::{Timestamp, Zoned};
use todo_core::{CreatedVia, ItemId, Status as ItemStatus};
use todo_proto::convert::{EnumField, timestamp_from_proto};
use todo_proto::v1::item_service_server::ItemService;
use todo_proto::v1::{
    self, CaptureItemRequest, CaptureItemResponse, Filter, ItemField, ListItemsRequest,
    ListItemsResponse, Operator, SortKey, clause, field_value, operand,
};
use todo_store::{CaptureInput, ListQuery};
use tonic::{Code, Request, Response, Status};
use ulid::Ulid;

use super::Ctx;
use crate::error;

pub(crate) struct Items(pub(crate) Ctx);

#[tonic::async_trait]
impl ItemService for Items {
    async fn capture_item(
        &self,
        request: Request<CaptureItemRequest>,
    ) -> Result<Response<CaptureItemResponse>, Status> {
        let req = request.into_inner();
        if req.request_id.parse::<Ulid>().is_err() {
            return Err(error::bad_request("request_id", "must be a ULID"));
        }
        if req.text.trim().is_empty() {
            return Err(error::bad_request("text", "must not be empty"));
        }
        let created_via =
            CreatedVia::from_i32("created_via", req.created_via).map_err(error::convert)?;
        let now = match &req.reference_time {
            Some(t) => timestamp_from_proto("reference_time", t).map_err(error::convert)?,
            None => Timestamp::now(),
        };
        let parent_id = req
            .parent_id
            .as_deref()
            .map(|s| {
                s.parse::<ItemId>()
                    .map_err(|_| error::bad_request("parent_id", "must be a ULID"))
            })
            .transpose()?;

        // P0: the raw text is the title; the capture parser arrives in P1.
        let input = CaptureInput {
            request_id: req.request_id,
            title: req.text,
            created_via,
            parent_id,
            spool_id: req.spool_id.filter(|s| !s.is_empty()),
            now,
        };
        let (item, result) = self.0.store.capture(input).await.map_err(error::store)?;
        Ok(Response::new(CaptureItemResponse {
            item: Some(v1::Item::from(item)),
            result: Some(result.into()),
        }))
    }

    async fn list_items(
        &self,
        request: Request<ListItemsRequest>,
    ) -> Result<Response<ListItemsResponse>, Status> {
        let req = request.into_inner();
        let filter = req.filter.unwrap_or_default();
        let statuses = p0_statuses(&filter)?;
        let unsupported = if !is_default_order(&req.sort) {
            Some("sort")
        } else if req.view_id.is_some() {
            Some("view_id")
        } else {
            None
        };
        if let Some(what) = unsupported {
            return Err(error::error_info(
                Code::Unimplemented,
                "NOT_IMPLEMENTED",
                format!("ListItems: {what} is not supported yet"),
                [],
            ));
        }
        let page_size = u32::try_from(req.page_size)
            .map_err(|_| error::bad_request("page_size", "must not be negative"))?;

        let query = ListQuery {
            statuses,
            include_archived: filter.include_archived,
            include_done: filter.include_done,
            include_deferred: filter.include_deferred,
            include_deleted: filter.include_deleted,
            page_size,
            page_token: Some(req.page_token).filter(|t| !t.is_empty()),
            today: Some(Zoned::now().date()),
        };
        let page = self.0.store.list_items(query).await.map_err(error::store)?;
        Ok(Response::new(ListItemsResponse {
            items: page.items.into_iter().map(v1::Item::from).collect(),
            next_page_token: page.next_page_token.unwrap_or_default(),
            revision: page.revision,
        }))
    }
}

/// The store's only P0 order is `created_at, id` ascending; a sort that asks
/// for exactly that (or a prefix of it) is accepted.
fn is_default_order(sort: &[SortKey]) -> bool {
    const DEFAULT: [ItemField; 2] = [ItemField::CreatedAt, ItemField::Id];
    sort.len() <= DEFAULT.len()
        && sort
            .iter()
            .zip(DEFAULT)
            .all(|(k, f)| k.key == f as i32 && !k.descending)
}

/// The P0 subset of `Filter`: at most one `status IN/EQ (...)` predicate in
/// `all_of`. Anything else is UNIMPLEMENTED until the P1 filter compiler.
fn p0_statuses(filter: &Filter) -> Result<Vec<ItemStatus>, Status> {
    let unimplemented = || {
        error::error_info(
            Code::Unimplemented,
            "NOT_IMPLEMENTED",
            "ListItems: only a single status IN/EQ predicate is supported yet",
            [],
        )
    };
    if !filter.any_of.is_empty() || filter.all_of.len() > 1 {
        return Err(unimplemented());
    }
    let Some(clause) = filter.all_of.first() else {
        return Ok(Vec::new());
    };
    let Some(clause::Kind::Predicate(p)) = &clause.kind else {
        return Err(unimplemented());
    };
    let op_ok = matches!(Operator::try_from(p.op), Ok(Operator::In | Operator::Eq));
    if p.target != ItemField::Status as i32 || !op_ok || p.operands.is_empty() {
        return Err(unimplemented());
    }
    p.operands
        .iter()
        .map(|o| match &o.kind {
            Some(operand::Kind::Value(v)) => match v.kind {
                Some(field_value::Kind::Status(s)) => {
                    ItemStatus::from_i32("filter.all_of.operands", s).map_err(error::convert)
                }
                _ => Err(error::bad_request(
                    "filter.all_of.operands",
                    "expected status values",
                )),
            },
            _ => Err(error::bad_request(
                "filter.all_of.operands",
                "expected status values",
            )),
        })
        .collect()
}
