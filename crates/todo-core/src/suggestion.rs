//! A Suggestion (design 3.11): a proposed change, never applied without a
//! person. P2 makes the sync kinds (ADR-0007).

use jiff::Timestamp;

use crate::field::ItemField;
use crate::ids::ItemId;
use crate::value::FieldValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    /// Jira changed a field while a local edit of it waited, and won.
    SyncConflict,
    /// Jira refused a push.
    PushFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionState {
    Pending,
    Applied,
    PartiallyApplied,
    Rejected,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchState {
    Pending,
    Applied,
    Rejected,
    /// Its field no longer holds `expected_old`.
    Stale,
}

/// One checkbox: set `field` to `new` while it still holds `expected_old`.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    pub id: String,
    pub field: ItemField,
    pub expected_old: FieldValue,
    pub new: FieldValue,
    pub state: PatchState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub id: String,
    pub item_id: ItemId,
    pub kind: SuggestionKind,
    pub rationale: String,
    /// `push_failed`: what Jira said.
    pub error: Option<String>,
    pub state: SuggestionState,
    pub patches: Vec<Patch>,
    pub created_at: Timestamp,
}
