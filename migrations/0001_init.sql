CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    timezone TEXT NOT NULL DEFAULT 'UTC',
    created_at TEXT NOT NULL
);

CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_sessions_user ON sessions(user_id);

CREATE TABLE routines (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    notes TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('weekdays','count')),
    weekdays_mask INTEGER CHECK (weekdays_mask IS NULL OR (weekdays_mask BETWEEN 1 AND 127)),
    period TEXT CHECK (period IS NULL OR period IN ('day','week','month','year')),
    per_period INTEGER CHECK (per_period IS NULL OR per_period > 0),
    iterations INTEGER CHECK (iterations IS NULL OR iterations > 0),
    start_date TEXT NOT NULL,
    end_date TEXT,
    time_of_day TEXT,
    time_end TEXT,
    created_at TEXT NOT NULL,
    archived_at TEXT
);

CREATE TABLE occurrences (
    id INTEGER PRIMARY KEY,
    routine_id INTEGER NOT NULL REFERENCES routines(id) ON DELETE CASCADE,
    due_date TEXT NOT NULL,
    period_start TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (routine_id, due_date)
);

CREATE TABLE completions (
    id INTEGER PRIMARY KEY,
    occurrence_id INTEGER NOT NULL UNIQUE REFERENCES occurrences(id) ON DELETE CASCADE,
    completed_at TEXT NOT NULL,
    evidence_note TEXT
);

CREATE TABLE pins (
    id INTEGER PRIMARY KEY,
    completion_id INTEGER NOT NULL UNIQUE REFERENCES completions(id) ON DELETE CASCADE,
    summary TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE attachments (
    id INTEGER PRIMARY KEY,
    pin_id INTEGER NOT NULL REFERENCES pins(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('link','photo')),
    url TEXT,
    file_path TEXT,
    created_at TEXT NOT NULL,
    CHECK (kind = 'link' AND url IS NOT NULL OR kind = 'photo' AND file_path IS NOT NULL)
);

CREATE TABLE punishment_catalog (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT,
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
    created_by INTEGER NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL
);

CREATE TABLE punishments (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    catalog_id INTEGER REFERENCES punishment_catalog(id) ON DELETE SET NULL,
    custom_title TEXT,
    source TEXT NOT NULL CHECK (source IN ('auto','manual')),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','cleared','waived')),
    occurrence_id INTEGER REFERENCES occurrences(id) ON DELETE SET NULL,
    assigned_at TEXT NOT NULL,
    cleared_at TEXT
);

CREATE TABLE todos (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    notes TEXT,
    deadline TEXT,
    completed_at TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE daily_notes (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    day TEXT NOT NULL,
    body TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (user_id, day)
);
