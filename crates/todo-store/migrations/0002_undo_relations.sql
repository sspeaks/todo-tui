-- M2: the undo stack (3.10), the order of events within an op, and local
-- relations (3.6). Forward only: databases at version 1 exist.

-- Ops. `seq` orders the stack (ids are not monotonic within a millisecond).
-- `undo_state` is NULL for ops that are not stack entries, which includes
-- every op recorded before this migration: their history is not undoable.
-- Undo and redo ops name the entry they acted on in `entry_op`; `summary` is
-- the toast text and `report` the JSON of the changes they skipped, both kept
-- so a replayed request returns the original response.
ALTER TABLE ops ADD COLUMN seq INTEGER;
UPDATE ops SET seq = rowid;
CREATE UNIQUE INDEX ops_seq ON ops (seq);
ALTER TABLE ops ADD COLUMN undo_state TEXT
    CHECK (undo_state IN ('done', 'undone', 'dropped', 'discarded'));
ALTER TABLE ops ADD COLUMN entry_op TEXT REFERENCES ops (id);
ALTER TABLE ops ADD COLUMN summary TEXT;
ALTER TABLE ops ADD COLUMN report TEXT;
CREATE INDEX ops_stack ON ops (undo_state, seq) WHERE undo_state IS NOT NULL;
CREATE INDEX ops_entry ON ops (entry_op, seq) WHERE entry_op IS NOT NULL;

-- Events. `ord` is the event's place in its op (0, 1, ...), across Items.
-- Kinds: 'create', 'set', 'derived' (field 78, old/new the staleness signals
-- before and after the op), 'relation_add' (new is the relation) and
-- 'relation_remove' (old is the relation), filed under the relation's from_id.
ALTER TABLE events ADD COLUMN ord INTEGER;
UPDATE events SET ord = numbered.ord
FROM (
    SELECT rowid AS event_rowid,
           ROW_NUMBER() OVER (PARTITION BY op_id ORDER BY rowid) - 1 AS ord
    FROM events
) AS numbered
WHERE events.rowid = numbered.event_rowid;
DROP INDEX events_op;
CREATE INDEX events_op ON events (op_id, ord);

-- Relations. The triple is the identity; relates_to is symmetric and stored
-- with from_id < to_id. Kept when an end is deleted.
CREATE TABLE relations (
    from_id    TEXT NOT NULL REFERENCES items (id),
    to_id      TEXT NOT NULL REFERENCES items (id),
    kind       TEXT NOT NULL CHECK (kind IN ('blocks', 'duplicates', 'relates_to')),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (from_id, to_id, kind),
    CHECK (from_id <> to_id),
    CHECK (kind <> 'relates_to' OR from_id < to_id)
) STRICT;
CREATE INDEX relations_to ON relations (to_id);
