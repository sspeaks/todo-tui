-- M8: the energy field (design 3.2), the GTD "how much focus" context.
-- Forward only: databases at version 4 exist. Local only, never pushed;
-- 'none' is a value, as priority's is, so the column is never NULL.
ALTER TABLE items ADD COLUMN energy TEXT NOT NULL DEFAULT 'none'
    CHECK (energy IN ('none', 'low', 'medium', 'high'));
