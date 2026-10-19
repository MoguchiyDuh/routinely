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

Rust 2024, axum, Askama, htmx, PostgreSQL (sqlx), Valkey (sessions), Caddy.

## Run

On a server with Docker and a domain pointing at it:

```bash
git clone https://github.com/MoguchiyDuh/routinely.git
cd routinely
cp .env.example .env
docker compose up -d --build
```

Fill `.env` first: `DOMAIN` and random values for the rest (`openssl rand -hex 24`). Caddy gets the certificate for `DOMAIN` by itself.

| Service | Image                    | Networks          |
| ------- | ------------------------ | ----------------- |
| `proxy` | `caddy:2-alpine`         | frontend          |
| `app`   | built from `Dockerfile`  | frontend, backend |
| `db`    | `postgres:18-alpine`     | backend           |
| `cache` | `valkey/valkey:9-alpine` | backend           |

`backend` is internal: database and cache have no route out and no published port.

Tests need a Postgres and a Valkey:

```bash
docker run -d --rm -p 127.0.0.1:55432:5432 -e POSTGRES_PASSWORD=postgres postgres:18-alpine
docker run -d --rm -p 127.0.0.1:56379:6379 valkey/valkey:9-alpine
cargo test
```

Override with `ROUTINELY_TEST_DATABASE_URL` and `ROUTINELY_TEST_VALKEY_URL`.

## Config

| Variable                     | Default          |
| ---------------------------- | ---------------- |
| `ROUTINELY_SIGNUP_CODE`      | required         |
| `ROUTINELY_DATABASE_URL`     | required         |
| `ROUTINELY_VALKEY_URL`       | required         |
| `ROUTINELY_BIND`             | `127.0.0.1:8080` |
| `ROUTINELY_SESSION_TTL_SECS` | `2592000`        |
| `ROUTINELY_COOKIE_SECURE`    | `false`          |
| `ROUTINELY_VALKEY_PREFIX`    | `routinely:`     |

New accounts need the signup code. Set `ROUTINELY_COOKIE_SECURE=true` behind HTTPS.

## Structure

```
src/auth/       # Signup, login, sessions
src/web/        # SSR pages and JSON API handlers
src/store.rs    # Queries and occurrence materialization
src/kv.rs       # Valkey sessions
src/recurrence.rs
src/punish.rs   # Miss evaluation and penalty draws
src/score.rs    # Streaks, percentages, heatmap
templates/      # Askama templates
static/         # CSS, icons, htmx
migrations/     # PostgreSQL schema
```
