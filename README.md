# routinely

Small self-hosted routine tracker for a group of friends. Track recurring routines, tick them off daily, and get a random penalty from a shared catalog when you miss one.

## Features

- **Routines**: fixed weekdays (with optional time) or N times per day/week/month/year, optional start date, end date or week count
- **Calendar**: day timeline, week time grid with weekly goal lanes, month overview
- **Ticking**: only today's routines can be ticked; past and future days are read-only
- **Proof**: attach a note, link or photos to a completed routine
- **Archive**: archived routines can be restored or deleted permanently with their history and photos
- **Todos and deadlines**: dated tasks shown alongside routines
- **Journal**: daily notes and proof history with text search and a per-routine filter (notes, links and photos)
- **Penalties**: missed days and unmet goals draw from a shared catalog, max 3 per missed day; clear or waive
- **Group**: leaderboard, profiles with heatmap, read-only view of other members' calendars
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

The image is built by GitHub Actions and pulled from GHCR. Update with `docker compose pull && docker compose up -d`.

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

## Structure

```
src/auth/       # Signup, login, sessions (SQLite)
src/web/        # SSR pages and JSON API handlers
src/store.rs    # Queries and occurrence materialization
src/recurrence.rs
src/punish.rs   # Miss evaluation and penalty draws
src/score.rs    # Streaks, percentages, heatmap
templates/      # Askama templates
static/         # CSS, icons, htmx
migrations/     # SQLite schema
.github/        # CI: builds and pushes the image to GHCR
```
