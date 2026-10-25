//! Mutating `ItemService` requests -> core [`Mutation`](core::Mutation)s and
//! [`Relation`](core::Relation)s.
//!
//! Errors name the offending field relative to the request (`item.title`,
//! `update_mask`, `id`); the daemon prefixes a batched mutation's errors with
//! its index through [`ConvertError::within`]. `request_id` is the daemon's
//! concern and is not read here, except that a batched mutation must not have
//! its own.

use jiff::civil::Date;
use todo_core as core;

use crate::convert::{
    ConvertError, EnumField, Result, date_from_proto, date_to_proto, duration_from_proto,
    iso_week_from_proto, item_field_from_path, item_field_path, parse_id, person_from_proto,
    require,
};
use crate::google::r#type as gtype;
use crate::v1;

/// The updates of an `UpdateItem`, one per `mask` path in mask order. Each
/// value is read from `item` in the field's canonical [`core::FieldValue`]
/// variant: an unset field is `Cleared`, except where the field has a domain
/// "none" (unset `priority` is `Priority::None`, unset `waiting.reason` is
/// ""). A `waiting` without `since` is waiting since `today`. Fields that core
/// rejects for their class (read-only, status and archived fields) are not
/// read: they get `Cleared`, and core reports them. Core also rejects the
/// update at its first repeated field, so from there on no value is read
/// either, and a long mask repeating a large field stays cheap. `relations`
/// is not a [`core::ItemField`]; it is refused here, at `item.relations`.
pub fn field_updates_from_proto(
    item: &v1::Item,
    mask: &prost_types::FieldMask,
    today: Date,
) -> Result<Vec<core::FieldUpdate>> {
    let invalid_mask = |reason: String| ConvertError::Invalid("update_mask".into(), reason);
    if mask.paths.is_empty() {
        return Err(invalid_mask("must not be empty".into()));
    }
    let mut read = Vec::new();
    let mut repeated = false;
    let mut updates = Vec::with_capacity(mask.paths.len());
    for path in &mask.paths {
        if path == "*" {
            return Err(invalid_mask("\"*\" is not supported".into()));
        }
        if path == "relations" {
            let reason = "changes only through AddRelation and RemoveRelation".into();
            return Err(ConvertError::Invalid("item.relations".into(), reason));
        }
        let field = item_field_from_path(path)
            .ok_or_else(|| invalid_mask(format!("unknown field path {path:?}")))?;
        repeated = repeated || read.contains(&field);
        let value = if repeated {
            core::FieldValue::Cleared
        } else {
            read.push(field);
            update_value(item, field, today).map_err(|e| e.within("item"))?
        };
        updates.push(core::FieldUpdate { field, value });
    }
    Ok(updates)
}

/// The value `item` sets `field` to; errors are relative to `item`.
fn update_value(item: &v1::Item, field: core::ItemField, today: Date) -> Result<core::FieldValue> {
    use core::FieldValue as V;
    use core::ItemField as F;
    let path = item_field_path(field);
    let waiting = item.waiting.as_ref();
    let date = |d: Option<&gtype::Date>| opt(d, |d| Ok(V::Date(date_from_proto(path, d)?)));
    let duration = |d: Option<&prost_types::Duration>| {
        opt(d, |d| Ok(V::Duration(duration_from_proto(path, d)?)))
    };
    let person = |p: Option<&v1::PersonRef>| opt(p, |p| Ok(V::Person(person_from_proto(path, p)?)));
    let value = match field {
        F::Title => V::String(item.title.clone()),
        F::Description => V::Markdown(item.description.clone()),
        F::ItemType => V::String(item.item_type.clone()),
        F::Priority if item.priority == v1::Priority::Unspecified as i32 => {
            V::Priority(core::Priority::None)
        }
        F::Priority => V::Priority(EnumField::from_i32(path, item.priority)?),
        F::Assignee => person(item.assignee.as_ref())?,
        F::Labels => V::StringSet(item.labels.clone()),
        F::Components => V::StringSet(item.components.clone()),
        F::FixVersions => V::StringSet(item.fix_versions.clone()),
        F::DueDate => date(item.due_date.as_ref())?,
        F::ParentId => opt(item.parent_id.as_deref(), |id| {
            Ok(V::ItemId(parse_id(path, id)?))
        })?,
        F::StoryPoints => opt(item.story_points, |n| Ok(V::Number(n)))?,
        F::OriginalEstimate => duration(item.original_estimate.as_ref())?,
        F::RemainingEstimate => duration(item.remaining_estimate.as_ref())?,
        F::Waiting => opt(waiting, |w| {
            let mut w = w.clone();
            w.since.get_or_insert_with(|| date_to_proto(today));
            Ok(V::Waiting((&w).try_into()?))
        })?,
        F::DeferUntil => date(item.defer_until.as_ref())?,
        F::PrivateNotes => V::Markdown(item.private_notes.clone()),
        F::FocusWeek => opt(item.focus_week.as_deref(), |w| {
            Ok(V::IsoWeek(iso_week_from_proto(path, w)?))
        })?,
        F::LocalPriorityOverride => opt(item.local_priority_override, |p| {
            Ok(V::Priority(EnumField::from_i32(path, p)?))
        })?,
        F::WaitingOn => person(waiting.and_then(|w| w.on.as_ref()))?,
        F::WaitingReason => V::String(waiting.map(|w| w.reason.clone()).unwrap_or_default()),
        F::WaitingSince => date(waiting.and_then(|w| w.since.as_ref()))?,
        F::WaitingFollowUpOn => date(waiting.and_then(|w| w.follow_up_on.as_ref()))?,
        F::Status
        | F::StatusCategory
        | F::ResolutionKind
        | F::TodoRefinement
        | F::Archived
        | F::ArchivedAt
        | F::ArchivedReason
        | F::Id
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
        | F::IsOverdue => V::Cleared,
    };
    Ok(value)
}

/// `Cleared` for `None`.
fn opt<T>(
    value: Option<T>,
    convert: impl FnOnce(T) -> Result<core::FieldValue>,
) -> Result<core::FieldValue> {
    value.map_or(Ok(core::FieldValue::Cleared), convert)
}

/// `today` is the user's day, for a `waiting` without `since`.
pub fn update_item_from_proto(req: &v1::UpdateItemRequest, today: Date) -> Result<core::Mutation> {
    let item = require("item", req.item.as_ref())?;
    let id = parse_id("item.id", &item.id)?;
    let mask = require("update_mask", req.update_mask.as_ref())?;
    Ok(core::Mutation {
        id,
        expected_revision: req.expected_revision,
        op: core::MutationOp::Update(field_updates_from_proto(item, mask, today)?),
    })
}

pub fn set_status_from_proto(req: &v1::SetStatusRequest) -> Result<core::Mutation> {
    Ok(core::Mutation {
        id: parse_id("id", &req.id)?,
        expected_revision: req.expected_revision,
        op: core::MutationOp::SetStatus(EnumField::from_i32("status", req.status)?),
    })
}

pub fn archive_item_from_proto(req: &v1::ArchiveItemRequest) -> Result<core::Mutation> {
    item_op(&req.id, req.expected_revision, core::MutationOp::Archive)
}

pub fn unarchive_item_from_proto(req: &v1::UnarchiveItemRequest) -> Result<core::Mutation> {
    item_op(&req.id, req.expected_revision, core::MutationOp::Unarchive)
}

pub fn delete_item_from_proto(req: &v1::DeleteItemRequest) -> Result<core::Mutation> {
    item_op(&req.id, req.expected_revision, core::MutationOp::Delete)
}

pub fn undelete_item_from_proto(req: &v1::UndeleteItemRequest) -> Result<core::Mutation> {
    item_op(&req.id, req.expected_revision, core::MutationOp::Undelete)
}

fn item_op(
    id: &str,
    expected_revision: Option<u64>,
    op: core::MutationOp,
) -> Result<core::Mutation> {
    Ok(core::Mutation {
        id: parse_id("id", id)?,
        expected_revision,
        op,
    })
}

/// The relation of an `AddRelation` or `RemoveRelation`, canonical (a
/// `RELATES_TO` has `from_id < to_id`). Errors are at `relation.*`; an Item
/// related to itself is invalid at `relation.to_id`.
pub fn relation_from_proto(relation: Option<&v1::Relation>) -> Result<core::Relation> {
    core::Relation::try_from(require("relation", relation)?).map_err(|e| e.within("relation"))
}

/// One mutation of a `BatchMutate`. Errors are prefixed with the oneof case
/// (`update_item.item.title`); an unset oneof is `Missing` at `kind`.
pub fn mutation_from_proto(m: &v1::Mutation, today: Date) -> Result<core::Mutation> {
    use v1::mutation::Kind;
    let (case, request_id, mutation) = match require("kind", m.kind.as_ref())? {
        Kind::UpdateItem(r) => (
            "update_item",
            &r.request_id,
            update_item_from_proto(r, today),
        ),
        Kind::SetStatus(r) => ("set_status", &r.request_id, set_status_from_proto(r)),
        Kind::ArchiveItem(r) => ("archive_item", &r.request_id, archive_item_from_proto(r)),
        Kind::UnarchiveItem(r) => (
            "unarchive_item",
            &r.request_id,
            unarchive_item_from_proto(r),
        ),
        Kind::DeleteItem(r) => ("delete_item", &r.request_id, delete_item_from_proto(r)),
        Kind::UndeleteItem(r) => ("undelete_item", &r.request_id, undelete_item_from_proto(r)),
    };
    if !request_id.is_empty() {
        let reason = "must be empty in a batch: the batch's request_id covers it";
        return Err(ConvertError::Invalid("request_id".into(), reason.into()).within(case));
    }
    mutation.map_err(|e| e.within(case))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::{SignedDuration, Timestamp};
    use prost_types::FieldMask;
    use todo_core::{FieldUpdate, FieldValue as V, ItemField as F, MutationOp, PersonRef};
    use ulid::Ulid;
    use v1::mutation::Kind;

    use super::*;
    use crate::convert::duration_to_proto;

    /// A Thursday.
    const TODAY: Date = date(2026, 10, 22);

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    fn mask(paths: &[&str]) -> FieldMask {
        FieldMask {
            paths: paths.iter().map(|&p| p.to_owned()).collect(),
        }
    }

    fn updates(item: &v1::Item, paths: &[&str]) -> Result<Vec<FieldUpdate>> {
        field_updates_from_proto(item, &mask(paths), TODAY)
    }

    /// The value the single `path` reads from `item`.
    fn value(item: &v1::Item, path: &str) -> Result<V> {
        let updates = updates(item, &[path])?;
        assert_eq!(updates.len(), 1);
        assert_eq!(item_field_path(updates[0].field), path);
        Ok(updates[0].value.clone())
    }

    fn waiting() -> core::Waiting {
        core::Waiting {
            on: PersonRef::FreeText("Dana".into()),
            reason: "review".into(),
            since: date(2026, 10, 1),
            follow_up_on: Some(date(2026, 10, 20)),
        }
    }

    /// Every writable field set.
    fn full_item() -> v1::Item {
        v1::Item {
            id: id(1).to_string(),
            title: "Fix flaky test".into(),
            description: "**md**".into(),
            item_type: "Bug".into(),
            priority: v1::Priority::High as i32,
            assignee: Some((&PersonRef::Person(id(2))).into()),
            labels: vec!["a".into(), "b".into()],
            components: vec!["api".into()],
            fix_versions: vec!["1.2".into()],
            due_date: Some(date_to_proto(date(2026, 11, 2))),
            parent_id: Some(id(3).to_string()),
            story_points: Some(2.5),
            original_estimate: Some(duration_to_proto(SignedDuration::from_hours(4))),
            remaining_estimate: Some(duration_to_proto(SignedDuration::from_mins(90))),
            waiting: Some((&waiting()).into()),
            defer_until: Some(date_to_proto(date(2026, 10, 25))),
            private_notes: "notes".into(),
            focus_week: Some("2026-W43".into()),
            local_priority_override: Some(v1::Priority::Highest as i32),
            ..Default::default()
        }
    }

    /// Every writable field: its path, its value in `full_item()`, and its
    /// value when unset.
    fn writable() -> Vec<(&'static str, V, V)> {
        let set = |v: &[&str]| V::StringSet(v.iter().map(|&s| s.to_owned()).collect());
        let empty = || V::String(String::new());
        vec![
            ("title", V::String("Fix flaky test".into()), empty()),
            (
                "description",
                V::Markdown("**md**".into()),
                V::Markdown(String::new()),
            ),
            ("item_type", V::String("Bug".into()), empty()),
            (
                "priority",
                V::Priority(core::Priority::High),
                V::Priority(core::Priority::None),
            ),
            ("assignee", V::Person(PersonRef::Person(id(2))), V::Cleared),
            ("labels", set(&["a", "b"]), set(&[])),
            ("components", set(&["api"]), set(&[])),
            ("fix_versions", set(&["1.2"]), set(&[])),
            ("due_date", V::Date(date(2026, 11, 2)), V::Cleared),
            ("parent_id", V::ItemId(id(3)), V::Cleared),
            ("story_points", V::Number(2.5), V::Cleared),
            (
                "original_estimate",
                V::Duration(SignedDuration::from_hours(4)),
                V::Cleared,
            ),
            (
                "remaining_estimate",
                V::Duration(SignedDuration::from_mins(90)),
                V::Cleared,
            ),
            ("waiting", V::Waiting(waiting()), V::Cleared),
            ("defer_until", V::Date(date(2026, 10, 25)), V::Cleared),
            (
                "private_notes",
                V::Markdown("notes".into()),
                V::Markdown(String::new()),
            ),
            ("focus_week", V::IsoWeek("2026-W43".into()), V::Cleared),
            (
                "local_priority_override",
                V::Priority(core::Priority::Highest),
                V::Cleared,
            ),
            (
                "waiting.on",
                V::Person(PersonRef::FreeText("Dana".into())),
                V::Cleared,
            ),
            ("waiting.reason", V::String("review".into()), empty()),
            ("waiting.since", V::Date(date(2026, 10, 1)), V::Cleared),
            (
                "waiting.follow_up_on",
                V::Date(date(2026, 10, 20)),
                V::Cleared,
            ),
        ]
    }

    fn core_item() -> core::Item {
        let mut item = core::Item::new_capture(
            id(1),
            "call dentist".into(),
            core::CreatedVia::Tui,
            Timestamp::from_second(1_000).unwrap(),
        );
        item.remote_link = Some(core::RemoteLink {
            tracker_kind: "jira".into(),
            instance_id: "work-jira".into(),
            remote_id: "10001".into(),
            key: "PLAT-530".into(),
            url: "https://jira.example.com/browse/PLAT-530".into(),
            last_pulled_version: None,
            link_state: core::LinkState::Ok,
            pinned: false,
        });
        // Differs from `waiting()` in every leaf.
        item.overlay.waiting = Some(core::Waiting {
            on: PersonRef::FreeText("the vendor".into()),
            reason: "quote".into(),
            since: date(2026, 9, 1),
            follow_up_on: None,
        });
        item
    }

    fn apply(
        item: &core::Item,
        updates: Vec<FieldUpdate>,
    ) -> Result<core::Applied, core::MutationError> {
        let ctx = core::ApplyCtx {
            now: Timestamp::from_second(5_000).unwrap(),
            actor: core::Actor::User,
        };
        core::apply(item, &MutationOp::Update(updates), &ctx)
    }

    #[test]
    fn writable_fields_read_their_value() {
        let full = full_item();
        let empty = v1::Item::default();
        for (path, set, unset) in writable() {
            assert_eq!(value(&full, path), Ok(set), "{path}");
            assert_eq!(value(&empty, path), Ok(unset), "{path}");
        }
    }

    /// The values are in the variants core takes for each field.
    #[test]
    fn core_applies_the_values() {
        let item = core_item();
        let full = full_item();
        for (path, ..) in writable() {
            let got = updates(&full, &[path]).unwrap();
            let applied = apply(&item, got).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(applied.changes.len(), 1, "{path}");
        }
    }

    #[test]
    fn other_fields_are_cleared_for_core_to_reject() {
        let writable: Vec<&str> = writable().iter().map(|(path, ..)| *path).collect();
        // Values that would fail to convert if they were read.
        let item = v1::Item {
            created_via: 99,
            status_category: 0,
            status: 99,
            remote_link: Some(v1::RemoteLink::default()),
            archived: Some(v1::Archived::default()),
            recurrence: Some(v1::ItemRecurrence {
                series_id: "nope".into(),
                occurrence_date: None,
            }),
            staleness: Some(v1::StalenessSignals {
                waiting_since: Some(prost_types::Timestamp {
                    seconds: 0,
                    nanos: -1,
                }),
                ..Default::default()
            }),
            merged_into: Some("nope".into()),
            ..full_item()
        };
        let core_item = core_item();
        for &field in core::ItemField::ALL {
            let path = item_field_path(field);
            if writable.contains(&path) {
                continue;
            }
            let got = updates(&item, &[path]).unwrap_or_else(|e| panic!("{path}: {e}"));
            let cleared = FieldUpdate {
                field,
                value: V::Cleared,
            };
            assert_eq!(got, [cleared], "{path}");
            let err = apply(&core_item, got).unwrap_err();
            assert!(
                matches!(
                    err,
                    core::MutationError::ReadOnly(f)
                    | core::MutationError::UseSetStatus(f)
                    | core::MutationError::UseArchive(f) if f == field
                ),
                "{path}: {err}"
            );
        }
    }

    #[test]
    fn priorities() {
        let with = |priority, local_priority_override| v1::Item {
            priority,
            local_priority_override,
            ..Default::default()
        };
        let none = v1::Priority::None as i32;
        assert_eq!(
            value(&with(none, None), "priority"),
            Ok(V::Priority(core::Priority::None))
        );
        assert_eq!(
            value(&with(99, None), "priority"),
            Err(ConvertError::UnknownEnum("item.priority".into(), 99))
        );
        // PRIORITY_NONE is passed on for core to reject.
        assert_eq!(
            value(&with(0, Some(none)), "local_priority_override"),
            Ok(V::Priority(core::Priority::None))
        );
        assert_eq!(
            value(&with(0, Some(0)), "local_priority_override"),
            Err(ConvertError::Unspecified(
                "item.local_priority_override".into()
            ))
        );
        assert_eq!(
            value(&with(0, Some(7)), "local_priority_override"),
            Err(ConvertError::UnknownEnum(
                "item.local_priority_override".into(),
                7
            ))
        );
    }

    #[test]
    fn waiting_since_defaults_to_today() {
        let on = v1::PersonRef::from(&PersonRef::FreeText("Dana".into()));
        let item = v1::Item {
            waiting: Some(v1::Waiting {
                on: Some(on),
                reason: "quote".into(),
                since: None,
                follow_up_on: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            value(&item, "waiting"),
            Ok(V::Waiting(core::Waiting {
                on: PersonRef::FreeText("Dana".into()),
                reason: "quote".into(),
                since: TODAY,
                follow_up_on: None,
            }))
        );
        // Only for the whole waiting.
        assert_eq!(value(&item, "waiting.since"), Ok(V::Cleared));

        let item = v1::Item {
            waiting: Some(v1::Waiting::default()),
            ..Default::default()
        };
        assert_eq!(
            value(&item, "waiting"),
            Err(ConvertError::Missing("item.waiting.on".into()))
        );
    }

    #[test]
    fn nested_waiting_paths_read_the_leaf() {
        let item = v1::Item {
            waiting: Some(v1::Waiting::default()),
            ..Default::default()
        };
        let rows = [
            ("waiting.on", V::Cleared),
            ("waiting.reason", V::String(String::new())),
            ("waiting.since", V::Cleared),
            ("waiting.follow_up_on", V::Cleared),
        ];
        for (path, want) in rows {
            assert_eq!(value(&item, path), Ok(want), "{path}");
        }
    }

    #[test]
    fn value_errors_name_the_item_field() {
        let bad_date = gtype::Date {
            year: 2026,
            month: 2,
            day: 30,
        };
        let bad_person = v1::PersonRef {
            r#ref: Some(v1::person_ref::Ref::PersonId("nope".into())),
        };
        let rows: Vec<(&str, v1::Item, &str)> = vec![
            (
                "due_date",
                v1::Item {
                    due_date: Some(bad_date),
                    ..Default::default()
                },
                "item.due_date",
            ),
            (
                "due_date",
                v1::Item {
                    // A year-less date.
                    due_date: Some(gtype::Date {
                        year: 0,
                        month: 10,
                        day: 24,
                    }),
                    ..Default::default()
                },
                "item.due_date",
            ),
            (
                "defer_until",
                v1::Item {
                    defer_until: Some(bad_date),
                    ..Default::default()
                },
                "item.defer_until",
            ),
            (
                "parent_id",
                v1::Item {
                    parent_id: Some("nope".into()),
                    ..Default::default()
                },
                "item.parent_id",
            ),
            (
                "assignee",
                v1::Item {
                    assignee: Some(v1::PersonRef::default()),
                    ..Default::default()
                },
                "item.assignee",
            ),
            (
                "assignee",
                v1::Item {
                    assignee: Some(bad_person.clone()),
                    ..Default::default()
                },
                "item.assignee",
            ),
            (
                "original_estimate",
                v1::Item {
                    original_estimate: Some(prost_types::Duration {
                        seconds: 1,
                        nanos: -1,
                    }),
                    ..Default::default()
                },
                "item.original_estimate",
            ),
            (
                "focus_week",
                v1::Item {
                    focus_week: Some("2026-W54".into()),
                    ..Default::default()
                },
                "item.focus_week",
            ),
            (
                "waiting",
                v1::Item {
                    waiting: Some(v1::Waiting {
                        since: Some(bad_date),
                        ..v1::Waiting::from(&waiting())
                    }),
                    ..Default::default()
                },
                "item.waiting.since",
            ),
            (
                "waiting.on",
                v1::Item {
                    waiting: Some(v1::Waiting {
                        on: Some(bad_person),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "item.waiting.on",
            ),
            (
                "waiting.follow_up_on",
                v1::Item {
                    waiting: Some(v1::Waiting {
                        follow_up_on: Some(bad_date),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "item.waiting.follow_up_on",
            ),
        ];
        for (path, item, want) in rows {
            let err = value(&item, path).unwrap_err();
            assert_eq!(err.path(), want, "{path}: {err}");
        }
        // Free text is left for core to accept or reject.
        let item = v1::Item {
            assignee: Some((&PersonRef::FreeText("Dana".into())).into()),
            ..Default::default()
        };
        assert_eq!(
            value(&item, "assignee"),
            Ok(V::Person(PersonRef::FreeText("Dana".into())))
        );
    }

    #[test]
    fn mask_errors() {
        let item = full_item();
        for paths in [
            &[][..],
            &["*"],
            &["title", "*"],
            &["bogus"],
            &["title", "Title"],
            &["waiting."],
            &[""],
            &["item.title"],
        ] {
            let err = updates(&item, paths).unwrap_err();
            assert!(
                matches!(&err, ConvertError::Invalid(f, _) if f == "update_mask"),
                "{paths:?}: {err}"
            );
        }
        assert_eq!(
            updates(&item, &[]).unwrap_err().to_string(),
            "update_mask: must not be empty"
        );
        assert_eq!(
            updates(&item, &["title", "bogus"]).unwrap_err().to_string(),
            "update_mask: unknown field path \"bogus\""
        );
    }

    #[test]
    fn relations_change_only_through_their_rpcs() {
        let err = updates(&full_item(), &["title", "relations"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "item.relations: changes only through AddRelation and RemoveRelation"
        );
        let mut req = update_request();
        req.update_mask = Some(mask(&["relations"]));
        let m = v1::Mutation {
            kind: Some(Kind::UpdateItem(req)),
        };
        let err = mutation_from_proto(&m, TODAY).unwrap_err();
        assert_eq!(err.path(), "update_item.item.relations");
        // A path into a repeated field is unknown, as anywhere else.
        let err = updates(&full_item(), &["relations.kind"]).unwrap_err();
        assert!(
            matches!(&err, ConvertError::Invalid(f, _) if f == "update_mask"),
            "{err}"
        );
    }

    #[test]
    fn mask_order_is_kept() {
        let paths = ["due_date", "title", "waiting.reason", "priority", "title"];
        let fields: Vec<_> = updates(&full_item(), &paths)
            .unwrap()
            .into_iter()
            .map(|u| u.field)
            .collect();
        // Repeats are core's to reject.
        assert_eq!(
            fields,
            [
                F::DueDate,
                F::Title,
                F::WaitingReason,
                F::Priority,
                F::Title
            ]
        );
    }

    /// Core rejects an update from its first repeated field on, so values
    /// from there are not read: repeating a large field costs nothing.
    #[test]
    fn values_after_a_repeat_are_not_read() {
        let description = "x".repeat(1 << 16);
        let item = v1::Item {
            description: description.clone(),
            due_date: Some(gtype::Date::default()),
            ..full_item()
        };
        let mut paths = vec!["title", "description", "waiting.reason", "title"];
        paths.extend(["description"; 10_000]);
        paths.push("due_date");
        let got = updates(&item, &paths).unwrap();
        assert_eq!(got.len(), paths.len());
        let fields: Vec<_> = got.iter().map(|u| item_field_path(u.field)).collect();
        assert_eq!(fields, paths);
        assert_eq!(got[0].value, V::String("Fix flaky test".into()));
        assert_eq!(got[1].value, V::Markdown(description));
        assert_eq!(got[2].value, V::String("review".into()));
        assert!(got[3..].iter().all(|u| u.value == V::Cleared));
        assert_eq!(
            apply(&core_item(), got),
            Err(core::MutationError::Duplicate(F::Title))
        );
        // Mask errors after a repeat are still reported.
        assert_eq!(
            updates(&item, &["title", "title", "bogus"])
                .unwrap_err()
                .path(),
            "update_mask"
        );
    }

    fn update_request() -> v1::UpdateItemRequest {
        v1::UpdateItemRequest {
            request_id: String::new(),
            item: Some(full_item()),
            update_mask: Some(mask(&["title", "due_date"])),
            expected_revision: Some(7),
        }
    }

    #[test]
    fn update_item_requests() {
        assert_eq!(
            update_item_from_proto(&update_request(), TODAY),
            Ok(core::Mutation {
                id: id(1),
                expected_revision: Some(7),
                op: MutationOp::Update(vec![
                    FieldUpdate {
                        field: F::Title,
                        value: V::String("Fix flaky test".into()),
                    },
                    FieldUpdate {
                        field: F::DueDate,
                        value: V::Date(date(2026, 11, 2)),
                    },
                ]),
            })
        );
        let mut req = update_request();
        req.expected_revision = None;
        assert_eq!(
            update_item_from_proto(&req, TODAY)
                .unwrap()
                .expected_revision,
            None
        );

        let fails = |edit: fn(&mut v1::UpdateItemRequest)| {
            let mut req = update_request();
            edit(&mut req);
            update_item_from_proto(&req, TODAY).unwrap_err()
        };
        assert_eq!(
            fails(|r| r.item = None),
            ConvertError::Missing("item".into())
        );
        assert_eq!(
            fails(|r| r.item.as_mut().unwrap().id = "nope".into()),
            ConvertError::InvalidId("item.id".into(), "nope".into())
        );
        assert_eq!(
            fails(|r| r.item.as_mut().unwrap().id.clear()),
            ConvertError::InvalidId("item.id".into(), String::new())
        );
        assert_eq!(
            fails(|r| r.update_mask = None),
            ConvertError::Missing("update_mask".into())
        );
        assert_eq!(
            fails(|r| r.update_mask = Some(mask(&[]))).path(),
            "update_mask"
        );
        let err = fails(|r| r.item.as_mut().unwrap().due_date = Some(gtype::Date::default()));
        assert_eq!(err.path(), "item.due_date");
        assert_eq!(
            err.within("mutations[3]").path(),
            "mutations[3].item.due_date"
        );
    }

    #[test]
    fn set_status_requests() {
        let req = |id: &str, status: i32| v1::SetStatusRequest {
            request_id: String::new(),
            id: id.to_owned(),
            status,
            expected_revision: Some(4),
        };
        let good = id(1).to_string();
        let statuses = [
            (v1::Status::Inbox, core::Status::Inbox),
            (v1::Status::Next, core::Status::Next),
            (v1::Status::Someday, core::Status::Someday),
            (v1::Status::InProgress, core::Status::InProgress),
            (v1::Status::Done, core::Status::Done),
            (v1::Status::Cancelled, core::Status::Cancelled),
        ];
        for (p, c) in statuses {
            assert_eq!(
                set_status_from_proto(&req(&good, p as i32)),
                Ok(core::Mutation {
                    id: id(1),
                    expected_revision: Some(4),
                    op: MutationOp::SetStatus(c),
                })
            );
        }
        let done = v1::Status::Done as i32;
        assert_eq!(
            set_status_from_proto(&req("nope", done)),
            Err(ConvertError::InvalidId("id".into(), "nope".into()))
        );
        assert_eq!(
            set_status_from_proto(&req(&good, 0)),
            Err(ConvertError::Unspecified("status".into()))
        );
        assert_eq!(
            set_status_from_proto(&req(&good, 99)),
            Err(ConvertError::UnknownEnum("status".into(), 99))
        );
    }

    #[test]
    fn item_op_requests() {
        type Convert = fn(&str, Option<u64>) -> Result<core::Mutation>;
        let rows: [(Convert, MutationOp); 4] = [
            (
                |id, expected_revision| {
                    archive_item_from_proto(&v1::ArchiveItemRequest {
                        request_id: String::new(),
                        id: id.to_owned(),
                        expected_revision,
                    })
                },
                MutationOp::Archive,
            ),
            (
                |id, expected_revision| {
                    unarchive_item_from_proto(&v1::UnarchiveItemRequest {
                        request_id: String::new(),
                        id: id.to_owned(),
                        expected_revision,
                    })
                },
                MutationOp::Unarchive,
            ),
            (
                |id, expected_revision| {
                    delete_item_from_proto(&v1::DeleteItemRequest {
                        request_id: String::new(),
                        id: id.to_owned(),
                        expected_revision,
                    })
                },
                MutationOp::Delete,
            ),
            (
                |id, expected_revision| {
                    undelete_item_from_proto(&v1::UndeleteItemRequest {
                        request_id: String::new(),
                        id: id.to_owned(),
                        expected_revision,
                    })
                },
                MutationOp::Undelete,
            ),
        ];
        for (convert, op) in rows {
            for expected_revision in [None, Some(9)] {
                assert_eq!(
                    convert(&id(1).to_string(), expected_revision),
                    Ok(core::Mutation {
                        id: id(1),
                        expected_revision,
                        op: op.clone(),
                    })
                );
            }
            assert_eq!(
                convert("nope", None),
                Err(ConvertError::InvalidId("id".into(), "nope".into())),
                "{op:?}"
            );
        }
    }

    /// One of each mutation kind on `id(1)`, with the op it converts to.
    fn batch() -> Vec<(&'static str, Kind, MutationOp)> {
        let item_id = || id(1).to_string();
        let rev = Some(3);
        vec![
            (
                "update_item",
                Kind::UpdateItem(v1::UpdateItemRequest {
                    update_mask: Some(mask(&["title"])),
                    expected_revision: rev,
                    ..update_request()
                }),
                MutationOp::Update(vec![FieldUpdate {
                    field: F::Title,
                    value: V::String("Fix flaky test".into()),
                }]),
            ),
            (
                "set_status",
                Kind::SetStatus(v1::SetStatusRequest {
                    request_id: String::new(),
                    id: item_id(),
                    status: v1::Status::Done as i32,
                    expected_revision: rev,
                }),
                MutationOp::SetStatus(core::Status::Done),
            ),
            (
                "archive_item",
                Kind::ArchiveItem(v1::ArchiveItemRequest {
                    request_id: String::new(),
                    id: item_id(),
                    expected_revision: rev,
                }),
                MutationOp::Archive,
            ),
            (
                "unarchive_item",
                Kind::UnarchiveItem(v1::UnarchiveItemRequest {
                    request_id: String::new(),
                    id: item_id(),
                    expected_revision: rev,
                }),
                MutationOp::Unarchive,
            ),
            (
                "delete_item",
                Kind::DeleteItem(v1::DeleteItemRequest {
                    request_id: String::new(),
                    id: item_id(),
                    expected_revision: rev,
                }),
                MutationOp::Delete,
            ),
            (
                "undelete_item",
                Kind::UndeleteItem(v1::UndeleteItemRequest {
                    request_id: String::new(),
                    id: item_id(),
                    expected_revision: rev,
                }),
                MutationOp::Undelete,
            ),
        ]
    }

    /// The nested request_id and Item id of a mutation.
    fn ids(kind: &mut Kind) -> (&mut String, &mut String) {
        match kind {
            Kind::UpdateItem(r) => (&mut r.request_id, &mut r.item.as_mut().unwrap().id),
            Kind::SetStatus(r) => (&mut r.request_id, &mut r.id),
            Kind::ArchiveItem(r) => (&mut r.request_id, &mut r.id),
            Kind::UnarchiveItem(r) => (&mut r.request_id, &mut r.id),
            Kind::DeleteItem(r) => (&mut r.request_id, &mut r.id),
            Kind::UndeleteItem(r) => (&mut r.request_id, &mut r.id),
        }
    }

    #[test]
    fn batch_mutations() {
        let convert = |kind: Kind| mutation_from_proto(&v1::Mutation { kind: Some(kind) }, TODAY);
        for (case, kind, op) in batch() {
            assert_eq!(
                convert(kind.clone()),
                Ok(core::Mutation {
                    id: id(1),
                    expected_revision: Some(3),
                    op,
                }),
                "{case}"
            );

            let mut nested = kind.clone();
            *ids(&mut nested).0 = todo_core::new_ulid().to_string();
            let err = convert(nested).unwrap_err();
            assert!(matches!(err, ConvertError::Invalid(..)), "{err}");
            assert_eq!(err.path(), format!("{case}.request_id"));

            let mut bad_id = kind;
            *ids(&mut bad_id).1 = "nope".into();
            let want = match case {
                "update_item" => "update_item.item.id".to_owned(),
                _ => format!("{case}.id"),
            };
            assert_eq!(
                convert(bad_id),
                Err(ConvertError::InvalidId(want.into(), "nope".into()))
            );
        }
        assert_eq!(
            mutation_from_proto(&v1::Mutation { kind: None }, TODAY),
            Err(ConvertError::Missing("kind".into()))
        );
    }

    #[test]
    fn batch_errors_name_the_case() {
        let mut req = update_request();
        req.update_mask = Some(mask(&["title", "focus_week"]));
        req.item.as_mut().unwrap().focus_week = Some("soon".into());
        let m = v1::Mutation {
            kind: Some(Kind::UpdateItem(req)),
        };
        let err = mutation_from_proto(&m, TODAY).unwrap_err();
        assert_eq!(err.path(), "update_item.item.focus_week");
        assert_eq!(
            err.within("mutations[3]").to_string(),
            "mutations[3].update_item.item.focus_week: \"soon\" is not an ISO week (YYYY-Www)"
        );

        let m = v1::Mutation {
            kind: Some(Kind::SetStatus(v1::SetStatusRequest {
                id: id(1).to_string(),
                ..Default::default()
            })),
        };
        assert_eq!(
            mutation_from_proto(&m, TODAY),
            Err(ConvertError::Unspecified("set_status.status".into()))
        );

        let m = v1::Mutation {
            kind: Some(Kind::UpdateItem(v1::UpdateItemRequest::default())),
        };
        assert_eq!(
            mutation_from_proto(&m, TODAY),
            Err(ConvertError::Missing("update_item.item".into()))
        );

        // A batched update also defaults waiting.since to today.
        let mut req = update_request();
        req.item.as_mut().unwrap().waiting.as_mut().unwrap().since = None;
        req.update_mask = Some(mask(&["waiting"]));
        let m = v1::Mutation {
            kind: Some(Kind::UpdateItem(req)),
        };
        let MutationOp::Update(got) = mutation_from_proto(&m, TODAY).unwrap().op else {
            panic!("not an update");
        };
        assert_eq!(
            got[0].value,
            V::Waiting(core::Waiting {
                since: TODAY,
                ..waiting()
            })
        );
    }

    #[test]
    fn relation_requests() {
        use todo_core::RelationKind as RK;
        let wire = |from: Ulid, to: Ulid, kind: v1::RelationKind| v1::Relation {
            from_id: from.to_string().to_lowercase(),
            to_id: to.to_string(),
            kind: kind as i32,
        };
        assert_eq!(
            relation_from_proto(Some(&wire(id(2), id(1), v1::RelationKind::Blocks))),
            Ok(core::Relation::new(id(2), id(1), RK::Blocks).unwrap())
        );
        // RELATES_TO named either way round is the same relation.
        let relates = core::Relation::new(id(1), id(2), RK::RelatesTo).unwrap();
        for (from, to) in [(id(1), id(2)), (id(2), id(1))] {
            let got = relation_from_proto(Some(&wire(from, to, v1::RelationKind::RelatesTo)));
            assert_eq!(got, Ok(relates));
        }

        let good = wire(id(1), id(2), v1::RelationKind::Duplicates);
        let fails = |r: v1::Relation| relation_from_proto(Some(&r)).unwrap_err();
        assert_eq!(
            relation_from_proto(None),
            Err(ConvertError::Missing("relation".into()))
        );
        assert_eq!(
            fails(v1::Relation {
                from_id: "nope".into(),
                ..good.clone()
            }),
            ConvertError::InvalidId("relation.from_id".into(), "nope".into())
        );
        assert_eq!(
            fails(v1::Relation {
                to_id: String::new(),
                ..good.clone()
            }),
            ConvertError::InvalidId("relation.to_id".into(), String::new())
        );
        assert_eq!(
            fails(v1::Relation {
                kind: 0,
                ..good.clone()
            }),
            ConvertError::Unspecified("relation.kind".into())
        );
        assert_eq!(
            fails(v1::Relation {
                kind: 9,
                ..good.clone()
            }),
            ConvertError::UnknownEnum("relation.kind".into(), 9)
        );
        // An Item related to itself, in any spelling of its id.
        assert_eq!(
            fails(v1::Relation {
                to_id: id(1).to_string(),
                ..good
            }),
            ConvertError::Invalid("relation.to_id".into(), "must differ from from_id".into())
        );
    }
}
