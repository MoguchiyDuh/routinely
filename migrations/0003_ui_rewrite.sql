ALTER TABLE routines ADD COLUMN previous_id INTEGER REFERENCES routines(id);

CREATE INDEX idx_routines_previous ON routines(previous_id);

ALTER TABLE users ADD COLUMN avatar_path TEXT;

ALTER TABLE users ADD COLUMN avatar_icon TEXT;

ALTER TABLE users ADD COLUMN avatar_color INTEGER;

ALTER TABLE users ADD COLUMN onboarding_dismissed_at TEXT;
