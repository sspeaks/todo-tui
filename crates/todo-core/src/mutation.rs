//! Applying mutations to one Item (design 3.2, 3.4, 3.7, 3.10, 4.8).
//!
//! [`apply`] validates and computes the new Item and its per-field changes.
//! The store owns everything that needs the database: revisions,
//! `expected_revision`, and references to other rows.

use std::collections::HashSet;

use jiff::Timestamp;
use jiff::civil::{ISOWeekDate, Weekday};
use ulid::Ulid;

use crate::field::ItemField;
use crate::ids::{ItemId, Revision};
use crate::item::{ArchiveReason, Archived, Item, PersonRef, Priority, Waiting};
use crate::status::Status;
use crate::value::FieldValue;

/// Who performs an op (design 3.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Actor {
    User,
    Sync,
    System,
    /// Applying patches of this Suggestion.
    Suggestion(Ulid),
}

/// Sets one field (`UpdateItem` mask path, `set_field` patch).
#[derive(Debug, Clone, PartialEq)]
pub struct FieldUpdate {
    pub field: ItemField,
    pub value: FieldValue,
}

/// What a mutation does to its Item.
#[derive(Debug, Clone, PartialEq)]
pub enum MutationOp {
    /// Fields in order; each at most once, and not together with its parent.
    Update(Vec<FieldUpdate>),
    SetStatus(Status),
    Archive,
    Unarchive,
    /// Soft delete (design 3.7).
    Delete,
    Undelete,
}

/// One single-item mutation, as requested.
#[derive(Debug, Clone, PartialEq)]
pub struct Mutation {
    pub id: ItemId,
    pub expected_revision: Option<Revision>,
    pub op: MutationOp,
}

/// The clock and actor a mutation is applied with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyCtx {
    pub now: Timestamp,
    pub actor: Actor,
}

/// One field's change, recorded as an event.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldChange {
    pub field: ItemField,
    pub old: FieldValue,
    pub new: FieldValue,
}

/// The Item after a mutation. `changes` is empty when nothing changed, and
/// then `item` is the input unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub item: Item,
    pub changes: Vec<FieldChange>,
}

/// Why a mutation was rejected. Nothing is applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MutationError {
    #[error("{0:?} is read-only")]
    ReadOnly(ItemField),
    #[error("{0:?} changes only through SetStatus")]
    UseSetStatus(ItemField),
    #[error("{0:?} changes only through Archive and Unarchive")]
    UseArchive(ItemField),
    #[error("{field:?} does not take a {got} value")]
    WrongType {
        field: ItemField,
        /// The value's variant.
        got: &'static str,
    },
    #[error("{field:?}: {reason}")]
    Invalid { field: ItemField, reason: String },
    #[error("{0:?} cannot be set while waiting is unset")]
    ParentUnset(ItemField),
    #[error("{0:?} is updated more than once")]
    Duplicate(ItemField),
    #[error("the Item is deleted")]
    Deleted,
    #[error("{field:?}: {reason}")]
    Unsupported { field: ItemField, reason: String },
}

impl MutationError {
    /// The offending field, if the error is about one.
    pub fn field(&self) -> Option<ItemField> {
        match self {
            MutationError::ReadOnly(field)
            | MutationError::UseSetStatus(field)
            | MutationError::UseArchive(field)
            | MutationError::ParentUnset(field)
            | MutationError::Duplicate(field)
            | MutationError::WrongType { field, .. }
            | MutationError::Invalid { field, .. }
            | MutationError::Unsupported { field, .. } => Some(*field),
            MutationError::Deleted => None,
        }
    }
}

/// Applies `op` to `item`. Never touches `revision`. Setting a field to its
/// current value (after normalisation) is not a change. When anything
/// changed, `updated_at` and the staleness signals are maintained without
/// their own [`FieldChange`]s.
pub fn apply(item: &Item, op: &MutationOp, ctx: &ApplyCtx) -> Result<Applied, MutationError> {
    if item.is_deleted() && !matches!(op, MutationOp::Delete | MutationOp::Undelete) {
        return Err(MutationError::Deleted);
    }
    let mut next = item.clone();
    let fields: Vec<ItemField> = match op {
        MutationOp::Update(updates) => {
            let mut fields: Vec<ItemField> = Vec::with_capacity(updates.len());
            for update in updates {
                let field = update.field;
                if fields
                    .iter()
                    .any(|&f| f == field || field.parent() == Some(f) || f.parent() == Some(field))
                {
                    return Err(MutationError::Duplicate(field));
                }
                set_field(&mut next, field, &update.value)?;
                fields.push(field);
            }
            fields
        }
        MutationOp::SetStatus(target) => {
            if item.status() != *target {
                next.set_status_fields(item.status_fields().with_status(*target));
            }
            vec![
                ItemField::StatusCategory,
                ItemField::ResolutionKind,
                ItemField::TodoRefinement,
            ]
        }
        MutationOp::Archive => {
            next.overlay.archived.get_or_insert(Archived {
                at: ctx.now,
                reason: ArchiveReason::User,
            });
            vec![ItemField::Archived]
        }
        MutationOp::Unarchive => {
            next.overlay.archived = None;
            vec![ItemField::Archived]
        }
        MutationOp::Delete => {
            next.derived.deleted_at.get_or_insert(ctx.now);
            vec![ItemField::DeletedAt]
        }
        MutationOp::Undelete => {
            next.derived.deleted_at = None;
            vec![ItemField::DeletedAt]
        }
    };
    let changes: Vec<FieldChange> = fields
        .into_iter()
        .filter_map(|field| {
            let old = FieldValue::of(item, field)?;
            let new = FieldValue::of(&next, field)?;
            (old != new).then_some(FieldChange { field, old, new })
        })
        .collect();
    if changes.is_empty() {
        return Ok(Applied {
            item: item.clone(),
            changes,
        });
    }
    maintain_derived(item, &mut next, ctx);
    Ok(Applied {
        item: next,
        changes,
    })
}

/// The derived bookkeeping of a change (design 3.10).
fn maintain_derived(before: &Item, after: &mut Item, ctx: &ApplyCtx) {
    let status_changed = before.status() != after.status();
    let waiting = (
        before.overlay.waiting.is_some(),
        after.overlay.waiting.is_some(),
    );
    after.updated_at = ctx.now;
    let staleness = &mut after.derived.staleness;
    if matches!(ctx.actor, Actor::User | Actor::Suggestion(_)) {
        staleness.last_user_touch_at = Some(ctx.now);
    }
    if status_changed {
        staleness.status_entered_at = Some(ctx.now);
    }
    match waiting {
        (false, true) => staleness.waiting_since = Some(ctx.now),
        (true, false) => staleness.waiting_since = None,
        _ => {}
    }
}

/// Validates `value` for `field` and writes it, normalised, into `item`.
fn set_field(item: &mut Item, field: ItemField, value: &FieldValue) -> Result<(), MutationError> {
    use FieldValue as V;
    use ItemField as F;
    let wrong_type = || MutationError::WrongType {
        field,
        got: value.kind(),
    };
    let invalid = |reason: &str| MutationError::Invalid {
        field,
        reason: reason.to_owned(),
    };
    match field {
        F::Title => {
            let V::String(title) = value else {
                return Err(wrong_type());
            };
            let title = title.trim();
            if title.is_empty() {
                return Err(invalid("must not be empty"));
            }
            if title.contains(is_line_break) {
                return Err(invalid("must be a single line"));
            }
            item.core.title = title.to_owned();
        }
        F::ItemType => {
            let V::String(item_type) = value else {
                return Err(wrong_type());
            };
            let item_type = item_type.trim();
            if item_type.is_empty() {
                return Err(invalid("must not be empty"));
            }
            item.core.item_type = item_type.to_owned();
        }
        F::Description | F::PrivateNotes => {
            let V::Markdown(text) = value else {
                return Err(wrong_type());
            };
            match field {
                F::Description => item.core.description.clone_from(text),
                _ => item.overlay.private_notes.clone_from(text),
            }
        }
        F::Priority => match value {
            V::Priority(priority) => item.core.priority = *priority,
            V::Cleared => return Err(invalid("no priority is priority none, never cleared")),
            _ => return Err(wrong_type()),
        },
        F::LocalPriorityOverride => {
            item.overlay.local_priority_override = match value {
                V::Priority(Priority::None) => {
                    return Err(invalid(
                        "remove the override with cleared, not priority none",
                    ));
                }
                V::Priority(priority) => Some(*priority),
                V::Cleared => None,
                _ => return Err(wrong_type()),
            }
        }
        F::Assignee => {
            item.core.assignee = match value {
                V::Person(PersonRef::Person(id)) => Some(PersonRef::Person(*id)),
                V::Person(PersonRef::FreeText(_)) => {
                    return Err(invalid("free text is only valid for waiting.on"));
                }
                V::Cleared => None,
                _ => return Err(wrong_type()),
            }
        }
        F::Labels => {
            let V::StringSet(labels) = value else {
                return Err(wrong_type());
            };
            if labels
                .iter()
                .any(|l| l.is_empty() || l.contains(char::is_whitespace))
            {
                return Err(invalid(
                    "labels must be non-empty and contain no whitespace",
                ));
            }
            item.core.labels = dedup(labels.iter().cloned());
        }
        F::Components | F::FixVersions => {
            if !item.is_linked() {
                return Err(MutationError::Unsupported {
                    field,
                    reason: "a tracker field, and the Item is not linked".to_owned(),
                });
            }
            let V::StringSet(names) = value else {
                return Err(wrong_type());
            };
            if names.iter().any(|n| n.trim().is_empty()) {
                return Err(invalid("names must not be empty"));
            }
            let names = dedup(names.iter().map(|n| n.trim().to_owned()));
            match field {
                F::Components => item.core.components = names,
                _ => item.core.fix_versions = names,
            }
        }
        F::DueDate | F::DeferUntil => {
            let date = match value {
                V::Date(date) => Some(*date),
                V::Cleared => None,
                _ => return Err(wrong_type()),
            };
            match field {
                F::DueDate => item.core.due_date = date,
                _ => item.overlay.defer_until = date,
            }
        }
        F::ParentId => {
            item.core.parent_id = match value {
                V::ItemId(id) if *id == item.id => {
                    return Err(invalid("an Item cannot be its own parent"));
                }
                V::ItemId(id) => Some(*id),
                V::Cleared => None,
                _ => return Err(wrong_type()),
            }
        }
        F::StoryPoints => {
            item.core.story_points = match value {
                V::Number(n) if n.is_finite() && *n >= 0.0 => Some(*n),
                V::Number(_) => return Err(invalid("must be a finite number >= 0")),
                V::Cleared => None,
                _ => return Err(wrong_type()),
            }
        }
        F::OriginalEstimate | F::RemainingEstimate => {
            let estimate = match value {
                V::Duration(d) if d.is_negative() => return Err(invalid("must not be negative")),
                V::Duration(d) => Some(*d),
                V::Cleared => None,
                _ => return Err(wrong_type()),
            };
            match field {
                F::OriginalEstimate => item.core.original_estimate = estimate,
                _ => item.core.remaining_estimate = estimate,
            }
        }
        F::Waiting => {
            item.overlay.waiting = match value {
                V::Waiting(waiting) => Some(Waiting {
                    on: waiting_on(&waiting.on)?,
                    ..waiting.clone()
                }),
                V::Cleared => None,
                _ => return Err(wrong_type()),
            }
        }
        F::WaitingOn | F::WaitingReason | F::WaitingSince | F::WaitingFollowUpOn => {
            let Some(waiting) = item.overlay.waiting.as_mut() else {
                return Err(MutationError::ParentUnset(field));
            };
            match (field, value) {
                (F::WaitingOn, V::Person(on)) => waiting.on = waiting_on(on)?,
                (F::WaitingOn, V::Cleared) => return Err(invalid("clear waiting instead")),
                (F::WaitingReason, V::String(reason)) => waiting.reason.clone_from(reason),
                (F::WaitingSince, V::Date(since)) => waiting.since = *since,
                (F::WaitingFollowUpOn, V::Date(date)) => waiting.follow_up_on = Some(*date),
                (F::WaitingFollowUpOn, V::Cleared) => waiting.follow_up_on = None,
                _ => return Err(wrong_type()),
            }
        }
        F::FocusWeek => {
            item.overlay.focus_week = match value {
                V::IsoWeek(week) if is_iso_week(week) => Some(week.clone()),
                V::IsoWeek(_) => return Err(invalid("must be an ISO week, YYYY-Www")),
                V::Cleared => None,
                _ => return Err(wrong_type()),
            }
        }
        F::Status | F::StatusCategory | F::ResolutionKind | F::TodoRefinement => {
            return Err(MutationError::UseSetStatus(field));
        }
        F::Archived | F::ArchivedAt | F::ArchivedReason => {
            return Err(MutationError::UseArchive(field));
        }
        F::Id
        | F::CreatedVia
        | F::CreatedAt
        | F::UpdatedAt
        | F::Revision
        | F::RemoteLink
        | F::RemoteLinkTrackerKind
        | F::RemoteLinkInstanceId
        | F::RemoteLinkRemoteId
        | F::RemoteLinkKey
        | F::RemoteLinkUrl
        | F::RemoteLinkLastPulledVersion
        | F::RemoteLinkLinkState
        | F::RemoteLinkPinned
        | F::RemoteStatusName
        | F::Reporter
        | F::Sprint
        | F::Resolution
        | F::RemoteCreatedAt
        | F::RemoteUpdatedAt
        | F::Recurrence
        | F::RecurrenceSeriesId
        | F::RecurrenceOccurrenceDate
        | F::RemoteExtras
        | F::EffectivePriority
        | F::IsMine
        | F::PendingFields
        | F::HasConflict
        | F::HasPushFailure
        | F::DescriptionLossy
        | F::DescriptionDirty
        | F::Staleness
        | F::StalenessLastUserTouchAt
        | F::StalenessLastRemoteChangeAt
        | F::StalenessStatusEnteredAt
        | F::StalenessWaitingSince
        | F::Scopes
        | F::DeletedAt
        | F::MergedInto
        | F::IsOverdue => return Err(MutationError::ReadOnly(field)),
    }
    Ok(())
}

/// A person id, or non-empty free text (stored trimmed).
fn waiting_on(on: &PersonRef) -> Result<PersonRef, MutationError> {
    match on {
        PersonRef::Person(id) => Ok(PersonRef::Person(*id)),
        PersonRef::FreeText(text) if text.trim().is_empty() => Err(MutationError::Invalid {
            field: ItemField::WaitingOn,
            reason: "free text must not be empty".to_owned(),
        }),
        PersonRef::FreeText(text) => Ok(PersonRef::FreeText(text.trim().to_owned())),
    }
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Drops repeats, keeping the first appearance.
fn dedup(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

/// `YYYY-Www` naming a week that exists in that ISO year.
fn is_iso_week(s: &str) -> bool {
    let Some((year, week)) = s.split_once("-W") else {
        return false;
    };
    if year.len() != 4
        || week.len() != 2
        || !year.bytes().chain(week.bytes()).all(|b| b.is_ascii_digit())
    {
        return false;
    }
    match (year.parse(), week.parse()) {
        (Ok(year), Ok(week)) => ISOWeekDate::new(year, week, Weekday::Monday).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;
    use jiff::civil::date;

    use super::*;
    use crate::item::{CreatedVia, LinkState, RemoteLink};
    use crate::status::{ResolutionKind, StatusCategory, TodoRefinement};

    use FieldValue as V;
    use ItemField as F;

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    fn now() -> Timestamp {
        ts(5_000)
    }

    fn ctx(actor: Actor) -> ApplyCtx {
        ApplyCtx { now: now(), actor }
    }

    fn item() -> Item {
        let mut item = Item::new_capture(id(1), "call dentist".into(), CreatedVia::Tui, ts(1_000));
        item.revision = 7;
        item
    }

    fn linked() -> Item {
        let mut item = item();
        item.remote_link = Some(RemoteLink {
            tracker_kind: "jira".into(),
            instance_id: "work-jira".into(),
            remote_id: "10001".into(),
            key: "PLAT-530".into(),
            url: "https://jira.example.com/browse/PLAT-530".into(),
            last_pulled_version: None,
            link_state: LinkState::Ok,
            pinned: false,
        });
        item
    }

    fn waiting(on: PersonRef) -> Waiting {
        Waiting {
            on,
            reason: "quote".into(),
            since: date(2026, 10, 20),
            follow_up_on: None,
        }
    }

    fn waiting_item() -> Item {
        let mut item = item();
        item.overlay.waiting = Some(waiting(PersonRef::FreeText("the vendor".into())));
        item.derived.staleness.waiting_since = Some(ts(900));
        item
    }

    fn run(item: &Item, op: MutationOp) -> Result<Applied, MutationError> {
        apply(item, &op, &ctx(Actor::User))
    }

    fn update(item: &Item, updates: &[(ItemField, FieldValue)]) -> Result<Applied, MutationError> {
        let updates = updates
            .iter()
            .map(|(field, value)| FieldUpdate {
                field: *field,
                value: value.clone(),
            })
            .collect();
        run(item, MutationOp::Update(updates))
    }

    fn set(item: &Item, field: ItemField, value: FieldValue) -> Result<Applied, MutationError> {
        update(item, &[(field, value)])
    }

    fn change(field: ItemField, old: FieldValue, new: FieldValue) -> FieldChange {
        FieldChange { field, old, new }
    }

    fn invalid(field: ItemField, value: FieldValue) {
        match set(&item(), field, value.clone()) {
            Err(MutationError::Invalid { field: f, .. }) if f == field => {}
            other => panic!("{field:?} = {value:?}: {other:?}"),
        }
    }

    const READ_ONLY: &[ItemField] = &[
        F::Id,
        F::CreatedVia,
        F::CreatedAt,
        F::UpdatedAt,
        F::Revision,
        F::RemoteLink,
        F::RemoteLinkTrackerKind,
        F::RemoteLinkInstanceId,
        F::RemoteLinkRemoteId,
        F::RemoteLinkKey,
        F::RemoteLinkUrl,
        F::RemoteLinkLastPulledVersion,
        F::RemoteLinkLinkState,
        F::RemoteLinkPinned,
        F::RemoteStatusName,
        F::Reporter,
        F::Sprint,
        F::Resolution,
        F::RemoteCreatedAt,
        F::RemoteUpdatedAt,
        F::Recurrence,
        F::RecurrenceSeriesId,
        F::RecurrenceOccurrenceDate,
        F::RemoteExtras,
        F::EffectivePriority,
        F::IsMine,
        F::PendingFields,
        F::HasConflict,
        F::HasPushFailure,
        F::DescriptionLossy,
        F::DescriptionDirty,
        F::Staleness,
        F::StalenessLastUserTouchAt,
        F::StalenessLastRemoteChangeAt,
        F::StalenessStatusEnteredAt,
        F::StalenessWaitingSince,
        F::Scopes,
        F::DeletedAt,
        F::MergedInto,
        F::IsOverdue,
    ];
    const SET_STATUS: &[ItemField] = &[
        F::Status,
        F::StatusCategory,
        F::ResolutionKind,
        F::TodoRefinement,
    ];
    const ARCHIVE: &[ItemField] = &[F::Archived, F::ArchivedAt, F::ArchivedReason];
    const WRITABLE: &[ItemField] = &[
        F::Title,
        F::Description,
        F::ItemType,
        F::Priority,
        F::Assignee,
        F::Labels,
        F::Components,
        F::FixVersions,
        F::DueDate,
        F::ParentId,
        F::StoryPoints,
        F::OriginalEstimate,
        F::RemainingEstimate,
        F::Waiting,
        F::DeferUntil,
        F::PrivateNotes,
        F::FocusWeek,
        F::LocalPriorityOverride,
        F::WaitingOn,
        F::WaitingReason,
        F::WaitingSince,
        F::WaitingFollowUpOn,
    ];

    #[test]
    fn every_field_is_classified() {
        let classes = [READ_ONLY, SET_STATUS, ARCHIVE, WRITABLE];
        for &field in ItemField::ALL {
            let n = classes.iter().filter(|c| c.contains(&field)).count();
            assert_eq!(n, 1, "{field:?} is in {n} classes");
        }
        assert_eq!(
            classes.iter().map(|c| c.len()).sum::<usize>(),
            ItemField::ALL.len()
        );

        let mut item = linked();
        item.overlay.waiting = Some(waiting(PersonRef::Person(id(2))));
        item.overlay.focus_week = Some("2026-W43".into());
        for &field in ItemField::ALL {
            let value = FieldValue::of(&item, field).unwrap_or(V::Cleared);
            let result = set(&item, field, value);
            if READ_ONLY.contains(&field) {
                assert_eq!(result, Err(MutationError::ReadOnly(field)));
            } else if SET_STATUS.contains(&field) {
                assert_eq!(result, Err(MutationError::UseSetStatus(field)));
            } else if ARCHIVE.contains(&field) {
                assert_eq!(result, Err(MutationError::UseArchive(field)));
            } else {
                // Writable: setting the current value is a no-op.
                let applied = result.unwrap_or_else(|e| panic!("{field:?}: {e}"));
                assert_eq!(applied.changes, [], "{field:?}");
                assert_eq!(applied.item, item, "{field:?}");
            }
        }
    }

    #[test]
    fn read_only_wins_over_value_type() {
        for &field in READ_ONLY {
            assert_eq!(
                set(&item(), field, V::Bool(true)),
                Err(MutationError::ReadOnly(field))
            );
        }
        assert_eq!(
            set(&item(), F::Status, V::Status(Status::Done)),
            Err(MutationError::UseSetStatus(F::Status))
        );
        assert_eq!(
            set(&item(), F::ArchivedReason, V::Cleared),
            Err(MutationError::UseArchive(F::ArchivedReason))
        );
    }

    #[test]
    fn wrong_types() {
        let rows = [
            (F::Title, V::Markdown("x".into())),
            (F::Title, V::Cleared),
            (F::ItemType, V::Cleared),
            (F::Description, V::String("x".into())),
            (F::PrivateNotes, V::Cleared),
            (F::Priority, V::Uint(1)),
            (F::LocalPriorityOverride, V::String("high".into())),
            (F::Assignee, V::String("dana".into())),
            (F::Labels, V::String("a".into())),
            (F::DueDate, V::Timestamp(now())),
            (F::DeferUntil, V::String("fri".into())),
            (F::ParentId, V::String(id(2).to_string())),
            (F::StoryPoints, V::Uint(3)),
            (F::OriginalEstimate, V::Number(1.0)),
            (F::RemainingEstimate, V::Uint(60)),
            (F::Waiting, V::Person(PersonRef::Person(id(2)))),
            (F::FocusWeek, V::String("2026-W43".into())),
            (F::WaitingOn, V::String("dana".into())),
            (F::WaitingReason, V::Cleared),
            (F::WaitingSince, V::Cleared),
            (F::WaitingFollowUpOn, V::Timestamp(now())),
        ];
        let item = waiting_item();
        for (field, value) in rows {
            assert_eq!(
                set(&item, field, value.clone()),
                Err(MutationError::WrongType {
                    field,
                    got: value.kind()
                }),
                "{field:?}"
            );
        }
        let linked = linked();
        assert_eq!(
            set(&linked, F::Components, V::String("api".into())),
            Err(MutationError::WrongType {
                field: F::Components,
                got: "string"
            })
        );
    }

    #[test]
    fn title_and_item_type_are_trimmed_single_line_text() {
        let applied = set(&item(), F::Title, V::String("  Fix flaky test \t".into())).unwrap();
        assert_eq!(applied.item.core.title, "Fix flaky test");
        assert_eq!(
            applied.changes,
            [change(
                F::Title,
                V::String("call dentist".into()),
                V::String("Fix flaky test".into())
            )]
        );
        for bad in ["", "   ", "a\nb", "a\r\nb", "a\u{2028}b", "a\u{85}b"] {
            invalid(F::Title, V::String(bad.into()));
        }
        // Surrounding line breaks are trimmed away.
        let applied = set(&item(), F::Title, V::String("\nx\n".into())).unwrap();
        assert_eq!(applied.item.core.title, "x");

        let applied = set(&item(), F::ItemType, V::String(" Bug ".into())).unwrap();
        assert_eq!(applied.item.core.item_type, "Bug");
        invalid(F::ItemType, V::String("  ".into()));
    }

    #[test]
    fn markdown_fields_take_any_text() {
        let applied = update(
            &item(),
            &[
                (F::Description, V::Markdown("# Steps\n\n1. a\n".into())),
                (F::PrivateNotes, V::Markdown("  keep spacing  ".into())),
            ],
        )
        .unwrap();
        assert_eq!(applied.item.core.description, "# Steps\n\n1. a\n");
        assert_eq!(applied.item.overlay.private_notes, "  keep spacing  ");
        assert_eq!(applied.changes.len(), 2);

        let applied = set(&applied.item, F::Description, V::Markdown(String::new())).unwrap();
        assert_eq!(applied.item.core.description, "");
    }

    #[test]
    fn priority_and_override() {
        let applied = set(&item(), F::Priority, V::Priority(Priority::High)).unwrap();
        assert_eq!(
            applied.changes,
            [change(
                F::Priority,
                V::Priority(Priority::None),
                V::Priority(Priority::High)
            )]
        );
        let back = set(&applied.item, F::Priority, V::Priority(Priority::None)).unwrap();
        assert_eq!(back.item.core.priority, Priority::None);
        invalid(F::Priority, V::Cleared);

        let applied = set(
            &item(),
            F::LocalPriorityOverride,
            V::Priority(Priority::Lowest),
        )
        .unwrap();
        assert_eq!(
            applied.item.overlay.local_priority_override,
            Some(Priority::Lowest)
        );
        assert_eq!(
            applied.changes,
            [change(
                F::LocalPriorityOverride,
                V::Cleared,
                V::Priority(Priority::Lowest)
            )]
        );
        let cleared = set(&applied.item, F::LocalPriorityOverride, V::Cleared).unwrap();
        assert_eq!(cleared.item.overlay.local_priority_override, None);
        invalid(F::LocalPriorityOverride, V::Priority(Priority::None));
    }

    #[test]
    fn assignee_is_a_person_or_cleared() {
        let applied = set(&item(), F::Assignee, V::Person(PersonRef::Person(id(2)))).unwrap();
        assert_eq!(applied.item.core.assignee, Some(PersonRef::Person(id(2))));
        let cleared = set(&applied.item, F::Assignee, V::Cleared).unwrap();
        assert_eq!(cleared.item.core.assignee, None);
        invalid(F::Assignee, V::Person(PersonRef::FreeText("dana".into())));
    }

    #[test]
    fn labels_are_validated_and_deduplicated() {
        let labels = |v: &[&str]| V::StringSet(v.iter().map(|&s| s.to_owned()).collect());
        let applied = set(&item(), F::Labels, labels(&["b", "a", "b", "A"])).unwrap();
        assert_eq!(applied.item.core.labels, ["b", "a", "A"]);
        assert_eq!(
            applied.changes,
            [change(F::Labels, labels(&[]), labels(&["b", "a", "A"]))]
        );
        // Equal after deduplication: no change.
        let again = set(&applied.item, F::Labels, labels(&["b", "b", "a", "A"])).unwrap();
        assert_eq!(again.changes, []);
        // Order is significant.
        let reordered = set(&applied.item, F::Labels, labels(&["a", "b", "A"])).unwrap();
        assert_eq!(reordered.changes.len(), 1);
        let cleared = set(&applied.item, F::Labels, labels(&[])).unwrap();
        assert!(cleared.item.core.labels.is_empty());
        for bad in [&[""][..], &["a b"], &["ok", " x"], &["tab\t"]] {
            invalid(F::Labels, labels(bad));
        }
    }

    #[test]
    fn many_labels_are_deduplicated_in_linear_time() {
        let labels = (0..100_000).map(|i| format!("l{}", i % 50_000)).collect();
        let started = std::time::Instant::now();
        let applied = set(&item(), F::Labels, V::StringSet(labels)).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(applied.item.core.labels.len(), 50_000);
        assert_eq!(applied.item.core.labels[49_999], "l49999");
        assert!(elapsed < std::time::Duration::from_secs(10), "{elapsed:?}");
    }

    #[test]
    fn tracker_set_fields_need_a_link() {
        let names = |v: &[&str]| V::StringSet(v.iter().map(|&s| s.to_owned()).collect());
        for field in [F::Components, F::FixVersions] {
            assert!(matches!(
                set(&item(), field, names(&["api"])),
                Err(MutationError::Unsupported { field: f, .. }) if f == field
            ));
            let applied = set(&linked(), field, names(&[" Backend API ", "web", "web"])).unwrap();
            let want = ["Backend API", "web"];
            match field {
                F::Components => assert_eq!(applied.item.core.components, want),
                _ => assert_eq!(applied.item.core.fix_versions, want),
            }
            assert!(matches!(
                set(&linked(), field, names(&["ok", "  "])),
                Err(MutationError::Invalid { field: f, .. }) if f == field
            ));
        }
    }

    #[test]
    fn dates() {
        for field in [F::DueDate, F::DeferUntil] {
            let applied = set(&item(), field, V::Date(date(2026, 10, 24))).unwrap();
            assert_eq!(
                applied.changes,
                [change(field, V::Cleared, V::Date(date(2026, 10, 24)))]
            );
            let cleared = set(&applied.item, field, V::Cleared).unwrap();
            assert_eq!(
                cleared.changes,
                [change(field, V::Date(date(2026, 10, 24)), V::Cleared)]
            );
            assert_eq!(cleared.item.core.due_date, None);
            assert_eq!(cleared.item.overlay.defer_until, None);
        }
    }

    #[test]
    fn parent_id() {
        let applied = set(&item(), F::ParentId, V::ItemId(id(2))).unwrap();
        assert_eq!(applied.item.core.parent_id, Some(id(2)));
        let cleared = set(&applied.item, F::ParentId, V::Cleared).unwrap();
        assert_eq!(cleared.item.core.parent_id, None);
        invalid(F::ParentId, V::ItemId(id(1)));
    }

    #[test]
    fn story_points_and_estimates() {
        for points in [0.0, 2.5, 13.0] {
            let applied = set(&item(), F::StoryPoints, V::Number(points)).unwrap();
            assert_eq!(applied.item.core.story_points, Some(points));
        }
        for bad in [-1.0, -0.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            invalid(F::StoryPoints, V::Number(bad));
        }
        let mut pointed = item();
        pointed.core.story_points = Some(3.0);
        let cleared = set(&pointed, F::StoryPoints, V::Cleared).unwrap();
        assert_eq!(
            cleared.changes,
            [change(F::StoryPoints, V::Number(3.0), V::Cleared)]
        );

        for field in [F::OriginalEstimate, F::RemainingEstimate] {
            let hours = SignedDuration::from_hours(4);
            let applied = set(&item(), field, V::Duration(hours)).unwrap();
            assert_eq!(
                applied.changes,
                [change(field, V::Cleared, V::Duration(hours))]
            );
            assert!(set(&item(), field, V::Duration(SignedDuration::ZERO)).is_ok());
            assert!(set(&applied.item, field, V::Cleared).is_ok());
            invalid(field, V::Duration(SignedDuration::from_secs(-1)));
        }
    }

    #[test]
    fn focus_week() {
        for week in ["2026-W43", "2026-W01", "2026-W53", "2020-W53"] {
            let applied = set(&item(), F::FocusWeek, V::IsoWeek(week.into())).unwrap();
            assert_eq!(applied.item.overlay.focus_week.as_deref(), Some(week));
        }
        for bad in [
            "2025-W53",
            "2026-W00",
            "2026-W54",
            "2026-W1",
            "2026-W001",
            "2026-w43",
            "26-W43",
            "2026W43",
            "2026-43",
            "2026-W4x",
            "+202-W43",
            "2026-W43 ",
            "",
        ] {
            invalid(F::FocusWeek, V::IsoWeek(bad.into()));
        }
        let mut focused = item();
        focused.overlay.focus_week = Some("2026-W43".into());
        let cleared = set(&focused, F::FocusWeek, V::Cleared).unwrap();
        assert_eq!(cleared.item.overlay.focus_week, None);
    }

    #[test]
    fn whole_waiting() {
        let set_to = waiting(PersonRef::FreeText("  the vendor  ".into()));
        let applied = set(&item(), F::Waiting, V::Waiting(set_to)).unwrap();
        let stored = applied.item.overlay.waiting.clone().unwrap();
        assert_eq!(stored.on, PersonRef::FreeText("the vendor".into()));
        assert_eq!(
            applied.changes,
            [change(F::Waiting, V::Cleared, V::Waiting(stored.clone()))]
        );
        let cleared = set(&applied.item, F::Waiting, V::Cleared).unwrap();
        assert_eq!(
            cleared.changes,
            [change(F::Waiting, V::Waiting(stored), V::Cleared)]
        );
        assert!(
            set(
                &item(),
                F::Waiting,
                V::Waiting(waiting(PersonRef::Person(id(2))))
            )
            .is_ok()
        );
        assert!(matches!(
            set(
                &item(),
                F::Waiting,
                V::Waiting(waiting(PersonRef::FreeText(" ".into())))
            ),
            Err(MutationError::Invalid {
                field: F::WaitingOn,
                ..
            })
        ));
    }

    #[test]
    fn nested_waiting() {
        let item = waiting_item();
        let applied = set(&item, F::WaitingReason, V::String("contract".into())).unwrap();
        assert_eq!(
            applied.changes,
            [change(
                F::WaitingReason,
                V::String("quote".into()),
                V::String("contract".into())
            )]
        );
        let applied = set(&item, F::WaitingOn, V::Person(PersonRef::Person(id(3)))).unwrap();
        assert_eq!(
            applied.item.overlay.waiting.as_ref().unwrap().on,
            PersonRef::Person(id(3))
        );
        let applied = set(
            &item,
            F::WaitingOn,
            V::Person(PersonRef::FreeText(" dana ".into())),
        )
        .unwrap();
        assert_eq!(
            applied.changes,
            [change(
                F::WaitingOn,
                V::Person(PersonRef::FreeText("the vendor".into())),
                V::Person(PersonRef::FreeText("dana".into()))
            )]
        );
        let applied = set(&item, F::WaitingSince, V::Date(date(2026, 10, 1))).unwrap();
        assert_eq!(
            applied.item.overlay.waiting.as_ref().unwrap().since,
            date(2026, 10, 1)
        );
        let applied = set(&item, F::WaitingFollowUpOn, V::Date(date(2026, 10, 30))).unwrap();
        assert_eq!(
            applied.changes,
            [change(
                F::WaitingFollowUpOn,
                V::Cleared,
                V::Date(date(2026, 10, 30))
            )]
        );
        let cleared = set(&applied.item, F::WaitingFollowUpOn, V::Cleared).unwrap();
        assert_eq!(cleared.item.overlay.waiting, item.overlay.waiting);

        assert!(matches!(
            set(&item, F::WaitingOn, V::Cleared),
            Err(MutationError::Invalid {
                field: F::WaitingOn,
                ..
            })
        ));
        assert!(matches!(
            set(
                &item,
                F::WaitingOn,
                V::Person(PersonRef::FreeText("".into()))
            ),
            Err(MutationError::Invalid {
                field: F::WaitingOn,
                ..
            })
        ));
    }

    #[test]
    fn nested_waiting_needs_waiting() {
        let rows = [
            (F::WaitingOn, V::Person(PersonRef::Person(id(2)))),
            (F::WaitingReason, V::String("x".into())),
            (F::WaitingSince, V::Date(date(2026, 10, 1))),
            (F::WaitingFollowUpOn, V::Cleared),
        ];
        for (field, value) in rows {
            assert_eq!(
                set(&item(), field, value),
                Err(MutationError::ParentUnset(field))
            );
        }
    }

    #[test]
    fn duplicates() {
        let title = |s: &str| V::String(s.into());
        assert_eq!(
            update(&item(), &[(F::Title, title("a")), (F::Title, title("b"))]),
            Err(MutationError::Duplicate(F::Title))
        );
        let item = waiting_item();
        let whole = V::Waiting(item.overlay.waiting.clone().unwrap());
        assert_eq!(
            update(
                &item,
                &[(F::Waiting, whole.clone()), (F::WaitingReason, title("x"))]
            ),
            Err(MutationError::Duplicate(F::WaitingReason))
        );
        assert_eq!(
            update(
                &item,
                &[(F::WaitingReason, title("x")), (F::Waiting, whole)]
            ),
            Err(MutationError::Duplicate(F::Waiting))
        );
        let applied = update(
            &item,
            &[
                (F::WaitingReason, title("x")),
                (F::WaitingFollowUpOn, V::Date(date(2026, 11, 1))),
            ],
        )
        .unwrap();
        assert_eq!(applied.changes.len(), 2);
    }

    #[test]
    fn update_records_changes_in_order_and_skips_unchanged_fields() {
        let applied = update(
            &item(),
            &[
                (F::DueDate, V::Date(date(2026, 10, 24))),
                (F::Title, V::String("call dentist".into())),
                (F::Priority, V::Priority(Priority::Medium)),
            ],
        )
        .unwrap();
        assert_eq!(
            applied.changes,
            [
                change(F::DueDate, V::Cleared, V::Date(date(2026, 10, 24))),
                change(
                    F::Priority,
                    V::Priority(Priority::None),
                    V::Priority(Priority::Medium)
                ),
            ]
        );
        assert_eq!(applied.item.revision, 7);
        assert_eq!(
            run(&item(), MutationOp::Update(vec![])).unwrap().changes,
            []
        );
    }

    #[test]
    fn a_failing_update_fails_whole() {
        assert!(matches!(
            update(
                &item(),
                &[
                    (F::Title, V::String("ok".into())),
                    (F::StoryPoints, V::Number(-1.0)),
                ],
            ),
            Err(MutationError::Invalid {
                field: F::StoryPoints,
                ..
            })
        ));
    }

    #[test]
    fn no_op_returns_the_item_unchanged() {
        let item = waiting_item();
        let ops = [
            MutationOp::Update(vec![FieldUpdate {
                field: F::Title,
                value: V::String(" call dentist ".into()),
            }]),
            MutationOp::SetStatus(Status::Inbox),
            MutationOp::Unarchive,
            MutationOp::Undelete,
        ];
        for op in ops {
            let applied = apply(&item, &op, &ctx(Actor::User)).unwrap();
            assert_eq!(applied.changes, [], "{op:?}");
            assert_eq!(applied.item, item, "{op:?}");
        }
    }

    #[test]
    fn status_transitions() {
        use ResolutionKind as R;
        use StatusCategory as C;
        use TodoRefinement as T;
        let mut item = item();
        let mut step = |target: Status, want: Vec<FieldChange>| {
            let applied = run(&item, MutationOp::SetStatus(target)).unwrap();
            assert_eq!(applied.changes, want, "-> {target:?}");
            assert_eq!(applied.item.status(), target);
            item = applied.item;
        };
        step(
            Status::Next,
            vec![change(
                F::TodoRefinement,
                V::TodoRefinement(T::Inbox),
                V::TodoRefinement(T::Next),
            )],
        );
        step(
            Status::InProgress,
            vec![change(
                F::StatusCategory,
                V::StatusCategory(C::Todo),
                V::StatusCategory(C::InProgress),
            )],
        );
        step(
            Status::Done,
            vec![
                change(
                    F::StatusCategory,
                    V::StatusCategory(C::InProgress),
                    V::StatusCategory(C::Done),
                ),
                change(F::ResolutionKind, V::Cleared, V::ResolutionKind(R::Done)),
            ],
        );
        step(
            Status::Cancelled,
            vec![change(
                F::ResolutionKind,
                V::ResolutionKind(R::Done),
                V::ResolutionKind(R::Cancelled),
            )],
        );
        step(Status::Cancelled, vec![]);
        step(
            Status::Someday,
            vec![
                change(
                    F::StatusCategory,
                    V::StatusCategory(C::Done),
                    V::StatusCategory(C::Todo),
                ),
                change(
                    F::ResolutionKind,
                    V::ResolutionKind(R::Cancelled),
                    V::Cleared,
                ),
                change(
                    F::TodoRefinement,
                    V::TodoRefinement(T::Next),
                    V::TodoRefinement(T::Someday),
                ),
            ],
        );
        step(
            Status::InProgress,
            vec![change(
                F::StatusCategory,
                V::StatusCategory(C::Todo),
                V::StatusCategory(C::InProgress),
            )],
        );
        // The refinement is retained outside todo.
        assert_eq!(item.overlay.todo_refinement, T::Someday);
    }

    #[test]
    fn archive_and_unarchive() {
        let applied = run(&item(), MutationOp::Archive).unwrap();
        let archived = Archived {
            at: now(),
            reason: ArchiveReason::User,
        };
        assert_eq!(applied.item.overlay.archived, Some(archived));
        assert_eq!(
            applied.changes,
            [change(F::Archived, V::Cleared, V::Archived(archived))]
        );
        let again = run(&applied.item, MutationOp::Archive).unwrap();
        assert_eq!(again.changes, []);
        assert_eq!(again.item, applied.item);

        let mut left_scope = item();
        left_scope.overlay.archived = Some(Archived {
            at: ts(100),
            reason: ArchiveReason::LeftScope,
        });
        assert_eq!(run(&left_scope, MutationOp::Archive).unwrap().changes, []);

        let unarchived = run(&applied.item, MutationOp::Unarchive).unwrap();
        assert_eq!(unarchived.item.overlay.archived, None);
        assert_eq!(
            unarchived.changes,
            [change(F::Archived, V::Archived(archived), V::Cleared)]
        );
        assert_eq!(
            run(&unarchived.item, MutationOp::Unarchive)
                .unwrap()
                .changes,
            []
        );
    }

    #[test]
    fn delete_and_undelete() {
        let deleted = run(&item(), MutationOp::Delete).unwrap();
        assert_eq!(deleted.item.derived.deleted_at, Some(now()));
        assert_eq!(
            deleted.changes,
            [change(F::DeletedAt, V::Cleared, V::Timestamp(now()))]
        );
        let again = apply(
            &deleted.item,
            &MutationOp::Delete,
            &ApplyCtx {
                now: ts(9_000),
                actor: Actor::User,
            },
        )
        .unwrap();
        assert_eq!(again.changes, []);
        assert_eq!(again.item, deleted.item);

        let rejected = [
            MutationOp::Update(vec![FieldUpdate {
                field: F::Title,
                value: V::String("x".into()),
            }]),
            MutationOp::Update(vec![]),
            MutationOp::SetStatus(Status::Done),
            MutationOp::Archive,
            MutationOp::Unarchive,
        ];
        for op in rejected {
            assert_eq!(
                run(&deleted.item, op.clone()),
                Err(MutationError::Deleted),
                "{op:?}"
            );
        }

        let undeleted = run(&deleted.item, MutationOp::Undelete).unwrap();
        assert_eq!(undeleted.item.derived.deleted_at, None);
        assert_eq!(
            undeleted.changes,
            [change(F::DeletedAt, V::Timestamp(now()), V::Cleared)]
        );
        assert_eq!(
            run(&undeleted.item, MutationOp::Undelete).unwrap().changes,
            []
        );
    }

    #[test]
    fn bookkeeping_per_actor() {
        let actors = [
            (Actor::User, true),
            (Actor::Suggestion(id(9)), true),
            (Actor::Sync, false),
            (Actor::System, false),
        ];
        for (actor, touches) in actors {
            let before = item();
            let op = MutationOp::Update(vec![FieldUpdate {
                field: F::Title,
                value: V::String("x".into()),
            }]);
            let after = apply(&before, &op, &ctx(actor)).unwrap().item;
            assert_eq!(after.updated_at, now(), "{actor:?}");
            assert_eq!(after.revision, before.revision);
            let staleness = after.derived.staleness;
            let touched = touches.then_some(now());
            assert_eq!(staleness.last_user_touch_at, touched, "{actor:?}");
            assert_eq!(staleness.status_entered_at, None, "{actor:?}");
            assert_eq!(staleness.waiting_since, None, "{actor:?}");

            for op in [
                MutationOp::SetStatus(Status::Next),
                MutationOp::Archive,
                MutationOp::Delete,
            ] {
                let after = apply(&before, &op, &ctx(actor)).unwrap().item;
                assert_eq!(after.updated_at, now(), "{op:?}");
                assert_eq!(
                    after.derived.staleness.last_user_touch_at, touched,
                    "{op:?}"
                );
            }
        }
    }

    #[test]
    fn status_entered_at_follows_the_derived_status() {
        let applied = run(&item(), MutationOp::SetStatus(Status::Next)).unwrap();
        assert_eq!(
            applied.item.derived.staleness.status_entered_at,
            Some(now())
        );
        let mut item = item();
        item.derived.staleness.status_entered_at = Some(ts(10));
        let archived = run(&item, MutationOp::Archive).unwrap();
        assert_eq!(
            archived.item.derived.staleness.status_entered_at,
            Some(ts(10))
        );
    }

    #[test]
    fn waiting_since_follows_waiting() {
        let set_to = V::Waiting(waiting(PersonRef::Person(id(2))));
        let applied = set(&item(), F::Waiting, set_to.clone()).unwrap();
        assert_eq!(applied.item.derived.staleness.waiting_since, Some(now()));

        let item = waiting_item();
        let replaced = set(&item, F::Waiting, set_to).unwrap();
        assert_eq!(replaced.changes.len(), 1);
        assert_eq!(replaced.item.derived.staleness.waiting_since, Some(ts(900)));
        let nested = set(&item, F::WaitingReason, V::String("x".into())).unwrap();
        assert_eq!(nested.item.derived.staleness.waiting_since, Some(ts(900)));
        let cleared = set(&item, F::Waiting, V::Cleared).unwrap();
        assert_eq!(cleared.item.derived.staleness.waiting_since, None);
    }

    #[test]
    fn errors_name_their_field() {
        let rows = [
            (MutationError::ReadOnly(F::Id), Some(F::Id)),
            (MutationError::UseSetStatus(F::Status), Some(F::Status)),
            (
                MutationError::UseArchive(F::ArchivedAt),
                Some(F::ArchivedAt),
            ),
            (
                MutationError::WrongType {
                    field: F::Title,
                    got: "bool",
                },
                Some(F::Title),
            ),
            (
                MutationError::Invalid {
                    field: F::FocusWeek,
                    reason: "x".into(),
                },
                Some(F::FocusWeek),
            ),
            (MutationError::ParentUnset(F::WaitingOn), Some(F::WaitingOn)),
            (MutationError::Duplicate(F::Labels), Some(F::Labels)),
            (MutationError::Deleted, None),
            (
                MutationError::Unsupported {
                    field: F::Components,
                    reason: "x".into(),
                },
                Some(F::Components),
            ),
        ];
        for (error, field) in rows {
            assert_eq!(error.field(), field, "{error}");
        }
        assert_eq!(
            set(&item(), F::Title, V::Bool(true))
                .unwrap_err()
                .to_string(),
            "Title does not take a bool value"
        );
        assert_eq!(
            set(&item(), F::Title, V::String(" ".into()))
                .unwrap_err()
                .to_string(),
            "Title: must not be empty"
        );
    }
}
