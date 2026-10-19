CREATE TABLE reminders (
    id INTEGER PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    target_type TEXT NOT NULL CHECK (target_type IN ('routine','todo')),
    target_id BIGINT NOT NULL,
    offset_value BIGINT NOT NULL CHECK (offset_value >= 0),
    offset_unit TEXT NOT NULL CHECK (offset_unit IN ('minutes','hours','days','weeks')),
    at_time TEXT,
    created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_reminders_unique ON reminders (target_type, target_id, offset_value, offset_unit, COALESCE(at_time, ''));
CREATE INDEX idx_reminders_user ON reminders (user_id);

CREATE TRIGGER trg_routines_delete_reminders AFTER DELETE ON routines
BEGIN
    DELETE FROM reminders WHERE target_type = 'routine' AND target_id = OLD.id;
END;

CREATE TRIGGER trg_todos_delete_reminders AFTER DELETE ON todos
BEGIN
    DELETE FROM reminders WHERE target_type = 'todo' AND target_id = OLD.id;
END;

CREATE TABLE notifications (
    id INTEGER PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    url TEXT,
    dedupe_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at TEXT,
    UNIQUE (user_id, dedupe_key)
);

CREATE INDEX idx_notifications_user_unread ON notifications (user_id, read_at, id DESC);

CREATE TABLE notification_prefs (
    user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    day_end_enabled BIGINT NOT NULL DEFAULT 0 CHECK (day_end_enabled IN (0,1)),
    day_end_time TEXT NOT NULL DEFAULT '22:00',
    penalty_enabled BIGINT NOT NULL DEFAULT 0 CHECK (penalty_enabled IN (0,1)),
    quiet_start TEXT,
    quiet_end TEXT
);

CREATE TABLE notification_deliveries (
    id INTEGER PRIMARY KEY,
    notification_id BIGINT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
    channel TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts BIGINT NOT NULL DEFAULT 0,
    last_error TEXT,
    sent_at TEXT,
    UNIQUE (notification_id, channel)
);
