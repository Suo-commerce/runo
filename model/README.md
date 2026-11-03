# TLA+ model

`Runo.tla` is milestone 1: claim, sing, refrain, crash, suspect, reserve, grant, install,
activate, old write, replica restart, closure in its steps, seal, late verse. It checks S1–S4 as
invariants and L1 as liveness, plus two action properties the spec states as rules: the closure
snapshot is never recomputed (`SnapshotFixed`) and a fence is never lowered (`FenceMonotone`).

Every action name is also a `runo-sim` event name, so a counterexample trace from TLC is
replayable as a scenario under `crates/runo-sim/scenarios/`.

## Running it

Get `tla2tools.jar` from the TLA+ releases, then:

    java -XX:+UseParallelGC -Xmx6g -cp tla2tools.jar tlc2.TLC -workers auto -deadlock \
         -config Runo.cfg Runo.tla

`-deadlock` is on purpose: a closed and sealed task has nothing left to do, which TLC would
otherwise report as a deadlock.

## Instances

All instances: voices a, b, c (c the allocator), epochs 0–2, replicas r1, r2.

| Config | Tasks | Seq per chain | Checks | Result on 3 Nov 2026 |
| --- | --- | --- | --- | --- |
| `Runo-small.cfg` | t1 | 1 | S1–S4, L1, both action properties | 113,412 distinct states, all hold, 35 s on 2 cores. **CI runs this.** |
| `Runo-broken.cfg` | t1 | 1 | S1–S4 with `RequireAllAcks = FALSE` | S1 violated in 9 steps, as it must: activation on one replica's ack, then the old incarnation's write lands as Current at the other. **CI asserts this.** |
| `Runo-deep.cfg` | t1 | 2 | as small | Past 2.9 million distinct states at depth 23 without finishing on this box; run offline on a bigger one |
| `Runo.cfg` | t1, t2 | 1 | as small | Running at the time of this commit; result in the PR body |

Two tasks do not interact in the model (the allocator's journal is per task), so the one-task
instance is the one that finds protocol bugs; the two-task instance is the spec's stated instance
and checks that per-task state keeps them apart. `Runo-deep.cfg` matters because with one seq per
chain the epoch-0 incarnation never gets to write after a relaunch (its only seq is the claim);
with two it does, which is the paused-resumes scenario at the first epoch.

## What the model abstracts

- A verse is a record `[voice, task, epoch, seq, cause]` and is its own id. Signatures are not
  modelled: S2 is checked as "every Current verse on a fenced chain names an activated grant".
- Relaunch is of the same singer at the next epoch; a task's holder never changes.
- The store is one writer over two replicas. Each replica admits a write as Current if its
  epoch is at or above that replica's fence, else files it as History. A write is tagged at
  admission with the chain's activated epochs, which is how S1 is stated without a clock.
- The allocator's journal is the variables `reserved`, `pending` and `closure`; its crashes are
  the interleavings between its steps.
- Fences are persisted, so `ReplicaRestart` changes nothing but a counter.
- Silence is a nondeterministic `Crash`, not a timer; peers then `Suspect`.

## A question the model raised

During a partial install — one replica has the new fence, the other does not yet — a write at
the old epoch is filed as History at the first replica and Current at the second. Replication
is set union of verses (Song spec §6), but the spec does not say which class wins when replicas
disagree. The model treats such a verse as work (it was written by the still-authorized
incarnation) and lets it into the closure snapshot; S3 only excludes verses that every replica
filed as History. Whether that is the intended rule is for the spec to say.
