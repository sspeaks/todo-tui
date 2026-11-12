-- P3 M12: Claude's Suggestions and the jobs that make them (design 6,
-- ADR-0010). 'refine' joins the suggestions kinds and 'claude' the sources.
-- SQLite cannot alter a CHECK, so this rebuilds `suggestions` the documented
-- way (https://sqlite.org/lang_altertable.html#otheralter): schema.rs runs it
-- with foreign_keys OFF (suggestion_patches references it) and checks
-- foreign_key_check before committing. Forward only: databases at version 9
-- exist.

CREATE TABLE suggestions_new (
    id         TEXT PRIMARY KEY,
    item_id    TEXT NOT NULL REFERENCES items (id),
    kind       TEXT NOT NULL CHECK (kind IN ('sync_conflict', 'push_failed', 'refine')),
    source     TEXT NOT NULL CHECK (source IN ('sync', 'claude')),
    rationale  TEXT NOT NULL,
    error      TEXT,
    state      TEXT NOT NULL
               CHECK (state IN ('pending', 'applied', 'partially_applied', 'rejected', 'expired')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
INSERT INTO suggestions_new SELECT * FROM suggestions;
DROP TABLE suggestions;
ALTER TABLE suggestions_new RENAME TO suggestions;
CREATE INDEX suggestions_pending ON suggestions (item_id) WHERE state = 'pending';

-- One request to Claude (design 6.3 step 7): what it was for, how it ended
-- and what it cost. Kept for `todo assistant usage`; not exported (7.4).
CREATE TABLE assistant_jobs (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('refine')),
    item_ids      TEXT NOT NULL,
    state         TEXT NOT NULL CHECK (state IN ('queued', 'running', 'done', 'failed')),
    error         TEXT,
    model         TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cached_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd      REAL NOT NULL DEFAULT 0,
    duration_ms   INTEGER NOT NULL DEFAULT 0,
    -- The Suggestions a done job made, as a JSON list of ids.
    suggestion_ids TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
) STRICT;
CREATE INDEX assistant_jobs_by_time ON assistant_jobs (created_at);
