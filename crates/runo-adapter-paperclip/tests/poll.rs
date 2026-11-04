//! The poller against a fake Paperclip server, and the feed's dedup and gap rules.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runo_adapter_paperclip::board::{BoardEvent, Issue, Run};
use runo_adapter_paperclip::poll::{Feed, Gap, List, Paperclip, PollError};
use runo_adapter_paperclip::time;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A one-route-per-path HTTP/1.1 server: `routes` maps a path prefix to (status, body). Every
/// request's path and `Authorization` header are recorded.
async fn fake(routes: BTreeMap<&'static str, (u16, String)>) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = req.split_whitespace().nth(1).unwrap_or_default().to_owned();
            let auth = req
                .lines()
                .find_map(|l| {
                    l.strip_prefix("authorization: ")
                        .or_else(|| l.strip_prefix("Authorization: "))
                })
                .map(str::to_owned);
            log.lock().unwrap().push((path.clone(), auth));
            let (status, body) = routes
                .iter()
                .find(|(p, _)| path.starts_with(**p))
                .map_or((404, "not found".to_owned()), |(_, r)| r.clone());
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (base, seen)
}

const ISSUES: &str = r#"[
  {"id":"i2","identifier":"HOD-44","status":"blocked","assigneeAgentId":"a","updatedAt":"2026-11-03T09:00:00Z","title":"x"},
  {"id":"i1","identifier":"HOD-43","status":"in_progress","assigneeAgentId":"a","updatedAt":"2026-11-03T08:00:00Z"}
]"#;
const RUNS: &str = r#"[
  {"id":"r2","agentId":"a","issueId":"i1","status":"running","createdAt":"2026-11-03T08:30:00Z","finishedAt":null},
  {"id":"r1","agentId":"a","issueId":"i1","status":"succeeded","livenessState":"advanced","createdAt":"2026-11-03T08:10:00Z","finishedAt":"2026-11-03T08:20:00Z"}
]"#;

#[tokio::test]
async fn reads_both_lists_with_the_key_newest_first() {
    let (base, seen) = fake(BTreeMap::from([
        ("/api/companies/c1/issues", (200, ISSUES.to_owned())),
        ("/api/companies/c1/heartbeat-runs", (200, RUNS.to_owned())),
        (
            "/api/health",
            (200, r#"{"status":"ok","version":"0.3.1"}"#.to_owned()),
        ),
    ]))
    .await;
    let p = Paperclip::new(&format!("{base}/"), "c1", Some("k3y".into()), 5000).unwrap();
    assert_eq!(
        p.page(),
        1000,
        "clamped to the largest page the board serves"
    );
    let issues = p.issues().await.unwrap();
    let runs = p.runs().await.unwrap();
    assert_eq!((issues.len(), runs.len()), (2, 2));
    assert_eq!(p.version().await.unwrap().as_deref(), Some("0.3.1"));

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].0,
        "/api/companies/c1/issues?sortField=updated&sortDir=desc&limit=1000"
    );
    assert_eq!(seen[1].0, "/api/companies/c1/heartbeat-runs?limit=1000");
    assert!(
        seen.iter().all(|(_, a)| a.as_deref() == Some("Bearer k3y")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn backs_off_on_429_and_503_and_names_any_other_refusal() {
    let (base, _) = fake(BTreeMap::from([
        ("/api/companies/busy/", (429, String::new())),
        ("/api/companies/down/", (503, String::new())),
        (
            "/api/companies/nokey/",
            (401, "{\"error\":\"Unauthorized\"}\nmore".to_owned()),
        ),
    ]))
    .await;
    let get = |c: &str| Paperclip::new(&base, c, None, 10).unwrap();
    assert!(matches!(
        get("busy").runs().await,
        Err(PollError::Throttled(429))
    ));
    assert!(matches!(
        get("down").issues().await,
        Err(PollError::Throttled(503))
    ));
    match get("nokey").issues().await {
        Err(PollError::Refused { status: 401, body }) => {
            assert_eq!(body, "{\"error\":\"Unauthorized\"}");
        }
        other => panic!("{other:?}"),
    }
}

fn issues(json: &str) -> Vec<Issue> {
    serde_json::from_str(json).unwrap()
}

fn runs(json: &str) -> Vec<Run> {
    serde_json::from_str(json).unwrap()
}

#[test]
fn each_change_is_one_event_in_board_time_order() {
    let mut feed = Feed::default();
    let (ev, gaps) = feed.take(&issues(ISSUES), &[], &runs(RUNS), 10);
    assert_eq!(gaps, []);
    let order: Vec<String> = ev
        .iter()
        .map(|e| match e {
            BoardEvent::Issue { name, .. } => name.clone(),
            BoardEvent::RunFinished { id, .. } => id.clone(),
        })
        .collect();
    assert_eq!(
        order,
        ["HOD-43", "r1", "HOD-44"],
        "a running run is not an event yet"
    );

    let (ev, _) = feed.take(&issues(ISSUES), &[], &runs(RUNS), 10);
    assert_eq!(ev, [], "the same pages again say nothing new");

    let changed = ISSUES
        .replace("09:00:00Z\",\"title", "09:30:00Z\",\"title")
        .replace("\"blocked\"", "\"in_progress\"");
    let finished = RUNS.replace("\"status\":\"running\",\"createdAt\":\"2026-11-03T08:30:00Z\",\"finishedAt\":null", "\"status\":\"failed\",\"createdAt\":\"2026-11-03T08:30:00Z\",\"finishedAt\":\"2026-11-03T09:10:00Z\"");
    let (ev, _) = feed.take(&issues(&changed), &[], &runs(&finished), 10);
    assert_eq!(ev.len(), 2, "HOD-44 changed and r2 finished: {ev:?}");
}

#[test]
fn a_full_page_that_does_not_reach_back_is_a_gap() {
    let mut feed = Feed::default();
    feed.take(&issues(ISSUES), &[], &runs(RUNS), 2);
    let later = r#"[
      {"id":"i4","status":"todo","updatedAt":"2026-11-03T12:00:00Z"},
      {"id":"i3","status":"todo","updatedAt":"2026-11-03T11:00:00Z"}
    ]"#;
    let (_, gaps) = feed.take(&issues(later), &[], &runs(RUNS), 2);
    assert_eq!(
        gaps,
        [Gap {
            list: List::Issues,
            after: time::parse("2026-11-03T09:00:00Z").unwrap(),
            before: time::parse("2026-11-03T11:00:00Z").unwrap(),
        }]
    );
    let (_, gaps) = feed.take(&issues(later), &[], &runs(RUNS), 3);
    assert_eq!(gaps, [], "a page that is not full holds everything");
}

#[test]
fn an_old_open_issue_off_the_newest_page_is_still_read_once() {
    let mut feed = Feed::default();
    let newest = issues(ISSUES);
    let open = issues(
        r#"[
      {"id":"i9","identifier":"HOD-9","status":"blocked","assigneeAgentId":"a","updatedAt":"2026-10-01T08:00:00Z"},
      {"id":"i2","identifier":"HOD-44","status":"blocked","assigneeAgentId":"a","updatedAt":"2026-11-03T09:00:00Z"}
    ]"#,
    );
    let (ev, gaps) = feed.take(&newest, &open, &[], 2);
    let names: Vec<&str> = ev
        .iter()
        .filter_map(|e| match e {
            BoardEvent::Issue { name, .. } => Some(name.as_str()),
            BoardEvent::RunFinished { .. } => None,
        })
        .collect();
    assert_eq!(
        names,
        ["HOD-9", "HOD-43", "HOD-44"],
        "the month-old block is read, and HOD-44, on both lists, once"
    );
    assert_eq!(
        gaps,
        [],
        "the open list does not count towards the gap check"
    );
    let (ev, _) = feed.take(&newest, &open, &[], 2);
    assert_eq!(ev, [], "nothing changed");
}
