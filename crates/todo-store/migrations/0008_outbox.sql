-- P2 M10: pushes to the tracker (design 5.6, ADR-0007), and the Suggestions
-- sync leaves when Jira wins (design 3.11). Forward only: databases at
-- version 7 exist.

-- One queued change of a linked Item: a field (kind 'update', one row per
-- Item and field while it waits) or its status (kind 'transition', one per
-- Item). `base` is the field's value when the first queued edit was made,
-- which until then only a pull had written: Jira's. `new` is what the push
-- sets; JSON in the events' codec.
CREATE TABLE outbox (
    id              TEXT PRIMARY KEY,
    item_id         TEXT NOT NULL REFERENCES items (id),
    instance_id     TEXT NOT NULL,
    -- The op that queued it last; none after an import without history.
    op_id           TEXT REFERENCES ops (id),
    kind            TEXT NOT NULL CHECK (kind IN ('update', 'transition')),
    field           INTEGER NOT NULL,
    base_json       TEXT NOT NULL,
    new_json        TEXT NOT NULL,
    state           TEXT NOT NULL
                    CHECK (state IN ('pending', 'in_flight', 'done', 'failed', 'cancelled')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER,
    last_error      TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
) STRICT;

-- At most one waiting row per Item and field: a later edit replaces it.
CREATE UNIQUE INDEX outbox_waiting ON outbox (item_id, field)
    WHERE state IN ('pending', 'in_flight');
CREATE INDEX outbox_due ON outbox (state, next_attempt_at);

-- A proposed change never applied without a person (design 3.11). M10
-- writes the sync kinds only.
CREATE TABLE suggestions (
    id         TEXT PRIMARY KEY,
    item_id    TEXT NOT NULL REFERENCES items (id),
    kind       TEXT NOT NULL CHECK (kind IN ('sync_conflict', 'push_failed')),
    source     TEXT NOT NULL CHECK (source IN ('sync')),
    rationale  TEXT NOT NULL,
    error      TEXT,
    state      TEXT NOT NULL
               CHECK (state IN ('pending', 'applied', 'partially_applied', 'rejected', 'expired')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX suggestions_pending ON suggestions (item_id) WHERE state = 'pending';

-- One checkbox: set `field` to `new` if it still holds `expected_old`.
CREATE TABLE suggestion_patches (
    id                 TEXT PRIMARY KEY,
    suggestion_id      TEXT NOT NULL REFERENCES suggestions (id),
    ord                INTEGER NOT NULL,
    field              INTEGER NOT NULL,
    expected_old_json  TEXT NOT NULL,
    new_json           TEXT NOT NULL,
    state              TEXT NOT NULL CHECK (state IN ('pending', 'applied', 'rejected', 'stale'))
) STRICT;
