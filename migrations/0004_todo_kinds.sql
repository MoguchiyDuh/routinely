ALTER TABLE todos ADD COLUMN kind TEXT NOT NULL DEFAULT 'todo' CHECK (kind IN ('todo','deadline'));

ALTER TABLE todos ADD COLUMN all_day INTEGER NOT NULL DEFAULT 0 CHECK (all_day IN (0,1));
