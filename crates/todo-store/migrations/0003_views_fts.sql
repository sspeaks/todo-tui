-- M3: saved views (design 2.4) and an accent-folding full-text index.
-- Forward only: databases at version 2 exist.

-- Views. `filter` is the JSON of the core Filter and `sort` of its sort
-- keys, in the todo_core::filter serde form. `position` is dense but not
-- UNIQUE, so renumbering cannot trip it. `seed` names the default a view
-- was seeded from, NULL for a view a user made.
CREATE TABLE views (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    filter     TEXT NOT NULL,
    sort       TEXT NOT NULL DEFAULT '[]',
    show_count INTEGER NOT NULL DEFAULT 1,
    position   INTEGER NOT NULL,
    seed       TEXT UNIQUE,
    revision   INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX views_name ON views (name COLLATE NOCASE);

-- The defaults seeded so far. A row outlives its view, so a deleted default
-- is never seeded again, and a default added later is seeded once.
CREATE TABLE view_seeds (
    seed      TEXT PRIMARY KEY,
    seeded_at INTEGER NOT NULL
) STRICT;

-- Full text, now folding every Latin diacritic (remove_diacritics 2), so
-- "tieng" finds "Tiếng" stored composed or decomposed. The columns are as
-- before; the triggers on items name the table, not its definition, so they
-- stay and keep the new table current.
DROP TABLE items_fts;
CREATE VIRTUAL TABLE items_fts USING fts5 (
    title, description, remote_key, labels,
    content = 'items', content_rowid = 'seq',
    tokenize = 'unicode61 remove_diacritics 2'
);
INSERT INTO items_fts (items_fts) VALUES ('rebuild');
