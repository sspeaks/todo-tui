-- The phone app (ADR-0004): 'mobile' joins the created_via CHECK. SQLite
-- cannot alter a CHECK, so this rebuilds `items` the documented way
-- (https://sqlite.org/lang_altertable.html#otheralter): schema.rs runs it
-- with foreign_keys OFF and checks foreign_key_check before committing.
-- Forward only: databases at version 4 exist.
--
-- The new table is the 0001 definition plus 0004's recurrence_rule, with
-- only the created_via CHECK changed. `seq` is copied, so items_fts (content
-- 'items', rowid seq) still matches; dropping `items` drops its indexes and
-- triggers, recreated below as 0001, 0004 left them.

CREATE TABLE items_new (
    seq         INTEGER PRIMARY KEY,
    id          TEXT NOT NULL UNIQUE,
    created_via TEXT NOT NULL CHECK (created_via IN
        ('tui', 'cli', 'mcp', 'remote_import', 'restore', 'split', 'recurrence', 'mobile')),
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    revision    INTEGER NOT NULL,

    remote_tracker_kind        TEXT,
    remote_instance_id         TEXT,
    remote_id                  TEXT,
    remote_key                 TEXT,
    remote_url                 TEXT,
    remote_last_pulled_version INTEGER,
    remote_link_state          TEXT CHECK (remote_link_state IN ('pending', 'ok', 'broken')),
    remote_pinned              INTEGER NOT NULL DEFAULT 0,

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

    remote_extras TEXT NOT NULL DEFAULT '{}',

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

    status TEXT GENERATED ALWAYS AS (
        CASE status_category
            WHEN 'todo' THEN todo_refinement
            WHEN 'in_progress' THEN 'in_progress'
            ELSE CASE resolution_kind WHEN 'cancelled' THEN 'cancelled' ELSE 'done' END
        END
    ) VIRTUAL,

    -- 0004, after the generated column where ALTER TABLE ADD COLUMN put it.
    recurrence_rule TEXT,

    CHECK ((archived_at IS NULL) = (archived_reason IS NULL)),
    CHECK ((remote_id IS NULL) = (remote_instance_id IS NULL))
);

INSERT INTO items_new (
    seq, id, created_via, created_at, updated_at, revision,
    remote_tracker_kind, remote_instance_id, remote_id, remote_key, remote_url,
    remote_last_pulled_version, remote_link_state, remote_pinned,
    title, description, item_type, status_category, resolution_kind,
    remote_status_name, priority, assignee_person_id, assignee_text,
    reporter_person_id, reporter_text, labels, components, fix_versions,
    due_date, parent_id, sprint, story_points, original_estimate,
    remaining_estimate, resolution, remote_created_at, remote_updated_at,
    todo_refinement, waiting_on_person_id, waiting_on_text, waiting_reason,
    waiting_since, waiting_follow_up_on, defer_until, private_notes, focus_week,
    local_priority_override, archived_at, archived_reason,
    recurrence_series_id, recurrence_occurrence_date, remote_extras,
    is_mine, pending_fields, has_conflict, has_push_failure, description_lossy,
    description_dirty, last_user_touch_at, last_remote_change_at,
    status_entered_at, staleness_waiting_since, scopes, deleted_at, merged_into,
    recurrence_rule
)
SELECT
    seq, id, created_via, created_at, updated_at, revision,
    remote_tracker_kind, remote_instance_id, remote_id, remote_key, remote_url,
    remote_last_pulled_version, remote_link_state, remote_pinned,
    title, description, item_type, status_category, resolution_kind,
    remote_status_name, priority, assignee_person_id, assignee_text,
    reporter_person_id, reporter_text, labels, components, fix_versions,
    due_date, parent_id, sprint, story_points, original_estimate,
    remaining_estimate, resolution, remote_created_at, remote_updated_at,
    todo_refinement, waiting_on_person_id, waiting_on_text, waiting_reason,
    waiting_since, waiting_follow_up_on, defer_until, private_notes, focus_week,
    local_priority_override, archived_at, archived_reason,
    recurrence_series_id, recurrence_occurrence_date, remote_extras,
    is_mine, pending_fields, has_conflict, has_push_failure, description_lossy,
    description_dirty, last_user_touch_at, last_remote_change_at,
    status_entered_at, staleness_waiting_since, scopes, deleted_at, merged_into,
    recurrence_rule
FROM items;

DROP TABLE items;
ALTER TABLE items_new RENAME TO items;

-- 0001.
CREATE INDEX items_created ON items (created_at, id);
CREATE INDEX items_status ON items (status);
CREATE INDEX items_parent ON items (parent_id) WHERE parent_id IS NOT NULL;
CREATE UNIQUE INDEX items_remote ON items (remote_instance_id, remote_id)
    WHERE remote_id IS NOT NULL;

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

-- 0004.
CREATE INDEX items_series ON items (recurrence_series_id)
    WHERE recurrence_series_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX items_auto_archive
    ON items (COALESCE(status_entered_at, updated_at))
    WHERE status_category = 'done' AND archived_at IS NULL AND deleted_at IS NULL;
CREATE INDEX items_auto_done_reopened ON items (archived_at)
    WHERE archived_reason = 'auto_done' AND status_category <> 'done'
          AND deleted_at IS NULL;
