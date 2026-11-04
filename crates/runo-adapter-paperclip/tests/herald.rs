//! The herald over board-shaped JSON: what is said, when, and what is never said on sight.

use runo_adapter_paperclip::board::{BoardEvent, Issue, Run};
use runo_adapter_paperclip::herald::{Herald, HeraldConfig, Signal};
use runo_adapter_paperclip::time;
use runo_clock::{Cleared, ClockState, Level};
use runo_core::time::Instant;
use runo_core::{Clock, SingerId};

const MIN: u64 = 60_000_000_000;

fn t0() -> Instant {
    time::parse("2026-11-03T08:00:00Z").unwrap()
}

fn stamp(minutes: u64) -> String {
    let (h, m) = (8 + minutes / 60, minutes % 60);
    format!("2026-11-03T{h:02}:{m:02}:00.000Z")
}

fn at(minutes: u64) -> Instant {
    Instant(t0().0 + minutes * MIN)
}

/// An issue record as the API returns it, with fields the herald does not declare.
fn issue(status: &str, agent: &str, minutes: u64) -> BoardEvent {
    let json = format!(
        r#"{{"id":"iss-1","companyId":"c","identifier":"HOD-43","title":"Fix the build",
            "status":"{status}","priority":"high","assigneeAgentId":"{agent}",
            "executionLockedAt":null,"updatedAt":"{}","statusVersion":7}}"#,
        stamp(minutes)
    );
    serde_json::from_str::<Issue>(&json)
        .unwrap()
        .event()
        .unwrap()
}

fn run(liveness: &str, error_code: Option<&str>, minutes: u64) -> BoardEvent {
    let code = error_code.map_or("null".into(), |c| format!("\"{c}\""));
    let json = format!(
        r#"{{"id":"run-{minutes}","companyId":"c","agentId":"builder","issueId":"iss-1",
            "status":"failed","livenessState":"{liveness}","livenessReason":"agent stopped\nsecond line",
            "errorCode":{code},"error":null,"finishedAt":"{}","usageJson":{{"tokens":3}}}}"#,
        stamp(minutes)
    );
    serde_json::from_str::<Run>(&json).unwrap().event().unwrap()
}

fn herald() -> Herald {
    Herald::new(HeraldConfig::default(), SingerId("herald".into()))
}

fn raised(signals: &[Signal]) -> Vec<(Clock, Level)> {
    signals
        .iter()
        .filter_map(|s| match s {
            Signal::Raised { clock, level, .. } => Some((*clock, *level)),
            Signal::Cleared { .. } => None,
        })
        .collect()
}

#[test]
fn a_blocked_issue_is_a_wait_and_is_said_only_when_overdue() {
    let mut h = herald();
    assert!(
        h.ingest(&issue("blocked", "builder", 0)).is_empty(),
        "never on sight"
    );
    assert!(
        h.tick(at(60)).is_empty(),
        "crossing the threshold gives the lane one more cadence"
    );
    assert!(h.tick(at(119)).is_empty(), "still inside that cadence");
    let s = h.tick(at(120));
    assert_eq!(raised(&s), [(Clock::Waiting, Level::Called)]);
    let Signal::Raised { lane, reason, .. } = &s[0] else {
        unreachable!()
    };
    assert_eq!(
        (lane.singer.0.as_str(), lane.task.0.as_str()),
        ("builder", "HOD-43")
    );
    assert!(reason.contains("blocked on the board"), "{reason}");

    let s = h.ingest(&issue("in_progress", "builder", 130));
    assert!(
        s.iter().any(|x| matches!(
            x,
            Signal::Cleared {
                clock: Clock::Waiting,
                why: Cleared::Evidence,
                ..
            }
        )),
        "{s:?}"
    );
}

#[test]
fn a_failure_paperclip_never_retries_is_said_at_once_and_clears_when_the_issue_ends() {
    let mut h = herald();
    h.ingest(&issue("in_progress", "builder", 0));
    let s = h.ingest(&run("failed", Some("budget_exhausted"), 5));
    assert_eq!(raised(&s), [(Clock::Task, Level::Suspected)]);
    let Signal::Raised { reason, .. } = &s[0] else {
        unreachable!()
    };
    assert!(
        reason.contains("budget_exhausted") && reason.ends_with("agent stopped"),
        "one line of Paperclip's reason: {reason}"
    );
    let s = h.ingest(&issue("done", "builder", 30));
    assert!(
        s.iter().any(|x| matches!(
            x,
            Signal::Cleared {
                clock: Clock::Task,
                why: Cleared::Completed,
                ..
            }
        )),
        "{s:?}"
    );
    assert!(
        h.chorus().rows(at(30)).is_empty(),
        "a done issue leaves the view"
    );
}

#[test]
fn a_failure_paperclip_retries_is_left_to_the_clocks() {
    let mut h = herald();
    h.ingest(&issue("in_progress", "builder", 0));
    assert!(
        h.ingest(&run("failed", Some("adapter_failed"), 5))
            .is_empty(),
        "Paperclip retries adapter_failed itself"
    );
    assert!(
        h.ingest(&run("failed", None, 6)).is_empty(),
        "a failure with no kind is left to the clocks"
    );
}

#[test]
fn advancing_runs_keep_progress_quiet_and_silence_does_not() {
    let mut h = herald();
    h.ingest(&issue("in_progress", "builder", 0));
    h.ingest(&run("advanced", None, 50));
    h.ingest(&run("completed", None, 100));
    let s = raised(&h.tick(at(160)));
    assert!(
        !s.iter().any(|(c, _)| *c == Clock::Progress),
        "progress at 100, so nothing on that clock until 220: {s:?}"
    );
    assert!(
        s.contains(&(Clock::Task, Level::Called)),
        "the task clock runs on: {s:?}"
    );
    let s = raised(&h.tick(at(220)));
    assert!(s.contains(&(Clock::Progress, Level::Called)), "{s:?}");
    assert!(
        s.contains(&(Clock::Task, Level::Suspected)),
        "the task clock never resets: {s:?}"
    );
}

#[test]
fn a_reassigned_issue_ends_one_lane_and_starts_another() {
    let mut h = herald();
    h.ingest(&issue("in_progress", "builder", 0));
    h.ingest(&issue("in_progress", "fixer", 10));
    let rows = h.chorus().rows(at(10));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].peer.0, "fixer");
    assert_eq!(
        rows[0].clocks[0],
        ClockState::Unknown,
        "Watch shows reachability as unknown"
    );
}

#[test]
fn review_is_a_wait_and_an_unfinished_run_is_no_event() {
    let mut h = herald();
    h.ingest(&issue("in_review", "builder", 0));
    assert_eq!(
        h.chorus().rows(at(1))[0].blocked_on.as_deref(),
        Some("review")
    );
    let unfinished: Run = serde_json::from_str(
        r#"{"id":"r","agentId":"builder","issueId":"iss-1","status":"running","finishedAt":null}"#,
    )
    .unwrap();
    assert_eq!(unfinished.event(), None);
}
