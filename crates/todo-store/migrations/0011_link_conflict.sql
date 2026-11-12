-- P2 M13: promote and attach (design 5.7, 3.3). 'link_conflict' joins the
-- suggestions kinds: an attach, or a promote Jira changed, offers back the
-- local values Jira's replaced. SQLite cannot alter a CHECK, so this rebuilds
-- `suggestions` as 0010 did (foreign_keys OFF, checked before commit).
-- Forward only: databases at version 10 exist.

CREATE TABLE suggestions_new (
    id         TEXT PRIMARY KEY,
    item_id    TEXT NOT NULL REFERENCES items (id),
    kind       TEXT NOT NULL CHECK (kind IN ('sync_conflict', 'push_failed', 'refine', 'link_conflict')),
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
