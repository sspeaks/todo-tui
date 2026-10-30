-- M5: the repeat rule on the Item (design 3.8, ADR-0003) and the indexes
-- the scheduler scans. Forward only: databases at version 3 exist.

-- The rule, as the JSON of todo_core::recurrence::Rule, NULL when the Item
-- does not recur. A row with a series id and no rule is a legacy instance
-- (recurrence was stored without its rule before M5): it reads back with
-- `rule: None` and never generates another.
ALTER TABLE items ADD COLUMN recurrence_rule TEXT;

-- Completing an instance generates the next one only when no other instance
-- of the series is still live and open (design 3.8).
CREATE INDEX items_series ON items (recurrence_series_id)
    WHERE recurrence_series_id IS NOT NULL AND deleted_at IS NULL;

-- The auto_done archive scan (design 3.7): the done and cancelled Items
-- (both have status_category 'done') that are live and unarchived, keyed by
-- when they closed, so the scheduler seeks to its threshold instead of
-- reading every closed Item. `status_entered_at` is unset on rows written
-- before it was recorded, hence the `updated_at` fallback the scan uses.
CREATE INDEX items_auto_archive
    ON items (COALESCE(status_entered_at, updated_at))
    WHERE status_category = 'done' AND archived_at IS NULL AND deleted_at IS NULL;

-- Healing (design 3.7): the live Items archived auto_done that are open
-- again, as an undo of their completion leaves them. Nearly always empty,
-- so the scheduler's scan after every user op reads nothing.
CREATE INDEX items_auto_done_reopened ON items (archived_at)
    WHERE archived_reason = 'auto_done' AND status_category <> 'done'
          AND deleted_at IS NULL;
