# ADR-0003: The repeat rule lives on the Item

- Status: accepted
- Date: 2026-10-29
- Amends: design 3.2 (the `recurrence` field), 3.8 (recurrence), 4.3 (services),
  4.4 (change stream), 7.2 (schema), 7.4 (export), 9.1 (P1's services)

## Context

Design 3.8 put a recurrence rule on a local **series** record, with the instances
pointing at it through `Item.recurrence.series_id`. Around that record the design
had a `RecurrenceService` for series CRUD (4.3), a `RecurrenceSeries` resource in
`Change` (field 14) on a `series` topic (4.4), a `recurrence_series` table (7.2) and
a `series` record in the export, between people and items (7.4).

Undo works per op on the events of that op (3.10), and `events.item_id` references
`items (id)` (`0001_init.sql`): an event can only describe a change to an Item. A
change to a separate series record would have no event, so creating a series,
editing its rule or ending it could never be undone, and an op that completed an
instance and advanced its series would undo only half of what it did. Every other
change a user makes is one `u` away.

## Decision

**The rule lives on the Item.** `ItemRecurrence` is
`{ series_id, occurrence_date, rule }` (`proto/todo/v1/item.proto`,
`todo_core::ItemRecurrence`), with `RecurrenceRule` a oneof of `daily`, `weekly`
(our own `Weekday` enum, Monday first), `monthly` and `after_completion`. A
**series** is the Items sharing a `series_id`; there is no series record, table,
service or topic.

- `SetRecurrence` and `ParseRecurrence` are `ItemService` RPCs
  (`proto/todo/v1/item_service.proto`). `SetRecurrence` is also a `BatchMutate`
  mutation (`Mutation.set_recurrence = 7`) and its own op kind
  (`OP_KIND_SET_RECURRENCE = 13`). A capture's `@` token sets the rule inside the
  capture op.
- Setting, changing and clearing a rule is `MutationOp::SetRecurrence`, which writes
  ordinary `set` events on `recurrence` and `due_date`; `restore_field` restores
  `recurrence`, so undo and redo need nothing new. `recurrence` stays read-only to
  `UpdateItem`.
- Completing an instance creates the next one in the same op, as a post-pass of
  `mutate_tx`. The new Item gets a `create` event (`NewEvent::Create`), whose
  inverse is a soft delete (`todo_core::inverse`, `EventBody::Create`), and redo
  undeletes it. The op's actor stays `user`, so complete + generate is one undo
  entry: `u` reopens the completed instance and removes the next one.

## Alternatives considered

- **Keep the series record and give it its own events.** Undo would have to restore
  a second kind of resource: `events.item_id` widened to a resource id, a second set
  of restores and "changed since" checks, and ops that span two resource kinds. All
  of that, for one field, when the Item already carries the overlay it could live in.
- **Keep the series record and leave its changes out of the history**, as the
  daemon's own changes are. `u` after setting a rule would then undo whatever came
  before it, and undoing a completion would reopen the instance but leave the series
  advanced.
- **Generate instances ahead of time, or from the scheduler.** Future instances
  would fill the views and the search, and a scheduler op has actor `system`, so it
  is never an undo entry (3.10): the completion and its next instance would be two
  steps, one of them not undoable.
- **Reuse one Item**: on completion, reopen it and move its due date. No new Item is
  needed, but the completed instance never shows as done or in Archive, and its
  history is overwritten with the next one's.

## Consequences

- No `series` resource in the change stream: series changes are `item` changes.
  `Change` field 14 is `reserved 14; reserved "series";`
  (`proto/todo/v1/watch_service.proto`), and `TOPIC_SERIES` stays in
  `common.proto`, documented as unused. A client subscribed to `TOPIC_ITEM` sees
  everything recurrence does.
- No table: recurrence is the `items` columns `recurrence_series_id` and
  `recurrence_occurrence_date` (from `0001`) and `recurrence_rule`, the JSON of
  `todo_core::recurrence::Rule` (from `0004_recurrence.sql`), with a partial index
  on live series ids for the "another open instance exists" guard. A row with a
  series and no rule (written before M5) reads back with `rule: None` and never
  generates.
- Deleting the open instance (`dd`) ends the series, since no open instance is left
  to complete; `u` undeletes it and the series resumes. Clearing the rule ends it
  too, and is undone like any field.
- No series-level view or edit. Changing the rule changes the open instance; past
  instances keep the rule they had, and the next instance copies the rule of the one
  completed. Listing a series is a filter on `recurrence.series_id`.
- The export (7.4) has no `series` record: each item carries its recurrence.
- In a bulk `SetRecurrence`, each Item without recurrence gets its own new
  `series_id`, filled in by `request.rs` so `apply` stays pure; an Item that already
  recurs keeps its series.
