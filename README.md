# routinely

Small self-hosted routine tracker for a group of friends. Track recurring routines, tick them off daily, and get a random penalty from a shared catalog when you miss one.

## Features

- **Routines**: fixed weekdays (with optional time) or N times per day/week/month/year, optional start date, end date or week count
- **Calendar**: day timeline, week time grid with weekly goal lanes, month overview
- **Ticking**: only today's routines can be ticked; past and future days are read-only
- **Day start**: per-user rollover hour (00:00–06:00, default midnight). Before it, today, ticking, streaks and penalty evaluation still belong to the previous day, so a 22:00 routine can be ticked at 02:00
- **Proof**: attach a note, link or photos to a completed routine
- **Archive**: archived routines can be restored or deleted permanently with their history and photos
- **Todos and deadlines**: dated tasks shown alongside routines
- **Journal**: daily notes and proof history with text search and a per-routine filter (notes, links and photos)
- **Penalties**: missed days and unmet goals draw from a shared catalog, max 3 per missed day; clear or waive
- **Group**: leaderboard, profiles with heatmap, read-only view of other members' calendars, journals and routines; copy any member's routine into a prefilled new-routine form
- **Reminders**: on by default, Google-Calendar-style (N minutes/hours/days/weeks before, plus a time for day/week offsets) on routines and todo deadlines, delivered to an in-app inbox
- Server-rendered HTML with htmx; JSON API under `/api/v1`

## Stack

Rust 2024, axum, Askama, htmx, SQLite (sqlx), one container.

## Run

```bash
git clone https://github.com/MoguchiyDuh/routinely.git
cd routinely
cp .env.example .env
mkdir data && sudo chown 1000:1000 data
docker compose up -d
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests, pushes `ghcr.io/moguchiyduh/routinely:latest` and `:sha-<short>`, then deploys over SSH. The server never builds.

Deploy target: a `deploy` user whose only authorized key is `restrict,command="/usr/local/bin/deploy"`. The script accepts `<app> <latest|sha-xxxxxxx>`, then runs `docker compose pull` and `docker compose up -d --wait` in `/opt/services/<app>` with `IMAGE_TAG` set. Repo secrets: `DEPLOY_HOST`, `DEPLOY_PORT`, `DEPLOY_USER`, `DEPLOY_KEY`, `DEPLOY_KNOWN_HOSTS`.

Rollback: `ssh deploy@host routinely sha-xxxxxxx`. Manual update: `docker compose pull && docker compose up -d`.

Listens on `127.0.0.1:8112`; put a reverse proxy with TLS in front. State (SQLite file, sessions, uploads) lives in `./data`.

Tests:

```bash
cargo test
```

## Config

| Variable                     | Default             |
| ---------------------------- | ------------------- |
| `ROUTINELY_SIGNUP_CODE`      | required            |
| `ROUTINELY_DATABASE_PATH`    | `data/routinely.db` |
| `ROUTINELY_BIND`             | `127.0.0.1:8080`    |
| `ROUTINELY_SESSION_TTL_SECS` | `2592000`           |
| `ROUTINELY_COOKIE_SECURE`    | `false`             |

New accounts need the signup code. Set `ROUTINELY_COOKIE_SECURE=true` behind HTTPS.

## Notifications

Notifications are on by default (master switch in prefs). A background tick (every 30 s) evaluates penalties and creates inbox rows from:

- **Reminders**: up to 5 per routine or todo. `minutes`/`hours` offsets are relative to the start time or deadline (no `at_time`); `days`/`weeks` offsets and untimed or all-day targets need an `at_time` (`HH:MM`). Offset `0 minutes` means "at start". A reminder fires only within 15 minutes after its fire time and never retroactively. Completed todos/occurrences and archived routines are skipped; count routines remind on every active day until the period goal is met.
- **Day-end nudge** (default on, 20:00): once per day after `day_end_time` if routines (timed or not) are still open.
- **Penalty issued/resolved** (default on): only penalties created after the user's prefs row was first resolved, max 24 h back.
- **Quiet hours** (optional): no notifications are created inside the window; reminders that would have fired are dropped, nudges and penalties follow after it ends.

Default reminders are copied into real reminder rows when a routine or todo is created without a `reminders` field (an explicit `[]` means none; SSR forms use the optional `reminders_json` field the same way). Defaults live in prefs: timed routine 10 min before, untimed routine none (optional same-day time), timed todo 60 min before, all-day todo 1 day before at 09:00. Existing routines and todos got these via migration 0004 without firing retroactively. Editing never re-applies defaults; reminders that become invalid after a timed/untimed change are dropped.

Notifications are deduplicated by key, read ones are purged after 30 days, unread after 90. `notification_deliveries` is schema-only, reserved for a future Telegram channel.

UI:

- Bell in the sidebar and top bar with an unread badge (polled every 60 s); `/notifications` is the inbox (newest first, unread highlighted, per-item and "Mark all read", "Load more"). Opening an item's link marks it read.
- Settings → Notifications edits every pref. Reminder defaults there apply to new routines and todos only.
- Routine forms and the deadline form have a reminder editor (up to 5 rows: amount, unit, optional time). New forms are pre-filled with your defaults, which you can edit or remove; removing every row means no reminders. The time field appears for day/week offsets and for untimed or all-day items. Todo deadlines list their reminders in the todo list.

JSON API (cookie auth, under `/api/v1`):

| Method | Path |
| --- | --- |
| GET | `/notifications?limit&before_id` |
| GET | `/notifications/unread_count` |
| POST | `/notifications/read`, `/notifications/{id}/read` |
| GET, PUT | `/notifications/prefs` |
| PUT | `/me/day_start` (`day_start`: 0–6) |
| GET | `/reminders?target_type&target_id` |
| POST | `/reminders` (`target_type`, `target_id`, `offset_value`, `offset_unit`, `at_time`) |
| DELETE | `/reminders/{id}` |

Routine and todo create (and routine update) payloads accept `reminders: [{offset_value, offset_unit, at_time}]`; on routine update the list replaces the existing set.

## Structure

```
src/auth/       # Signup, login, sessions (SQLite)
src/web/        # SSR pages and JSON API handlers
src/store.rs    # Queries and occurrence materialization
src/recurrence.rs
src/punish.rs   # Miss evaluation and penalty draws
src/notify.rs   # Reminder/notification tick, inbox, prefs
src/score.rs    # Streaks, percentages, heatmap
templates/      # Askama templates
static/         # CSS, icons, htmx
migrations/     # SQLite schema
.github/        # CI: builds and pushes the image to GHCR
```
