//! Generated todo.v1 protobuf code and core conversions.

pub mod convert;
pub mod export;
pub mod request;

/// `google.*` types compiled from the vendored protos.
#[allow(clippy::all)]
pub mod google {
    pub mod r#type {
        include!(concat!(env!("OUT_DIR"), "/google.r#type.rs"));
    }
}

#[allow(clippy::all)]
pub mod todo {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/todo.v1.rs"));
    }
    pub mod export {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/todo.export.v1.rs"));
        }
    }
}

pub use todo::v1;

/// Encoded `FileDescriptorSet` of todo.v1 and all its imports, for
/// prost-reflect (proto3 JSON) and gRPC reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/todo_descriptor.bin"));

/// The `api_minor` this build serves (design 4.2). 2 added the M2 RPCs as
/// stubs; 3 is the first that implements them. 4 added the M3 RPCs as
/// stubs; 5 is the first that implements them. 6 added the Item's
/// subtask counts and parent title. 7 added recurrence rules and the M5
/// RPCs (SetRecurrence, ParseRecurrence) as stubs; 8 is the first that
/// implements them, reports generated instances and runs the scheduler.
/// 9 added the config: GetConfig, ReloadConfig and Change.config. 10 added
/// query text: ViewService.ParseQuery and View.query. 11 added the view
/// edits: CreateView, UpdateView, DeleteView and MoveView. 12 added
/// SystemService.Backup. 13 added Export and Import. 14 added the
/// Item's energy. 15 added GetActivityReport and the [l48] config. 16
/// added BuildMeetingLink and the [meetings] config. 17 added the
/// [[instances]] of the config (P2, ADR-0006). 18 added the SyncService
/// (GetSyncStatus, SyncNow), Notice.sync_status and OP_KIND_SYNC. 19
/// added SyncNowResponse.request and SyncStatus.served_request. 20 added
/// ClientKind.MOBILE and CreatedVia.MOBILE for the phone app and the
/// remote listener (ADR-0008; the phone-app branch numbered it 9).
/// 21 added pushes (ADR-0007): Suggestion and Change.suggestion, SyncStatus's
/// pending_ops and failed_ops; a linked Item takes edits of its pushed
/// fields. 22 added the push worker's confirm_not_mine (UpdateItem,
/// SetStatus, SetRecurrence, BatchMutate) and NOT_MINE. 23 added the
/// SuggestionService (ListSuggestions, ApplySuggestionPatches,
/// RejectSuggestion), SyncService.ListOutbox, OP_KIND_APPLY_SUGGESTION and
/// OP_KIND_REJECT_SUGGESTION; pending_ops counts a status once and
/// failed_ops only while its push_failed Suggestion waits. 24: a linked
/// Item's description is Markdown, converted from the tracker's wiki markup
/// (ADR-0009), no longer the tracker's text. 25: the description is a
/// pushed field: a person's edit of it is taken and queued, as the title's.
pub const API_MINOR: u32 = 25;
