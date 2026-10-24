use jiff::civil::Date;
use jiff::{Timestamp, Zoned};
use todo_core::{
    Actor, CreatedVia, ItemId, MAX_CAPTURE_BYTES, Mutation, Status as ItemStatus, parse_capture,
};
use todo_proto::convert::{ConvertError, EnumField, timestamp_from_proto};
use todo_proto::request;
use todo_proto::v1::item_service_server::ItemService;
use todo_proto::v1::{
    self, AddRelationRequest, AddRelationResponse, ArchiveItemRequest, ArchiveItemResponse,
    BatchMutateRequest, BatchMutateResponse, CaptureItemRequest, CaptureItemResponse,
    DeleteItemRequest, DeleteItemResponse, Filter, GetItemRequest, GetItemResponse,
    GetUndoStateRequest, GetUndoStateResponse, ItemField, ListItemsRequest, ListItemsResponse,
    Operator, ParseCaptureRequest, ParseCaptureResponse, RedoRequest, RedoResponse,
    RemoveRelationRequest, RemoveRelationResponse, SetStatusRequest, SetStatusResponse, SortKey,
    UnarchiveItemRequest, UnarchiveItemResponse, UndeleteItemRequest, UndeleteItemResponse,
    UndoRequest, UndoResponse, UpdateItemRequest, UpdateItemResponse, clause, field_value, operand,
};
use todo_store::{CaptureInput, ListQuery, MutateInput, MutateOutput, OpKind};
use tonic::{Code, Request, Response, Status};
use ulid::Ulid;

use super::Ctx;
use crate::error::{self, Shape};

/// The most mutations one `BatchMutate` may carry.
const MAX_BATCH: usize = 1000;

pub(crate) struct Items(pub(crate) Ctx);

#[tonic::async_trait]
impl ItemService for Items {
    async fn capture_item(
        &self,
        request: Request<CaptureItemRequest>,
    ) -> Result<Response<CaptureItemResponse>, Status> {
        let req = request.into_inner();
        let request_id = check_request_id(&req.request_id)?;
        check_capture_length(&req.text)?;
        if req.text.trim().is_empty() {
            return Err(error::bad_request("text", "must not be empty"));
        }
        let created_via =
            CreatedVia::from_i32("created_via", req.created_via).map_err(error::convert)?;
        if !matches!(
            created_via,
            CreatedVia::Tui | CreatedVia::Cli | CreatedVia::Mcp
        ) {
            return Err(error::bad_request(
                "created_via",
                "must be TUI, CLI or MCP; the other values are reserved for the daemon",
            ));
        }
        let now = Zoned::now();
        let reference = reference_time(req.reference_time.as_ref(), &now)?;
        let parent_id = req
            .parent_id
            .as_deref()
            .map(|s| {
                s.parse::<ItemId>()
                    .map_err(|_| error::bad_request("parent_id", "must be a ULID"))
            })
            .transpose()?;
        let spool_id = req
            .spool_id
            .filter(|s| !s.is_empty())
            .map(|s| {
                canonical_ulid(&s).ok_or_else(|| error::bad_request("spool_id", "must be a ULID"))
            })
            .transpose()?;

        let draft = parse_capture(&req.text, reference.date());
        let parsed = v1::CaptureDraft::from(&draft);
        let input = CaptureInput {
            request_id,
            draft,
            created_via,
            parent_id,
            spool_id,
            now: reference.timestamp(),
            today: now.date(),
        };
        let (item, result) = self
            .0
            .store
            .capture(input)
            .await
            .map_err(|e| error::mutation(e, &Shape::Capture, &[]))?;
        // A replay returns the Item the original request created at its
        // reference time, so the draft is the text as parsed then.
        let created = item.created_at.to_zoned(now.time_zone().clone()).date();
        let parsed = if created == reference.date() {
            parsed
        } else {
            (&parse_capture(&req.text, created)).into()
        };
        Ok(Response::new(CaptureItemResponse {
            item: Some(v1::Item::from(item)),
            result: Some(result.into()),
            draft: Some(parsed),
        }))
    }

    async fn parse_capture(
        &self,
        request: Request<ParseCaptureRequest>,
    ) -> Result<Response<ParseCaptureResponse>, Status> {
        let req = request.into_inner();
        check_capture_length(&req.text)?;
        let reference = reference_time(req.reference_time.as_ref(), &Zoned::now())?;
        let draft = parse_capture(&req.text, reference.date());
        Ok(Response::new(ParseCaptureResponse {
            draft: Some((&draft).into()),
        }))
    }

    async fn get_item(
        &self,
        request: Request<GetItemRequest>,
    ) -> Result<Response<GetItemResponse>, Status> {
        let req = request.into_inner();
        let id = req
            .id
            .parse::<ItemId>()
            .map_err(|_| error::bad_request("id", "must be a ULID"))?;
        let out = self
            .0
            .store
            .get_item(id, Zoned::now().date())
            .await
            .map_err(error::store)?;
        Ok(Response::new(GetItemResponse {
            item: Some(v1::Item::from(out.item)),
            revision: out.revision,
            resolved_from: out.resolved_from.map(|id| id.to_string()),
        }))
    }

    async fn update_item(
        &self,
        request: Request<UpdateItemRequest>,
    ) -> Result<Response<UpdateItemResponse>, Status> {
        let req = request.into_inner();
        let (item, result) = self
            .single(&req.request_id, OpKind::Update, Shape::Update, |today| {
                request::update_item_from_proto(&req, today)
            })
            .await?;
        Ok(Response::new(UpdateItemResponse { item, result }))
    }

    async fn set_status(
        &self,
        request: Request<SetStatusRequest>,
    ) -> Result<Response<SetStatusResponse>, Status> {
        let req = request.into_inner();
        let (item, result) = self
            .single(&req.request_id, OpKind::SetStatus, Shape::SetStatus, |_| {
                request::set_status_from_proto(&req)
            })
            .await?;
        Ok(Response::new(SetStatusResponse { item, result }))
    }

    async fn archive_item(
        &self,
        request: Request<ArchiveItemRequest>,
    ) -> Result<Response<ArchiveItemResponse>, Status> {
        let req = request.into_inner();
        let (item, result) = self
            .single(&req.request_id, OpKind::Archive, Shape::ItemOp, |_| {
                request::archive_item_from_proto(&req)
            })
            .await?;
        Ok(Response::new(ArchiveItemResponse { item, result }))
    }

    async fn unarchive_item(
        &self,
        request: Request<UnarchiveItemRequest>,
    ) -> Result<Response<UnarchiveItemResponse>, Status> {
        let req = request.into_inner();
        let (item, result) = self
            .single(&req.request_id, OpKind::Unarchive, Shape::ItemOp, |_| {
                request::unarchive_item_from_proto(&req)
            })
            .await?;
        Ok(Response::new(UnarchiveItemResponse { item, result }))
    }

    async fn delete_item(
        &self,
        request: Request<DeleteItemRequest>,
    ) -> Result<Response<DeleteItemResponse>, Status> {
        let req = request.into_inner();
        let (item, result) = self
            .single(&req.request_id, OpKind::Delete, Shape::ItemOp, |_| {
                request::delete_item_from_proto(&req)
            })
            .await?;
        Ok(Response::new(DeleteItemResponse { item, result }))
    }

    async fn undelete_item(
        &self,
        request: Request<UndeleteItemRequest>,
    ) -> Result<Response<UndeleteItemResponse>, Status> {
        let req = request.into_inner();
        let (item, result) = self
            .single(&req.request_id, OpKind::Undelete, Shape::ItemOp, |_| {
                request::undelete_item_from_proto(&req)
            })
            .await?;
        Ok(Response::new(UndeleteItemResponse { item, result }))
    }

    async fn batch_mutate(
        &self,
        request: Request<BatchMutateRequest>,
    ) -> Result<Response<BatchMutateResponse>, Status> {
        let req = request.into_inner();
        let request_id = check_request_id(&req.request_id)?;
        if !(1..=MAX_BATCH).contains(&req.mutations.len()) {
            return Err(error::bad_request(
                "mutations",
                format!("must hold 1 to {MAX_BATCH} mutations"),
            ));
        }
        let now = Zoned::now();
        let mutations = req
            .mutations
            .iter()
            .enumerate()
            .map(|(i, m)| {
                request::mutation_from_proto(m, now.date())
                    .map_err(|e| e.within(format!("mutations[{i}]")))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(error::convert)?;
        let shape = Shape::Batch(&req.mutations);
        let out = self
            .mutate(&request_id, OpKind::Batch, mutations, &now, &shape)
            .await?;
        Ok(Response::new(BatchMutateResponse {
            items: out.items.into_iter().map(v1::Item::from).collect(),
            result: Some(out.result.into()),
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

    async fn undo(&self, _request: Request<UndoRequest>) -> Result<Response<UndoResponse>, Status> {
        Err(not_implemented("Undo"))
    }

    async fn redo(&self, _request: Request<RedoRequest>) -> Result<Response<RedoResponse>, Status> {
        Err(not_implemented("Redo"))
    }

    async fn get_undo_state(
        &self,
        _request: Request<GetUndoStateRequest>,
    ) -> Result<Response<GetUndoStateResponse>, Status> {
        Err(not_implemented("GetUndoState"))
    }

    async fn add_relation(
        &self,
        _request: Request<AddRelationRequest>,
    ) -> Result<Response<AddRelationResponse>, Status> {
        Err(not_implemented("AddRelation"))
    }

    async fn remove_relation(
        &self,
        _request: Request<RemoveRelationRequest>,
    ) -> Result<Response<RemoveRelationResponse>, Status> {
        Err(not_implemented("RemoveRelation"))
    }
}

impl Items {
    /// A single-item RPC: checks `request_id`, converts the request with
    /// `convert` (given today) and runs it as one op of kind `kind`.
    async fn single(
        &self,
        request_id: &str,
        kind: OpKind,
        shape: Shape<'static>,
        convert: impl FnOnce(Date) -> Result<Mutation, ConvertError>,
    ) -> Result<(Option<v1::Item>, Option<v1::MutationResult>), Status> {
        let request_id = check_request_id(request_id)?;
        let now = Zoned::now();
        let mutation = convert(now.date()).map_err(error::convert)?;
        let out = self
            .mutate(&request_id, kind, vec![mutation], &now, &shape)
            .await?;
        let item = out.items.into_iter().next().map(v1::Item::from);
        Ok((item, Some(out.result.into())))
    }

    /// Runs `mutations` as one op by the user at `now`; store errors name
    /// the request fields laid out by `shape`.
    async fn mutate(
        &self,
        request_id: &str,
        kind: OpKind,
        mutations: Vec<Mutation>,
        now: &Zoned,
        shape: &Shape<'_>,
    ) -> Result<MutateOutput, Status> {
        let ids: Vec<ItemId> = mutations.iter().map(|m| m.id).collect();
        let input = MutateInput {
            request_id: request_id.to_owned(),
            actor: Actor::User,
            kind,
            mutations,
            now: now.timestamp(),
            today: now.date(),
        };
        self.0
            .store
            .mutate(input)
            .await
            .map_err(|e| error::mutation(e, shape, &ids))
    }
}

/// UNIMPLEMENTED for an RPC this daemon does not serve yet.
fn not_implemented(rpc: &str) -> Status {
    error::error_info(
        Code::Unimplemented,
        "NOT_IMPLEMENTED",
        format!("{rpc} is not implemented yet"),
        [],
    )
}

/// Every mutating request carries a client ULID (design 4.3 rule 4),
/// returned in canonical form.
fn check_request_id(request_id: &str) -> Result<String, Status> {
    canonical_ulid(request_id).ok_or_else(|| error::bad_request("request_id", "must be a ULID"))
}

/// `s` as the ULID's canonical (upper-case) spelling, so that every spelling
/// of one ULID is one idempotency key; `None` if it is not a ULID.
pub(crate) fn canonical_ulid(s: &str) -> Option<String> {
    s.parse::<Ulid>().ok().map(|ulid| ulid.to_string())
}

/// Refuses capture text over [`MAX_CAPTURE_BYTES`], without echoing it.
fn check_capture_length(text: &str) -> Result<(), Status> {
    if text.len() > MAX_CAPTURE_BYTES {
        let limit = MAX_CAPTURE_BYTES / 1024;
        return Err(error::bad_request(
            "text",
            format!("is longer than {limit} KiB"),
        ));
    }
    Ok(())
}

/// The capture reference time in `now`'s zone: `reference_time` if set
/// and storable, else `now`.
fn reference_time(t: Option<&prost_types::Timestamp>, now: &Zoned) -> Result<Zoned, Status> {
    let Some(t) = t else {
        return Ok(now.clone());
    };
    let t = timestamp_from_proto("reference_time", t).map_err(error::convert)?;
    if !storable(t) {
        return Err(error::bad_request(
            "reference_time",
            "must be between 1677-09-21T00:12:44Z and 2262-04-11T23:47:16Z",
        ));
    }
    Ok(t.to_zoned(now.time_zone().clone()))
}

/// Whether the store can record `t`: its timestamps are i64 nanoseconds.
pub(crate) fn storable(t: Timestamp) -> bool {
    i64::try_from(t.as_nanosecond()).is_ok()
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
