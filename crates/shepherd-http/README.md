# shepherd-http

HTTP management API server for Shepherd.

## Overview

This crate exposes a local/LAN REST API that lets a parent or administrator manage Shepherd from a phone or browser on the same network, without needing direct access to the launcher UI.

All routes are under `/api/v1/`. An optional Bearer token can be configured for authentication.

## API Routes

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Liveness check |
| `GET` | `/state` | Daemon state summary |
| `GET` | `/entries` | List all entries with current availability |
| `GET` | `/entries/{id}` | Single entry (supports `?at=` datetime preview) |
| `GET` | `/sessions/current` | Active session info |
| `POST` | `/sessions` | Request a session launch |
| `DELETE` | `/sessions/current` | Terminate the active session |
| `POST` | `/sessions/current/extend` | Add or remove time (`{ "seconds": N }`) |
| `GET` | `/overrides` | List daily overrides (`?date=YYYY-MM-DD`) |
| `GET` | `/overrides/{entry_id}` | Get override for one entry |
| `PUT` | `/overrides/{entry_id}` | Upsert override (`availability`, `quota_delta_seconds`) |
| `DELETE` | `/overrides/{entry_id}` | Clear override |
| `GET` | `/usage` | Screen-time stats (`?from=&to=`) |
| `GET` | `/usage/{entry_id}` | Stats for one entry |
| `GET` | `/volume` | Current volume |
| `PUT` | `/volume` | Set volume or mute |
| `POST` | `/config/reload` | Hot-reload configuration |
| `POST` | `/user/logout` | End the user's desktop session |
| `GET` | `/debug/windows` | Read-only list of compositor windows (Sway) |
| `POST` | `/debug/windows/{id}/close` | Ask the window to close (sway `kill`) |
| `POST` | `/debug/windows/{id}/hide` | Move the window to the scratchpad |
| `POST` | `/debug/windows/{id}/show` | Pull the window out of the scratchpad |
| `GET` | `/events` | Server-Sent Events stream |

## Configuration

```toml
[service.management_api]
port = 8080
bind = "0.0.0.0"
auth_token = "changeme"  # optional
```

## Server-Sent Events

`GET /api/v1/events` streams the same events that IPC clients receive, serialized as JSON. Useful for building live web UIs.

## Dependencies

- `axum` 0.8 — HTTP framework
- `tokio-stream` — BroadcastStream for SSE
- `shepherd-core` — Policy engine access
- `shepherd-store` — Persistence (daily overrides, usage)
- `shepherd-config` — ManagementApiConfig
- `shepherd-api` — Shared types (DailyOverride, UsageStat, Event, …)
