//! What the herald reads from a Paperclip board, and the one normalized event each record
//! becomes. Only the fields the herald uses are declared; everything else is ignored on purpose,
//! the reverse of Runo's own wire rule, because the board is not signed data and its schema moves
//! between versions (`docs/paperclip-api-check.md`).

use runo_core::time::Instant;
use serde::Deserialize;

use crate::time;

/// An issue as `GET /api/companies/:id/issues` returns it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    /// Board id.
    pub id: String,
    /// Human identifier, such as `HOD-43`.
    pub identifier: Option<String>,
    /// `backlog`, `todo`, `in_progress`, `in_review`, `done`, `blocked`, `cancelled`.
    pub status: String,
    /// The agent it is assigned to, if an agent.
    pub assignee_agent_id: Option<String>,
    /// Last change.
    pub updated_at: String,
}

/// A heartbeat run as `GET /api/companies/:id/heartbeat-runs` returns it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    /// Board id.
    pub id: String,
    /// The agent that ran.
    pub agent_id: String,
    /// The issue it ran for, if any.
    pub issue_id: Option<String>,
    /// `queued`, `scheduled_retry`, `running`, `succeeded`, `interrupted`, `failed`,
    /// `cancelled`, `timed_out`.
    pub status: String,
    /// Paperclip's own verdict on a finished run.
    pub liveness_state: Option<String>,
    /// Why, in Paperclip's words.
    pub liveness_reason: Option<String>,
    /// The failure kind, if it failed.
    pub error_code: Option<String>,
    /// The error text.
    pub error: Option<String>,
    /// When it finished; `None` while running.
    pub finished_at: Option<String>,
    /// When it was created; the run list is ordered by it, newest first.
    pub created_at: Option<String>,
}

/// An agent as `GET /api/companies/:id/agents` returns it: only its name is used, for the view.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    /// Board id.
    pub id: String,
    /// Display name.
    pub name: String,
}

/// One board record, normalized, at the board time it happened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BoardEvent {
    /// An issue's state as of `at`.
    Issue {
        /// Board id.
        id: String,
        /// The identifier the view shows (the board id if the issue has none).
        name: String,
        /// Its status.
        status: String,
        /// Its assigned agent.
        agent: Option<String>,
        /// When.
        at: Instant,
    },
    /// A run finished.
    RunFinished {
        /// Board id.
        id: String,
        /// The agent that ran.
        agent: String,
        /// The issue it ran for.
        issue: Option<String>,
        /// Final status.
        status: String,
        /// Paperclip's liveness verdict.
        liveness: Option<String>,
        /// Paperclip's reason, or the error text.
        reason: Option<String>,
        /// The failure kind.
        error_code: Option<String>,
        /// When.
        at: Instant,
    },
}

impl BoardEvent {
    /// When it happened.
    #[must_use]
    pub fn at(&self) -> Instant {
        match self {
            BoardEvent::Issue { at, .. } | BoardEvent::RunFinished { at, .. } => *at,
        }
    }
}

impl Issue {
    /// The event this record shows; `None` if its time cannot be read.
    #[must_use]
    pub fn event(&self) -> Option<BoardEvent> {
        Some(BoardEvent::Issue {
            id: self.id.clone(),
            name: self.identifier.clone().unwrap_or_else(|| self.id.clone()),
            status: self.status.clone(),
            agent: self.assignee_agent_id.clone(),
            at: time::parse(&self.updated_at)?,
        })
    }
}

impl Run {
    /// The event this record shows; `None` while the run is unfinished or its time unreadable.
    #[must_use]
    pub fn event(&self) -> Option<BoardEvent> {
        Some(BoardEvent::RunFinished {
            id: self.id.clone(),
            agent: self.agent_id.clone(),
            issue: self.issue_id.clone(),
            status: self.status.clone(),
            liveness: self.liveness_state.clone(),
            reason: self.liveness_reason.clone().or_else(|| self.error.clone()),
            error_code: self.error_code.clone(),
            at: time::parse(self.finished_at.as_deref()?)?,
        })
    }
}
