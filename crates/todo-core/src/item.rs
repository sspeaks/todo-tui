//! The Item, mirroring `todo.v1.Item` layer by layer (design 3.1-3.3).

use std::collections::BTreeMap;

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};

use crate::field::ItemField;
use crate::ids::{ItemId, PersonId, Revision};
use crate::status::{ResolutionKind, Status, StatusCategory, StatusFields, TodoRefinement};

/// `None` sorts after `Lowest` in views; the derived `Ord` here is by rank
/// (`None` lowest) and is not a sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Priority {
    #[default]
    None,
    Lowest,
    Low,
    Medium,
    High,
    Highest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CreatedVia {
    Tui,
    Cli,
    Mcp,
    RemoteImport,
    Restore,
    Split,
    Recurrence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveReason {
    User,
    LeftScope,
    RemoteUnavailable,
    AutoDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkState {
    Pending,
    Ok,
    Broken,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PersonRef {
    Person(PersonId),
    /// Only valid for `waiting.on`.
    FreeText(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub on: PersonRef,
    pub reason: String,
    pub since: Date,
    pub follow_up_on: Option<Date>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Archived {
    pub at: Timestamp,
    pub reason: ArchiveReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLink {
    pub tracker_kind: String,
    pub instance_id: String,
    /// The join key.
    pub remote_id: String,
    /// Display only.
    pub key: String,
    pub url: String,
    pub last_pulled_version: Option<Timestamp>,
    pub link_state: LinkState,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRecurrence {
    pub series_id: ulid::Ulid,
    pub occurrence_date: Option<Date>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StalenessSignals {
    pub last_user_touch_at: Option<Timestamp>,
    pub last_remote_change_at: Option<Timestamp>,
    pub status_entered_at: Option<Timestamp>,
    pub waiting_since: Option<Timestamp>,
}

impl StalenessSignals {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Core layer: remote-owned once linked.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreFields {
    pub title: String,
    /// Markdown.
    pub description: String,
    pub item_type: String,
    pub status_category: StatusCategory,
    pub resolution_kind: Option<ResolutionKind>,
    pub remote_status_name: Option<String>,
    pub priority: Priority,
    pub assignee: Option<PersonRef>,
    pub reporter: Option<PersonRef>,
    pub labels: Vec<String>,
    pub components: Vec<String>,
    pub fix_versions: Vec<String>,
    pub due_date: Option<Date>,
    pub parent_id: Option<ItemId>,
    pub sprint: Option<String>,
    pub story_points: Option<f64>,
    pub original_estimate: Option<SignedDuration>,
    pub remaining_estimate: Option<SignedDuration>,
    pub resolution: Option<String>,
    pub remote_created_at: Option<Timestamp>,
    pub remote_updated_at: Option<Timestamp>,
}

impl Default for CoreFields {
    fn default() -> Self {
        let status = StatusFields::default();
        Self {
            title: String::new(),
            description: String::new(),
            item_type: Item::DEFAULT_ITEM_TYPE.to_owned(),
            status_category: status.status_category,
            resolution_kind: status.resolution_kind,
            remote_status_name: None,
            priority: Priority::None,
            assignee: None,
            reporter: None,
            labels: Vec::new(),
            components: Vec::new(),
            fix_versions: Vec::new(),
            due_date: None,
            parent_id: None,
            sprint: None,
            story_points: None,
            original_estimate: None,
            remaining_estimate: None,
            resolution: None,
            remote_created_at: None,
            remote_updated_at: None,
        }
    }
}

/// Overlay layer: always local, never pushed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OverlayFields {
    pub todo_refinement: TodoRefinement,
    pub waiting: Option<Waiting>,
    pub defer_until: Option<Date>,
    /// Markdown.
    pub private_notes: String,
    /// ISO week, `YYYY-Www`.
    pub focus_week: Option<String>,
    pub local_priority_override: Option<Priority>,
    pub archived: Option<Archived>,
    pub recurrence: Option<ItemRecurrence>,
}

/// Derived and sync-state fields maintained by the daemon. `status` and
/// `effective_priority` are computed by [`Item`] methods instead.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DerivedFields {
    pub is_mine: bool,
    pub pending_fields: Vec<ItemField>,
    pub has_conflict: bool,
    pub has_push_failure: bool,
    pub description_lossy: bool,
    pub description_dirty: bool,
    pub staleness: StalenessSignals,
    pub scopes: Vec<String>,
    pub deleted_at: Option<Timestamp>,
    pub merged_into: Option<ItemId>,
    pub is_overdue: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: ItemId,
    pub created_via: CreatedVia,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub revision: Revision,
    pub remote_link: Option<RemoteLink>,
    pub core: CoreFields,
    pub overlay: OverlayFields,
    /// Tracker field id -> raw JSON value. Read-only.
    pub remote_extras: BTreeMap<String, serde_json::Value>,
    pub derived: DerivedFields,
}

impl Item {
    pub const DEFAULT_ITEM_TYPE: &str = "task";

    /// A fresh capture: title only, `status = inbox`. `revision` is 0 until stored.
    pub fn new_capture(id: ItemId, title: String, created_via: CreatedVia, now: Timestamp) -> Self {
        Self {
            id,
            created_via,
            created_at: now,
            updated_at: now,
            revision: 0,
            remote_link: None,
            core: CoreFields {
                title,
                ..CoreFields::default()
            },
            overlay: OverlayFields::default(),
            remote_extras: BTreeMap::new(),
            derived: DerivedFields::default(),
        }
    }

    pub fn status_fields(&self) -> StatusFields {
        StatusFields {
            status_category: self.core.status_category,
            resolution_kind: self.core.resolution_kind,
            todo_refinement: self.overlay.todo_refinement,
        }
    }

    pub fn set_status_fields(&mut self, fields: StatusFields) {
        self.core.status_category = fields.status_category;
        self.core.resolution_kind = fields.resolution_kind;
        self.overlay.todo_refinement = fields.todo_refinement;
    }

    /// The derived 6-value status.
    pub fn status(&self) -> Status {
        self.status_fields().status()
    }

    /// `local_priority_override` if set, else `priority`.
    pub fn effective_priority(&self) -> Priority {
        self.overlay
            .local_priority_override
            .unwrap_or(self.core.priority)
    }

    pub fn is_linked(&self) -> bool {
        self.remote_link.is_some()
    }

    pub fn is_deleted(&self) -> bool {
        self.derived.deleted_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::new_ulid;

    #[test]
    fn capture_is_inbox_with_defaults() {
        let item = Item::new_capture(
            new_ulid(),
            "call dentist".into(),
            CreatedVia::Cli,
            Timestamp::UNIX_EPOCH,
        );
        assert_eq!(item.status(), Status::Inbox);
        assert_eq!(item.core.item_type, "task");
        assert_eq!(item.effective_priority(), Priority::None);
        assert!(!item.is_linked());
        assert!(!item.is_deleted());
    }

    #[test]
    fn effective_priority_prefers_override() {
        let mut item = Item::new_capture(
            new_ulid(),
            "x".into(),
            CreatedVia::Tui,
            Timestamp::UNIX_EPOCH,
        );
        item.core.priority = Priority::High;
        assert_eq!(item.effective_priority(), Priority::High);
        item.overlay.local_priority_override = Some(Priority::Lowest);
        assert_eq!(item.effective_priority(), Priority::Lowest);
    }

    #[test]
    fn set_status_through_item() {
        let mut item = Item::new_capture(
            new_ulid(),
            "x".into(),
            CreatedVia::Tui,
            Timestamp::UNIX_EPOCH,
        );
        item.set_status_fields(item.status_fields().with_status(Status::Cancelled));
        assert_eq!(item.status(), Status::Cancelled);
        assert_eq!(item.core.resolution_kind, Some(ResolutionKind::Cancelled));
        assert_eq!(item.overlay.todo_refinement, TodoRefinement::Inbox);
    }
}
