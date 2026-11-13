-- P2 M14: People (design 3.5), made from the tracker's users. A person has
-- one row, and each tracker account of theirs an identity; a pull finds the
-- person by identity. Emails are a JSON array of strings. Forward only:
-- databases at version 11 exist.
CREATE TABLE people (
    id           TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    username     TEXT NOT NULL DEFAULT '',
    emails       TEXT NOT NULL DEFAULT '[]',
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE person_identities (
    person_id   TEXT NOT NULL REFERENCES people (id),
    instance_id TEXT NOT NULL,
    account_id  TEXT NOT NULL,
    PRIMARY KEY (instance_id, account_id)
) STRICT;

CREATE INDEX person_identities_person ON person_identities (person_id);

-- Each linked Item is read again (as 0009 did), so its assignee and
-- reporter, free text until now, become People: an ordinary sync change.
UPDATE items SET remote_last_pulled_version = NULL
WHERE remote_instance_id IS NOT NULL;
