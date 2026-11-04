# Running `runo watch` on a Paperclip board

Watch mode (technical spec §15) reads a Paperclip board and tells a person, once per stall, what needs them. It never writes to the board. This page is the first run, start to finish.

## You need

- The board's address, such as `https://board.example`.
- The company id on that board. `runo doctor --paperclip <board>` without `--company` lists the companies the key can open, with their ids.
- An API key that can read that company's issues, runs and agents. A board user's key works: `npx paperclipai auth login --api-base <board>` asks you to approve it in the browser and stores it in `~/.paperclip/auth.json`. Runo only reads with it. Put it in an environment variable, never in a file you share or on the command line:

```sh
export PAPERCLIP_API_KEY='…'
```

## 1. Build

```sh
cargo build --release -p runo-cli
alias runo="$PWD/target/release/runo"
```

## 2. Check what it can reach

```sh
runo doctor --paperclip https://board.example --company <company-id>
```

This prints every setting in use (thresholds, state file, delivery) and then reads the board once: the Paperclip version, and how many agents, issues and runs it can see. If it says `refused (401)` the key is wrong; `(403)` means the key cannot read that company.

## 3. One look

```sh
runo watch --paperclip https://board.example --company <company-id> --once
```

This polls once, prints a message for each lane that is stuck right now, prints the view, and exits:

```
HOD-101 · Bob · task needs a person: failed run r5: budget_exhausted, which Paperclip does not retry: Monthly budget reached
  https://board.example/issues/HOD-101

runo watch · https://board.example · 2026-11-08T09:00:00Z
4 lanes in progress · reachability unknown (the board does not say whether a worker responds)

Open (4), worst first, with how long each has been overdue:
  needs a person task         HOD-101    Bob   10m     failed run r5: budget_exhausted, …
  needs a person waiting      HOD-102    Bob   1h20m   blocked on the board for longer than the waiting threshold
  …
Resolved: 1 (1 before the herald started, kept as history and not sent)
Gaps: none
```

## 4. Keep watching

```sh
runo watch --paperclip https://board.example --company <company-id>
```

It polls every minute (`--interval` to change), prints each new message once, and reprints the view when the open list changes. Stop it with Ctrl-C; start it again any time. It remembers what it has already told you in `~/.local/state/runo/watch/episodes.json` (`--state` to move it), so a restart repeats nothing. The file keeps every open episode and the newest 500 resolved ones, so it stays small. Links in messages use the company's issue prefix (`<board>/HOD/issues/HOD-3971`).

## What the view means

- **A lane** is one agent working one issue.
- **Waits** (`blocked`, `in_review`) are never reported on sight, only when they run past the waiting threshold.
- **A failed run** of a kind Paperclip itself never retries (21 error codes, such as `budget_exhausted` or `setup_failed`) is reported at once and listed first.
- **The ladder:** a clock that crosses its threshold gets one more cadence of grace, then becomes *overdue*, *still overdue*, and *needs a person*. Defaults are one hour, escalating at 1×, 1.5× and 2×; `runo doctor` prints them. `--lane-minutes` sets a lane's (progress and time in progress), `--waiting-minutes` a wait's: on a board where issues are often parked on dependencies, raise the waiting threshold so a deliberate dependency wait is not reported as a stall.
- **History:** on the very first start, stalls that ended before the herald was running are kept, not sent.
- **Gaps:** if more changed between two polls than one page holds, the view says so (`paperclip-gap`) rather than hiding what it missed.
- **Reachability is unknown** in Watch mode: the board does not say whether a worker responds. That needs the lanes' own refrains (Chorus mode).

## Not yet

Messages go to the terminal only. Comments on the issue (behind `--apply`), Telegram, a systemd unit and a container image come next. Thresholds derived from the board's own history replace the one-hour defaults later.
