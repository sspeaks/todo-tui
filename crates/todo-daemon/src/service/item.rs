use jiff::civil::Date;
use jiff::{Timestamp, Zoned};
use todo_core::{
    Actor, CreatedVia, ItemId, MAX_CAPTURE_BYTES, Mutation, Status as ItemStatus, parse_capture,
};
use todo_proto::convert::{
    ConvertError, EnumField, timestamp_from_proto, timestamp_to_proto, undo_skip_to_proto,
};
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
use todo_store::{
    CaptureInput, EndProblem, ListQuery, MutateInput, MutateOutput, OpKind, OpSummaryRow,
    RelationInput, RelationOutput, StoreError, UndoInput, UndoSkipRow,
};
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

    async fn undo(&self, request: Request<UndoRequest>) -> Result<Response<UndoResponse>, Status> {
        let req = request.into_inner();
        let out = self
            .undo_or_redo(&req.request_id, req.expected_op_id.as_deref(), false)
            .await?;
        Ok(Response::new(UndoResponse {
            result: out.result,
            entry: out.entry,
            items: out.items,
            skipped: out.skipped,
        }))
    }

    async fn redo(&self, request: Request<RedoRequest>) -> Result<Response<RedoResponse>, Status> {
        let req = request.into_inner();
        let out = self
            .undo_or_redo(&req.request_id, req.expected_op_id.as_deref(), true)
            .await?;
        Ok(Response::new(RedoResponse {
            result: out.result,
            entry: out.entry,
            items: out.items,
            skipped: out.skipped,
        }))
    }

    async fn get_undo_state(
        &self,
        _request: Request<GetUndoStateRequest>,
    ) -> Result<Response<GetUndoStateResponse>, Status> {
        let state = self.0.store.undo_state().await.map_err(error::store)?;
        Ok(Response::new(GetUndoStateResponse {
            undo: state.undo.map(op_summary),
            redo: state.redo.map(op_summary),
        }))
    }

    async fn add_relation(
        &self,
        request: Request<AddRelationRequest>,
    ) -> Result<Response<AddRelationResponse>, Status> {
        let req = request.into_inner();
        let out = self
            .relation(&req.request_id, req.relation.as_ref(), true)
            .await?;
        Ok(Response::new(AddRelationResponse {
            from_item: Some(out.from_item.into()),
            to_item: Some(out.to_item.into()),
            result: Some(out.result.into()),
        }))
    }

    async fn remove_relation(
        &self,
        request: Request<RemoveRelationRequest>,
    ) -> Result<Response<RemoveRelationResponse>, Status> {
        let req = request.into_inner();
        let out = self
            .relation(&req.request_id, req.relation.as_ref(), false)
            .await?;
        Ok(Response::new(RemoveRelationResponse {
            from_item: Some(out.from_item.into()),
            to_item: Some(out.to_item.into()),
            result: Some(out.result.into()),
        }))
    }
}

/// The fields `UndoResponse` and `RedoResponse` share.
struct Undone {
    result: Option<v1::MutationResult>,
    entry: Option<v1::OpSummary>,
    items: Vec<v1::Item>,
    skipped: Vec<v1::UndoSkip>,
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

    /// `Undo`, or `Redo` when `redo`, by the user now.
    async fn undo_or_redo(
        &self,
        request_id: &str,
        expected_op_id: Option<&str>,
        redo: bool,
    ) -> Result<Undone, Status> {
        let request_id = check_request_id(request_id)?;
        let expected_op_id = expected_op_id
            .map(|id| {
                id.parse::<Ulid>()
                    .map_err(|_| error::bad_request("expected_op_id", "must be a ULID"))
            })
            .transpose()?;
        let now = Zoned::now();
        let input = UndoInput {
            request_id,
            expected_op_id,
            now: now.timestamp(),
            today: now.date(),
        };
        let store = &self.0.store;
        let out = if redo {
            store.redo(input).await
        } else {
            store.undo(input).await
        }
        .map_err(error::store)?;
        Ok(Undone {
            result: Some(out.result.into()),
            entry: out.entry.map(op_summary),
            items: out.items.into_iter().map(v1::Item::from).collect(),
            skipped: out.skipped.iter().map(undo_skip).collect(),
        })
    }

    /// `AddRelation`, or `RemoveRelation` unless `add`, by the user now.
    async fn relation(
        &self,
        request_id: &str,
        relation: Option<&v1::Relation>,
        add: bool,
    ) -> Result<RelationOutput, Status> {
        let request_id = check_request_id(request_id)?;
        let canonical = request::relation_from_proto(relation).map_err(error::convert)?;
        let now = Zoned::now();
        let input = RelationInput {
            request_id,
            relation: canonical,
            now: now.timestamp(),
            today: now.date(),
        };
        let store = &self.0.store;
        let out = if add {
            store.add_relation(input).await
        } else {
            store.remove_relation(input).await
        };
        // Converted above, so `relation` is set.
        let unset = v1::Relation::default();
        let asked = relation.unwrap_or(&unset);
        // The store checks the ends as stored, so for a RELATES_TO the
        // request names the other way round it blames the request's to_id
        // first. The request's from_id comes first, as for every kind.
        let out = match out {
            Err(e @ StoreError::RelationEnd { item_id, .. })
                if item_id == canonical.from()
                    && asked.from_id.parse::<ItemId>().ok() != Some(item_id) =>
            {
                match self.end_problem(canonical.to(), add, now.date()).await? {
                    Some(problem) => Err(StoreError::RelationEnd {
                        item_id: canonical.to(),
                        problem,
                    }),
                    None => Err(e),
                }
            }
            other => other,
        };
        out.map_err(|e| error::mutation(e, &Shape::Relation(asked), &[]))
    }

    /// What makes `id` unfit as a relation end, for an add (`add`) or a
    /// remove: it names no Item or, for an add, a deleted one.
    async fn end_problem(
        &self,
        id: ItemId,
        add: bool,
        today: Date,
    ) -> Result<Option<EndProblem>, Status> {
        match self.0.store.get_item(id, today).await {
            Ok(out) if add && out.item.is_deleted() => Ok(Some(EndProblem::Deleted)),
            Ok(_) => Ok(None),
            Err(StoreError::NotFound(_)) => Ok(Some(EndProblem::Missing)),
            Err(e) => Err(error::store(e)),
        }
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

/// An undo stack entry.
fn op_summary(row: OpSummaryRow) -> v1::OpSummary {
    v1::OpSummary {
        op_id: row.op_id.to_string(),
        kind: op_kind(row.kind) as i32,
        item_ids: row.item_ids.iter().map(ItemId::to_string).collect(),
        at: Some(timestamp_to_proto(row.at)),
        description: row.description,
    }
}

fn op_kind(kind: OpKind) -> v1::OpKind {
    match kind {
        OpKind::Capture => v1::OpKind::Capture,
        OpKind::Update => v1::OpKind::Update,
        OpKind::SetStatus => v1::OpKind::SetStatus,
        OpKind::Archive => v1::OpKind::Archive,
        OpKind::Unarchive => v1::OpKind::Unarchive,
        OpKind::Delete => v1::OpKind::Delete,
        OpKind::Undelete => v1::OpKind::Undelete,
        OpKind::Batch => v1::OpKind::Batch,
        OpKind::Undo => v1::OpKind::Undo,
        OpKind::Redo => v1::OpKind::Redo,
        OpKind::AddRelation => v1::OpKind::AddRelation,
        OpKind::RemoveRelation => v1::OpKind::RemoveRelation,
    }
}

/// A change an undo or redo left alone.
fn undo_skip(skip: &UndoSkipRow) -> v1::UndoSkip {
    undo_skip_to_proto(
        skip.item_id,
        &skip.fields,
        &skip.reason,
        skip.relation.as_ref(),
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_op_kind_has_its_own_proto_kind() {
        let kinds = [
            OpKind::Capture,
            OpKind::Update,
            OpKind::SetStatus,
            OpKind::Archive,
            OpKind::Unarchive,
            OpKind::Delete,
            OpKind::Undelete,
            OpKind::Batch,
            OpKind::Undo,
            OpKind::Redo,
            OpKind::AddRelation,
            OpKind::RemoveRelation,
        ];
        let proto: HashSet<i32> = kinds.iter().map(|&k| op_kind(k) as i32).collect();
        assert_eq!(proto.len(), kinds.len());
        assert!(!proto.contains(&(v1::OpKind::Unspecified as i32)));
        // Every proto kind but UNSPECIFIED is one of them.
        assert!((1..=kinds.len() as i32).all(|n| proto.contains(&n)));
        assert!(v1::OpKind::try_from(kinds.len() as i32 + 1).is_err());
        // The names match: OP_KIND_<the store's snake_case kind>.
        assert_eq!(
            op_kind(OpKind::AddRelation).as_str_name(),
            "OP_KIND_ADD_RELATION"
        );
        assert_eq!(
            op_kind(OpKind::SetStatus).as_str_name(),
            "OP_KIND_SET_STATUS"
        );
    }
}
