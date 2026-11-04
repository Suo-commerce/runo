//! `runo watch`'s loop body on board-shaped pages: first start, restart, crash and escalation.

use std::path::{Path, PathBuf};

use runo_adapter_paperclip::board::{Agent, Issue, Run};
use runo_adapter_paperclip::herald::HeraldConfig;
use runo_adapter_paperclip::time;
use runo_adapter_paperclip::watch::Watch;
use runo_core::time::Instant;

const MIN: u64 = 60_000_000_000;
const BASE: &str = "https://bob.example";

fn at(minutes: u64) -> Instant {
    Instant(time::parse("2026-11-08T06:00:00Z").unwrap().0 + minutes * MIN)
}

fn stamp(minutes: u64) -> String {
    time::format(at(minutes))
}

fn issue(id: &str, name: &str, status: &str, minutes: u64) -> String {
    format!(
        r#"{{"id":"{id}","identifier":"{name}","status":"{status}","assigneeAgentId":"ag-1","updatedAt":"{}"}}"#,
        stamp(minutes)
    )
}

fn page(rows: &[String]) -> Vec<Issue> {
    serde_json::from_str(&format!("[{}]", rows.join(","))).unwrap()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("runo-watch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn open(d: &Path) -> Watch {
    let mut w = Watch::open(HeraldConfig::default(), d, BASE).unwrap();
    w.set_agents(&[Agent {
        id: "ag-1".into(),
        name: "Bob".into(),
    }]);
    w
}

/// A first page: HOD-1 is done now, but its run failed at 10 minutes with a kind Paperclip never
/// retries (a stall that began and ended before the herald started); HOD-2 is blocked since 0.
fn first_page() -> (Vec<Issue>, Vec<Run>) {
    let issues = page(&[
        issue("i1", "HOD-1", "done", 200),
        issue("i2", "HOD-2", "blocked", 0),
    ]);
    let runs = serde_json::from_str(&format!(
        r#"[{{"id":"r1","agentId":"ag-1","issueId":"i1","status":"failed","livenessState":"failed",
            "errorCode":"setup_failed","createdAt":"{}","finishedAt":"{}"}}]"#,
        stamp(5),
        stamp(10)
    ))
    .unwrap();
    (issues, runs)
}

#[test]
fn a_first_start_sends_what_is_stuck_now_and_keeps_history_quiet() {
    let d = dir("first");
    let mut w = open(&d);
    let (issues, runs) = first_page();
    let out = w.step(&issues, &runs, 10, at(300)).unwrap();
    assert_eq!(out.len(), 1, "only HOD-2 is stuck now: {out:?}");
    assert_eq!(
        out[0].text,
        "HOD-2 · Bob · waiting needs a person: blocked on the board for longer than the waiting threshold\n  https://bob.example/issues/HOD-2"
    );
    let view = w.view(at(300));
    assert!(view.contains("Open (1), worst first"), "{view}");
    assert!(
        view.contains("Resolved: 1 (1 before the herald started"),
        "{view}"
    );
    assert!(view.contains("reachability unknown"), "{view}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_restart_replays_history_without_repeating_a_message() {
    let d = dir("restart");
    let mut w = open(&d);
    let (issues, runs) = first_page();
    for o in w.step(&issues, &runs, 10, at(300)).unwrap() {
        w.delivered(&o.notice).unwrap();
    }
    drop(w);

    let mut again = open(&d);
    let out = again.step(&issues, &runs, 10, at(305)).unwrap();
    assert_eq!(
        out,
        [],
        "the same history is the same episodes, already told"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_crash_before_recording_delivery_resends_the_same_message() {
    let d = dir("crash");
    let mut w = open(&d);
    let first = w
        .step(
            &page(&[issue("i2", "HOD-2", "blocked", 0)]),
            &[],
            10,
            at(130),
        )
        .unwrap();
    assert_eq!(first.len(), 1);
    drop(w); // delivered, then the process died before `delivered` was saved

    let mut again = open(&d);
    let out = again
        .step(
            &page(&[issue("i2", "HOD-2", "blocked", 0)]),
            &[],
            10,
            at(131),
        )
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].notice.key(),
        first[0].notice.key(),
        "same key, so the channel drops the copy"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_stall_getting_worse_is_one_more_message_and_a_failure_comes_first() {
    let d = dir("worse");
    let mut w = open(&d);
    let issues = page(&[
        issue("i2", "HOD-2", "blocked", 0),
        issue("i1", "HOD-1", "in_progress", 100),
    ]);
    let out = w.step(&issues, &[], 10, at(125)).unwrap();
    assert_eq!(out.len(), 1, "{out:?}");
    w.delivered(&out[0].notice).unwrap();
    assert_eq!(
        w.step(&issues, &[], 10, at(140)).unwrap(),
        [],
        "nothing new yet"
    );

    let failed: Vec<Run> = serde_json::from_str(&format!(
        r#"[{{"id":"r9","agentId":"ag-1","issueId":"i1","status":"failed","livenessState":"failed",
            "errorCode":"budget_exhausted","error":"monthly budget reached","createdAt":"{}","finishedAt":"{}"}}]"#,
        stamp(140),
        stamp(145)
    ))
    .unwrap();
    let out = w.step(&issues, &failed, 10, at(160)).unwrap();
    let texts: Vec<&str> = out.iter().map(|o| o.text.as_str()).collect();
    assert_eq!(out.len(), 2, "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|t| t
                .starts_with("HOD-1 · Bob · task needs a person: failed run r9: budget_exhausted")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("HOD-2 · Bob · waiting still overdue")),
        "{texts:?}"
    );
    let view = w.view(at(160));
    let hod1 = view.find("HOD-1").unwrap();
    let hod2 = view.find("HOD-2").unwrap();
    assert!(
        hod1 < hod2,
        "a failure that needs a person is listed first:\n{view}"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn links_open_the_boards_own_page_under_the_company_prefix() {
    let d = dir("prefix");
    let mut w = open(&d);
    w.set_prefix("HOD");
    let out = w
        .step(
            &page(&[issue("i2", "HOD-2", "blocked", 0)]),
            &[],
            10,
            at(130),
        )
        .unwrap();
    assert!(
        out[0]
            .text
            .ends_with("\n  https://bob.example/HOD/issues/HOD-2"),
        "{}",
        out[0].text
    );
    std::fs::remove_dir_all(&d).unwrap();
}
