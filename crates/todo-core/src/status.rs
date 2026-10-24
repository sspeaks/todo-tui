//! Status storage and derivation (design 3.4).

use serde::{Deserialize, Serialize};

/// Core: tracker status category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    Todo,
    InProgress,
    Done,
}

/// Core: set only while the category is `Done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionKind {
    Done,
    Cancelled,
}

/// Overlay: meaningful only while the category is `Todo`; retained otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoRefinement {
    #[default]
    Inbox,
    Next,
    Someday,
}

/// The derived 6-value status clients see and set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Inbox,
    Next,
    Someday,
    InProgress,
    Done,
    Cancelled,
}

impl Status {
    /// `Done` or `Cancelled`.
    pub fn is_closed(self) -> bool {
        matches!(self, Status::Done | Status::Cancelled)
    }
}

/// The three stored fields behind [`Status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusFields {
    pub status_category: StatusCategory,
    pub resolution_kind: Option<ResolutionKind>,
    pub todo_refinement: TodoRefinement,
}

impl Default for StatusFields {
    fn default() -> Self {
        Self::from_status(Status::Inbox)
    }
}

impl StatusFields {
    /// The fields for a new Item with the given status.
    pub fn from_status(status: Status) -> Self {
        Self::default_refinement(TodoRefinement::Inbox).with_status(status)
    }

    fn default_refinement(todo_refinement: TodoRefinement) -> Self {
        Self {
            status_category: StatusCategory::Todo,
            resolution_kind: None,
            todo_refinement,
        }
    }

    pub fn status(&self) -> Status {
        derive_status(
            self.status_category,
            self.resolution_kind,
            self.todo_refinement,
        )
    }

    /// The `SetStatus` decomposition: todo statuses set the category and the
    /// refinement; other statuses set the category (and resolution) and keep
    /// the current refinement. The resolution is cleared outside `Done`.
    #[must_use]
    pub fn with_status(self, target: Status) -> Self {
        let (status_category, resolution_kind, todo_refinement) = match target {
            Status::Inbox => (StatusCategory::Todo, None, TodoRefinement::Inbox),
            Status::Next => (StatusCategory::Todo, None, TodoRefinement::Next),
            Status::Someday => (StatusCategory::Todo, None, TodoRefinement::Someday),
            Status::InProgress => (StatusCategory::InProgress, None, self.todo_refinement),
            Status::Done => (
                StatusCategory::Done,
                Some(ResolutionKind::Done),
                self.todo_refinement,
            ),
            Status::Cancelled => (
                StatusCategory::Done,
                Some(ResolutionKind::Cancelled),
                self.todo_refinement,
            ),
        };
        Self {
            status_category,
            resolution_kind,
            todo_refinement,
        }
    }

    /// A resolution kind is set only in the `Done` category. `Done` without
    /// one is allowed (a tracker may resolve without a resolution).
    pub fn is_consistent(&self) -> bool {
        self.resolution_kind.is_none() || self.status_category == StatusCategory::Done
    }

    /// Whether moving to `target` changes the Core category or resolution
    /// (for a linked Item that means a tracker transition).
    pub fn changes_category(&self, target: Status) -> bool {
        let next = self.with_status(target);
        next.status_category != self.status_category || next.resolution_kind != self.resolution_kind
    }
}

/// Derives the 6-value status. A `Done` category without a resolution kind
/// reads as `Done`; a resolution kind outside `Done` is ignored.
pub fn derive_status(
    category: StatusCategory,
    resolution_kind: Option<ResolutionKind>,
    refinement: TodoRefinement,
) -> Status {
    match category {
        StatusCategory::Todo => match refinement {
            TodoRefinement::Inbox => Status::Inbox,
            TodoRefinement::Next => Status::Next,
            TodoRefinement::Someday => Status::Someday,
        },
        StatusCategory::InProgress => Status::InProgress,
        StatusCategory::Done => match resolution_kind {
            Some(ResolutionKind::Cancelled) => Status::Cancelled,
            Some(ResolutionKind::Done) | None => Status::Done,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Status; 6] = [
        Status::Inbox,
        Status::Next,
        Status::Someday,
        Status::InProgress,
        Status::Done,
        Status::Cancelled,
    ];

    #[test]
    fn table_from_design_3_4() {
        use ResolutionKind as R;
        use StatusCategory as C;
        use TodoRefinement as T;
        assert_eq!(derive_status(C::Todo, None, T::Inbox), Status::Inbox);
        assert_eq!(derive_status(C::Todo, None, T::Next), Status::Next);
        assert_eq!(derive_status(C::Todo, None, T::Someday), Status::Someday);
        for t in [T::Inbox, T::Next, T::Someday] {
            assert_eq!(derive_status(C::InProgress, None, t), Status::InProgress);
            assert_eq!(derive_status(C::Done, Some(R::Done), t), Status::Done);
            assert_eq!(
                derive_status(C::Done, Some(R::Cancelled), t),
                Status::Cancelled
            );
        }
        assert_eq!(derive_status(C::Done, None, T::Inbox), Status::Done);
    }

    #[test]
    fn set_status_round_trips_from_every_state() {
        for from in ALL {
            for to in ALL {
                let fields = StatusFields::from_status(from).with_status(to);
                assert_eq!(fields.status(), to, "{from:?} -> {to:?}");
                if fields.status_category != StatusCategory::Done {
                    assert_eq!(fields.resolution_kind, None);
                }
            }
        }
    }

    #[test]
    fn refinement_is_retained_outside_todo() {
        let someday = StatusFields::from_status(Status::Someday);
        let done = someday.with_status(Status::Done);
        assert_eq!(done.todo_refinement, TodoRefinement::Someday);
        let in_progress = someday.with_status(Status::InProgress);
        assert_eq!(in_progress.todo_refinement, TodoRefinement::Someday);
        assert_eq!(
            done.with_status(Status::Next).todo_refinement,
            TodoRefinement::Next
        );
    }

    #[test]
    fn consistency() {
        use ResolutionKind as R;
        use StatusCategory as C;
        for from in ALL {
            assert!(StatusFields::from_status(from).is_consistent(), "{from:?}");
        }
        let fields = |status_category, resolution_kind| StatusFields {
            status_category,
            resolution_kind,
            todo_refinement: TodoRefinement::Next,
        };
        assert!(fields(C::Done, None).is_consistent());
        assert!(!fields(C::Todo, Some(R::Done)).is_consistent());
        assert!(!fields(C::InProgress, Some(R::Cancelled)).is_consistent());
    }

    #[test]
    fn category_change_detection() {
        let next = StatusFields::from_status(Status::Next);
        assert!(!next.changes_category(Status::Someday));
        assert!(next.changes_category(Status::InProgress));
        let done = next.with_status(Status::Done);
        assert!(done.changes_category(Status::Cancelled));
        assert!(!done.changes_category(Status::Done));
    }
}
