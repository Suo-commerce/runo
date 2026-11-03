# Runo replay — step 1 of the proof plan

Replays the HODD Bob pipeline's four days (28–31 Oct 2026) through the Core clocks of
the Dwarven Song spec, and compares each detection with Paperclip's own signals and
with when a human first commented. Everything here is read-only against your data.

Files

    export-paperclip.sh   Postgres -> CSV, one file per table (psql, read-only SELECTs)
    export-forgejo.py     Forgejo API -> CSV: PRs, reviews, comments, CI statuses (GET only)
    replay.py             the Core clocks + gray-work signals over the CSVs (Python 3, stdlib)

## 1. Export Paperclip

Preferably from a restored backup so the live instance is untouched:

    mkdir -p /tmp/pc-restore && cd /tmp/pc-restore
    ls ~/.paperclip/instances/default/data/backups/      # pick the newest .sql.gz from 31 Oct or later
    createdb paperclip_replay
    gunzip -c ~/.paperclip/instances/default/data/backups/<file>.sql.gz | psql paperclip_replay
    cd ~/runo-replay
    DATABASE_URL=postgres://localhost/paperclip_replay ./export-paperclip.sh

Or straight from the embedded database (read-only queries, port 54329):

    ./export-paperclip.sh
    # equals: DATABASE_URL=postgres://paperclip:paperclip@127.0.0.1:54329/paperclip FROM=2026-10-27 TO=2026-11-01 OUT=./paperclip-corpus

If the instance runs in Docker, run psql through it:

    DATABASE_URL="$(docker exec <paperclip-container> printenv DATABASE_URL)" ./export-paperclip.sh

Output: `./paperclip-corpus/*.csv`, one row count per table printed.

## 2. Export Forgejo

Make a read-only token (Settings → Applications → Generate token, scope: read:repository).

    FORGEJO_URL=https://git.dinar.fi \
    FORGEJO_TOKEN=<token> \
    REPOS=<owner>/<repo>,<owner>/<repo2> \
    FROM=2026-10-27 TO=2026-11-01 OUT=./forgejo-corpus \
    python3 export-forgejo.py

List every repo the lanes pushed to in the window. Output: `./forgejo-corpus/*.csv`.

## 3. Run the replay

    python3 replay.py --paperclip ./paperclip-corpus --forgejo ./forgejo-corpus --out ./replay-out
    cat replay-out/summary.txt

`replay-out/detections.csv` has one row per clock crossing: when Runo would have called,
called again or suspected, which lane and clock, the evidence, and the first human comment
after it with the lead time in minutes. `baseline.csv` lists what Paperclip itself noticed
(liveness states, recovery actions, watchdog decisions) for the same lanes.

Fixed Core cadences live at the top of `replay.py` (`CADENCE`, minutes). The v0.1 values are
the starting point; change them there and rerun, never in the data.

## What to look for

- Pattern 17 dead lanes: `reachability / suspect` rows with "dead lane" in the evidence.
- Pattern 8 long runs: `task / suspect` rows, and how early they come before the run ended.
- Pattern 19 green PRs: `progress / call` rows naming a PR.
- Lead time: median minutes between a `suspect` and the first human comment. Positive and large
  is the result we want; empty means no human ever commented on that lane in the window.
- Gray-work: looping, spawning and not-starting counts at the bottom of the summary, against the
  post-mortem (review round 14; seven tickets for one policy apply; seven hours of planning).

## Sharing the result

Attach `replay-out/summary.txt` and `replay-out/detections.csv` to the next session. The corpus
CSVs contain comment heads and titles; share them only if that is fine for the content.
