#!/usr/bin/env python3
"""Runo replay v0: run the Core clocks over an exported Paperclip (+ Forgejo) corpus.

Usage:
  python3 replay.py --paperclip ./paperclip-corpus [--forgejo ./forgejo-corpus] [--out ./replay-out]

What it does
  Rebuilds, per lane (issue) and per run, the four Core clocks from the spec:
    reachability  last run event / output from the run
    progress      last new deliverable: PR updated, commit status, CI result, run 'advanced'
    task          time since the run started, against a fixed promise
    waiting       time in 'blocked' status
  and applies the Core escalation: call at 1x cadence, again at 1.5x, suspect at 2x.

  It also computes Paperclip's own baseline (liveness_state, recovery actions, watchdog
  decisions) and the time of first human attention (first owner/user comment on the issue
  after the stall began), so each Runo detection can be compared with both.

Outputs (in --out)
  detections.csv   one row per clock crossing: when, lane, clock, level, evidence
  baseline.csv     Paperclip's own signals with timestamps
  summary.txt      counts, lead times, and the gray-work signals (looping, spawning, not starting)

Fixed Core cadences (minutes) are at the top of the file; change them there, not in the data.
"""
import argparse, csv, os, sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone

# ---- Core fixed cadences (v0.1 values), in minutes ------------------------------------
CADENCE = {
    "reachability": 15,     # a running lane should produce output at least this often
    "progress": 30,         # 'building' without a new deliverable
    "task": 90,             # a single run longer than this is overdue (post-mortem: 60-90 min)
    "waiting": 60,          # blocked without a change
}
ESCALATION = [(1.0, "call"), (1.5, "call-again"), (2.0, "suspect")]
GREEN_PR_WAIT_H = 24        # pattern 19: green CI, open PR, no merge


def ts(s):
    if not s:
        return None
    s = s.strip()
    if not s:
        return None
    try:
        t = datetime.fromisoformat(s.replace("Z", "+00:00"))
    except ValueError:
        return None
    return t if t.tzinfo else t.replace(tzinfo=timezone.utc)


def read(path):
    if not os.path.exists(path):
        print(f"missing {path}, skipping", file=sys.stderr)
        return []
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--paperclip", required=True)
    ap.add_argument("--forgejo")
    ap.add_argument("--out", default="./replay-out")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    P = lambda n: read(os.path.join(a.paperclip, f"{n}.csv"))
    F = (lambda n: read(os.path.join(a.forgejo, f"{n}.csv"))) if a.forgejo else (lambda n: [])

    agents = {r["id"]: r for r in P("agents")}
    issues = {r["id"]: r for r in P("issues")}
    runs = P("heartbeat_runs")
    events = P("heartbeat_run_events")
    comments = P("issue_comments")
    activity = P("activity_log")
    decisions = P("issue_execution_decisions")
    products = P("issue_work_products")
    relations = P("issue_relations")
    recovery = P("issue_recovery_actions")
    watchdog = P("heartbeat_run_watchdog_decisions")
    pulls = F("pulls")
    statuses = F("commit_statuses")

    name = lambda i: (issues.get(i, {}).get("identifier") or i or "?")
    agent = lambda i: (agents.get(i, {}).get("name") or i or "?")

    # ---- observations per issue: progress and reachability -----------------------------
    progress_obs = defaultdict(list)   # issue -> [(t, evidence)]
    reach_obs = defaultdict(list)      # run -> [t]
    for e in events:
        t = ts(e["created_at"])
        if t:
            reach_obs[e["run_id"]].append(t)
    for p in products:
        for col in ("created_at", "updated_at"):
            t = ts(p[col])
            if t:
                progress_obs[p["issue_id"]].append((t, f"work_product {p['type']} {p['status']}/{p['review_state']}"))
    for r in runs:
        t = ts(r["finished_at"])
        if t and r["liveness_state"] in ("advanced", "completed"):
            progress_obs[r["issue_id"]].append((t, f"run {r['liveness_state']}"))
    # Forgejo: PR updates and CI statuses count as progress for the PR's issue, if the
    # branch name carries the issue identifier (post-mortem pattern 27 says it should).
    ident_to_issue = {v["identifier"].lower(): k for k, v in issues.items() if v.get("identifier")}
    pr_issue = {}
    for pr in pulls:
        branch = (pr.get("head_branch") or "").lower()
        for ident, iid in ident_to_issue.items():
            if ident in branch:
                pr_issue[(pr["repo"], pr["number"])] = iid
                for col in ("created_at", "updated_at"):
                    t = ts(pr[col])
                    if t:
                        progress_obs[iid].append((t, f"PR #{pr['number']} {col}"))
                break
    for st in statuses:
        iid = pr_issue.get((st["repo"], st["number"]))
        t = ts(st["created_at"])
        if iid and t:
            progress_obs[iid].append((t, f"CI {st['context']} {st['status']}"))
    for k in progress_obs:
        progress_obs[k].sort()

    # ---- human attention: first owner/user comment per issue after a given time --------
    human_comments = defaultdict(list)
    for c in comments:
        if c["author_type"] == "user" or c.get("author_user_id"):
            t = ts(c["created_at"])
            if t:
                human_comments[c["issue_id"]].append(t)
    for k in human_comments:
        human_comments[k].sort()

    def first_human_after(issue, t):
        for h in human_comments.get(issue, []):
            if h >= t:
                return h
        return None

    detections = []

    def detect(when, issue, clock, level, evidence, run_id=""):
        h = first_human_after(issue, when)
        detections.append({
            "when": when.isoformat(), "lane": name(issue), "run": run_id, "clock": clock,
            "level": level, "evidence": evidence,
            "human_noticed_at": h.isoformat() if h else "",
            "lead_minutes": round((h - when).total_seconds() / 60) if h else "",
        })

    def escalate(issue, clock, start, end, run_id=""):
        """Emit call / call-again / suspect as the clock runs past its cadence."""
        cad = timedelta(minutes=CADENCE[clock])
        for mult, level in ESCALATION:
            at = start + cad * mult
            if at < end:
                detect(at, issue, clock, level, f"{clock} silent since {start.isoformat()}", run_id)

    # ---- reachability and task clocks, per run -----------------------------------------
    for r in runs:
        iid, rid = r["issue_id"], r["id"]
        start, end = ts(r["started_at"]), ts(r["finished_at"]) or ts(r["updated_at"])
        if not start or not end or not iid:
            continue
        # task clock: fixed promise, never reset within the run
        escalate(iid, "task", start, end, rid)
        # reachability: gaps between run events
        marks = sorted(reach_obs.get(rid, [])) or [start]
        prev = start
        for t in marks + [end]:
            escalate(iid, "reachability", prev, min(t, end), rid)
            prev = t

    # ---- progress clock, per issue while in_progress -----------------------------------
    status_changes = defaultdict(list)  # issue -> [(t, new_status)]
    for a_ in activity:
        if a_["entity_type"] == "issue" and a_["action"] in ("issue.updated", "issue.created"):
            t = ts(a_["created_at"])
            d = a_.get("details") or ""
            if t and '"status"' in d:
                for s in ("in_progress", "in_review", "blocked", "done", "todo", "backlog", "cancelled"):
                    if f'"{s}"' in d:
                        status_changes[a_["entity_id"]].append((t, s))
                        break
    for iid, iss in issues.items():
        changes = sorted(status_changes.get(iid, []))
        if not changes and iss.get("started_at"):
            changes = [(ts(iss["started_at"]), "in_progress")]
        for i, (t0, s) in enumerate(changes):
            t1 = changes[i + 1][0] if i + 1 < len(changes) else (ts(iss["completed_at"]) or ts(iss["updated_at"]) or t0)
            if s == "in_progress":
                marks = [t for t, _ in progress_obs.get(iid, []) if t0 <= t <= t1]
                prev = t0
                for t in marks + [t1]:
                    escalate(iid, "progress", prev, t)
                    prev = t
            elif s == "blocked":
                escalate(iid, "waiting", t0, t1)

    # ---- dead lanes (pattern 17): locked, todo/in_progress, no run ever started --------
    runs_by_issue = defaultdict(list)
    for r in runs:
        runs_by_issue[r["issue_id"]].append(r)
    for iid, iss in issues.items():
        if iss.get("execution_locked_at") and iss["status"] in ("todo", "in_progress") and not runs_by_issue.get(iid):
            t = ts(iss["execution_locked_at"])
            if t:
                detect(t + timedelta(minutes=CADENCE["reachability"]), iid, "reachability", "suspect",
                       "execution lock held, no run ever started (dead lane)")

    # ---- green PRs waiting (pattern 19) ------------------------------------------------
    latest_status = {}
    for st in sorted(statuses, key=lambda s: s["created_at"]):
        latest_status[(st["repo"], st["number"], st["sha"], st["context"])] = st
    for pr in pulls:
        if pr["state"] != "open" and not pr.get("merged_at"):
            continue
        sts = [s for k, s in latest_status.items() if k[0] == pr["repo"] and k[1] == pr["number"] and k[2] == pr["head_sha"]]
        if sts and all(s["status"] == "success" for s in sts):
            green = max(ts(s["created_at"]) for s in sts)
            closed = ts(pr.get("merged_at")) or ts(pr.get("closed_at")) or datetime.now(timezone.utc)
            if closed - green > timedelta(hours=GREEN_PR_WAIT_H):
                iid = pr_issue.get((pr["repo"], pr["number"]), "")
                detect(green + timedelta(hours=GREEN_PR_WAIT_H), iid, "progress", "call",
                       f"PR #{pr['number']} green since {green.isoformat()}, not merged")

    # ---- gray-work signals --------------------------------------------------------------
    rulings_per_head = defaultdict(int)
    for d in decisions:
        rulings_per_head[(d["issue_id"], d["stage_type"])] += 1
    looping = {k: n for k, n in rulings_per_head.items() if n > 2}
    children = defaultdict(int)
    for iid, iss in issues.items():
        if iss.get("parent_id"):
            children[iss["parent_id"]] += 1
    for rel in relations:
        if rel["type"] in ("blocks", "relates_to", "duplicate_of"):
            children[rel["issue_id"]] += 0  # presence only; parent_id is the spawning link
    spawning = {k: n for k, n in children.items() if n >= 5}
    not_starting = []
    for iid, iss in issues.items():
        s = ts(iss.get("started_at"))
        firsts = [t for t, ev in progress_obs.get(iid, []) if "PR" in ev or "work_product" in ev]
        if s and firsts and (min(firsts) - s) > timedelta(hours=4):
            not_starting.append((name(iid), round((min(firsts) - s).total_seconds() / 3600, 1)))

    # ---- baseline: what Paperclip itself noticed ----------------------------------------
    baseline = []
    for r in runs:
        if r["liveness_state"] in ("plan_only", "empty_response", "blocked", "failed", "needs_followup"):
            baseline.append({"when": r["finished_at"], "lane": name(r["issue_id"]), "source": "liveness_state",
                             "detail": f"{r['liveness_state']}: {r['liveness_reason'][:80]}"})
    for x in recovery:
        baseline.append({"when": x["created_at"], "lane": name(x["source_issue_id"]), "source": "recovery_action",
                         "detail": f"{x['kind']} {x['cause'][:80]}"})
    for w in watchdog:
        baseline.append({"when": w["created_at"], "lane": name(w["evaluation_issue_id"]), "source": "watchdog",
                         "detail": f"{w['decision']} {w['reason_head'][:80]}"})
    baseline.sort(key=lambda b: b["when"])

    # ---- write ------------------------------------------------------------------------
    detections.sort(key=lambda d: d["when"])
    with open(os.path.join(a.out, "detections.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(detections[0].keys()) if detections else ["when"])
        w.writeheader(); w.writerows(detections)
    with open(os.path.join(a.out, "baseline.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["when", "lane", "source", "detail"])
        w.writeheader(); w.writerows(baseline)

    by_clock = defaultdict(lambda: defaultdict(int))
    for d in detections:
        by_clock[d["clock"]][d["level"]] += 1
    leads = [d["lead_minutes"] for d in detections if d["level"] == "suspect" and d["lead_minutes"] != ""]
    lines = [
        f"Runo replay v0 over {a.paperclip}" + (f" + {a.forgejo}" if a.forgejo else ""),
        f"issues {len(issues)}  runs {len(runs)}  run events {len(events)}  comments {len(comments)}  PRs {len(pulls)}",
        "",
        "Detections by clock and level:",
    ]
    for clock in CADENCE:
        lv = by_clock.get(clock, {})
        lines.append(f"  {clock:<13} call {lv.get('call',0):>4}  again {lv.get('call-again',0):>4}  suspect {lv.get('suspect',0):>4}")
    if leads:
        leads.sort()
        lines += ["", f"Suspects later followed by a human comment: {len(leads)}",
                  f"  lead time to human (minutes): median {leads[len(leads)//2]}, min {leads[0]}, max {leads[-1]}"]
    lines += ["", f"Paperclip baseline signals: {len(baseline)}",
              "", "Gray-work signals:",
              f"  looping   (>2 rulings on one stage): {len(looping)}  " + ", ".join(f"{name(k[0])}/{k[1]}x{n}" for k, n in list(looping.items())[:8]),
              f"  spawning  (>=5 children):             {len(spawning)}  " + ", ".join(f"{name(k)}x{n}" for k, n in list(spawning.items())[:8]),
              f"  not starting (>4 h to first PR):     {len(not_starting)}  " + ", ".join(f"{n} {h}h" for n, h in not_starting[:8])]
    text = "\n".join(lines)
    with open(os.path.join(a.out, "summary.txt"), "w") as f:
        f.write(text + "\n")
    print(text)


if __name__ == "__main__":
    main()
