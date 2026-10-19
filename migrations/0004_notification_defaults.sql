ALTER TABLE notification_prefs ADD COLUMN enabled BIGINT NOT NULL DEFAULT 1 CHECK (enabled IN (0,1));
ALTER TABLE notification_prefs ADD COLUMN default_routine_timed_minutes BIGINT DEFAULT 10;
ALTER TABLE notification_prefs ADD COLUMN default_routine_allday_enabled BIGINT NOT NULL DEFAULT 0 CHECK (default_routine_allday_enabled IN (0,1));
ALTER TABLE notification_prefs ADD COLUMN default_routine_allday_time TEXT NOT NULL DEFAULT '09:00';
ALTER TABLE notification_prefs ADD COLUMN default_todo_timed_minutes BIGINT DEFAULT 60;
ALTER TABLE notification_prefs ADD COLUMN default_todo_allday_days BIGINT DEFAULT 1;
ALTER TABLE notification_prefs ADD COLUMN default_todo_allday_time TEXT NOT NULL DEFAULT '09:00';
ALTER TABLE notification_prefs ADD COLUMN notifications_since TEXT;

INSERT OR IGNORE INTO reminders (user_id, target_type, target_id, offset_value, offset_unit, at_time, created_at)
SELECT r.user_id, 'routine', r.id, 10, 'minutes', NULL, strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now')
FROM routines r
WHERE r.archived_at IS NULL
  AND r.time_of_day IS NOT NULL AND TRIM(r.time_of_day) != ''
  AND NOT EXISTS (SELECT 1 FROM routines s WHERE s.previous_id = r.id)
  AND NOT EXISTS (SELECT 1 FROM reminders m WHERE m.target_type = 'routine' AND m.target_id = r.id);

INSERT OR IGNORE INTO reminders (user_id, target_type, target_id, offset_value, offset_unit, at_time, created_at)
SELECT t.user_id, 'todo', t.id,
  CASE WHEN t.all_day = 1 THEN 1 ELSE 60 END,
  CASE WHEN t.all_day = 1 THEN 'days' ELSE 'minutes' END,
  CASE WHEN t.all_day = 1 THEN '09:00' ELSE NULL END,
  strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now')
FROM todos t
WHERE t.completed_at IS NULL AND t.deadline IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM reminders m WHERE m.target_type = 'todo' AND m.target_id = t.id);
