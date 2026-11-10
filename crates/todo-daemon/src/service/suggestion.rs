//! SuggestionService (design 3.11, 2.6): what waits, and applying or
//! rejecting it. P2's Suggestions are the sync kinds (ADR-0007).

use jiff::Zoned;
use todo_core::ItemId;
use todo_proto::v1::suggestion_service_server::SuggestionService;
use todo_proto::v1::{
    self, ApplySuggestionPatchesRequest, ApplySuggestionPatchesResponse, ListSuggestionsRequest,
    ListSuggestionsResponse, RejectSuggestionRequest, RejectSuggestionResponse,
};
use todo_store::{ApplyInput, MAX_PATCHES};
use tonic::{Request, Response, Status};

use super::Ctx;
use super::item::{canonical_ulid, check_request_id};
use crate::error;

pub(crate) struct Suggestions(pub(crate) Ctx);

#[tonic::async_trait]
impl SuggestionService for Suggestions {
    async fn list_suggestions(
        &self,
        request: Request<ListSuggestionsRequest>,
    ) -> Result<Response<ListSuggestionsResponse>, Status> {
        let req = request.into_inner();
        let item = req
            .item_id
            .filter(|id| !id.is_empty())
            .map(|id| {
                id.parse::<ItemId>()
                    .map_err(|_| error::bad_request("item_id", "must be a ULID"))
            })
            .transpose()?;
        let (all, revision) = self
            .0
            .store
            .list_suggestions(item)
            .await
            .map_err(error::store)?;
        Ok(Response::new(ListSuggestionsResponse {
            suggestions: all.iter().map(v1::Suggestion::from).collect(),
            revision,
        }))
    }

    async fn apply_suggestion_patches(
        &self,
        request: Request<ApplySuggestionPatchesRequest>,
    ) -> Result<Response<ApplySuggestionPatchesResponse>, Status> {
        let req = request.into_inner();
        let request_id = check_request_id(&req.request_id)?;
        if !(1..=MAX_PATCHES).contains(&req.patch_ids.len()) {
            return Err(error::bad_request(
                "patch_ids",
                format!("must hold 1 to {MAX_PATCHES} ids"),
            ));
        }
        // Patch ids are ULIDs: anything else names no patch, and is not
        // echoed back at length.
        let mut patch_ids = Vec::with_capacity(req.patch_ids.len());
        for (i, id) in req.patch_ids.iter().enumerate() {
            patch_ids.push(
                canonical_ulid(id).ok_or_else(|| {
                    error::bad_request(&format!("patch_ids[{i}]"), "must be a ULID")
                })?,
            );
        }
        let now = Zoned::now();
        let out = self
            .0
            .store
            .apply_suggestion_patches(ApplyInput {
                request_id,
                patch_ids,
                confirm_not_mine: req.confirm_not_mine,
                now: now.timestamp(),
                today: now.date(),
            })
            .await
            .map_err(error::store)?;
        // An apply may reopen what the scheduler archived.
        self.0.wake.poke();
        Ok(Response::new(ApplySuggestionPatchesResponse {
            result: Some(out.result.into()),
            items: out.items.into_iter().map(v1::Item::from).collect(),
            suggestions: out.suggestions.iter().map(v1::Suggestion::from).collect(),
            skipped_patch_ids: out.skipped,
        }))
    }

    async fn reject_suggestion(
        &self,
        request: Request<RejectSuggestionRequest>,
    ) -> Result<Response<RejectSuggestionResponse>, Status> {
        let req = request.into_inner();
        let request_id = check_request_id(&req.request_id)?;
        let id =
            canonical_ulid(&req.id).ok_or_else(|| error::bad_request("id", "must be a ULID"))?;
        let now = Zoned::now();
        let out = self
            .0
            .store
            .reject_suggestion(request_id, id, now.timestamp(), now.date())
            .await
            .map_err(error::store)?;
        Ok(Response::new(RejectSuggestionResponse {
            suggestion: Some(v1::Suggestion::from(&out.suggestion)),
            result: Some(out.result.into()),
        }))
    }
}
