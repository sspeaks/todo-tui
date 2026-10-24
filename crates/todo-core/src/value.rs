//! Typed field values, mirroring `todo.v1.FieldValue` (design 4.8).

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::field::ItemField;
use crate::ids::ItemId;
use crate::item::{
    ArchiveReason, Archived, CreatedVia, Item, ItemRecurrence, LinkState, PersonRef, Priority,
    RemoteLink, StalenessSignals, Waiting,
};
use crate::status::{ResolutionKind, Status, StatusCategory, TodoRefinement};

/// The value of one [`ItemField`]: event `old`/`new` payloads, patch values
/// and filter operands. The serde form is persisted in events, so it must
/// stay stable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldValue {
    /// Absence, for fields without a domain "none". Never used for priority.
    Cleared,
    String(String),
    Markdown(String),
    Bool(bool),
    Number(f64),
    Uint(u64),
    Date(Date),
    Timestamp(Timestamp),
    Duration(SignedDuration),
    StringSet(Vec<String>),
    Person(PersonRef),
    ItemId(ItemId),
    /// `YYYY-Www`.
    IsoWeek(String),
    Priority(Priority),
    Status(Status),
    StatusCategory(StatusCategory),
    ResolutionKind(ResolutionKind),
    TodoRefinement(TodoRefinement),
    CreatedVia(CreatedVia),
    ArchiveReason(ArchiveReason),
    LinkState(LinkState),
    ItemFieldSet(Vec<ItemField>),
    Waiting(Waiting),
    Archived(Archived),
    Recurrence(ItemRecurrence),
    RemoteLink(RemoteLink),
    Staleness(StalenessSignals),
}

impl FieldValue {
    /// The current value of `field` on `item` in its canonical variant;
    /// `None` only for `RemoteExtras`, which has no `FieldValue` form.
    pub fn of(item: &Item, field: ItemField) -> Option<FieldValue> {
        use FieldValue as V;
        use ItemField as F;
        let core = &item.core;
        let overlay = &item.overlay;
        let derived = &item.derived;
        let waiting = overlay.waiting.as_ref();
        let archived = overlay.archived.as_ref();
        let link = item.remote_link.as_ref();
        let recurrence = overlay.recurrence.as_ref();
        let staleness = &derived.staleness;
        let value = match field {
            F::Id => V::ItemId(item.id),
            F::CreatedVia => V::CreatedVia(item.created_via),
            F::CreatedAt => V::Timestamp(item.created_at),
            F::UpdatedAt => V::Timestamp(item.updated_at),
            F::Revision => V::Uint(item.revision),
            F::RemoteLink => opt(link.cloned(), V::RemoteLink),
            F::Title => V::String(core.title.clone()),
            F::Description => V::Markdown(core.description.clone()),
            F::ItemType => V::String(core.item_type.clone()),
            F::StatusCategory => V::StatusCategory(core.status_category),
            F::ResolutionKind => opt(core.resolution_kind, V::ResolutionKind),
            F::RemoteStatusName => opt(core.remote_status_name.clone(), V::String),
            F::Priority => V::Priority(core.priority),
            F::Assignee => opt(core.assignee.clone(), V::Person),
            F::Reporter => opt(core.reporter.clone(), V::Person),
            F::Labels => V::StringSet(core.labels.clone()),
            F::Components => V::StringSet(core.components.clone()),
            F::FixVersions => V::StringSet(core.fix_versions.clone()),
            F::DueDate => opt(core.due_date, V::Date),
            F::ParentId => opt(core.parent_id, V::ItemId),
            F::Sprint => opt(core.sprint.clone(), V::String),
            F::StoryPoints => opt(core.story_points, V::Number),
            F::OriginalEstimate => opt(core.original_estimate, V::Duration),
            F::RemainingEstimate => opt(core.remaining_estimate, V::Duration),
            F::Resolution => opt(core.resolution.clone(), V::String),
            F::RemoteCreatedAt => opt(core.remote_created_at, V::Timestamp),
            F::RemoteUpdatedAt => opt(core.remote_updated_at, V::Timestamp),
            F::TodoRefinement => V::TodoRefinement(overlay.todo_refinement),
            F::Waiting => opt(waiting.cloned(), V::Waiting),
            F::DeferUntil => opt(overlay.defer_until, V::Date),
            F::PrivateNotes => V::Markdown(overlay.private_notes.clone()),
            F::FocusWeek => opt(overlay.focus_week.clone(), V::IsoWeek),
            F::LocalPriorityOverride => opt(overlay.local_priority_override, V::Priority),
            F::Archived => opt(archived.copied(), V::Archived),
            F::Recurrence => opt(recurrence.cloned(), V::Recurrence),
            F::RemoteExtras => return None,
            F::Status => V::Status(item.status()),
            F::EffectivePriority => V::Priority(item.effective_priority()),
            F::IsMine => V::Bool(derived.is_mine),
            F::PendingFields => V::ItemFieldSet(derived.pending_fields.clone()),
            F::HasConflict => V::Bool(derived.has_conflict),
            F::HasPushFailure => V::Bool(derived.has_push_failure),
            F::DescriptionLossy => V::Bool(derived.description_lossy),
            F::DescriptionDirty => V::Bool(derived.description_dirty),
            F::Staleness => opt((!staleness.is_empty()).then_some(*staleness), V::Staleness),
            F::Scopes => V::StringSet(derived.scopes.clone()),
            F::DeletedAt => opt(derived.deleted_at, V::Timestamp),
            F::MergedInto => opt(derived.merged_into, V::ItemId),
            F::IsOverdue => V::Bool(derived.is_overdue),
            F::WaitingOn => opt(waiting.map(|w| w.on.clone()), V::Person),
            F::WaitingReason => opt(waiting.map(|w| w.reason.clone()), V::String),
            F::WaitingSince => opt(waiting.map(|w| w.since), V::Date),
            F::WaitingFollowUpOn => opt(waiting.and_then(|w| w.follow_up_on), V::Date),
            F::ArchivedAt => opt(archived.map(|a| a.at), V::Timestamp),
            F::ArchivedReason => opt(archived.map(|a| a.reason), V::ArchiveReason),
            F::RemoteLinkTrackerKind => opt(link.map(|l| l.tracker_kind.clone()), V::String),
            F::RemoteLinkInstanceId => opt(link.map(|l| l.instance_id.clone()), V::String),
            F::RemoteLinkRemoteId => opt(link.map(|l| l.remote_id.clone()), V::String),
            F::RemoteLinkKey => opt(link.map(|l| l.key.clone()), V::String),
            F::RemoteLinkUrl => opt(link.map(|l| l.url.clone()), V::String),
            F::RemoteLinkLastPulledVersion => {
                opt(link.and_then(|l| l.last_pulled_version), V::Timestamp)
            }
            F::RemoteLinkLinkState => opt(link.map(|l| l.link_state), V::LinkState),
            F::RemoteLinkPinned => opt(link.map(|l| l.pinned), V::Bool),
            F::RecurrenceSeriesId => opt(recurrence.map(|r| r.series_id.to_string()), V::String),
            F::RecurrenceOccurrenceDate => opt(recurrence.and_then(|r| r.occurrence_date), V::Date),
            F::StalenessLastUserTouchAt => opt(staleness.last_user_touch_at, V::Timestamp),
            F::StalenessLastRemoteChangeAt => opt(staleness.last_remote_change_at, V::Timestamp),
            F::StalenessStatusEnteredAt => opt(staleness.status_entered_at, V::Timestamp),
            F::StalenessWaitingSince => opt(staleness.waiting_since, V::Timestamp),
        };
        Some(value)
    }

    /// The variant name, as serialized.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            FieldValue::Cleared => "cleared",
            FieldValue::String(_) => "string",
            FieldValue::Markdown(_) => "markdown",
            FieldValue::Bool(_) => "bool",
            FieldValue::Number(_) => "number",
            FieldValue::Uint(_) => "uint",
            FieldValue::Date(_) => "date",
            FieldValue::Timestamp(_) => "timestamp",
            FieldValue::Duration(_) => "duration",
            FieldValue::StringSet(_) => "string_set",
            FieldValue::Person(_) => "person",
            FieldValue::ItemId(_) => "item_id",
            FieldValue::IsoWeek(_) => "iso_week",
            FieldValue::Priority(_) => "priority",
            FieldValue::Status(_) => "status",
            FieldValue::StatusCategory(_) => "status_category",
            FieldValue::ResolutionKind(_) => "resolution_kind",
            FieldValue::TodoRefinement(_) => "todo_refinement",
            FieldValue::CreatedVia(_) => "created_via",
            FieldValue::ArchiveReason(_) => "archive_reason",
            FieldValue::LinkState(_) => "link_state",
            FieldValue::ItemFieldSet(_) => "item_field_set",
            FieldValue::Waiting(_) => "waiting",
            FieldValue::Archived(_) => "archived",
            FieldValue::Recurrence(_) => "recurrence",
            FieldValue::RemoteLink(_) => "remote_link",
            FieldValue::Staleness(_) => "staleness",
        }
    }
}

/// `Cleared` for `None`.
fn opt<T>(value: Option<T>, variant: fn(T) -> FieldValue) -> FieldValue {
    value.map_or(FieldValue::Cleared, variant)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use jiff::civil::date;
    use serde_json::json;
    use ulid::Ulid;

    use super::*;

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    fn bare() -> Item {
        Item::new_capture(id(1), "call dentist".into(), CreatedVia::Cli, ts(1_000))
    }

    fn full() -> Item {
        let mut item = bare();
        item.updated_at = ts(2_000);
        item.revision = 42;
        item.remote_link = Some(RemoteLink {
            tracker_kind: "jira".into(),
            instance_id: "work-jira".into(),
            remote_id: "10001".into(),
            key: "PLAT-530".into(),
            url: "https://jira.example.com/browse/PLAT-530".into(),
            last_pulled_version: Some(ts(3_000)),
            link_state: LinkState::Ok,
            pinned: true,
        });
        let core = &mut item.core;
        core.description = "**bold**".into();
        core.item_type = "Bug".into();
        core.status_category = StatusCategory::Done;
        core.resolution_kind = Some(ResolutionKind::Cancelled);
        core.remote_status_name = Some("Won't Do".into());
        core.priority = Priority::High;
        core.assignee = Some(PersonRef::Person(id(2)));
        core.reporter = Some(PersonRef::Person(id(3)));
        core.labels = vec!["auth".into(), "oncall".into()];
        core.components = vec!["api".into()];
        core.fix_versions = vec!["1.2".into()];
        core.due_date = Some(date(2026, 10, 24));
        core.parent_id = Some(id(4));
        core.sprint = Some("Sprint 7".into());
        core.story_points = Some(3.5);
        core.original_estimate = Some(SignedDuration::from_hours(4));
        core.remaining_estimate = Some(SignedDuration::from_mins(90));
        core.resolution = Some("Won't Do".into());
        core.remote_created_at = Some(ts(4_000));
        core.remote_updated_at = Some(ts(5_000));
        let overlay = &mut item.overlay;
        overlay.todo_refinement = TodoRefinement::Someday;
        overlay.waiting = Some(Waiting {
            on: PersonRef::FreeText("the vendor".into()),
            reason: "quote".into(),
            since: date(2026, 10, 20),
            follow_up_on: Some(date(2026, 10, 27)),
        });
        overlay.defer_until = Some(date(2026, 11, 1));
        overlay.private_notes = "notes".into();
        overlay.focus_week = Some("2026-W43".into());
        overlay.local_priority_override = Some(Priority::Lowest);
        overlay.archived = Some(Archived {
            at: ts(6_000),
            reason: ArchiveReason::AutoDone,
        });
        overlay.recurrence = Some(ItemRecurrence {
            series_id: id(5),
            occurrence_date: Some(date(2026, 10, 21)),
        });
        item.remote_extras
            .insert("customfield_1".into(), json!({"x": 1}));
        let derived = &mut item.derived;
        derived.is_mine = true;
        derived.pending_fields = vec![ItemField::Title, ItemField::Priority];
        derived.has_conflict = true;
        derived.has_push_failure = true;
        derived.description_lossy = true;
        derived.description_dirty = true;
        derived.staleness = StalenessSignals {
            last_user_touch_at: Some(ts(7_000)),
            last_remote_change_at: Some(ts(8_000)),
            status_entered_at: Some(ts(9_000)),
            waiting_since: Some(ts(10_000)),
        };
        derived.scopes = vec!["mine".into()];
        derived.deleted_at = Some(ts(11_000));
        derived.merged_into = Some(id(6));
        derived.is_overdue = true;
        item
    }

    #[test]
    fn of_every_field_on_a_full_item() {
        use FieldValue as V;
        use ItemField as F;
        let item = full();
        let s = |v: &str| V::String(v.to_owned());
        let set = |v: &[&str]| V::StringSet(v.iter().map(|&s| s.to_owned()).collect());
        let rows = vec![
            (F::Id, V::ItemId(id(1))),
            (F::CreatedVia, V::CreatedVia(CreatedVia::Cli)),
            (F::CreatedAt, V::Timestamp(ts(1_000))),
            (F::UpdatedAt, V::Timestamp(ts(2_000))),
            (F::Revision, V::Uint(42)),
            (
                F::RemoteLink,
                V::RemoteLink(item.remote_link.clone().unwrap()),
            ),
            (F::Title, s("call dentist")),
            (F::Description, V::Markdown("**bold**".into())),
            (F::ItemType, s("Bug")),
            (F::StatusCategory, V::StatusCategory(StatusCategory::Done)),
            (
                F::ResolutionKind,
                V::ResolutionKind(ResolutionKind::Cancelled),
            ),
            (F::RemoteStatusName, s("Won't Do")),
            (F::Priority, V::Priority(Priority::High)),
            (F::Assignee, V::Person(PersonRef::Person(id(2)))),
            (F::Reporter, V::Person(PersonRef::Person(id(3)))),
            (F::Labels, set(&["auth", "oncall"])),
            (F::Components, set(&["api"])),
            (F::FixVersions, set(&["1.2"])),
            (F::DueDate, V::Date(date(2026, 10, 24))),
            (F::ParentId, V::ItemId(id(4))),
            (F::Sprint, s("Sprint 7")),
            (F::StoryPoints, V::Number(3.5)),
            (
                F::OriginalEstimate,
                V::Duration(SignedDuration::from_hours(4)),
            ),
            (
                F::RemainingEstimate,
                V::Duration(SignedDuration::from_mins(90)),
            ),
            (F::Resolution, s("Won't Do")),
            (F::RemoteCreatedAt, V::Timestamp(ts(4_000))),
            (F::RemoteUpdatedAt, V::Timestamp(ts(5_000))),
            (
                F::TodoRefinement,
                V::TodoRefinement(TodoRefinement::Someday),
            ),
            (
                F::Waiting,
                V::Waiting(item.overlay.waiting.clone().unwrap()),
            ),
            (F::DeferUntil, V::Date(date(2026, 11, 1))),
            (F::PrivateNotes, V::Markdown("notes".into())),
            (F::FocusWeek, V::IsoWeek("2026-W43".into())),
            (F::LocalPriorityOverride, V::Priority(Priority::Lowest)),
            (F::Archived, V::Archived(item.overlay.archived.unwrap())),
            (
                F::Recurrence,
                V::Recurrence(item.overlay.recurrence.clone().unwrap()),
            ),
            (F::Status, V::Status(Status::Cancelled)),
            (F::EffectivePriority, V::Priority(Priority::Lowest)),
            (F::IsMine, V::Bool(true)),
            (
                F::PendingFields,
                V::ItemFieldSet(vec![F::Title, F::Priority]),
            ),
            (F::HasConflict, V::Bool(true)),
            (F::HasPushFailure, V::Bool(true)),
            (F::DescriptionLossy, V::Bool(true)),
            (F::DescriptionDirty, V::Bool(true)),
            (F::Staleness, V::Staleness(item.derived.staleness)),
            (F::Scopes, set(&["mine"])),
            (F::DeletedAt, V::Timestamp(ts(11_000))),
            (F::MergedInto, V::ItemId(id(6))),
            (F::IsOverdue, V::Bool(true)),
            (
                F::WaitingOn,
                V::Person(PersonRef::FreeText("the vendor".into())),
            ),
            (F::WaitingReason, s("quote")),
            (F::WaitingSince, V::Date(date(2026, 10, 20))),
            (F::WaitingFollowUpOn, V::Date(date(2026, 10, 27))),
            (F::ArchivedAt, V::Timestamp(ts(6_000))),
            (F::ArchivedReason, V::ArchiveReason(ArchiveReason::AutoDone)),
            (F::RemoteLinkTrackerKind, s("jira")),
            (F::RemoteLinkInstanceId, s("work-jira")),
            (F::RemoteLinkRemoteId, s("10001")),
            (F::RemoteLinkKey, s("PLAT-530")),
            (
                F::RemoteLinkUrl,
                s("https://jira.example.com/browse/PLAT-530"),
            ),
            (F::RemoteLinkLastPulledVersion, V::Timestamp(ts(3_000))),
            (F::RemoteLinkLinkState, V::LinkState(LinkState::Ok)),
            (F::RemoteLinkPinned, V::Bool(true)),
            (F::RecurrenceSeriesId, s(&id(5).to_string())),
            (F::RecurrenceOccurrenceDate, V::Date(date(2026, 10, 21))),
            (F::StalenessLastUserTouchAt, V::Timestamp(ts(7_000))),
            (F::StalenessLastRemoteChangeAt, V::Timestamp(ts(8_000))),
            (F::StalenessStatusEnteredAt, V::Timestamp(ts(9_000))),
            (F::StalenessWaitingSince, V::Timestamp(ts(10_000))),
        ];
        let covered: BTreeSet<_> = rows.iter().map(|(f, _)| *f).collect();
        let expected: BTreeSet<_> = ItemField::ALL
            .iter()
            .copied()
            .filter(|&f| f != F::RemoteExtras)
            .collect();
        assert_eq!(covered, expected);
        for (field, want) in rows {
            assert_eq!(FieldValue::of(&item, field), Some(want), "{field:?}");
        }
        assert_eq!(FieldValue::of(&item, F::RemoteExtras), None);
    }

    #[test]
    fn of_unset_fields_is_cleared() {
        use ItemField as F;
        let item = bare();
        let cleared: BTreeSet<_> = [
            F::RemoteLink,
            F::ResolutionKind,
            F::RemoteStatusName,
            F::Assignee,
            F::Reporter,
            F::DueDate,
            F::ParentId,
            F::Sprint,
            F::StoryPoints,
            F::OriginalEstimate,
            F::RemainingEstimate,
            F::Resolution,
            F::RemoteCreatedAt,
            F::RemoteUpdatedAt,
            F::Waiting,
            F::DeferUntil,
            F::FocusWeek,
            F::LocalPriorityOverride,
            F::Archived,
            F::Recurrence,
            F::Staleness,
            F::DeletedAt,
            F::MergedInto,
            F::WaitingOn,
            F::WaitingReason,
            F::WaitingSince,
            F::WaitingFollowUpOn,
            F::ArchivedAt,
            F::ArchivedReason,
            F::RemoteLinkTrackerKind,
            F::RemoteLinkInstanceId,
            F::RemoteLinkRemoteId,
            F::RemoteLinkKey,
            F::RemoteLinkUrl,
            F::RemoteLinkLastPulledVersion,
            F::RemoteLinkLinkState,
            F::RemoteLinkPinned,
            F::RecurrenceSeriesId,
            F::RecurrenceOccurrenceDate,
            F::StalenessLastUserTouchAt,
            F::StalenessLastRemoteChangeAt,
            F::StalenessStatusEnteredAt,
            F::StalenessWaitingSince,
        ]
        .into_iter()
        .collect();
        for &field in ItemField::ALL {
            let value = FieldValue::of(&item, field);
            if field == F::RemoteExtras {
                assert_eq!(value, None);
            } else if cleared.contains(&field) {
                assert_eq!(value, Some(FieldValue::Cleared), "{field:?}");
            } else {
                assert_ne!(value, Some(FieldValue::Cleared), "{field:?}");
            }
        }
        assert_eq!(
            FieldValue::of(&item, F::Priority),
            Some(FieldValue::Priority(Priority::None))
        );
        assert_eq!(
            FieldValue::of(&item, F::EffectivePriority),
            Some(FieldValue::Priority(Priority::None))
        );
        assert_eq!(
            FieldValue::of(&item, F::Status),
            Some(FieldValue::Status(Status::Inbox))
        );
        assert_eq!(
            FieldValue::of(&item, F::Labels),
            Some(FieldValue::StringSet(Vec::new()))
        );
    }

    /// One value of every variant.
    fn samples() -> Vec<FieldValue> {
        let item = full();
        vec![
            FieldValue::Cleared,
            FieldValue::String("Fix flaky \"login\" test".into()),
            FieldValue::Markdown("# Notes\n\n- a\n".into()),
            FieldValue::Bool(true),
            FieldValue::Number(0.1),
            FieldValue::Uint(u64::MAX),
            FieldValue::Date(date(2026, 10, 24)),
            FieldValue::Timestamp(Timestamp::new(1_761_140_000, 123_456_789).unwrap()),
            FieldValue::Duration(SignedDuration::new(5_400, 500)),
            FieldValue::StringSet(vec!["b".into(), "a".into()]),
            FieldValue::Person(PersonRef::Person(id(2))),
            FieldValue::ItemId(id(4)),
            FieldValue::IsoWeek("2026-W53".into()),
            FieldValue::Priority(Priority::None),
            FieldValue::Status(Status::InProgress),
            FieldValue::StatusCategory(StatusCategory::InProgress),
            FieldValue::ResolutionKind(ResolutionKind::Cancelled),
            FieldValue::TodoRefinement(TodoRefinement::Someday),
            FieldValue::CreatedVia(CreatedVia::RemoteImport),
            FieldValue::ArchiveReason(ArchiveReason::LeftScope),
            FieldValue::LinkState(LinkState::Broken),
            FieldValue::ItemFieldSet(vec![ItemField::WaitingOn, ItemField::Title]),
            FieldValue::Waiting(item.overlay.waiting.clone().unwrap()),
            FieldValue::Archived(item.overlay.archived.unwrap()),
            FieldValue::Recurrence(ItemRecurrence {
                series_id: id(5),
                occurrence_date: None,
            }),
            FieldValue::RemoteLink(item.remote_link.clone().unwrap()),
            FieldValue::Staleness(StalenessSignals {
                status_entered_at: Some(ts(9_000)),
                ..StalenessSignals::default()
            }),
        ]
    }

    #[test]
    fn serde_round_trips_every_variant() {
        let samples = samples();
        let kinds: BTreeSet<_> = samples.iter().map(FieldValue::kind).collect();
        // `kind` matches exhaustively: a new variant also needs a sample and
        // a new count here.
        assert_eq!(kinds.len(), 27);
        assert_eq!(kinds.len(), samples.len());
        for value in samples {
            let json = serde_json::to_string(&value).unwrap();
            let back: FieldValue = serde_json::from_str(&json).unwrap();
            assert_eq!(back, value, "{json}");
            let tag = match serde_json::from_str::<serde_json::Value>(&json).unwrap() {
                serde_json::Value::String(tag) => tag,
                serde_json::Value::Object(map) => map.keys().next().unwrap().clone(),
                other => panic!("unexpected {other}"),
            };
            assert_eq!(tag, value.kind());
        }
    }

    #[test]
    fn golden_json() {
        let person: Ulid = "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z".parse().unwrap();
        let rows = vec![
            (FieldValue::Cleared, json!("cleared")),
            (
                FieldValue::String("Fix it".into()),
                json!({"string": "Fix it"}),
            ),
            (
                FieldValue::Priority(Priority::None),
                json!({"priority": "none"}),
            ),
            (
                FieldValue::Priority(Priority::Highest),
                json!({"priority": "highest"}),
            ),
            (
                FieldValue::Status(Status::InProgress),
                json!({"status": "in_progress"}),
            ),
            (
                FieldValue::CreatedVia(CreatedVia::RemoteImport),
                json!({"created_via": "remote_import"}),
            ),
            (
                FieldValue::ArchiveReason(ArchiveReason::LeftScope),
                json!({"archive_reason": "left_scope"}),
            ),
            (
                FieldValue::Date(date(2026, 10, 24)),
                json!({"date": "2026-10-24"}),
            ),
            (
                FieldValue::Timestamp(Timestamp::new(1_761_140_000, 500_000_000).unwrap()),
                json!({"timestamp": "2025-10-22T13:33:20.5Z"}),
            ),
            (
                FieldValue::Duration(SignedDuration::from_mins(90)),
                json!({"duration": "PT1H30M"}),
            ),
            (
                FieldValue::Person(PersonRef::FreeText("the vendor".into())),
                json!({"person": {"free_text": "the vendor"}}),
            ),
            (
                FieldValue::Person(PersonRef::Person(person)),
                json!({"person": {"person": "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z"}}),
            ),
            (
                FieldValue::Waiting(Waiting {
                    on: PersonRef::Person(person),
                    reason: "review".into(),
                    since: date(2026, 10, 20),
                    follow_up_on: None,
                }),
                json!({"waiting": {
                    "on": {"person": "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z"},
                    "reason": "review",
                    "since": "2026-10-20",
                    "follow_up_on": null,
                }}),
            ),
            (
                FieldValue::ItemFieldSet(vec![ItemField::Priority, ItemField::WaitingOn]),
                json!({"item_field_set": [16, 41001]}),
            ),
            (
                FieldValue::Archived(Archived {
                    at: ts(6_000),
                    reason: ArchiveReason::User,
                }),
                json!({"archived": {"at": "1970-01-01T01:40:00Z", "reason": "user"}}),
            ),
        ];
        for (value, want) in rows {
            assert_eq!(serde_json::to_value(&value).unwrap(), want, "{value:?}");
            assert_eq!(serde_json::from_value::<FieldValue>(want).unwrap(), value);
        }
    }

    /// Enum values serialize as the store's SQL encodings.
    #[test]
    fn enum_strings_match_the_store() {
        use FieldValue as V;
        let rows = [
            (V::Priority(Priority::None), "none"),
            (V::Priority(Priority::Lowest), "lowest"),
            (V::Priority(Priority::Low), "low"),
            (V::Priority(Priority::Medium), "medium"),
            (V::Priority(Priority::High), "high"),
            (V::Priority(Priority::Highest), "highest"),
            (V::Status(Status::Inbox), "inbox"),
            (V::Status(Status::Next), "next"),
            (V::Status(Status::Someday), "someday"),
            (V::Status(Status::InProgress), "in_progress"),
            (V::Status(Status::Done), "done"),
            (V::Status(Status::Cancelled), "cancelled"),
            (V::StatusCategory(StatusCategory::Todo), "todo"),
            (V::StatusCategory(StatusCategory::InProgress), "in_progress"),
            (V::StatusCategory(StatusCategory::Done), "done"),
            (V::ResolutionKind(ResolutionKind::Done), "done"),
            (V::ResolutionKind(ResolutionKind::Cancelled), "cancelled"),
            (V::TodoRefinement(TodoRefinement::Inbox), "inbox"),
            (V::TodoRefinement(TodoRefinement::Next), "next"),
            (V::TodoRefinement(TodoRefinement::Someday), "someday"),
            (V::CreatedVia(CreatedVia::Tui), "tui"),
            (V::CreatedVia(CreatedVia::Cli), "cli"),
            (V::CreatedVia(CreatedVia::Mcp), "mcp"),
            (V::CreatedVia(CreatedVia::RemoteImport), "remote_import"),
            (V::CreatedVia(CreatedVia::Restore), "restore"),
            (V::CreatedVia(CreatedVia::Split), "split"),
            (V::CreatedVia(CreatedVia::Recurrence), "recurrence"),
            (V::ArchiveReason(ArchiveReason::User), "user"),
            (V::ArchiveReason(ArchiveReason::LeftScope), "left_scope"),
            (
                V::ArchiveReason(ArchiveReason::RemoteUnavailable),
                "remote_unavailable",
            ),
            (V::ArchiveReason(ArchiveReason::AutoDone), "auto_done"),
            (V::LinkState(LinkState::Pending), "pending"),
            (V::LinkState(LinkState::Ok), "ok"),
            (V::LinkState(LinkState::Broken), "broken"),
        ];
        for (value, want) in rows {
            let json = serde_json::to_value(&value).unwrap();
            assert_eq!(json[value.kind()], want, "{value:?}");
        }
    }

    #[test]
    fn unknown_tags_and_fields_are_rejected() {
        assert!(serde_json::from_value::<FieldValue>(json!("nothing")).is_err());
        assert!(serde_json::from_value::<FieldValue>(json!({"priority": "urgent"})).is_err());
        assert!(serde_json::from_value::<FieldValue>(json!({"item_field_set": [9]})).is_err());
    }
}
