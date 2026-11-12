-- P2 M11: a linked Item's description is Markdown, converted from Jira's wiki
-- markup (ADR-0009); until now it held the wiki text as Jira stores it. The
-- conversion is Rust's, so each linked Item is marked as not pulled: the next
-- pass reads it again by id (the tracked set's `stale`, design 5.4) and writes
-- the Markdown, as an ordinary sync change. Deleted ones too, so an undelete
-- reads them again. Forward only: databases at version 8 exist.
UPDATE items SET remote_last_pulled_version = NULL
WHERE remote_instance_id IS NOT NULL;
