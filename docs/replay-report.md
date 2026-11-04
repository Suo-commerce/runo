# Runo Replay Report: HODD Bob, 27–31 October 2026

Nov 2, 2026 · @Jarmo

The replay did not invalidate Runo; it changed its priorities. With v0.1 cadences, Runo's clocks would not have beaten Paperclip's own detection, but Paperclip's 1,326 signals on 533 lanes reached almost no one: only 110 of 778 stall episodes were ever followed by a human comment. Detection was not the gap. Delivery was. Runo's first job is to route existing signals to a person; its own clocks come second, re-tuned from this data.

## 1. Summary and verdict

The milestone 0 gate asked whether Runo's Core clocks, replayed over four days of the HODD Bob pipeline, would have caught stalls clearly earlier than the post-mortem's timeline, with under one false call per lane-day. **The gate is not met as tuned**, and the reason reorders the plan rather than ending it.

| Question | Answer from the data |
| --- | --- |
| Did Runo detect stalls earlier than Paperclip? | No. Paperclip flagged 533 lanes to Runo's 289 and was first on 134 of the 224 lanes both flagged. |
| Did Runo detect stalls earlier than humans? | Yes where humans reacted at all: median 3.5 hours ahead. But humans reacted to only 110 of 778 stall episodes. |
| Were Paperclip's signals acted on? | Almost never. 1,326 signals on 533 lanes sat in its database with no route to a person. |
| Were the gray-work signals real? | Yes. 32 tasks with five or more children (HOD-2990: 55); 12 PRs with ten or more reviews (#630: 19); 12 lanes over four hours to first PR (HOD-861: 110 h). |
| Were Runo's clocks noisy? | Yes. The 15-minute reachability clock fired on runs waiting legitimately, 310 of 778 suspects. |

The conclusion: **detection was not the gap; delivery was.** The Song's founding rule — silence never means fine — was broken not by missing signals but by signals nobody heard. Runo's first shippable form is therefore the routing layer: take existing signals, deduplicate per lane, escalate on a cadence, deliver to a person. Its own clocks stay in the design, re-tuned from this corpus, and fencing, tomes and the simulation suite are unaffected, since the replay did not exercise recovery.

## 2. Scope and method

**Data.** Exported read-only from Paperclip's embedded Postgres on `worker-paperclip` and from the Forgejo API for `hodd/hodd_launch`, window 27 October to 1 November 2026 (UTC).

| Source | Records |
| --- | --- |
| Paperclip issues | 2,075 |
| Agent runs | 7,792 |
| Run events | 40,435 |
| Issue comments | 4,837 |
| Pull requests | 177 |
| PR reviews | 564 |
| CI status records | 14,586 |

**Clocks replayed (v0.1 cadences).** Reachability 15 min (no output from a run), progress 30 min (no new deliverable while in progress), task 90 min (one run too long), waiting 60 min (blocked without change). Each escalates to *call* at 1×, *call again* at 1.5× and *suspect* at 2× its cadence.

**Paperclip baseline.** Paperclip's own signals on the same lanes: run liveness states (`blocked`, `failed`, waits with no target, stranded issues) and recovery actions. Its watchdog-decision and review-decision tables were empty.

**Human attention.** The first comment by a person on the lane after a detection. This is a proxy: a comment made for another reason counts as noticing, and silence does not prove nobody looked.

**Corrections made before the run.** The replay tool had three defects, all fixed: it read the previous status from activity records as the new one; it matched branch names by substring (`HOD-14` matched `hod-1441`); and the Paperclip export assumed a `heartbeat_runs.issue_id` column this version lacks, so run-to-issue links now come from `context_snapshot`, then cost rows, then the issue's own run references. Open issues created before the window brought in clocks that started as early as 20 October; the figures below count only detections inside 27–31 October (2,646 of 3,231) and each stall episode once.

## 3. Detection: Runo against Paperclip

|  | Runo v0.1 | Paperclip |
| --- | --- | --- |
| Lanes flagged | 289 | 533 |
| First, on the 224 lanes both flagged | 90 lanes, median 355 min ahead | 134 lanes, median 120 min ahead |
| Flagged by only one of them | 65 lanes | 309 lanes |

Paperclip's 1,326 signals with a lane break down as 1,177 run liveness states (885 `blocked`, 291 `failed`, the rest waits with no target, stranded issues and watchdog reconciliation) and 149 recovery actions.

Runo's 778 suspects by clock: reachability 310, waiting 240, task 163, progress 65. The reachability clock is the noisiest. It counts gaps between run events, and a run that is deliberately polling CI in a `sleep 240` loop looks the same as a stuck one. In production the Song separates these with `waiting` verses and `blocked_on`; the corpus has neither, so part of this noise is an artefact of replaying from Paperclip's records rather than from verses. It is still the right reading of the gate: as tuned, Runo adds flags Paperclip already has.

## 4. Lead time to a human

Of 778 suspect episodes on 289 lanes, 110 were later followed by a human comment. For those, Runo's suspect came a median 212 minutes (3.5 hours) before the comment; a quarter under 63 minutes, a quarter over 694 minutes (11.6 hours). The other 668 episodes never got a human comment inside the window.

That last number is the report's central fact. Whatever the detector, 86% of stall episodes were never acknowledged by a person. The post-mortem's anti-pattern 17 (silent dead lanes) was stated from a handful of cases; this is it measured across the whole pipeline.

### 4.1 Baseline: from signal to human action

Proof plan step 2 was run on the same window on 2 November: for every Paperclip signal that needed a person, how long until a person acted — a comment, a status change, a cancelled run, an answered question, a reassignment or a merge. Episodes that healed on their own are excluded, leaving 560. Both board identities are taken to be the owner's; if the second is not, the baseline is worse than shown.

|  | Baseline |
| --- | --- |
| A person acted at all within the window | **17%** |
| Acted within 15 min / 1 h / 4 h / 24 h | 5% / 7% / 9% / 14% |
| When someone did act: median / p75 / p90 | 3.0 h / 8.9 h / 36.6 h |
| Median wait for a person (Kaplan–Meier) | **Not reached**: 77% were never acted on in five days |

There is no single N in hours; most signals waited forever. The baseline is best stated as: **7% of signals that needed a person got one within an hour.**

**The owner was not away.** They were active somewhere within half an hour of 88% of the signals and acted on 17% of them. When a daytime signal was acted on, it took a median of 55 minutes; at night, 5.7 hours. About 130 signal episodes arrived per day across 533 lanes, each repeated 2.1 times on average. The failure is volume, not absence: there were too many signals to tell which mattered.

| Signal class | Episodes | Acted on | Reading |
| --- | --- | --- | --- |
| `blocked` | 419 | 12% | Mostly lanes legitimately waiting on another lane: noise, until the wait itself is overdue |
| `failed` | 108 | 41%, median wait about 50 h | The ones that need a person, and the ones that wait longest |
| Waiting without a target | 31 | 6% | Noise |

Replay 1's comment measure had missed half the human actions: 45 of the 95 first actions were something other than a comment, and 22 of those lanes never got a comment at all.

**What this changes.** Routing as notification would make the volume worse. Routing as **triage** is the fix: fold repeats into one episode per lane, separate what needs a person from what is waiting legitimately, and rank what remains. The `blocked` class is the Song's waiting clock in practice — blocked is not stuck until the wait is overdue — and the `failed` class is where Runo's first measurable claim lives: today a failed run that needs the owner is acted on 41% of the time after a median of about 50 hours; after routing, it should be acted on within one hour in daytime. The same script, rerun on a later window, measures it.

## 5. Gray-work signals and review loops

The three gray-work signals in the Usefulness and Safety spec all produced evidence, though one had to come from Forgejo because Paperclip recorded no review decisions.

| Signal | Result | Worst cases |
| --- | --- | --- |
| Spawning (≥ 5 child tasks) | 32 tasks | HOD-2990: 55 children; HOD-443: 28; HOD-978: 12; HOD-607: 11 |
| Not starting (> 4 h to first PR) | 12 lanes | HOD-861: 110 h; HOD-2402: 26 h |
| Looping (reviews per head) | 0 from Paperclip; visible in Forgejo | See below |

**Reviews.** 564 reviews on 122 PRs: 222 change requests, 207 approvals, 134 comments; 55 PRs were never reviewed in Forgejo. Median 4 reviews per reviewed PR, mean 4.6; 47 PRs had five or more, 12 had ten or more.

| PR | Reviews | Change requests | Heads reviewed | Pushes | Title |
| --- | --- | --- | --- | --- | --- |
| #630 | 19 | 16 | 19 | 12 | HDD-1247: the subresource witness |
| #505 | 19 | 10 | 12 | 9 | HDD-462: the Allow click sends the gateway session choice |
| #584 | 19 | 6 | 11 | 9 | HDD-444 K2: plaintext shape rule, wired |
| #528 | 16 | 3 | 11 | 2 | HDD-522: a 401 after sign-in |
| #686 | 15 | 13 | 15 | 11 | HDD-1345 C3: the assistant runtime |
| #524 | 15 | 4 | 9 | 12 | HDD-245: render LaTeX math in chat replies |

Reviews per day on the review account fell from 95 on 28 October to 82, 42 and 13 on the following days, after the owner's product-focus ruling on 29 October. One ruling removed most of the loop; this is anti-pattern 1 with a before-and-after.

**Merges.** 135 PRs merged in the window, median two hours from open to merge; one in ten took more than 56 hours, the slowest 152. Ten closed without merge; none was open at the end of the window. The fast median and slow tail together say what anti-pattern 19 said: a PR merges quickly once nothing blocks it, and the blocking was the reviewer, not CI.

## 6. Accounts, tokens and access

**Quota failures.** 504 runs failed with `provider_quota`, 492 of them on 27 October, the day the Claude account was switched; about 12 over the rest of the window. This is anti-pattern 26 (one model account as a single point of failure) as a single-day spike.

**Idle polling.** 4,632 of 7,792 runs (59%) had no task: 4,341 timer wakes and 287 automations. QA Tester 1 woke 1,351 times in five days, Iris 999, Ada 707, Otto 638. Timer wakes used 28% of new tokens but about 2% of recorded cost, because they ran mostly on the ChatGPT subscription.

**Tokens by billing line.**

| Model | Billed as | New tokens (M) | Cached reads (M) | Recorded $ |
| --- | --- | --- | --- | --- |
| `xlarge-high` | anthropic, metered API | 2,795 | 26,593 | 38,674 |
| gpt-6.1 sol / terra / astra | ChatGPT subscription | 3,038 | 2,728 | 0 |
| `litellm/xlarge-high` | LiteLLM | 85 | 4,922 | 0 |
| claude-opus-5-5 | Claude subscription | 98 | 1,323 | 0 |
| **Total** |  | **6,016** | **35,565** |  |

The $38,674 is Paperclip's list-price estimate for a model served through the team's own gateway, not an invoice. The actual spend is in the LiteLLM dashboard and has not been read; treat the figure as an upper bound until it is.

**Access.** The former consultant's Forgejo account posted 58 review comments on 27–28 October (56 on the 27th, the last at 02:37 UTC on the 28th), after he had left. The pattern fits an agent or script still holding his token. This is anti-pattern 5 and the offboarding checklist, with a live example.

## 7. Limits of this replay

- **Human attention is a proxy.** A comment is not proof that someone noticed the stall, and absence of a comment is not proof nobody did.
- **The reachability clock cannot tell waiting from stuck** in this corpus, because it has no `waiting` verses or `blocked_on`; it counts gaps between run events, and a deliberate poll loop fires it.
- **The dead-lane rule found nothing.** It needs an execution lock with no run ever started; the 13 locked lanes known from 28 October were locked before the window, so their earlier runs are not in the export.
- **Review loops are visible only in Forgejo.** Paperclip recorded no review decisions, so the replay's own looping signal was blind.
- **Forgejo timelines failed for 16 PRs** (null entries in the API response). The replay does not use timelines; review and status data are complete.
- **The comparison is detection, not recovery.** Nothing in the replay exercises fencing, closure or tomes; the simulation suite does that.
- **One pipeline, one week, under stress.** The numbers describe HODD Bob between 27 and 31 October, not agent factories in general.

## 8. Consequences for Runo

The architecture stands; the order of building changes. The Core design already separates evidence from the act of hearing it, and the replay says hearing is where the value is.

| Before the replay | After |
| --- | --- |
| Milestone 3: Runo's own four clocks over a jsonl chorus | Milestone 3: the **routing layer** — Paperclip's signals enter the song through the adapter (moved from Later to Core), Runo deduplicates per lane, runs the call ladder and the owner's view |
| Reachability clock at 15 min, counting run-event gaps | Reachability from refrains and `last_output_at`, disabled while a lane is `waiting`; the fixed cadence raised to a value this corpus supports (45 min as the first estimate) |
| Four clocks weighted equally | Task and waiting clocks first, since Paperclip lacks them; progress next; reachability last |
| Gray-work signals from Paperclip's decision table | A review-loop signal from Forgejo (change requests per PR, reviews per head) and an idle-polling signal (runs and tokens per agent with no task), both added to the Usefulness tab |
| Milestone 0 gate: beat the timeline | Milestone 0 recorded as **not met as tuned**; a new gate for milestone 3 from the baseline in §4.1: failed runs that need the owner are acted on within one hour in daytime (today 41%, median 50 h), with repeats folded to one episode per lane and `blocked` shown only when the wait is overdue |

What does not change: the verse and refrain format, signing and the fixed roster, fencing with grants and activations, closure snapshots and tomes, the fourteen simulation scenarios, and the Later seams. None of them was tested by this replay, and none is contradicted by it.

The Song's own principle explains the result. Principle 3 says silence never means fine; Paperclip had the signals and still let them be silent. Routing is that principle made executable.

## 9. Actions outside Runo

- [ ] Revoke the former consultant's Forgejo tokens and find which agent or script was still using them on 27–28 October.
- [ ] Read the actual 27–31 October spend from the LiteLLM dashboard and replace the $38,674 list-price estimate.
- [ ] Move timer-woken agents to event wakes, or lengthen the timers and make an empty wake cheap; QA Tester 1 alone woke 1,351 times with nothing to do.
- [ ] Record the 27 October quota spike as the evidence for a configured fallback runtime per agent.
- [ ] Add one-line evidence pointers to this report under anti-patterns 1, 5, 17 and 26 in the post-mortem.

## 10. Next measurement

Proof plan step 2, the signal-to-human-action baseline, is done (§4.1). The next measurement is **replay 3**: the routing prototype over a later window, scored with the same script on three numbers — the share of failed runs acted on within one hour in daytime, episodes per day shown to the owner after folding, and the share of `blocked` signals suppressed as legitimate waits. One gap to close first: the baseline script does not yet print the within-one-hour share for the failed class on its own.

**Tool changes for that run**

- [ ] Clip detections to the window inside `replay.py`, not in the analysis afterwards.
- [ ] Reachability from `last_output_at` and run liveness, or cadence raised to 45 minutes; disabled while the run is in a waiting state.
- [ ] Export `biller`, `billing_type` and `cached_input_tokens` with cost events.
- [ ] Skip null timeline entries in `export-forgejo.py`.
- [ ] Count rows, not lines, in `export-paperclip.sh`.
- [ ] Add the signal-to-human-action measurement and the review-loop and idle-polling signals.

The replay kit, its fixes and the corpus layout are the first test fixtures for `runo replay` in the Core build, so the detectors that ship are the ones measured here.
