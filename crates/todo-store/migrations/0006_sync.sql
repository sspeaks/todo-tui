-- P2: the sync engine's own state (design 5.3, 5.4, ADR-0006). Forward
-- only: databases at version 5 exist.

-- Per (instance, scope): the highest remote_updated_at a pull has seen and
-- committed, in nanoseconds (design 5.3). Pulls ask for updated >= cursor
-- minus an overlap; the cursor moves only after a page is committed.
CREATE TABLE sync_cursors (
    instance_id TEXT NOT NULL,
    scope       TEXT NOT NULL,
    cursor_at   INTEGER NOT NULL,
    PRIMARY KEY (instance_id, scope)
) STRICT;
