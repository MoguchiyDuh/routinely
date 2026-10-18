ALTER TABLE punishments ADD COLUMN miss_day TEXT;
ALTER TABLE punishments ADD COLUMN routine_id INTEGER REFERENCES routines(id) ON DELETE SET NULL;
ALTER TABLE punishments ADD COLUMN period_start TEXT;

UPDATE punishments
SET miss_day = (SELECT o.due_date FROM occurrences o WHERE o.id = punishments.occurrence_id)
WHERE occurrence_id IS NOT NULL;

CREATE INDEX idx_punishments_miss_day ON punishments(user_id, miss_day) WHERE source = 'auto';
