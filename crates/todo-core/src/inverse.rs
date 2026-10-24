//! The inverse of an op, built from its recorded events (design 3.10).
//!
//! Undo applies the inverse of a stack entry's events; redo applies the
//! inverse of the undo op's own events, so a partial undo gives a matching
//! partial redo. Restores are guarded by the recorded `new` values and write
//! the recorded `old` values exactly, never a fresh `now` (except that the
//! inverse of a capture is a soft delete at `now`).

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;

use crate::field::ItemField;
use crate::ids::ItemId;
use crate::item::StalenessSignals;
use crate::mutation::{FieldRestore, RestoreUnit, STALENESS_SIGNALS, STATUS_TRIPLE};
use crate::relation::Relation;
use crate::value::FieldValue;

/// One event of an op, as the store reads it back.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedEvent {
    /// For relation events, the relation's `from` end.
    pub item_id: ItemId,
    pub body: EventBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventBody {
    /// The Item was captured.
    Create,
    /// One [`FieldChange`](crate::FieldChange).
    Set {
        field: ItemField,
        old: FieldValue,
        new: FieldValue,
    },
    /// The Item's staleness signals before and after the op.
    Derived {
        old: StalenessSignals,
        new: StalenessSignals,
    },
    RelationAdd(Relation),
    RelationRemove(Relation),
}

/// One step of an inverse. The store applies the steps in order, restores
/// through [`MutationOp::Restore`](crate::MutationOp::Restore).
#[derive(Debug, Clone, PartialEq)]
pub enum InverseStep {
    Restore { item_id: ItemId, unit: RestoreUnit },
    AddRelation(Relation),
    RemoveRelation(Relation),
}

/// The inverse of `events` (in op order), in reverse event order:
/// - `Set` restores `old` where the field still holds `new`. A run of `Set`
///   events of one Item on the status triple is one unit, in event order.
/// - `Create` soft-deletes at `now`, if the Item is still live.
/// - `Derived` restores each staleness signal that differs, one unit each.
///   Restores skip on a deleted Item, so when the inverse also deletes or
///   undeletes that Item, its staleness units move to where it is live: just
///   before the first step that deletes it, or just after the first step
///   that undeletes it. Where the store records the `Derived` event among
///   the Item's events therefore does not matter.
/// - A relation add is removed, and a removed relation added back.
///
/// Events do not record where one mutation ends: a status run is the
/// longest run in `SetStatus` field order (category, resolution,
/// refinement), so two `SetStatus` of one Item in one op are two units
/// unless the second only touches fields after the first's. Merging them
/// only makes the undo coarser. `Restore` records a status unit's changes in
/// the unit's order, so redo groups them the same way.
pub fn inverse(events: &[RecordedEvent], now: Timestamp) -> Vec<InverseStep> {
    let steps: Vec<InverseStep> = events
        .chunk_by(in_one_status_run)
        .rev()
        .flat_map(|run| invert(run, now))
        .collect();
    let toggled: HashSet<ItemId> = steps
        .iter()
        .filter_map(|step| deleted_at(step).map(|(item_id, _)| item_id))
        .collect();
    let len = steps.len();
    let mut signals: HashMap<ItemId, Vec<InverseStep>> = HashMap::new();
    let mut rest = Vec::with_capacity(len);
    for step in steps {
        match staleness_of(&step) {
            Some(item_id) if toggled.contains(&item_id) => {
                signals.entry(item_id).or_default().push(step);
            }
            _ => rest.push(step),
        }
    }
    let mut placed = Vec::with_capacity(len);
    for step in rest {
        let Some((item_id, deletes)) = deleted_at(&step) else {
            placed.push(step);
            continue;
        };
        let mine = signals.remove(&item_id).unwrap_or_default();
        if deletes {
            placed.extend(mine);
            placed.push(step);
        } else {
            placed.push(step);
            placed.extend(mine);
        }
    }
    placed
}

/// The Item of a step that restores its `deleted_at`, and whether the step
/// deletes it (rather than undeleting it).
fn deleted_at(step: &InverseStep) -> Option<(ItemId, bool)> {
    let InverseStep::Restore { item_id, unit } = step else {
        return None;
    };
    let restore = unit
        .restores
        .iter()
        .find(|r| r.field == ItemField::DeletedAt)?;
    Some((*item_id, restore.value != FieldValue::Cleared))
}

/// The Item of a step that restores only staleness signals.
fn staleness_of(step: &InverseStep) -> Option<ItemId> {
    let InverseStep::Restore { item_id, unit } = step else {
        return None;
    };
    let signals_only = !unit.restores.is_empty()
        && unit
            .restores
            .iter()
            .all(|r| STALENESS_SIGNALS.contains(&r.field));
    signals_only.then_some(*item_id)
}

/// Whether `b` continues `a`'s status run.
fn in_one_status_run(a: &RecordedEvent, b: &RecordedEvent) -> bool {
    let position = |e: &RecordedEvent| match e.body {
        EventBody::Set { field, .. } => STATUS_TRIPLE.iter().position(|&f| f == field),
        _ => None,
    };
    match (position(a), position(b)) {
        (Some(a_at), Some(b_at)) => a.item_id == b.item_id && a_at < b_at,
        _ => false,
    }
}

/// The steps for one event, or for one status run of `Set` events.
fn invert(run: &[RecordedEvent], now: Timestamp) -> Vec<InverseStep> {
    let item_id = run[0].item_id;
    let restore = |restores| InverseStep::Restore {
        item_id,
        unit: RestoreUnit { restores },
    };
    match &run[0].body {
        EventBody::Set { .. } => {
            let restores = run
                .iter()
                .filter_map(|e| match &e.body {
                    EventBody::Set { field, old, new } => Some(FieldRestore {
                        field: *field,
                        expected: new.clone(),
                        value: old.clone(),
                    }),
                    _ => None,
                })
                .collect();
            vec![restore(restores)]
        }
        EventBody::Create => vec![restore(vec![FieldRestore {
            field: ItemField::DeletedAt,
            expected: FieldValue::Cleared,
            value: FieldValue::Timestamp(now),
        }])],
        EventBody::Derived { old, new } => signals(old)
            .zip(signals(new))
            .filter(|((_, old), (_, new))| old != new)
            .map(|((field, old), (_, new))| {
                restore(vec![FieldRestore {
                    field,
                    expected: timestamp(new),
                    value: timestamp(old),
                }])
            })
            .collect(),
        EventBody::RelationAdd(relation) => vec![InverseStep::RemoveRelation(*relation)],
        EventBody::RelationRemove(relation) => vec![InverseStep::AddRelation(*relation)],
    }
}

/// Each signal with its nested field.
fn signals(s: &StalenessSignals) -> impl Iterator<Item = (ItemField, Option<Timestamp>)> {
    STALENESS_SIGNALS.into_iter().zip([
        s.last_user_touch_at,
        s.last_remote_change_at,
        s.status_entered_at,
        s.waiting_since,
    ])
}

fn timestamp(at: Option<Timestamp>) -> FieldValue {
    at.map_or(FieldValue::Cleared, FieldValue::Timestamp)
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use ulid::Ulid;

    use super::*;
    use crate::item::{ArchiveReason, Archived, CreatedVia, Item, PersonRef, Priority, Waiting};
    use crate::mutation::{
        Actor, Applied, ApplyCtx, FieldUpdate, MutationOp, RestoreSkip, SkipReason, apply,
    };
    use crate::relation::RelationKind;
    use crate::status::{ResolutionKind, Status, StatusCategory, TodoRefinement};

    use FieldValue as V;
    use ItemField as F;

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    fn set(item: u128, field: ItemField, old: FieldValue, new: FieldValue) -> RecordedEvent {
        RecordedEvent {
            item_id: id(item),
            body: EventBody::Set { field, old, new },
        }
    }

    fn unit(item: u128, restores: &[(ItemField, FieldValue, FieldValue)]) -> InverseStep {
        InverseStep::Restore {
            item_id: id(item),
            unit: RestoreUnit {
                restores: restores
                    .iter()
                    .map(|(field, expected, value)| FieldRestore {
                        field: *field,
                        expected: expected.clone(),
                        value: value.clone(),
                    })
                    .collect(),
            },
        }
    }

    fn title(s: &str) -> FieldValue {
        V::String(s.into())
    }

    fn category(c: StatusCategory) -> FieldValue {
        V::StatusCategory(c)
    }

    fn refinement(t: TodoRefinement) -> FieldValue {
        V::TodoRefinement(t)
    }

    #[test]
    fn steps_come_in_reverse_event_order() {
        let events = [
            set(1, F::Title, title("a"), title("b")),
            set(
                2,
                F::Priority,
                V::Priority(Priority::None),
                V::Priority(Priority::High),
            ),
            set(1, F::Title, title("b"), title("c")),
            set(1, F::DeletedAt, V::Cleared, V::Timestamp(ts(10))),
        ];
        assert_eq!(
            inverse(&events, ts(99)),
            [
                unit(1, &[(F::DeletedAt, V::Timestamp(ts(10)), V::Cleared)]),
                unit(1, &[(F::Title, title("c"), title("b"))]),
                unit(
                    2,
                    &[(
                        F::Priority,
                        V::Priority(Priority::High),
                        V::Priority(Priority::None)
                    )]
                ),
                unit(1, &[(F::Title, title("b"), title("a"))]),
            ]
        );
        assert_eq!(inverse(&[], ts(99)), []);
    }

    #[test]
    fn status_runs_are_one_unit_each() {
        use StatusCategory as C;
        use TodoRefinement as T;
        let done = V::ResolutionKind(ResolutionKind::Done);
        // SetStatus(next), then SetStatus(done), of Item 1 in one op.
        let events = [
            set(
                1,
                F::TodoRefinement,
                refinement(T::Inbox),
                refinement(T::Next),
            ),
            set(1, F::StatusCategory, category(C::Todo), category(C::Done)),
            set(1, F::ResolutionKind, V::Cleared, done.clone()),
        ];
        assert_eq!(
            inverse(&events, ts(99)),
            [
                unit(
                    1,
                    &[
                        (F::StatusCategory, category(C::Done), category(C::Todo)),
                        (F::ResolutionKind, done.clone(), V::Cleared),
                    ]
                ),
                unit(
                    1,
                    &[(F::TodoRefinement, refinement(T::Next), refinement(T::Inbox))]
                ),
            ]
        );

        // Runs end at another Item, at a non-status event, and on a repeat.
        let events = [
            set(
                1,
                F::StatusCategory,
                category(C::Todo),
                category(C::InProgress),
            ),
            set(
                2,
                F::StatusCategory,
                category(C::Todo),
                category(C::InProgress),
            ),
            set(2, F::Title, title("a"), title("b")),
            set(
                2,
                F::TodoRefinement,
                refinement(T::Inbox),
                refinement(T::Next),
            ),
            set(
                2,
                F::TodoRefinement,
                refinement(T::Next),
                refinement(T::Someday),
            ),
        ];
        assert_eq!(inverse(&events, ts(99)).len(), 5);

        // One SetStatus touching all three fields.
        let events = [
            set(3, F::StatusCategory, category(C::Done), category(C::Todo)),
            set(3, F::ResolutionKind, done.clone(), V::Cleared),
            set(
                3,
                F::TodoRefinement,
                refinement(T::Next),
                refinement(T::Someday),
            ),
        ];
        let steps = inverse(&events, ts(99));
        assert_eq!(steps.len(), 1);
        let InverseStep::Restore { unit, .. } = &steps[0] else {
            panic!("{steps:?}");
        };
        let fields: Vec<_> = unit.restores.iter().map(|r| r.field).collect();
        assert_eq!(fields, STATUS_TRIPLE);
    }

    #[test]
    fn create_inverts_to_a_soft_delete_at_now() {
        let events = [RecordedEvent {
            item_id: id(1),
            body: EventBody::Create,
        }];
        assert_eq!(
            inverse(&events, ts(99)),
            [unit(1, &[(F::DeletedAt, V::Cleared, V::Timestamp(ts(99)))])]
        );
    }

    #[test]
    fn derived_restores_each_changed_signal() {
        let old = StalenessSignals {
            last_user_touch_at: Some(ts(1)),
            last_remote_change_at: Some(ts(2)),
            status_entered_at: None,
            waiting_since: Some(ts(4)),
        };
        let new = StalenessSignals {
            last_user_touch_at: Some(ts(10)),
            last_remote_change_at: Some(ts(2)),
            status_entered_at: Some(ts(10)),
            waiting_since: None,
        };
        let events = [RecordedEvent {
            item_id: id(1),
            body: EventBody::Derived { old, new },
        }];
        assert_eq!(
            inverse(&events, ts(99)),
            [
                unit(
                    1,
                    &[(
                        F::StalenessLastUserTouchAt,
                        V::Timestamp(ts(10)),
                        V::Timestamp(ts(1))
                    )]
                ),
                unit(
                    1,
                    &[(
                        F::StalenessStatusEnteredAt,
                        V::Timestamp(ts(10)),
                        V::Cleared
                    )]
                ),
                unit(
                    1,
                    &[(F::StalenessWaitingSince, V::Cleared, V::Timestamp(ts(4)))]
                ),
            ]
        );
        let same = [RecordedEvent {
            item_id: id(1),
            body: EventBody::Derived { old, new: old },
        }];
        assert_eq!(inverse(&same, ts(99)), []);
    }

    /// Staleness units land where the Item is live, wherever the store put
    /// the `Derived` event among the Item's events.
    #[test]
    fn staleness_restores_go_where_the_item_is_live() {
        let touch = |at| StalenessSignals {
            last_user_touch_at: Some(ts(at)),
            ..StalenessSignals::default()
        };
        let derived = |item, old, new| RecordedEvent {
            item_id: id(item),
            body: EventBody::Derived {
                old: touch(old),
                new: touch(new),
            },
        };
        let signal = |item, expected, value| {
            unit(
                item,
                &[(
                    F::StalenessLastUserTouchAt,
                    V::Timestamp(ts(expected)),
                    V::Timestamp(ts(value)),
                )],
            )
        };
        let rename = |old, new| set(1, F::Title, title(old), title(new));
        let deleted_at = |old, new| set(1, F::DeletedAt, old, new);
        let at_10 = || V::Timestamp(ts(10));
        let create = RecordedEvent {
            item_id: id(1),
            body: EventBody::Create,
        };
        let rows = [
            // Undo of [edit, delete]: the undelete comes first.
            (
                [
                    rename("a", "b"),
                    deleted_at(V::Cleared, at_10()),
                    derived(1, 1, 10),
                ],
                vec![
                    unit(1, &[(F::DeletedAt, at_10(), V::Cleared)]),
                    signal(1, 10, 1),
                    unit(1, &[(F::Title, title("b"), title("a"))]),
                ],
            ),
            // Its redo: the delete comes last.
            (
                [
                    deleted_at(at_10(), V::Cleared),
                    rename("b", "a"),
                    derived(1, 10, 1),
                ],
                vec![
                    unit(1, &[(F::Title, title("a"), title("b"))]),
                    signal(1, 1, 10),
                    unit(1, &[(F::DeletedAt, V::Cleared, at_10())]),
                ],
            ),
            // Undo of [undelete, delete]: live again in between.
            (
                [
                    deleted_at(V::Timestamp(ts(5)), V::Cleared),
                    deleted_at(V::Cleared, at_10()),
                    derived(1, 1, 10),
                ],
                vec![
                    unit(1, &[(F::DeletedAt, at_10(), V::Cleared)]),
                    signal(1, 10, 1),
                    unit(1, &[(F::DeletedAt, V::Cleared, V::Timestamp(ts(5)))]),
                ],
            ),
        ];
        for (events, want) in rows {
            let [a, b, c] = events;
            for events in [[a.clone(), b.clone(), c.clone()], [c, a, b]] {
                assert_eq!(inverse(&events, ts(99)), want, "{events:?}");
            }
        }

        // Undo of a capture deletes last; another Item's signals stay put.
        for events in [
            [create.clone(), derived(1, 1, 10), derived(2, 1, 10)],
            [derived(1, 1, 10), create.clone(), derived(2, 1, 10)],
        ] {
            assert_eq!(
                inverse(&events, ts(99)),
                [
                    signal(2, 10, 1),
                    signal(1, 10, 1),
                    unit(1, &[(F::DeletedAt, V::Cleared, V::Timestamp(ts(99)))]),
                ],
                "{events:?}"
            );
        }
    }

    #[test]
    fn relations_swap_add_and_remove() {
        let blocks = Relation::new(id(1), id(2), RelationKind::Blocks).unwrap();
        let relates = Relation::new(id(3), id(1), RelationKind::RelatesTo).unwrap();
        let events = [
            RecordedEvent {
                item_id: id(1),
                body: EventBody::RelationAdd(blocks),
            },
            RecordedEvent {
                item_id: relates.from(),
                body: EventBody::RelationRemove(relates),
            },
        ];
        assert_eq!(
            inverse(&events, ts(99)),
            [
                InverseStep::AddRelation(relates),
                InverseStep::RemoveRelation(blocks),
            ]
        );
    }

    // ---- round trips through apply ----------------------------------------

    /// What the store would record for an op over one Item: the `Set` events
    /// in order, then one `Derived` event if the signals changed.
    fn record(before: &Item, applied: &[Applied]) -> Vec<RecordedEvent> {
        let after = applied.last().map_or(before, |a| &a.item);
        let mut events: Vec<_> = applied
            .iter()
            .flat_map(|a| &a.changes)
            .map(|c| RecordedEvent {
                item_id: before.id,
                body: EventBody::Set {
                    field: c.field,
                    old: c.old.clone(),
                    new: c.new.clone(),
                },
            })
            .collect();
        if before.derived.staleness != after.derived.staleness {
            events.push(RecordedEvent {
                item_id: before.id,
                body: EventBody::Derived {
                    old: before.derived.staleness,
                    new: after.derived.staleness,
                },
            });
        }
        events
    }

    /// Applies `ops` in sequence as one op, returning the Item and its events.
    fn run(item: &Item, ops: &[MutationOp], ctx: ApplyCtx) -> (Item, Vec<RecordedEvent>) {
        let mut applied: Vec<Applied> = Vec::new();
        for op in ops {
            let current = applied.last().map_or(item, |a| &a.item);
            applied.push(apply(current, op, &ctx).unwrap());
        }
        let events = record(item, &applied);
        (applied.last().map_or(item, |a| &a.item).clone(), events)
    }

    /// Undo or redo of `events` on `item`: the Item, the skips and the events.
    fn undo(
        item: &Item,
        events: &[RecordedEvent],
        now: Timestamp,
    ) -> (Item, Vec<RestoreSkip>, Vec<RecordedEvent>) {
        let ops: Vec<_> = inverse(events, now)
            .into_iter()
            .map(|step| match step {
                InverseStep::Restore { item_id, unit } => {
                    assert_eq!(item_id, item.id);
                    MutationOp::Restore(vec![unit])
                }
                other => panic!("{other:?}"),
            })
            .collect();
        let ctx = ApplyCtx {
            now,
            actor: Actor::User,
        };
        let mut skipped = Vec::new();
        let mut applied = Vec::new();
        for op in &ops {
            let current = applied.last().map_or(item, |a: &Applied| &a.item);
            let a = apply(current, op, &ctx).unwrap();
            skipped.extend(a.skipped.clone());
            applied.push(a);
        }
        let after = applied.last().map_or(item, |a| &a.item).clone();
        (after, skipped, record(item, &applied))
    }

    /// The Item without the write metadata undo does not restore.
    fn content(item: &Item) -> Item {
        Item {
            updated_at: Timestamp::UNIX_EPOCH,
            ..item.clone()
        }
    }

    fn start() -> Item {
        let mut item = Item::new_capture(id(1), "call dentist".into(), CreatedVia::Tui, ts(1_000));
        item.revision = 7;
        item.core.labels = vec!["b".into(), "a".into()];
        item.derived.staleness = StalenessSignals {
            last_user_touch_at: Some(ts(1_000)),
            last_remote_change_at: None,
            status_entered_at: Some(ts(1_000)),
            waiting_since: None,
        };
        item
    }

    fn user(now: Timestamp) -> ApplyCtx {
        ApplyCtx {
            now,
            actor: Actor::User,
        }
    }

    #[test]
    fn undo_then_redo_round_trips() {
        let original = start();
        let waiting = Waiting {
            on: PersonRef::FreeText("the vendor".into()),
            reason: "quote".into(),
            since: date(2026, 10, 20),
            follow_up_on: None,
        };
        let ops = [
            MutationOp::Update(vec![
                FieldUpdate {
                    field: F::Title,
                    value: title("call the dentist"),
                },
                FieldUpdate {
                    field: F::Labels,
                    value: V::StringSet(vec!["c".into()]),
                },
                FieldUpdate {
                    field: F::StoryPoints,
                    value: V::Number(0.1 + 0.2),
                },
                FieldUpdate {
                    field: F::Waiting,
                    value: V::Waiting(waiting),
                },
            ]),
            MutationOp::SetStatus(Status::Next),
            MutationOp::SetStatus(Status::Done),
            MutationOp::Update(vec![FieldUpdate {
                field: F::WaitingReason,
                value: title("contract"),
            }]),
            MutationOp::Archive,
        ];
        let (done, events) = run(&original, &ops, user(ts(2_000)));
        assert_eq!(done.status(), Status::Done);
        assert!(
            events
                .iter()
                .any(|e| matches!(e.body, EventBody::Derived { .. }))
        );

        // Through JSON, as the store keeps the values.
        let events: Vec<_> = events
            .into_iter()
            .map(|e| match e.body {
                EventBody::Set { field, old, new } => {
                    let json = |v: &FieldValue| {
                        serde_json::from_str(&serde_json::to_string(v).unwrap()).unwrap()
                    };
                    set(1, field, json(&old), json(&new))
                }
                _ => e,
            })
            .collect();

        let (undone, skipped, undo_events) = undo(&done, &events, ts(3_000));
        assert_eq!(skipped, []);
        assert_eq!(content(&undone), content(&original));
        assert_eq!(undone.updated_at, ts(3_000));

        let (redone, skipped, redo_events) = undo(&undone, &undo_events, ts(4_000));
        assert_eq!(skipped, []);
        assert_eq!(content(&redone), content(&done));

        // Undo again, from the redo's events.
        let (again, skipped, _) = undo(&redone, &redo_events, ts(5_000));
        assert_eq!(skipped, []);
        assert_eq!(content(&again), content(&original));
    }

    #[test]
    fn undo_of_a_capture_and_its_redo() {
        let item = start();
        let events = [RecordedEvent {
            item_id: item.id,
            body: EventBody::Create,
        }];
        let (deleted, skipped, undo_events) = undo(&item, &events, ts(3_000));
        assert_eq!(skipped, []);
        assert_eq!(deleted.derived.deleted_at, Some(ts(3_000)));
        assert_eq!(deleted.derived.staleness, item.derived.staleness);

        let (back, skipped, _) = undo(&deleted, &undo_events, ts(4_000));
        assert_eq!(skipped, []);
        assert_eq!(content(&back), content(&item));

        // Already deleted: the soft delete no longer matches.
        let (_, skipped, undo_events) = undo(&deleted, &events, ts(5_000));
        assert_eq!(
            skipped,
            [RestoreSkip {
                fields: vec![F::DeletedAt],
                reason: SkipReason::ChangedSince,
            }]
        );
        assert_eq!(undo_events, []);
    }

    #[test]
    fn undo_and_redo_of_a_delete_restore_staleness_and_deleted_at() {
        let mut item = start();
        item.overlay.archived = Some(Archived {
            at: ts(500),
            reason: ArchiveReason::AutoDone,
        });
        let ops = [
            MutationOp::Unarchive,
            MutationOp::Update(vec![FieldUpdate {
                field: F::Title,
                value: title("x"),
            }]),
            MutationOp::Delete,
        ];
        let (deleted, events) = run(&item, &ops, user(ts(2_000)));
        assert_eq!(
            deleted.derived.staleness.last_user_touch_at,
            Some(ts(2_000))
        );

        let (undone, skipped, undo_events) = undo(&deleted, &events, ts(3_000));
        assert_eq!(skipped, []);
        assert_eq!(content(&undone), content(&item));

        let (redone, skipped, _) = undo(&undone, &undo_events, ts(4_000));
        assert_eq!(skipped, []);
        assert_eq!(content(&redone), content(&deleted));
        assert_eq!(redone.derived.deleted_at, Some(ts(2_000)));
    }

    /// Edit X, then an off-stack op deletes X: undoing the edit leaves the
    /// tombstone alone, its staleness signals included, so it writes nothing.
    #[test]
    fn undo_after_the_item_was_deleted_leaves_the_tombstone_alone() {
        let item = start();
        let ops = [MutationOp::Update(vec![FieldUpdate {
            field: F::Title,
            value: title("x"),
        }])];
        let (edited, events) = run(&item, &ops, user(ts(2_000)));
        let system = ApplyCtx {
            now: ts(2_500),
            actor: Actor::System,
        };
        let (deleted, _) = run(&edited, &[MutationOp::Delete], system);

        let (undone, skipped, undo_events) = undo(&deleted, &events, ts(3_000));
        assert_eq!(undone, deleted);
        assert_eq!(undo_events, []);
        assert_eq!(
            skipped,
            [
                RestoreSkip {
                    fields: vec![F::StalenessLastUserTouchAt],
                    reason: SkipReason::ItemDeleted,
                },
                RestoreSkip {
                    fields: vec![F::Title],
                    reason: SkipReason::ItemDeleted,
                },
            ]
        );
    }

    #[test]
    fn a_partial_undo_gives_a_matching_partial_redo() {
        let item = start();
        let ops = [MutationOp::Update(vec![
            FieldUpdate {
                field: F::Title,
                value: title("b"),
            },
            FieldUpdate {
                field: F::Priority,
                value: V::Priority(Priority::High),
            },
        ])];
        let (edited, events) = run(&item, &ops, user(ts(2_000)));
        // A system op changes the title since.
        let system = ApplyCtx {
            now: ts(2_500),
            actor: Actor::System,
        };
        let (changed, _) = run(
            &edited,
            &[MutationOp::Update(vec![FieldUpdate {
                field: F::Title,
                value: title("c"),
            }])],
            system,
        );

        let (undone, skipped, undo_events) = undo(&changed, &events, ts(3_000));
        assert_eq!(
            skipped,
            [RestoreSkip {
                fields: vec![F::Title],
                reason: SkipReason::ChangedSince,
            }]
        );
        assert_eq!(undone.core.title, "c");
        assert_eq!(undone.core.priority, Priority::None);
        assert_eq!(
            undone.derived.staleness.last_user_touch_at,
            item.derived.staleness.last_user_touch_at
        );

        let (redone, skipped, _) = undo(&undone, &undo_events, ts(4_000));
        assert_eq!(skipped, []);
        assert_eq!(redone.core.title, "c");
        assert_eq!(redone.core.priority, Priority::High);
        assert_eq!(redone.derived.staleness, edited.derived.staleness);
    }
}
