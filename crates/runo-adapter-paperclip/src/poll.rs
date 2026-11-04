//! Reading the board over the Paperclip API (`docs/paperclip-api-check.md`).
//!
//! [`Paperclip`] fetches the two lists the herald needs, newest first, one page each. Neither list
//! takes a `since`, so [`Feed`] turns successive pages into new [`BoardEvent`]s:
//!
//! - an issue is an event whenever its `updatedAt`, or the blocker state Paperclip computes when
//!   read, differs from the last page;
//! - a run is an event once, when it is first seen finished;
//! - a **gap** is reported when a full page does not reach back to the newest row of the previous
//!   page: rows were missed. The herald says so (`paperclip-gap`) rather than hiding it, because
//!   a herald that silently loses signals repeats the failure the replay measured.
//!
//! Only rows in the current page are remembered: a row that falls off a newest-first page comes
//! back only if it changes, and then it is new anyway.

use std::collections::{BTreeMap, BTreeSet};

use runo_core::time::Instant;

use crate::board::{Agent, BoardEvent, Company, Issue, Run};
use crate::time;

/// The largest page either list serves (Paperclip `f7ad1dc`: issues and runs, 1000).
pub const MAX_PAGE: usize = 1000;

/// A read that failed.
#[derive(Debug, thiserror::Error)]
pub enum PollError {
    /// The request did not complete.
    #[error("paperclip unreachable: {0}")]
    Http(#[from] reqwest::Error),
    /// The board asked us to slow down (`429` or `503`); back off and retry.
    #[error("paperclip asked to back off ({0})")]
    Throttled(u16),
    /// Any other refusal, with the body's first line.
    #[error("paperclip refused ({status}): {body}")]
    Refused {
        /// HTTP status.
        status: u16,
        /// The response body, first line.
        body: String,
    },
}

/// A Paperclip API client for one company. Read-only: it never writes to the board.
pub struct Paperclip {
    http: reqwest::Client,
    base: String,
    company: String,
    token: Option<String>,
    page: usize,
}

impl Paperclip {
    /// A client for `base` (such as `https://board.example`) and one company. The token is the
    /// key's value, read by the caller from the environment: secrets by reference only (§15).
    ///
    /// # Errors
    /// The HTTP client cannot be built (no TLS roots on the system).
    pub fn new(
        base: &str,
        company: &str,
        token: Option<String>,
        page: usize,
    ) -> Result<Self, PollError> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("runo-herald/", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base: base.trim_end_matches('/').to_owned(),
            company: company.to_owned(),
            token,
            page: page.clamp(1, MAX_PAGE),
        })
    }

    /// The page size in use.
    #[must_use]
    pub fn page(&self) -> usize {
        self.page
    }

    /// Issues, most recently updated first.
    ///
    /// # Errors
    /// See [`PollError`].
    pub async fn issues(&self) -> Result<Vec<Issue>, PollError> {
        let path = format!(
            "/api/companies/{}/issues?sortField=updated&sortDir=desc&limit={}",
            self.company, self.page
        );
        self.get(&path).await
    }

    /// Every open issue (in progress, blocked or in review), however long ago it last changed:
    /// a board larger than one page can hold its longest stalls outside the newest page.
    ///
    /// # Errors
    /// See [`PollError`].
    pub async fn open_issues(&self) -> Result<Vec<Issue>, PollError> {
        let path = format!(
            "/api/companies/{}/issues?status=in_progress,blocked,in_review&sortField=updated&sortDir=desc&limit={MAX_PAGE}",
            self.company
        );
        self.get(&path).await
    }

    /// Heartbeat runs, newest first.
    ///
    /// # Errors
    /// See [`PollError`].
    pub async fn runs(&self) -> Result<Vec<Run>, PollError> {
        let path = format!(
            "/api/companies/{}/heartbeat-runs?limit={}",
            self.company, self.page
        );
        self.get(&path).await
    }

    /// The companies this key may open, for finding the id `--company` takes.
    ///
    /// # Errors
    /// See [`PollError`].
    pub async fn companies(&self) -> Result<Vec<Company>, PollError> {
        self.get("/api/companies?scope=accessible").await
    }

    /// This client's company, for its name and issue prefix.
    ///
    /// # Errors
    /// See [`PollError`].
    pub async fn company(&self) -> Result<Company, PollError> {
        self.get(&format!("/api/companies/{}", self.company)).await
    }

    /// The company's agents, for names in the view. The route takes no parameters.
    ///
    /// # Errors
    /// See [`PollError`].
    pub async fn agents(&self) -> Result<Vec<Agent>, PollError> {
        self.get(&format!("/api/companies/{}/agents", self.company))
            .await
    }

    /// The server's version, from `GET /api/health`, for `runo doctor`.
    ///
    /// # Errors
    /// See [`PollError`].
    pub async fn version(&self) -> Result<Option<String>, PollError> {
        #[derive(serde::Deserialize)]
        struct Health {
            version: Option<String>,
        }
        Ok(self.get::<Health>("/api/health").await?.version)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, PollError> {
        let mut req = self.http.get(format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        match status {
            200..=299 => Ok(resp.json().await?),
            429 | 503 => Err(PollError::Throttled(status)),
            _ => Err(PollError::Refused {
                status,
                body: resp
                    .text()
                    .await
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect(),
            }),
        }
    }
}

/// Which list lost rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum List {
    /// The issue list.
    Issues,
    /// The run list.
    Runs,
}

/// Rows were missed between two polls: the board changed more than one page's worth.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Gap {
    /// Which list.
    pub list: List,
    /// The newest row of the previous page.
    pub after: Instant,
    /// The oldest row of this page; what lay between was not seen.
    pub before: Instant,
}

/// Successive pages to new events.
#[derive(Default)]
pub struct Feed {
    issues: BTreeMap<String, String>,
    runs: BTreeSet<String>,
    newest_issue: Option<Instant>,
    newest_run: Option<Instant>,
}

impl Feed {
    /// The new events in these pages, in board-time order, and any gap. `issues` is the newest
    /// page by update, `open` every open issue; an issue in both is read once. `page` is the
    /// page size the lists were fetched with; the gap check uses the newest page only.
    pub fn take(
        &mut self,
        issues: &[Issue],
        open: &[Issue],
        runs: &[Run],
        page: usize,
    ) -> (Vec<BoardEvent>, Vec<Gap>) {
        let mut events = Vec::new();
        let mut gaps = Vec::new();

        let stamps: Vec<Instant> = issues
            .iter()
            .filter_map(|i| time::parse(&i.updated_at))
            .collect();
        if let Some(g) = gap(List::Issues, self.newest_issue, &stamps, page) {
            gaps.push(g);
        }
        self.newest_issue = stamps.iter().copied().max().or(self.newest_issue);
        let mut seen = BTreeMap::new();
        for i in issues.iter().chain(open) {
            if seen.contains_key(&i.id) {
                continue;
            }
            let print = i.fingerprint();
            if self.issues.get(&i.id) != Some(&print) {
                events.extend(i.event());
            }
            seen.insert(i.id.clone(), print);
        }
        self.issues = seen;

        let stamps: Vec<Instant> = runs
            .iter()
            .filter_map(|r| time::parse(r.created_at.as_deref()?))
            .collect();
        if let Some(g) = gap(List::Runs, self.newest_run, &stamps, page) {
            gaps.push(g);
        }
        self.newest_run = stamps.iter().copied().max().or(self.newest_run);
        let mut seen = BTreeSet::new();
        for r in runs.iter().filter(|r| r.finished_at.is_some()) {
            if !self.runs.contains(&r.id) {
                events.extend(r.event());
            }
            seen.insert(r.id.clone());
        }
        self.runs = seen;

        events.sort_by_key(BoardEvent::at);
        (events, gaps)
    }
}

fn gap(list: List, previous: Option<Instant>, stamps: &[Instant], page: usize) -> Option<Gap> {
    let after = previous?;
    let before = *stamps.iter().min()?;
    (stamps.len() >= page && before > after).then_some(Gap {
        list,
        after,
        before,
    })
}
