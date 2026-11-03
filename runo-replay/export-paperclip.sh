#!/usr/bin/env bash
# Export the HODD Bob Paperclip records for the Runo replay.
#
# Usage:
#   ./export-paperclip.sh                      # embedded DB, default window
#   DATABASE_URL=postgres://... ./export-paperclip.sh
#   FROM=2026-10-27 TO=2026-11-01 OUT=./corpus ./export-paperclip.sh
#
# Writes one CSV per table into $OUT. Large payloads (run context, results,
# full comment bodies, event messages) are truncated or left out on purpose:
# the replay needs boundaries and timing, not transcripts.
# Read-only: every statement is a SELECT inside \copy.

set -euo pipefail

DB="${DATABASE_URL:-postgres://paperclip:paperclip@127.0.0.1:54329/paperclip}"
FROM="${FROM:-2026-10-27}"
TO="${TO:-2026-11-01}"
OUT="${OUT:-./paperclip-corpus}"
mkdir -p "$OUT"

# Window predicate for a timestamp column.
win() { echo "$1 >= '${FROM}'::timestamptz AND $1 < '${TO}'::timestamptz"; }

export_table() {
  local name="$1" query="$2"
  psql "$DB" -v ON_ERROR_STOP=1 -q -c "\\copy (${query}) TO '${OUT}/${name}.csv' WITH (FORMAT csv, HEADER true)"
  printf '%-34s %8s rows\n' "$name" "$(($(wc -l < "${OUT}/${name}.csv") - 1))"
}

# Voices: every agent, so ids resolve.
export_table agents "
  SELECT id, company_id, name, role, title, status, adapter_type, reports_to, created_at
  FROM agents"

# Tasks: issues created or touched in the window (initial task, scope, status).
export_table issues "
  SELECT id, company_id, identifier, parent_id, goal_id, project_id, title,
         left(description, 1000) AS description_head,
         status, work_mode, priority, review_policy,
         assignee_agent_id, assignee_user_id, created_by_agent_id, created_by_user_id,
         origin_kind, origin_id, origin_run_id, request_depth,
         checkout_run_id, execution_run_id, execution_agent_name_key, execution_locked_at,
         execution_policy, blocked_transition_at,
         started_at, completed_at, cancelled_at, created_at, updated_at
  FROM issues
  WHERE $(win created_at) OR $(win updated_at)
     OR (created_at < '${FROM}'::timestamptz AND status NOT IN ('done','cancelled'))"

export_table issue_relations "
  SELECT id, issue_id, related_issue_id, type, created_by_agent_id, created_by_user_id, created_at
  FROM issue_relations WHERE $(win created_at)"

# Runs: the lane sessions. Liveness fields are Paperclip's own detector, the baseline to beat.
export_table heartbeat_runs "
  SELECT id, company_id, agent_id, issue_id, scope_kind, invocation_source, trigger_detail,
         status, started_at, finished_at, error_code, left(error, 300) AS error_head, exit_code, signal,
         retry_of_run_id, process_loss_retry_count, scheduled_retry_at, scheduled_retry_reason,
         liveness_state, liveness_reason, last_output_at, last_useful_action_at,
         continuation_attempt, execution_stage, next_action, wakeup_request_id,
         usage_json, created_at, updated_at
  FROM heartbeat_runs
  WHERE $(win created_at) OR $(win finished_at)"

# Run events: the fine-grained boundaries inside each run.
export_table heartbeat_run_events "
  SELECT e.run_id, e.agent_id, e.seq, e.event_type, e.stream, e.level,
         left(e.message, 200) AS message_head, e.created_at
  FROM heartbeat_run_events e
  WHERE $(win e.created_at)"

# Comments: status reports, questions, owner rulings in chat.
export_table issue_comments "
  SELECT id, issue_id, author_type, author_agent_id, author_user_id, created_by_run_id,
         origin, length(body) AS body_chars, left(body, 500) AS body_head,
         deleted_at, created_at
  FROM issue_comments WHERE $(win created_at)"

# Activity: status transitions, assignments, approvals and every other action.
export_table activity_log "
  SELECT id, actor_type, actor_id, action, entity_type, entity_id, agent_id, run_id, details, created_at
  FROM activity_log WHERE $(win created_at)"

# Wakes: who woke whom and why; skipped or failed wakes are pattern 17.
export_table agent_wakeup_requests "
  SELECT id, agent_id, source, trigger_detail, reason, status, coalesced_count,
         requested_by_actor_type, requested_by_actor_id, run_id,
         requested_at, claimed_at, finished_at, left(error, 300) AS error_head
  FROM agent_wakeup_requests WHERE $(win requested_at)"

# Rulings: review and approval stage outcomes (patterns 1 and 22).
export_table issue_execution_decisions "
  SELECT id, issue_id, stage_id, stage_type, actor_agent_id, actor_user_id, outcome,
         left(body, 300) AS body_head, created_by_run_id, created_at
  FROM issue_execution_decisions WHERE $(win created_at)"

# Work products: PRs and other deliverables, with review state.
export_table issue_work_products "
  SELECT id, issue_id, type, provider, external_id, title, url, status, review_state,
         is_primary, health_status, created_by_run_id, created_at, updated_at
  FROM issue_work_products WHERE $(win created_at) OR $(win updated_at)"

# Recovery and watchdog actions: what the system itself noticed, and when.
export_table issue_recovery_actions "
  SELECT id, source_issue_id, recovery_issue_id, kind, status, cause, next_action,
         attempt_count, outcome, created_at, resolved_at
  FROM issue_recovery_actions WHERE $(win created_at)"

export_table heartbeat_run_watchdog_decisions "
  SELECT id, run_id, evaluation_issue_id, decision, snoozed_until, left(reason, 300) AS reason_head,
         created_by_agent_id, created_by_user_id, created_at
  FROM heartbeat_run_watchdog_decisions WHERE $(win created_at)"

# Cost events: model and provider per call (patterns 7 and 26), activity between boundaries.
export_table cost_events "
  SELECT agent_id, issue_id, heartbeat_run_id, provider, model, input_tokens,
         cached_input_tokens, output_tokens, cost_cents, occurred_at
  FROM cost_events WHERE $(win occurred_at)"

echo "Done: ${OUT}"
