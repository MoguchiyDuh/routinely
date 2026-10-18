# routinely

Small self-hosted routine tracker for a group of friends. Track recurring routines, tick them off daily, and get a random penalty from a shared catalog when you miss one.

## Features

- **Routines**: fixed weekdays (with optional time) or N times per day/week/month/year, optional start date, end date or week count
- **Calendar**: day timeline, week time grid with weekly goal lanes, month overview
- **Ticking**: only today's routines can be ticked; past and future days are read-only
- **Proof**: attach a note, link or photos to a completed routine
- **Todos and deadlines**: dated tasks shown alongside routines
- **Journal**: daily notes and proof history with search
- **Penalties**: missed days and unmet goals draw from a shared catalog, max 3 per missed day; clear or waive
- **Group**: leaderboard, profiles with heatmap, read-only view of other members' calendars
- Server-rendered HTML with htmx; JSON API under `/api/v1`

## Stack

Rust 2024, axum, Askama, SQLite (sqlx), htmx.

## Run

```bash
git clone https://github.com/MoguchiyDuh/routinely.git
cd routinely

echo "ROUTINELY_SIGNUP_CODE=<code>" > .env
docker compose up -d --build
```

Listens on `127.0.0.1:8080`. Data lives in the `routinely-data` volume.

Without Docker:

```bash
ROUTINELY_SIGNUP_CODE=<code> cargo run --release
cargo test
```

## Config

| Variable                     | Default              |
| ---------------------------- | -------------------- |
| `ROUTINELY_SIGNUP_CODE`      | required             |
| `ROUTINELY_BIND`             | `127.0.0.1:8080`     |
| `ROUTINELY_DB_PATH`          | `data/routinely.db`  |
| `ROUTINELY_SESSION_TTL_SECS` | `2592000`            |
| `ROUTINELY_COOKIE_SECURE`    | `false`              |

New accounts need the signup code. Set `ROUTINELY_COOKIE_SECURE=true` behind HTTPS.

## Structure

```
src/auth/       # Signup, login, sessions
src/web/        # SSR pages and JSON API handlers
src/store.rs    # Queries and occurrence materialization
src/recurrence.rs
src/punish.rs   # Miss evaluation and penalty draws
src/score.rs    # Streaks, percentages, heatmap
templates/      # Askama templates
static/         # CSS, icons, htmx
migrations/     # SQLite schema
```
