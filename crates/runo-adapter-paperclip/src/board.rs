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
    /// When it last became blocked.
    #[serde(default)]
    pub blocked_transition_at: Option<String>,
    /// Paperclip's own view of a blocked issue's blockers, computed when read (not stored, so it
    /// can change without `updatedAt` changing).
    #[serde(default)]
    pub blocker_attention: Option<BlockerAttention>,
    /// Who must act to unblock it, and how, when Paperclip knows.
    #[serde(default)]
    pub unblock_descriptor: Option<UnblockDescriptor>,
}

/// `blockerAttention`: whether a blocked issue's chain is moving.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockerAttention {
    /// `none`, `covered` (an active child or dependency is moving), `stalled`, `needs_attention`.
    pub state: Option<String>,
    /// The blocker in the chain that has stalled, if any.
    pub sample_stalled_blocker_identifier: Option<String>,
    /// A blocker in the chain.
    pub sample_blocker_identifier: Option<String>,
}

/// `unblockDescriptor`: who must act, and what to do.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnblockDescriptor {
    /// `"board"`, `{ "userId": … }` or `{ "agentId": … }`.
    pub owner: Option<serde_json::Value>,
    /// What to do, in Paperclip's words.
    pub action: Option<String>,
}

/// What a blocked issue waits on, as the board explains it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Block {
    /// Blocked behind work that is moving (`covered`): not this lane's stall. The blocker's own
    /// lane is watched instead.
    Parked,
    /// A wait that can stall.
    Waits {
        /// One line on what it waits on.
        on: String,
        /// When it became blocked (`blockedTransitionAt`, else the last update).
        since: Instant,
    },
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

/// A company as `GET /api/companies` returns it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Company {
    /// Board id: what `--company` takes.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The prefix of its issue identifiers and page paths, such as `HOD`.
    pub issue_prefix: Option<String>,
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
        /// What it waits on, when blocked.
        block: Option<Block>,
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
        let at = time::parse(&self.updated_at)?;
        Some(BoardEvent::Issue {
            id: self.id.clone(),
            name: self.identifier.clone().unwrap_or_else(|| self.id.clone()),
            status: self.status.clone(),
            agent: self.assignee_agent_id.clone(),
            block: (self.status == "blocked").then(|| self.block(at)),
            at,
        })
    }

    /// What tells one reading of this issue from the next: its update time, and the blocker
    /// fields Paperclip computes when read.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        format!(
            "{}|{}|{}",
            self.updated_at,
            self.blocker_attention
                .as_ref()
                .and_then(|a| a.state.as_deref())
                .unwrap_or_default(),
            self.unblock_descriptor
                .as_ref()
                .and_then(|u| u.owner.as_ref())
                .map(ToString::to_string)
                .unwrap_or_default()
        )
    }

    fn block(&self, updated: Instant) -> Block {
        let attention = self.blocker_attention.as_ref();
        let state = attention.and_then(|a| a.state.as_deref());
        if state == Some("covered") {
            return Block::Parked;
        }
        let owner = self
            .unblock_descriptor
            .as_ref()
            .and_then(|u| u.owner.as_ref());
        let mut on = match (owner, state) {
            (Some(serde_json::Value::String(o)), _) if o == "board" => {
                "blocked; the board must unblock it".to_owned()
            }
            (Some(o), _) if o.get("userId").is_some() => {
                "blocked; a person is named to unblock it".to_owned()
            }
            (Some(o), _) if o.get("agentId").is_some() => {
                "blocked; an agent is named to unblock it".to_owned()
            }
            (_, Some("needs_attention")) => {
                "blocked and needs attention; nobody is named to unblock it".to_owned()
            }
            (_, Some("stalled")) => "blocked on a stalled chain".to_owned(),
            _ => "blocked on the board".to_owned(),
        };
        if let Some(action) = self
            .unblock_descriptor
            .as_ref()
            .and_then(|u| u.action.as_deref())
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            let action: String = action
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(100)
                .collect();
            on = format!("{on}: {action}");
        }
        if let Some(blocker) = attention.and_then(|a| {
            a.sample_stalled_blocker_identifier
                .as_deref()
                .or(a.sample_blocker_identifier.as_deref())
        }) {
            on = format!("{on} (blocker {blocker})");
        }
        Block::Waits {
            on,
            since: self
                .blocked_transition_at
                .as_deref()
                .and_then(time::parse)
                .unwrap_or(updated),
        }
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
