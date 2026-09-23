ALTER TABLE users ADD COLUMN last_evaluated_day TEXT;

CREATE TABLE period_misses (
    routine_id INTEGER NOT NULL REFERENCES routines(id) ON DELETE CASCADE,
    period_start TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (routine_id, period_start)
);

CREATE UNIQUE INDEX idx_punishments_occurrence ON punishments(occurrence_id) WHERE occurrence_id IS NOT NULL;
