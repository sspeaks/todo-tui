-- Initial schema. Databases have applied it: change the schema in a new
-- migration, never here (design 7.2).
-- Encodings: timestamps and durations are INTEGER nanoseconds (Unix epoch for
-- instants); civil dates are TEXT 'YYYY-MM-DD'; enums are snake_case TEXT;
-- string sets and maps are JSON TEXT; ids are ULID TEXT.

CREATE TABLE meta (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    revision INTEGER NOT NULL
) STRICT;
INSERT INTO meta (id, revision) VALUES (1, 0);

CREATE TABLE items (
    -- Stable rowid for items_fts; never exposed.
    seq         INTEGER PRIMARY KEY,
    id          TEXT NOT NULL UNIQUE,
    created_via TEXT NOT NULL CHECK (created_via IN
        ('tui', 'cli', 'mcp', 'remote_import', 'restore', 'split', 'recurrence')),
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    revision    INTEGER NOT NULL,

    -- Remote link (3.3); all NULL when unlinked.
    remote_tracker_kind        TEXT,
    remote_instance_id         TEXT,
    remote_id                  TEXT,
    remote_key                 TEXT,
    remote_url                 TEXT,
    remote_last_pulled_version INTEGER,
    remote_link_state          TEXT CHECK (remote_link_state IN ('pending', 'ok', 'broken')),
    remote_pinned              INTEGER NOT NULL DEFAULT 0,

    -- Core.
    title               TEXT NOT NULL,
    description         TEXT NOT NULL DEFAULT '',
    item_type           TEXT NOT NULL DEFAULT 'task',
    status_category     TEXT NOT NULL CHECK (status_category IN ('todo', 'in_progress', 'done')),
    resolution_kind     TEXT CHECK (resolution_kind IN ('done', 'cancelled')),
    remote_status_name  TEXT,
    priority            TEXT NOT NULL DEFAULT 'none' CHECK (priority IN
        ('none', 'lowest', 'low', 'medium', 'high', 'highest')),
    assignee_person_id  TEXT,
    assignee_text       TEXT,
    reporter_person_id  TEXT,
    reporter_text       TEXT,
    labels              TEXT NOT NULL DEFAULT '[]',
    components          TEXT NOT NULL DEFAULT '[]',
    fix_versions        TEXT NOT NULL DEFAULT '[]',
    due_date            TEXT,
    parent_id           TEXT REFERENCES items (id),
    sprint              TEXT,
    story_points        REAL,
    original_estimate   INTEGER,
    remaining_estimate  INTEGER,
    resolution          TEXT,
    remote_created_at   INTEGER,
    remote_updated_at   INTEGER,

    -- Overlay.
    todo_refinement          TEXT NOT NULL DEFAULT 'inbox' CHECK (todo_refinement IN
        ('inbox', 'next', 'someday')),
    waiting_on_person_id     TEXT,
    waiting_on_text          TEXT,
    waiting_reason           TEXT,
    waiting_since            TEXT,
    waiting_follow_up_on     TEXT,
    defer_until              TEXT,
    private_notes            TEXT NOT NULL DEFAULT '',
    focus_week               TEXT,
    local_priority_override  TEXT CHECK (local_priority_override IN
        ('none', 'lowest', 'low', 'medium', 'high', 'highest')),
    archived_at              INTEGER,
    archived_reason          TEXT CHECK (archived_reason IN
        ('user', 'left_scope', 'remote_unavailable', 'auto_done')),
    recurrence_series_id     TEXT,
    recurrence_occurrence_date TEXT,

    -- Remote extras: JSON object.
    remote_extras TEXT NOT NULL DEFAULT '{}',

    -- Derived and sync state.
    is_mine                    INTEGER NOT NULL DEFAULT 0,
    pending_fields             TEXT NOT NULL DEFAULT '[]',
    has_conflict               INTEGER NOT NULL DEFAULT 0,
    has_push_failure           INTEGER NOT NULL DEFAULT 0,
    description_lossy          INTEGER NOT NULL DEFAULT 0,
    description_dirty          INTEGER NOT NULL DEFAULT 0,
    last_user_touch_at         INTEGER,
    last_remote_change_at      INTEGER,
    status_entered_at          INTEGER,
    staleness_waiting_since    INTEGER,
    scopes                     TEXT NOT NULL DEFAULT '[]',
    deleted_at                 INTEGER,
    merged_into                TEXT REFERENCES items (id),

    -- The derived 6-value status (3.4).
    status TEXT GENERATED ALWAYS AS (
        CASE status_category
            WHEN 'todo' THEN todo_refinement
            WHEN 'in_progress' THEN 'in_progress'
            ELSE CASE resolution_kind WHEN 'cancelled' THEN 'cancelled' ELSE 'done' END
        END
    ) VIRTUAL,

    CHECK ((archived_at IS NULL) = (archived_reason IS NULL)),
    CHECK ((remote_id IS NULL) = (remote_instance_id IS NULL))
);

CREATE INDEX items_created ON items (created_at, id);
CREATE INDEX items_status ON items (status);
CREATE INDEX items_parent ON items (parent_id) WHERE parent_id IS NOT NULL;
CREATE UNIQUE INDEX items_remote ON items (remote_instance_id, remote_id)
    WHERE remote_id IS NOT NULL;

CREATE VIRTUAL TABLE items_fts USING fts5 (
    title, description, remote_key, labels,
    content = 'items', content_rowid = 'seq'
);

CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
    INSERT INTO items_fts (rowid, title, description, remote_key, labels)
    VALUES (new.seq, new.title, new.description, new.remote_key, new.labels);
END;

CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, title, description, remote_key, labels)
    VALUES ('delete', old.seq, old.title, old.description, old.remote_key, old.labels);
END;

CREATE TRIGGER items_fts_au AFTER UPDATE OF title, description, remote_key, labels ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, title, description, remote_key, labels)
    VALUES ('delete', old.seq, old.title, old.description, old.remote_key, old.labels);
    INSERT INTO items_fts (rowid, title, description, remote_key, labels)
    VALUES (new.seq, new.title, new.description, new.remote_key, new.labels);
END;

-- Ops: the unit of undo (3.10). Undo/redo state arrives in P1.
CREATE TABLE ops (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    actor      TEXT NOT NULL,
    at         INTEGER NOT NULL,
    request_id TEXT,
    revision   INTEGER NOT NULL
) STRICT;

CREATE TABLE events (
    id      TEXT PRIMARY KEY,
    op_id   TEXT NOT NULL REFERENCES ops (id),
    item_id TEXT NOT NULL REFERENCES items (id),
    at      INTEGER NOT NULL,
    actor   TEXT NOT NULL,
    kind    TEXT NOT NULL,
    -- ItemField number; NULL for whole-item events such as 'create'.
    field   INTEGER,
    old     TEXT,
    new     TEXT
) STRICT;
CREATE INDEX events_item ON events (item_id, at);
CREATE INDEX events_op ON events (op_id);

-- Watch replay (4.4). One revision per row (D9).
CREATE TABLE changes (
    revision    INTEGER PRIMARY KEY,
    topic       TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    deleted     INTEGER NOT NULL DEFAULT 0,
    at          INTEGER NOT NULL
) STRICT;

-- Idempotency (24h window).
CREATE TABLE request_ids (
    request_id TEXT PRIMARY KEY,
    op_id      TEXT NOT NULL,
    revision   INTEGER NOT NULL,
    item_id    TEXT,
    at         INTEGER NOT NULL
) STRICT;
CREATE INDEX request_ids_at ON request_ids (at);

CREATE TABLE spool_imports (
    spool_id TEXT PRIMARY KEY,
    op_id    TEXT NOT NULL,
    revision INTEGER NOT NULL,
    item_id  TEXT NOT NULL,
    at       INTEGER NOT NULL
) STRICT;
