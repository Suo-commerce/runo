# Paperclip API check for the herald

3 November 2026. Technical decision 8 says: use the Paperclip API first, and verify it exposes
the signals the herald needs, and how it pages, before building on it. This is that check.

**Source.** `paperclipai/paperclip` at `f7ad1dc` (main, 3 Nov 2026), read from the server's
route and service code, not from the docs site. The HODD Bob board may run an older build (the
replay found a schema that lacked `heartbeat_runs.issue_id`), so the herald reads `GET /api/health`
(`version`) on start and `runo doctor` prints it.

## Verdict

**The API is sufficient. No database adapter is needed.** Every signal the replay kit read from
Postgres has an HTTP route, and an API key can authenticate both REST and the live-events socket.
Two lists have no time filter, so the herald reads them by polling newest-first with
deduplication and a gap check. That is safe at the HODD Bob volume, and the herald reports a gap
rather than hiding one.

## Signals, mapped

| Herald needs (replay table) | Route | Incremental read |
| --- | --- | --- |
| Runs and their liveness: `status`, `liveness_state`, `liveness_reason`, `last_output_at`, `error_code` (`heartbeat_runs`) | `GET /api/companies/:companyId/heartbeat-runs?limit=&agentId=` (default 200, max 1000); `GET /api/companies/:companyId/live-runs` (live only, liveness fields included); `GET /api/heartbeat-runs/:runId` | Newest first, no `since`: poll and dedupe by id + `updatedAt` |
| Run events (`heartbeat_run_events`) | `GET /api/heartbeat-runs/:runId/events?afterSeq=&limit=` | **Cursor** (`afterSeq`) |
| Issues, status, execution lock, parent (`issues`) | `GET /api/companies/:companyId/issues?sortField=&sortDir=&limit=&offset=` | Sort by update time, page with `offset` |
| Status transitions, assignments, every other action (`activity_log`) | `GET /api/companies/:companyId/activity?entityType=&entityId=&agentId=&limit=` (default 100, max 500) | Newest first, no `since`: poll and dedupe by id |
| Comments, human acts (`issue_comments`) | `GET /api/issues/:id/comments?after=&order=asc&limit=` | **Cursor** (`after` comment id) |
| Recovery actions (`issue_recovery_actions`) | `GET /api/issues/:id/recovery-actions`; company-wide `GET /api/companies/:companyId/recovery-observability` (weekly aggregate) | Per issue, fetched when an issue changes |
| Wakes (`agent_wakeup_requests`) | `GET /api/issues/:id/diagnostics/wakes` | Per issue, on demand |
| Runs per issue | `GET /api/issues/:id/runs` | Per issue |

## Push: the live-events socket

`GET /api/companies/:companyId/events/ws` (WebSocket), `Authorization: Bearer <api key>`; an
agent API key is accepted and checked for revocation. Event types include `heartbeat.run.queued`,
`heartbeat.run.status`, `heartbeat.run.progress`, `heartbeat.run.event`, `agent.status` and
`activity.logged`.

- `heartbeat.run.status` carries run id, agent, issue, status, error code and timestamps, but
  **not** `livenessState` / `livenessReason`. When a run's status changes, the herald fetches
  the run (or the live-runs list) for Paperclip's liveness verdict.
- There is **no replay or resume** on reconnect. The socket is a latency improvement only: on
  every (re)connect the herald backfills from REST, and REST polling remains the source of truth.

## How the herald reads, then

1. On start and on every socket reconnect: backfill runs (newest 1000), activity (newest 500) and
   issues (by update time) back to the last cursor persisted in the herald's own store.
2. While connected: react to `heartbeat.run.status` and `activity.logged` at once, and poll
   anyway on the probe interval (the socket may drop events silently).
3. **Gap check.** If a newest-first page's oldest row is newer than the last row already seen,
   rows were missed. The herald widens the page (runs up to 1000) and, if a gap remains,
   records an incident (`paperclip-gap`, with the window). `runo view` shows it. A herald that
   silently loses signals would repeat the exact failure the replay measured.
4. At HODD Bob volume the gap check should not fire: 7,792 runs and about 130 signal episodes a
   day over five days fit easily in one poll at a one-minute interval. The first live run
   measures this instead of assuming it.

## What this does not settle

- **Write access for delivery.** The human `CallChannel` posts a comment on the lane
  (`POST /api/issues/:id/comments`). The key needs that permission; `runo doctor` must check it
  without posting (the rule: nothing writes to a board without `--apply`).
- **Rate limits.** None were found in the routes read. The herald still backs off on `429` or
  `503`.
- **Schema drift between Paperclip versions.** The herald deserializes only the fields it uses
  and tolerates unknown ones. This is the reverse of Runo's own wire rule, on purpose: the
  board is not signed data.
