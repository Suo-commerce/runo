# Usefulness and Safety

Runo's contribution to failure detection is narrow on purpose: liveness that sees progress and usefulness, on a record you can trust. This tab defines usefulness, shows how Runo makes acceptance easy without deciding it, names the gray-work signals, states the safety and liveness properties, and plans how to prove them. Policy, acceptance decisions and agent management stay with the layers that own them.

## 1. Usefulness

A dwarf sings its initial task when it starts, and every later verse is about that task. **Work is useful when it follows the initial task.** When a product task drifts into process work — a ticket about tickets, a plan about a plan — that work is not directly useful. Drift is not forbidden, since some process work is needed; it is visible, and counted as not directly useful.

Two additions to the verse make this measurable:

| Verse | New field | Holds |
| --- | --- | --- |
| `claimed` | `task` | `deliver`: one line on what the task delivers; `scope`: the area or paths it works in; `kind`: `product` or `process` |
| `work` | `changed` | What this step changed: files, artifacts, the new head |

Any listener can then compare each step against the initial task. Changes outside the scope, or new tasks of a different kind spawned under it, are drift.

## 2. Acceptance, made easy

Acceptance is not Runo's job: the owner accepts, per slice. Runo's job is to make approving drastically simple, so the owner does not become the bottleneck of pattern 21.

When a slice is done, the dwarf sings a `boundary` verse with event `ready-for-acceptance` carrying four things:

| Part | Example |
| --- | --- |
| Asked | A reference to the initial task |
| Claim | One line: "The Assistant page answers a typed task." |
| See it | One step: "Open the Assistant page on pilot3, type a task, see the answer." |
| Evidence | Head, CI result, links |

The dashboard shows these as a short queue. The owner sings a `ruling`: accept, or reject with a one-line reason, which becomes the cause of the next slice.

**The one-step rule.** If a dwarf cannot write the "see it" step as one line, the slice is too big and must be split. This keeps slices small enough to approve in a minute, and it is the check pattern 14 lacked: a slice that cannot be seen working cannot be claimed done.

## 3. Gray-work signals

Gray work is busy but not useful. Runo computes three signals from the record and shows them on the dashboard; none of them triggers the call ladder.

| Signal | Meaning | Computed from | Post-mortem example |
| --- | --- | --- | --- |
| Looping | The same work is judged again and again with nothing changing | Rulings on the same head | Review round 14 |
| Spawning | One task keeps creating new tasks | Tasks whose `cause` traces back to one task | Seven tickets for one policy apply |
| Not starting | Time since `claimed` with no `changed` on the deliverable | `claimed` time against the first `work` verse in scope | Seven hours of planning, no branch |

Drift (§1) is shown beside them. Highlighting levels come from measured data, not guesses: the replay (§7) sets them first, Aegloss refines them later.

## 4. Safety: what must never happen

| ID | Invariant | Enforced by |
| --- | --- | --- |
| S1 | No write is accepted from a stale epoch | Epoch fencing at the Smith and in storage |
| S2 | No unsigned or invalid verse is appended | Signature, checksum and chain checks on ingress |
| S3 | Every tome covers its whole chain, no verse missing or added | Merkle root over every verse of every epoch of the task |
| S4 | Every `cause` points to a verse that exists | Cause check on ingress; a dangling cause is rejected |

## 5. Liveness: what must eventually happen

| ID | Property | What breaks it |
| --- | --- | --- |
| L1 | Every claim ends in a tome or a `suspect` | A lane that falls silent without anyone noticing |
| L2 | Every offer is claimed or withdrawn | Work offered into the void |
| L3 | Every ruling is applied or contested | A waiver in chat with the gate still in configuration |

"Eventually" holds only while at least two of three voices are alive and can reach each other. FLP forbids more: no protocol guarantees progress in a fully asynchronous network, so Runo promises safety always and liveness when the chorus is healthy.

## 6. Gaming

Agents will optimise for whatever is measured, so the measures stay out of their hands.

- **Signals are derived, never reported.** Looping, spawning, not starting and drift are computed from the record by Runo; no lane reports its own usefulness.
- **The lane writes the "see it" step; the owner runs it.** A false claim fails in front of the owner, not inside a lane's own check.
- **Strengthen only.** Invariants and signals may be added or tightened, never loosened, except by an owner ruling sung in the open.

## 7. How we prove it

Three steps, each answering a different question, in this order:

| Step | Question | Method | Done when |
| --- | --- | --- | --- |
| 1. Replay | Is Runo worth building? | Convert the HODD Bob Paperclip records (runs, locks, comments, PRs, CI) into verses; run the detectors and signals over them; compare each detection with when a human noticed | Detections beat the post-mortem's timeline clearly, with under one false call per lane-day |
| 2. TLA+ model | Is the design safe? | A small model of three voices, two tasks and two epochs — claim, relaunch, fence, tome, ruling — explored exhaustively by the TLC model checker | S1–S4 hold in every reachable state; L1–L3 hold under fairness |
| 3. Deterministic simulation | Is the implementation safe? | Runo in Rust with clock, network and storage behind interfaces; thousands of seeded runs with injected crashes, partitions, delays, corrupt writes and stale relaunches; invariants checked after every step | No invariant violation across the agreed number of seeds; every failure replays exactly from its seed |

The replay comes first because it is the cheapest test of the whole idea and it produces the corpus the other two reuse. The model comes before code so that design bugs cost hours, not rewrites. Simulation runs alongside the implementation for as long as Runo exists.
