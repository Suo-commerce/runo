//! The herald's core (technical spec §2, "The herald"; §15, Watch): board events in, signals out.
//! Pure and synchronous, so the same code serves `runo watch` (live board times) and
//! `runo diagnose` (a board's history replayed), and is tested on fixtures.
//!
//! A lane is `(assigned agent, issue identifier)`. The mapping, each rule named once:
//!
//! | Board | Evidence for the lane's clocks |
//! | --- | --- |
//! | Issue enters `in_progress`, `blocked` or `in_review` with an agent | `Started` under the advisory promise |
//! | `blocked` | `Blocked("blocked on the board")`: a wait, escalated only when overdue, never on sight |
//! | `in_review` | `Blocked("review")`: the lane waits on a reviewer |
//! | Back to `in_progress` | `Unblocked` |
//! | `done`, `cancelled`, `todo`, `backlog`, or reassigned | `Ended` (a reassignment starts the new agent's lane) |
//! | A run finishes `advanced` or `completed` | `Progressed` |
//! | A run fails with a kind that never clears on its own | an immediate `Task` signal at `Suspected` |
//!
//! "Never clears on its own" is Paperclip's own list of failures it does not retry
//! (`NON_RETRYABLE_CONTINUATION_ERROR_CODES` in its recovery service, `f7ad1dc`); the default
//! below copies it, and the configuration can replace it. Such a failure clears when the issue
//! ends or a later run on it advances.
//!
//! Signals start at `Called`: `Crossed` is the wait-one-cadence step, where the lane gets one
//! cadence to show its own next event before anyone is told.

use std::collections::{BTreeMap, BTreeSet};

use runo_clock::{Chorus, Cleared, Config, Evidence, Level, Observation};
use runo_core::time::Instant;
use runo_core::{Cadence, Clock, Millis, Promise, SingerId, TaskId};
use runo_node::episodes::Lane;

use crate::board::BoardEvent;

const MINUTE: u64 = 60_000;

/// Paperclip's non-retryable failure kinds, as of `f7ad1dc`.
pub const NEVER_CLEARS: [&str; 21] = [
    "project_access_denied",
    "native_provider_model_rejected",
    "provider_tool_definition_invalid",
    "adapter_engine_unavailable",
    "agent_not_invokable",
    "agent_not_found",
    "budget_blocked",
    "budget_exhausted",
    "issue_paused",
    "issue_dependencies_blocked",
    "setup_failed",
    "workspace_git_scan_timeout",
    "workspace_git_scan_saturated",
    "workspace_git_scan_cancelled",
    "workspace_git_scan_output_limit",
    "workspace_git_scan_failed",
    "low_trust_isolation_unavailable",
    "low_trust_requires_isolated_workspace",
    "low_trust_boundary_mismatch",
    "low_trust_requires_sandbox_environment",
    "low_trust_runtime_services_denied",
];

/// How every failed-run reason begins, so the view can put failures that need a person first.
pub const FAILED_RUN: &str = "failed run ";

/// How the herald judges a board.
#[derive(Clone, Debug)]
pub struct HeraldConfig {
    /// The advisory promise every lane starts under (Watch mode has no claims).
    pub promise: Promise,
    /// How long a wait may run.
    pub waiting: Cadence,
    /// Failure kinds that need a person at once.
    pub never_clears: BTreeSet<String>,
}

fn ladder(base_minutes: u64) -> Cadence {
    let base = base_minutes * MINUTE;
    Cadence {
        call: Millis(base),
        again: Millis(base * 15 / 10),
        suspect: Millis(base * 2),
    }
}

impl HeraldConfig {
    /// The defaults with a lane's promise and the waiting threshold set in minutes, each
    /// escalating at 1×, 1.5× and 2×.
    #[must_use]
    pub fn with_minutes(lane: u64, waiting: u64) -> Self {
        let mut c = Self::default();
        c.promise.cadence = ladder(lane.max(1));
        c.promise.source = "watch, set on the command line".into();
        c.waiting = ladder(waiting.max(1));
        c
    }
}

impl Default for HeraldConfig {
    /// One hour for a lane's work and for a wait, escalating at 1×, 1.5× and 2×. These are
    /// placeholders until thresholds are derived from the board's history on first start
    /// (technical spec §15); every value is printed by `runo doctor`.
    fn default() -> Self {
        Self {
            promise: Promise {
                unit: "issue".into(),
                cadence: ladder(60),
                checkpoints: true,
                source: "watch default".into(),
            },
            waiting: ladder(60),
            never_clears: NEVER_CLEARS.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
}

/// What the herald has to say about a lane.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signal {
    /// A clock is overdue: the herald sings `Suspect` and the episode ledger opens or raises.
    Raised {
        /// The lane.
        lane: Lane,
        /// Which clock.
        clock: Clock,
        /// How far up the ladder.
        level: Level,
        /// How far past the threshold.
        overdue_by: Millis,
        /// One line on why, in board terms.
        reason: String,
        /// When the stall began: the same for every level of one stall, and on every replay of
        /// the same history, so it identifies the episode.
        since: Instant,
        /// Board time.
        at: Instant,
    },
    /// A raised clock cleared: the episode resolves.
    Cleared {
        /// The lane.
        lane: Lane,
        /// Which clock.
        clock: Clock,
        /// Why.
        why: Cleared,
        /// Board time.
        at: Instant,
    },
}

#[derive(Default)]
struct Seen {
    name: Option<String>,
    lane: Option<Lane>,
    mark: u64,
    /// The lane a never-clearing failure was raised on, until the issue ends or advances.
    failed: Option<Lane>,
}

/// The herald: a chorus of one over a board.
pub struct Herald {
    config: HeraldConfig,
    chorus: Chorus,
    issues: BTreeMap<String, Seen>,
    voice: SingerId,
}

impl Herald {
    /// A herald with this configuration; `voice` is its own singer id.
    #[must_use]
    pub fn new(config: HeraldConfig, voice: SingerId) -> Self {
        let chorus = Chorus::new(Config {
            reachability: None, // Watch: unknown, shown as such
            waiting: Some(config.waiting),
            ..Config::default()
        });
        Self {
            config,
            chorus,
            issues: BTreeMap::new(),
            voice,
        }
    }

    /// Learn an issue's display name ahead of its events: on a first start an issue's snapshot is
    /// dated at its last update, later than the runs replayed before it, and a failure on it must
    /// still be named and cleared by the issue.
    pub fn learn(&mut self, id: &str, name: &str) {
        self.issues.entry(id.to_owned()).or_default().name = Some(name.to_owned());
    }

    /// Read one board event. Events must arrive in board-time order.
    pub fn ingest(&mut self, ev: &BoardEvent) -> Vec<Signal> {
        let mut out = Vec::new();
        match ev {
            BoardEvent::Issue {
                id,
                name,
                status,
                agent,
                at,
            } => self.issue(id, name, status, agent.as_deref(), *at, &mut out),
            BoardEvent::RunFinished {
                id,
                agent,
                issue,
                liveness,
                reason,
                error_code,
                at,
                ..
            } => {
                let lane = issue
                    .as_ref()
                    .and_then(|i| self.issues.get(i))
                    .and_then(|s| s.lane.clone());
                if matches!(liveness.as_deref(), Some("advanced" | "completed"))
                    && let Some(lane) = &lane
                {
                    let seen = self
                        .issues
                        .entry(issue.clone().unwrap_or_default())
                        .or_default();
                    seen.mark += 1;
                    let mark = seen.mark;
                    if let Some(failed) = seen.failed.take() {
                        out.push(Signal::Cleared {
                            lane: failed,
                            clock: Clock::Task,
                            why: Cleared::Evidence,
                            at: *at,
                        });
                    }
                    self.infer(*at, lane, Evidence::Progressed(mark));
                }
                if let Some(code) = error_code
                    .as_deref()
                    .filter(|c| self.config.never_clears.contains(*c))
                {
                    let name = issue.as_ref().map(|i| {
                        self.issues
                            .get(i)
                            .and_then(|s| s.name.clone())
                            .unwrap_or_else(|| i.clone())
                    });
                    let lane = lane.unwrap_or_else(|| Lane {
                        singer: SingerId(agent.clone()),
                        task: TaskId(name.unwrap_or_else(|| "no issue".into())),
                    });
                    if let Some(i) = issue {
                        self.issues.entry(i.clone()).or_default().failed = Some(lane.clone());
                    }
                    let why = reason
                        .as_deref()
                        .map_or(String::new(), |r| format!(": {}", one_line(r)));
                    out.push(Signal::Raised {
                        lane,
                        clock: Clock::Task,
                        level: Level::Suspected,
                        overdue_by: Millis(0),
                        reason: format!(
                            "{FAILED_RUN}{id}: {code}, which Paperclip does not retry{why}"
                        ),
                        since: *at,
                        at: *at,
                    });
                }
            }
        }
        self.drain(ev.at(), &mut out);
        out
    }

    /// Advance to `now` and return what crossed or cleared since the last call.
    pub fn tick(&mut self, now: Instant) -> Vec<Signal> {
        let mut out = Vec::new();
        let escalations = self.chorus.tick(now);
        if !escalations.is_empty() {
            let rows = self.chorus.rows(now);
            for e in escalations.into_iter().filter(|e| e.level >= Level::Called) {
                let Some(task) = e.task else { continue };
                let blocked_on = rows
                    .iter()
                    .find(|r| r.peer == e.peer && r.task == task)
                    .and_then(|r| r.blocked_on.clone());
                let reason = match e.clock {
                    Clock::Waiting => match blocked_on.as_deref() {
                        Some("review") => "in review for longer than the waiting threshold".into(),
                        Some(b) => format!("{b} for longer than the waiting threshold"),
                        None => "waiting for longer than the waiting threshold".into(),
                    },
                    Clock::Progress => "no run has advanced it since the last progress".into(),
                    Clock::Task => "in progress for longer than the advisory promise".into(),
                    Clock::Reachability => "not heard from".into(),
                };
                let cadence = if e.clock == Clock::Waiting {
                    self.config.waiting
                } else {
                    self.config.promise.cadence
                };
                let behind = runo_clock::escalate::threshold_of(&cadence, e.level)
                    .0
                    .saturating_add(e.overdue_by.0);
                out.push(Signal::Raised {
                    lane: Lane {
                        singer: e.peer,
                        task,
                    },
                    clock: e.clock,
                    level: e.level,
                    overdue_by: e.overdue_by,
                    reason,
                    since: Instant(now.0.saturating_sub(behind.saturating_mul(1_000_000))),
                    at: now,
                });
            }
        }
        self.drain(now, &mut out);
        out
    }

    /// The clocks, for the view.
    #[must_use]
    pub fn chorus(&self) -> &Chorus {
        &self.chorus
    }

    fn issue(
        &mut self,
        id: &str,
        name: &str,
        status: &str,
        agent: Option<&str>,
        at: Instant,
        out: &mut Vec<Signal>,
    ) {
        let active = matches!(status, "in_progress" | "blocked" | "in_review");
        let lane = agent.map(|a| Lane {
            singer: SingerId(a.to_owned()),
            task: TaskId(name.to_owned()),
        });
        let seen = self.issues.entry(id.to_owned()).or_default();
        seen.name = Some(name.to_owned());
        if !active && let Some(failed) = seen.failed.take() {
            out.push(Signal::Cleared {
                lane: failed,
                clock: Clock::Task,
                why: Cleared::Completed,
                at,
            });
        }
        let old = seen.lane.clone();
        if let Some(old) = old.filter(|o| !active || lane.as_ref() != Some(o)) {
            self.issues.entry(id.to_owned()).or_default().lane = None;
            self.infer(at, &old, Evidence::Ended);
        }
        let Some(lane) = lane.filter(|_| active) else {
            return;
        };
        if self.issues.get(id).is_some_and(|s| s.lane.is_none()) {
            let promise = self.config.promise.clone();
            self.infer(at, &lane, Evidence::Started(&promise));
            if let Some(seen) = self.issues.get_mut(id) {
                seen.lane = Some(lane.clone());
            }
        }
        let what = match status {
            "blocked" => Evidence::Blocked("blocked on the board"),
            "in_review" => Evidence::Blocked("review"),
            _ => Evidence::Unblocked,
        };
        self.infer(at, &lane, what);
    }

    fn infer(&mut self, at: Instant, lane: &Lane, what: Evidence<'_>) {
        self.chorus.observe(
            at,
            &self.voice,
            Observation::Inferred {
                singer: &lane.singer,
                task: &lane.task,
                what,
            },
        );
    }

    fn drain(&mut self, at: Instant, out: &mut Vec<Signal>) {
        out.extend(
            self.chorus
                .take_resolutions()
                .into_iter()
                .map(|r| Signal::Cleared {
                    lane: Lane {
                        singer: r.peer,
                        task: r.task,
                    },
                    clock: r.clock,
                    why: r.why,
                    at,
                }),
        );
    }
}

/// The first line of a reason, at most 120 characters.
fn one_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(120) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}
