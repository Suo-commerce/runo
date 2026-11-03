#!/usr/bin/env python3
"""Export Forgejo PR, review and CI-status records for the Runo replay.

Usage:
  FORGEJO_URL=https://git.example.fi FORGEJO_TOKEN=... \
  REPOS=org/repo1,org/repo2 FROM=2026-10-27 TO=2026-11-01 OUT=./forgejo-corpus \
  python3 export-forgejo.py

Read-only: GET requests only. Writes one CSV per record type into $OUT.
Needs a token with read access to the repositories (read:repository is enough).
"""
import csv, json, os, sys, urllib.request, urllib.parse
from datetime import datetime, timezone

URL = os.environ.get("FORGEJO_URL", "").rstrip("/")
TOKEN = os.environ.get("FORGEJO_TOKEN", "")
REPOS = [r.strip() for r in os.environ.get("REPOS", "").split(",") if r.strip()]
FROM = os.environ.get("FROM", "2026-10-27")
TO = os.environ.get("TO", "2026-11-01")
OUT = os.environ.get("OUT", "./forgejo-corpus")

if not URL or not TOKEN or not REPOS:
    sys.exit("set FORGEJO_URL, FORGEJO_TOKEN and REPOS (owner/repo,owner/repo)")

os.makedirs(OUT, exist_ok=True)
t_from = datetime.fromisoformat(FROM).replace(tzinfo=timezone.utc)
t_to = datetime.fromisoformat(TO).replace(tzinfo=timezone.utc)


def get(path, **params):
    q = urllib.parse.urlencode(params)
    req = urllib.request.Request(f"{URL}/api/v1{path}?{q}" if q else f"{URL}/api/v1{path}")
    req.add_header("Authorization", f"token {TOKEN}")
    req.add_header("Accept", "application/json")
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def paged(path, **params):
    page = 1
    while True:
        batch = get(path, page=page, limit=50, **params)
        if not batch:
            return
        yield from batch
        if len(batch) < 50:
            return
        page += 1


def ts(s):
    if not s:
        return None
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def in_window(s):
    t = ts(s)
    return t is not None and t_from <= t < t_to


class Sink:
    def __init__(self, name, fields):
        self.f = open(os.path.join(OUT, f"{name}.csv"), "w", newline="")
        self.w = csv.DictWriter(self.f, fieldnames=fields)
        self.w.writeheader()
        self.n = 0
        self.name = name

    def row(self, **kw):
        self.w.writerow(kw)
        self.n += 1

    def close(self):
        self.f.close()
        print(f"{self.name:<22} {self.n:>6} rows")


pulls = Sink("pulls", ["repo", "number", "title", "state", "merged", "head_sha", "head_branch", "base_branch",
                       "user", "created_at", "updated_at", "merged_at", "closed_at", "comments", "mergeable", "url"])
reviews = Sink("pr_reviews", ["repo", "number", "review_id", "user", "state", "commit_id", "submitted_at", "body_chars"])
comments = Sink("pr_comments", ["repo", "number", "comment_id", "user", "created_at", "body_chars", "body_head"])
statuses = Sink("commit_statuses", ["repo", "number", "sha", "context", "status", "created_at", "target_url"])
timeline = Sink("pr_timeline", ["repo", "number", "event_id", "type", "user", "created_at", "ref_commit_sha"])

for repo in REPOS:
    seen_shas = set()
    for pr in paged(f"/repos/{repo}/pulls", state="all", sort="recentupdate"):
        if not (in_window(pr.get("created_at")) or in_window(pr.get("updated_at"))
                or in_window(pr.get("merged_at")) or in_window(pr.get("closed_at"))):
            # recentupdate sort: once we are older than the window we can stop
            if ts(pr.get("updated_at")) and ts(pr["updated_at"]) < t_from:
                break
            continue
        n = pr["number"]
        head = pr.get("head") or {}
        base = pr.get("base") or {}
        pulls.row(repo=repo, number=n, title=pr.get("title", ""), state=pr.get("state"), merged=pr.get("merged"),
                  head_sha=head.get("sha"), head_branch=head.get("ref"), base_branch=base.get("ref"),
                  user=(pr.get("user") or {}).get("login"), created_at=pr.get("created_at"),
                  updated_at=pr.get("updated_at"), merged_at=pr.get("merged_at"), closed_at=pr.get("closed_at"),
                  comments=pr.get("comments"), mergeable=pr.get("mergeable"), url=pr.get("html_url"))

        for rv in paged(f"/repos/{repo}/pulls/{n}/reviews"):
            reviews.row(repo=repo, number=n, review_id=rv.get("id"), user=(rv.get("user") or {}).get("login"),
                        state=rv.get("state"), commit_id=rv.get("commit_id"), submitted_at=rv.get("submitted_at"),
                        body_chars=len(rv.get("body") or ""))

        for c in paged(f"/repos/{repo}/issues/{n}/comments"):
            body = c.get("body") or ""
            comments.row(repo=repo, number=n, comment_id=c.get("id"), user=(c.get("user") or {}).get("login"),
                         created_at=c.get("created_at"), body_chars=len(body), body_head=body[:300].replace("\n", " "))

        try:
            for ev in paged(f"/repos/{repo}/issues/{n}/timeline"):
                timeline.row(repo=repo, number=n, event_id=ev.get("id"), type=ev.get("type"),
                             user=(ev.get("user") or {}).get("login"), created_at=ev.get("created_at"),
                             ref_commit_sha=ev.get("ref_commit_sha"))
        except Exception as e:  # timeline is optional on older Forgejo
            print(f"timeline unavailable for {repo}#{n}: {e}", file=sys.stderr)

        # CI statuses for every head commit this PR had: current head plus any from the timeline/reviews
        shas = {head.get("sha")} | {rv.get("commit_id") for rv in paged(f"/repos/{repo}/pulls/{n}/reviews")}
        for sha in filter(None, shas):
            if sha in seen_shas:
                continue
            seen_shas.add(sha)
            try:
                for st in paged(f"/repos/{repo}/statuses/{sha}"):
                    statuses.row(repo=repo, number=n, sha=sha, context=st.get("context"), status=st.get("status"),
                                 created_at=st.get("created_at"), target_url=st.get("target_url"))
            except Exception as e:
                print(f"statuses unavailable for {sha[:8]}: {e}", file=sys.stderr)

for s in (pulls, reviews, comments, statuses, timeline):
    s.close()
print(f"Done: {OUT}")
