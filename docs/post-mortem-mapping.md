# Runo Against the HODD Bob Post-Mortem

Nov 1, 2026 · @Jarmo

Of the 27 anti-patterns recorded while fixing the HODD Bob pipeline (28–31 October 2026), Runo would catch 5 with the v2 spec as written, and 12 more once two planned additions land: a usefulness level and a correct, causally linked record. It can only measure 4, and 6 lie outside a witness layer altogether.

## 1. The verdict

Each pattern is placed by what Runo does about it: catches it now, catches it once usefulness or a correct record is in the spec, only measures it, or cannot touch it.

| # | Pattern | Runo's role | Mechanism |
| --- | --- | --- | --- |
| 1 | Review gates with no stopping condition | Needs usefulness | Repeated rulings on the same head are progress without usefulness |
| 2 | Tickets that breed tickets | Needs usefulness | Offers spawned per hoard entry, read from the cause tree |
| 3 | Relay layers between worker and decision | Needs record | Blackboard: the worker reads the owner's verse directly; order by `cause`, not arrival |
| 4 | Automated code, manual infrastructure | Needs record | A merge whose required infrastructure verse never arrives is an open cause |
| 5 | A single person as a production dependency | Outside | Access and staffing |
| 6 | Live state drifting from the repo | Outside | Control plane reconciliation |
| 7 | Maximum effort everywhere | Measures | Effort per run is policy |
| 8 | No time or turn budget | Catches | Past the Aegloss P95 promise, peers call, then suspect |
| 9 | Too many parallel lanes | Measures | Lane count is policy |
| 10 | Rules that cannot be overridden | Needs record | An owner ruling with no matching configuration verse stays an open cause |
| 11 | Brittle coupling through exact literals | Outside | Policy design |
| 12 | Watchdogs that depend on what they watch | Catches | Independence by construction; a model-free sentinel |
| 13 | Infrastructure nobody owns | Outside | Asset ownership |
| 14 | Decomposition with no owner for "it works" | Needs usefulness | Acceptance on the root offer; lanes done under an unaccepted root are not useful |
| 15 | Briefs that freeze the paths the work needs | Outside | Brief writing; Runo only shows the `blocked_on` |
| 16 | Instructions that assume things exist | Needs record | Offers reference content by hash; a dangling reference fails verification |
| 17 | Silent dead lanes | Catches | A claimed task with no verses hits the call ladder; silence never means fine |
| 18 | Agents reinterpreting settled decisions | Needs record | The decision is a signed ruling linked to the task, referenced rather than re-asked |
| 19 | Green PRs left waiting | Catches | A `package` verse with no ruling past cadence calls the Smith |
| 20 | Verbosity as a cost nobody counts | Measures | Verse size and `note` length |
| 21 | The bottleneck moved | Measures | `blocked_on` concentrated on one voice |
| 22 | Review gates built into the tool | Needs record | The owner's waiver with no configuration change stays open |
| 23 | Planning instead of building | Needs usefulness | Time from `claimed` to first head |
| 24 | Hardening with diminishing returns | Needs usefulness | Lanes added under a root whose acceptance does not move |
| 25 | Storage that only grows | Outside | Paperclip's retention; a lesson for Runo's own storage (§6) |
| 26 | One model account as a single point of failure | Catches | The stall is called; the provider is a declared failure domain |
| 27 | Status reports that outlive the facts | Needs record | Status rendered from signed verses sung at boundaries, never restated |

| Role | Count |
| --- | --- |
| Catches today | 5 |
| Needs usefulness | 5 |
| Needs a correct record | 7 |
| Measures only | 4 |
| Outside Runo | 6 |

## 2. What Runo catches today

The five patterns Runo catches as written are all silences: work that stopped while the board looked alive. Pattern 17 is the clearest case — 13 lanes held execution locks, showed as `todo`, and never ran, while the idle-lane watcher skipped them without logging.

In Runo each of those lanes had sung `claimed` and then nothing. Silence never means fine, so a peer would call within the lane's promised cadence, call again, and sing `suspect` — minutes, not a week. The same ladder catches the review stage stalled for ten hours (22), the lane stopped by an unavailable model account (26), the green PRs waiting for a ruling (19) and the 3.5- and 9-hour runs that blew past any reasonable promise (8).

Pattern 12 is caught by construction rather than by the ladder. Paperclip's hung-runs cleaner and backup check both called the hung API, so nothing recovered for 3.5 hours. Runo requires at least one voice in an independent failure domain, ideally a model-free sentinel on separate hardware, and refuses to start a chorus without one.

## 3. What needs usefulness

Five patterns are agents that were alive and producing output, but not producing value. Liveness detection cannot see them, because every protocol signal is healthy.

| # | What happened | Signal from the cause tree |
| --- | --- | --- |
| 1 | PR #702 reached review round 14 on the same head | Rulings per head; a second round without a new head is a loop |
| 2 | One policy apply spawned seven tickets | Offers spawned per hoard entry under one root |
| 14 | Five lanes merged; the runtime could not run one task | Root acceptance unmet while child lanes close |
| 23 | Seven hours of planning and a "PO question", no branch | Time from `claimed` to first head |
| 24 | Lane K5 added 1,681 lines for rarer and rarer evasions | Lanes added under a root whose acceptance does not move |

The common lesson is pattern 14's: usefulness measured bottom-up fails. Every lane reported done and nothing worked. Usefulness has to be defined top-down, from an acceptance on the root offer phrased from the user's side, and inherited down the cause tree. That definition is the subject of the companion spec.

## 4. What needs a correct record

Seven patterns are failures of truth, not of liveness: agents acted on instructions, decisions or status that were stale, missing or never applied. A record that is signed, ordered by cause and generated at the boundary would have prevented or exposed each one.

- **Stale relays (3).** An old relay overtook the owner's newer instruction. On a blackboard there is no relay: the worker reads the owner's verse directly, and `seq` and `cause` make "newer" unambiguous.
- **Phantom artifacts (16).** A lane was told to wire a tool list that did not exist. An offer that references content by hash cannot point at nothing; the dangling reference fails verification before work starts.
- **Re-asked decisions (18).** A PM replied that a decision "does not specify a decision" because it was posted in another task. A ruling is one signed verse, linked by `cause` to every task it governs.
- **Rulings that never took effect (4, 10, 22).** The owner waived review in chat; the stage in configuration stayed and stalled the lane for ten hours. In Runo a ruling is a cause, and the configuration change that applies it is the answer. A ruling without its answer is an open cause, visible to every voice — the stall is caught by the call ladder, the reason by the record.
- **Status that outlived the facts (27).** The epic said PR #686 was open a day after it merged. The dashboard renders only from verses sung by the system at the boundary, so status cannot be restated from memory.

## 5. What Runo only measures, and what lies outside it

Ten patterns are decisions about how to run the factory. A witness can make them visible but cannot make them for you.

**Measured (4).** Maximum effort everywhere (7), too many lanes (9) and verbosity (20) are policy choices; Runo supplies the numbers — effort per run, merges and rebases per day, verse and note size. The moved bottleneck (21) is the most useful measurement: when most `blocked_on` fields name one voice, the owner is the single point of failure, and the song shows it before an absence does.

**Outside (6).** These belong to other layers of the factory:

| # | Pattern | Layer that owns it |
| --- | --- | --- |
| 5 | A single person as a production dependency | Access and staffing |
| 6 | Live state drifting from the repo | Control plane |
| 11 | Brittle coupling through exact literals | Policy design |
| 13 | Infrastructure nobody owns | Asset ownership |
| 15 | Briefs that freeze the paths the work needs | Work intake |
| 25 | Storage that only grows | Each system's own retention |

## 6. Lessons for Runo itself

Several patterns are warnings Runo must heed in its own design, because a witness that fails the same way is worse than none.

- **Don't depend on what you watch (12).** No Runo voice may rely on the system it judges for its own health. The sentinel runs on separate hardware, credentials and implementation.
- **Retention from day one (25).** Paperclip's run records reached 1.4 GB, mostly stored context. Verses stay small and reference large state by hash; tomes compact closed chains; retention for cold verses is set when the chorus is created.
- **Every line is paid for many times (20).** Every listener reads every verse. `note` stays one line, and payloads travel by reference.
- **No coordination without output (2, 23).** Cascades must be idempotent by `cause`, so three voices noticing the same event produce one reaction, not three verses of chatter.
- **A skipped wake is a verse (17).** A voice that decides not to act sings why. Silence never means fine — including the watcher's own.
- **The owner's ruling has a fast path (10).** Runo's own rules, such as cadence fallbacks and quorum sizes, must be changeable by an owner ruling without a rebuild.

## 7. The replay test

The four days are a ready-made test corpus. Under the Magos principle, the claims above start as estimates and are settled by replay.

**Method.** Rebuild a song from the pipeline's own records — Paperclip runs and locks, issue comments, PR and CI events, deploy times — mapping each event to the verse a Runo voice would have sung. Run Runo's detectors over the rebuilt song, and compare each detection with the moment a human actually noticed. Silence needs no reconstruction: a verse that was never sung is exactly what the detectors look for.

| Claim | Estimate | Measured as |
| --- | --- | --- |
| Silences are caught early | Every pattern in §2 flagged within one promised cadence, hours before a human noticed | Detection time against the post-mortem's timeline |
| Gray work is visible | Patterns 1, 2, 14, 23 and 24 each raise a usefulness signal within the first day | First signal per pattern |
| Open causes are visible | Rulings without their configuration change (4, 10, 22) appear as open causes within one cadence | Time from ruling to open-cause flag |
| False alarms are rare | Fewer than one false call per lane-day | Calls on lanes that were in fact progressing |

The post-mortem's own metrics carry over as the factory's standing measures: PR lead time, review rounds per PR, tickets per merged PR, product versus coordination runtime, hours to first commit, and lanes with no run in 24 hours while not done.
