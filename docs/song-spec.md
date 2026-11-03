# The Dwarven Song: Specification v3

Oct 31, 2026 · @Jarmo

The Song (Runo) is a standalone protocol and library for mutual liveness: every participant sings durable, signed verses into a shared append-only record, repeats a refrain while it works, and its peers read both to tell who is reachable, who is making progress, and whose work is any good. The Song supplies evidence; the authority to act on it lives in a small fencing contract enforced at the resources that work touches.

This is one specification in two layers. **Core** is the smallest version that carries the correctness argument and is built first. **Later** is specified here so the Core leaves room for it, and is built once the Core's tests pass. Every section marks which is which.

## 1. Lineage, layers and principles

The Song began as *Dwarven Song concept v0.1*, a pacing layer for the worker-bob mine. v2 lifted it out of the mine, added signatures, tomes, epochs and Aegloss cadences. v3 follows a specification review that found v2 treating evidence as stronger than it is: epochs were called fences without anything enforcing them, three voices were credited with attributing faults they can only corroborate, and quorum signatures were asked to settle authority they cannot settle. v3 also adds the refrain, which separates "can we hear it" from "is it moving".

| Layer | Holds | Why there |
| --- | --- | --- |
| **Core** | Fixed roster; signed per-task chains; the refrain and four separate clocks; a stated durability contract; one fencing interface; deterministic tomes with a closure manifest; declared independence | Each has defined state transitions and can be tested now |
| **Later** | Dynamic membership; key pre-rotation and owner recovery; Aegloss-learned cadences; intent verses (offers and claims); verified independence | Each needs transitions the Core must first make observable |

**Carried from v0.1**

1. **Progress, not liveness.** The song counts movement, not uptime.
2. **Sing at state boundaries.** Boundary verses are progress; the refrain (new in v3) is only reachability.
3. **Silence never means fine.** A singer with nothing to report sings `waiting`, with `blocked_on` filled.
4. **Listen before singing** — and, from v3, on a timer as well.
5. **Re-sing until acknowledged.** A singer never trusts that a verse was heard.
6. **Verses are never edited.** The song is the pulse; the ledger remains the narrative.
7. **Not a scheduler, not a message bus.** The song records; work travels by other channels.
8. **The watchdog is the audience.** It steps in only when the whole chorus has gone silent.

**Added in v2, corrected in v3**

9. **None above.** Every verse is sung by one dwarf to all the others. The watchdog, the Smith and the owner sing and listen like any other voice. Authority over work exists — a fence allocator, a merge gate, an owner's ruling — but it is exercised through verses everyone hears, and it exempts nobody from being watched.
10. **Three independent voices corroborate; they do not attribute.** If A cannot hear B but C just vouched for B, A has evidence of a path problem. If neither can hear B, B may be crashed, isolated or merely delayed, and the song does not decide which. Three is the minimum for indirect probing and corroboration, under the connectivity assumptions in §5.
11. **Promises and a refrain, not timers alone.** Each singer promises its cadence when it claims work and repeats a refrain while it works; peers hold it to the promise and notice when the refrain stops.
12. **Wrong guesses are cheap only where they are fenced.** No detector is certain, so a suspicion that triggers a relaunch must be safe: every effect on a protected resource carries a fence that the resource itself enforces (§6). Effects outside the fence are listed, not assumed safe.
13. **Evidence, not authority.** The Song proves who said what, in what order, and what was durably written. It does not prove a voice is dead, a credential is independent, or a change is correct. Each of those is decided elsewhere, on the Song's evidence.

**Added after the replays**

14. **The listener comes first.** The Song exists so that a person can read a chorus in a minute and act with a word, and so that a dwarf can sing with one line and a machine can come online with a sentence. A pipeline with every signal and no readable view lost 83% of them. Whatever the protocol adds — a verse kind, a clock, a key — is measured against the time it costs the one who sets it up, the one who sings, and the one who reads; a step that needs more explanation than its command is a step to remove.

## 2. Architecture

The Song core owns the verse and refrain formats, chain checks, the four clocks and suspicion levels. Consumers use it, storage backends implement its interface, and a fence allocator supplied by the consumer enforces recovery at the resources work touches. **Core.**

&#91;embedded content: Song architecture · two consumers, one interface, two backends\]

Dependencies point toward the core and its interfaces, never toward a backend, so the Song runs on a jsonl file today and on Taso when the fleet needs it. The **fence allocator** is the consumer's control plane and is a voice: every epoch it grants is a \`grant\` verse followed by an \`activation\` verse (§6). Two things sit beside the core without singing: the **scribe**, which computes tomes (§6), and the **cadence source** (fixed cadences in Core, Aegloss Later, §5). The owner's dashboard only renders.

## 3. The verse

A verse is one immutable, signed record. Its identity is its content hash. Each singer's verses for one task form a chain keyed by `(singer, task, epoch, seq)` and linked by `prev`; the Song is the union of all chains, which replicates as a grow-only set. **Core.**

| Field | Type | Meaning |
| --- | --- | --- |
| `v` | integer | Protocol version, 3 |
| `singer` | string | Stable identity of the voice, bound to a key in the roster (§6) |
| `roster` | integer | The roster version whose key signs this verse |
| `task` | string | The issue or task this chain belongs to; a Goliath machine uses its boot session |
| `epoch` | integer | Granted by the fence allocator (§6); starts at 0 per task and rises on each relaunch |
| `grant` | hash | The `grant` verse that granted this epoch to this singer for this task; writes also require its \`activation\` to be known |
| `seq` | integer | Strictly increasing within an epoch |
| `prev` | hash | This singer's previous verse in the chain; empty only for the first verse of an epoch |
| `cause` | hash, optional | The verse this one answers or results from |
| `kind` | enum | One of the kinds in §4 |
| `progress` | object | `height` and `head` of the singer's last durable commit; for a lane, `head` is the git SHA |
| `promise` | object, optional | On `claimed`: unit type, expected cadence, whether intermediate checkpoints are expected, and the version of the source that produced it |
| `blocked_on` | string, optional | What a waiting singer needs; required when the event is `waiting` |
| `about` | object, optional | The full identity of the verse or voice this concerns: `singer`, `task`, `epoch`, and the verse `hash` when one is meant |
| `body` | object | Kind-specific content, kept small; large state lives in storage and is referenced by hash |
| `note` | string, optional | One human-readable line for the owner's view |
| `sung_at` | timestamp | Advisory wall-clock time; never used for ordering |
| `sum` | hash | Content hash over the canonical serialization (CBOR, sorted keys) of every field except `sum` and `sig`; the verse's identity |
| `sig` | signature | The singer's Ed25519 signature over `sum` |

**Ordering and arrival.** Order within a chain is `seq`; across chains there is none. A verse that arrives before its predecessor, or whose `cause` or `grant` is not yet known, is held as pending while the missing verse is fetched; absence is not evidence of forgery. Two verses with the same `(singer, task, epoch, seq)` and different hashes are both kept, indexed under the same key, and marked as a fork. A verse from an epoch older than the installed fence is still recorded as history (§6); recording it authorizes nothing.

Example: the laptop sings before its lid closes.

```json
{"v":3,"singer":"goliath-laptop","task":"boot-2026-11-02","epoch":0,"seq":208,
 "prev":"b3:9f2c…","kind":"boundary",
 "progress":{"height":51734,"head":"b3:41aa…"},
 "body":{"event":"going-offline","state":"taso://vessels/session/51734"},
 "sung_at":"2026-11-02T21:04:11Z","sum":"b3:c07e…","sig":"ed25519:5Kq1…"}
```

### The refrain

While a dwarf works a task it keeps singing the same verse: a **refrain**, repeated on a short timer, until the task finishes, the dwarf fails, or it changes task. The refrain is how peers know the dwarf is still reachable; it is not a verse and never counts as progress. **Core.**

| Field | Meaning |
| --- | --- |
| `singer`, `task`, `epoch` | Which chain is being worked |
| `n` | Counter, strictly increasing within the epoch |
| `last` | Hash of the singer's latest verse in that chain |
| `progress` | The same `height` and `head` as that verse |
| `sent_at`, `sig` | Advisory time; signed under the same key |

- **Timer.** The interval is set by the profile (§7): about a minute for a lane, a quarter of an hour for an awake laptop, none while asleep.
- **Freshness.** A refrain counts as an observation only if its `n` is higher than the last one seen from that chain. Reading the same refrain again, or an older one delivered late, never resets a clock. Elapsed time is measured on the observer's monotonic clock, never on `sent_at`.
- **Storage.** A refrain is not appended to the chain. Storage keeps only the latest per `(singer, task, epoch)`.
- **What it proves.** That the singer signed and reached storage at some point after its previous refrain — not "just now". When now matters, the probe loop asks (§5). A refrain whose `progress` has not changed for an hour is the gray case made visible: reachable, not moving.
- **What it never does.** It never resets the progress clock, the task clock or the waiting clock (§5).

## 4. Verse kinds

Kinds fall into four families; anything else belongs in `body`.

| Family | Kind | Sung when | Carries | Layer |
| --- | --- | --- | --- | --- |
| Self | `boundary` | A state boundary is crossed | The event and, if state was saved, a reference to it | Core |
| Self | `work` | A unit of work starts, advances or ends | The task reference and the commit it produced | Core |
| Peer | `ack` | A singer hears a verse that asked to be heard | `about` with the verse hash | Core |
| Peer | `vouch` | A peer directly or indirectly confirms another is reachable | `about` + how it confirmed | Core |
| Peer | `suspect` | A peer's clocks for another run past their thresholds | `about` + which clock and by how much | Core |
| Peer | `ruling` | A judgment of work: merge, reject, concur, dissent, accept, retract, permit, deny | `about` the work's verse; the verdict | Core |
| Record | `grant` | The allocator reserves an epoch for a singer and task | The holder, task, epoch and the resources that must install it; its hash is the grant id | Core |
| Record | `activation` | Every required resource has acknowledged the grant | The grant id and the acks; only now may the holder write | Core |
| Record | `close` | The allocator closes a task | A closure snapshot: the grants, each chain's endpoint, the forks | Core |
| Record | `roster` | The roster changes at a run boundary | The new roster version, signed by the owner | Core |
| Record | `tome` | A closed task's chains are compacted | The manifest and Merkle root (§6) | Core |
| Record | `addendum` | A signed verse arrives after its task's closure | The tome and the late verse, both unchanged | Core |
| Intent | `offer`, `claim`, `handoff` | Work is put on the blackboard, taken, or passed on | The task, its spec by hash, the intended voice | Later |

**Which verses ask to be heard.** Only `boundary` verses that reference saved state and `work` verses that end a unit (`package`) are re-sung until acknowledged. Acks, vouches, suspects and refrains are never acknowledged, so there are no acks of acks.

**Tasks are not verses in Core.** A `work` verse records that work happened; the assignment arrives through the consumer's own channel. The intent family (Later) moves assignment onto the song as a blackboard — offered and claimed in the open, never commanded — and is specified once the Core's clocks and fences are proven.

## 5. Liveness

Every voice judges every other on four levels of evidence and four separate clocks, and never reaches a verdict — only a suspicion level that the recovery authority acts on. **Core**, except where marked.

### Four levels of evidence

| Level | Evidence | Answers |
| --- | --- | --- |
| Reachability | A fresh signed refrain or verse arrived | Can we hear it? |
| Capability | The verse was durably written before it was announced (§6), so its own commit is visible in storage | Can it still write? |
| Progress | `progress.height` advanced, or a `work` verse names a new head | Is it moving? |
| Usefulness | Its work follows its initial task and passes acceptance (Usefulness and Safety tab) | Is what it does any good? |

Usefulness is evidence, not proof: merge yield depends on task difficulty and reviewer quality as much as on the lane. The Loremaster (§7) audits that limit. The four levels are what infrastructure detectors lack; a lane that refrains, commits and pushes carts that keep failing assay is alive by every protocol and dead by function.

### Four clocks

The v2 spec mixed expected task duration with expected time until the next observation. They are different quantities, so each voice keeps four clocks per peer.

| Clock | Fed by | Reset by | Overdue means |
| --- | --- | --- | --- |
| Reachability | Any signed refrain or verse | Every refrain | We cannot hear it |
| Progress | A verse with a new `height` or `head` | New progress only | Reachable but not moving |
| Task | `claimed` time against the promise, minus waits | Never, within an epoch | Taking longer than promised |
| Waiting | `blocked_on` set | `blocked_on` cleared | The dependency itself is stale |

- **Only new work resets the progress clock.** Acks, vouches and refrains never do. The task clock never resets within an epoch: the deadline is fixed for the claimed unit, minus defined waits.
- **`blocked_on` pauses the task clock and starts the waiting clock.** Waiting has its own cadence per profile; a wait past it is called like any other silence. Declaring a block never exempts work from scrutiny indefinitely.
- **Some operations promise no intermediate checkpoint.** A `promise` that says so disables progress-clock escalation for that unit; the task clock alone decides whether it is overdue.

### Promised cadence

When a singer claims work it sings a `promise`: the unit type and the cadence its peers may hold it to. **Core** reads per-state cadences from the pipeline's own corpus — the last ten closed chains of the same unit and area, or the slice's declared risk line — and falls back to the profile's fixed values below ten; a cadence tuned on one pipeline is wrong on the next (90 minutes was one pipeline's run length and another's quarter-slice). Escalation is fixed: a peer calls at 1× the cadence, calls again at 1.5×, and sings `suspect` at 2×. **Later**, Aegloss supplies learned P50/P80/P95 per unit type behind the same `CadenceSource` interface, and the thresholds become P80, P95 and beyond; it provides and never sings, and the source version rides in every promise so a stale estimator is visible in the song. When the source is unreachable, the last promise for that unit type or the profile's fixed cadence applies.

### Suspicion and corroboration

Each voice keeps a suspicion level per peer and per clock, rising with how far past its threshold that clock is. Evidence from others adjusts only the clock it speaks to: a `vouch` lowers *reachability* suspicion and nothing else, because a peer reaching B says nothing about whether B is progressing or overdue. A `suspect` names its clock. Both are weighted by the independence the voices declared (§6).

What three voices give is corroboration, not diagnosis, and only for reachability. If A cannot hear B and C just vouched for B, A has evidence of a path problem, and A's reachability suspicion alone should not trigger recovery. If neither A nor C can hear B, B may be crashed, isolated or delayed; the song records that two voices agree, and the recovery authority decides. A one-way cut belongs to a path, not a voice, and is recorded as such. Progress and task suspicion are never lowered by a vouch; a lane that everyone can hear and that has not moved past 2× its cadence is suspected however many vouches it has.

**Assumptions for eventual recovery.** A suspicion becomes an action only while a majority of the roster is alive and can reach storage and each other, and the fence allocator is reachable. Outside those conditions the Song keeps recording and nothing is relaunched.

### Listening: on a timer and at boundaries

Listening at boundaries alone is not enough: if every voice reads only before its next verse, stalled voices stop observing each other. So each voice runs a probe loop on its profile's interval, and also listens before every verse.

1. **Read** the latest refrain and verse per peer; update the four clocks.
2. **Probe** any peer whose reachability clock is overdue: a direct request through the consumer's channel.
3. **Probe indirectly** if that fails: ask one other voice to reach the peer and `vouch` or `suspect`, as SWIM does.
4. **Wait one cadence** after a progress, task or waiting clock first crosses, for the lane's own next event. Most crossings clear themselves (on the second replay, 58% of unanswered suspects moved within six hours); a call on every crossing is wrong more often than right. The exception is a kind that never clears on its own — an authentication or setup failure, a lock with no run, an allocator's `NotActivated` — which is called at once.
5. **Call** a peer whose clock is still overdue after that — a direct message, never a verse: "your promise was 10 min, you are at 14, your height has not moved — are you held?"
6. **Call again** after one more cadence.
7. **Sing `suspect`**, naming the clock.
8. **Hand over.** The recovery authority reads the suspicion and its corroboration and decides; a relaunch goes through the fence (§6).

The watchdog is a voice like any other: it sings its own refrain, and when it checks whether the chorus has gone silent it excludes its own verses from the count. Its silence is noticed by the others, so it needs no watcher above it.

### The owner sings

The owner is a voice, not a listener above the chorus. They sing presence (`present`, `away`), every permission granted or denied as a `ruling` about the waiting lane, acceptances, retractions, and the roster. The owner's cadence is human, so the owner does not count toward the minimum three voices, and `blocked_on: human` while the owner is `away` is expected rather than stuck. The owner's key lives on a hardware token, which makes the owner the most independent voice in the chorus.

The owner's view renders the song: one row per voice with its four clocks, what it waits on and its gray-work signals; quiet voices flagged; and a tempo bar of verses per 15 minutes — throughput measured in progress, not in API calls.

## 6. Storage, fencing, trust and independence

A signature proves who signed some bytes. It does not prove the bytes are durable, that a voice is dead, that credentials are independent, or that a relaunch is safe. Each of those has its own contract here, and the Core's correctness argument rests on them.

### Storage and durability

The Song defines a storage interface and depends on no backend. A verse is appended durably first and announced second, so singing proves the write path works. **Core.**

```rust
pub enum Durability { Process, Host, Replicated(u8) }

pub trait SongStore {
    /// Append to an unfenced chain (refrains' chains, peer verses, records).
    /// Refused with FenceError::Fenced if (singer, task) has an installed fence.
    fn append(&self, verse: &Verse, level: Durability) -> Result<Receipt, StoreError>;
    /// Append to a fenced chain. Succeeds only if verse.grant is a valid allocator
    /// grant for (singer, task, epoch) and verse.epoch >= the installed fence.
    /// Check and append are one atomic step, on every writable replica.
    fn append_fenced(&self, verse: &Verse, level: Durability) -> Result<Receipt, FenceError>;
    /// Install a fence. Durable at `Host` or better before it acknowledges;
    /// never lowers an existing fence.
    fn install_fence(&self, grant: &FenceGrant) -> Result<FenceAck, StoreError>;
    fn refrain(&self, r: &Refrain) -> Result<(), StoreError>;
    fn read_since(&self, cursor: Cursor) -> Result<Vec<Verse>, StoreError>;
    /// Latest (epoch, seq, hash) for one chain.
    fn head(&self, singer: &SingerId, task: &TaskId) -> Result<Option<Head>, StoreError>;
}
```

| Level | A verse at this level survives | Used by |
| --- | --- | --- |
| `Process` | The singer's process crashing | Tests only |
| `Host` | Power loss on the host: fsync has returned | A single host; the jsonl backend |
| `Replicated(k)` | Loss of fewer than k replicas | The fleet and the mine, on Taso |

A profile states the level a verse must reach before it counts as sung. "One flush" is not a contract; the level is. Replication of verses is set union; replication of fences is not (below).

### Integrity rules

1. The singer computes `sum` and signs it when it creates the verse. No intermediate node recomputes or re-signs it.
2. Every node verifies `sig` against the key of the roster version the verse names, then `sum`, then the `prev` chain, on ingress and again on read.
3. A verse that fails any check is not appended. The receiver sings a `suspect` about its singer, citing the failed check.
4. Verses are stored by content hash. The key `(singer, task, epoch, seq)` indexes every variant under it; two variants are a fork, kept as evidence and reported.
5. A verse whose `prev`, `cause` or `grant` is missing is held as pending and the missing verse fetched. Arrival order never causes permanent rejection.
6. A verse from a fenced chain is accepted only through `append_fenced`. A verse from an epoch below the installed fence is recorded as history and authorizes nothing.

### Fencing: the recovery contract

An epoch number becomes a fence only when the resource it protects enforces it. Set-union replication cannot do that on its own: a stale replica would accept epoch 7 after epoch 8 began, and two peers could both start epoch 8. So recovery has one small contract. **Core.**

| Who | Does |
| --- | --- |
| **Fence allocator** | One per consumer, supplied by its control plane. Reserves `(singer, task, epoch)` monotonically and durably before anything else, so there is exactly one epoch 8 for a given singer and task, and a duplicate request returns the pending grant. It is a voice: a relaunch is a `grant` verse, then an `activation` verse once every resource has acknowledged. |
| **Protected resources** | The song store, the merge gate (the Smith's forge), and any store the task writes. Each keeps an installed fence per `(singer, task)` with its grant id, and accepts a mutation only if the mutation's grant is installed there, its activation is known, and its epoch is at least the installed fence — checking and applying in one atomic step. |
| **The replacement** | Starts only after the `activation` verse exists: every resource the grant lists has acknowledged. |

- **A grant, not a number.** A write carries the id of the allocator's signed `grant` verse (whose hash is known before any resource is touched) as its `grant`, bound to the singer, the task and the epoch. Resources check the grant, then the epoch. A worker cannot choose a higher number for itself.
- **Persistent and monotonic.** An acknowledged fence survives resource restart (durable at `Host` or better before the ack), and installing an older fence never lowers the current one. Otherwise fence 8 could vanish in a crash and epoch 7 become writable again.
- **Complete enforcement.** Every writable replica or access path to a protected resource enforces the fence or refuses writes. Activation waits for every path the profile lists; one replica's ack cannot authorize activation while another still accepts stale work.
- **After fence 8 is installed, every mutation carrying fence 7 is rejected** at that resource, including from a paused worker that resumes later. Its late verses are still archived as evidence.
- **If the allocator cannot be reached, nothing is relaunched.** The suspicion stays visible on the song and in the owner's view.
- **If a resource cannot be reached to install the fence,** the replacement is not activated for effects on that resource. It may read and plan; it may not write there.
- **Scope of "wrong guesses cost only work".** The guarantee holds for fenced effects. An external side effect — an API call, a deploy, a message sent — can be duplicated across epochs when an acknowledgement is lost. Such effects are deduplicated by a stable `(task, operation_id)` chosen when the operation is first attempted, so a retry in epoch 8 reuses epoch 7's key; the epoch travels separately and authorizes the attempt. Each profile lists which effects are fenced, which are keyed this way, and which are neither; the last are named as a risk, never assumed safe.

### Signing and the roster

Every voice holds an Ed25519 identity key and signs every verse and refrain. The **roster** lists each voice's current public key, its declared failure domains and the roster version. Replicating evidence and agreeing on authority are different problems, so the roster is governed differently from the song.

**Core: a roster fixed for the run.**

- The owner signs the genesis roster at `runo init`, and membership is fixed for the run: no voice is added, removed or re-keyed while the chorus is running.
- A change is a new roster version, signed by the owner and sung as a `roster` verse at a run boundary: the chorus stops, the version activates, the chorus restarts. Every verse names the roster version that signs it, so old verses verify against old keys, a revoked key cannot sign under the new version, and delayed historical verses are admitted under the version they name.
- Versions are kept, never deleted: history stays verifiable.

**Later: live membership and key recovery.** Changing the roster while the chorus runs, quorum-signed admission, key pre-rotation (a committed next key, after KERI) and owner recovery through a committed offline key or a delayed quorum are specified only once their state transitions are written down: which roster version authorizes the next change, what two overlapping quorums may not both sign, and what survives a signer's restart.

### Tomes

When a task finishes, its chains are closed and sealed into a **tome**: one finished task, one tome. A tome is computed, not authored: the same closed chains always yield the same tome, so any voice can recompute and verify it. **Core.**

- **Closure.** The consumer's control plane — the same authority that allocates fences — closes a task by installing a \*closed\* fence above every epoch at each resource, then singing a `close` verse carrying an immutable closure snapshot: the grants it issued, each chain's endpoint, and the forks from the fork index. Closure stops writes; anything arriving afterwards is history. The tome is computed from the snapshot, never from live heads, so a late verse cannot change what a sealed tome says.
- **The scribe.** Tomes are written by the scribe, a component beside storage that is not a voice. It needs no signature of its own; it provides, like the cadence source.
- **Closure manifest.** The tome lists, for every granted chain of the task, `(singer, epoch, last seq, last hash)`, any forks, and a Merkle root over the included verses. A Merkle root commits to what is included; the manifest, checked against the grants, states that nothing granted was left out.
- **Late verses and forks.** A signed verse that arrives after closure is kept exactly as sung, and the scribe writes a separate addendum record naming both the verse and the tome; the tome itself is never reopened. Forks present at closure are listed in the manifest, so a tome never hides equivocation.
- **Summary.** Verse count, final progress, outcome, net time and total waits — the run record the cadence source learns from. Refrain counts are telemetry, not part of the verifiable summary.
- **Who watches the scribe.** Liveness property L1 says every closed chain ends in a tome. A chain left untomed past its cadence is visible to every voice, so a stalled scribe is noticed without singing.

**Retention.** S3 (a tome covers its chains), provenance, the spawning signal and the cadence source all read tomes.

| Record | Hot while | Then |
| --- | --- | --- |
| Verses | Their task is open | Cold storage in Taso once the tome is sealed; still provable against it |
| Refrains | Only the latest per chain is ever kept | Nothing to retain |
| Tomes | Any open task's cause tree reaches them, plus a signal window (30 days to start) | Cold storage; still provable |

Both windows are set at `runo init`, never later.

### Independence: declared, then verified

Mutual liveness works only if voices fail independently, so independence is part of the protocol. What the Core can do is check what voices *declare*; proving independence is Later.

- **Declared domains.** Each roster entry declares the voice's failure domains: host, credential set, model provider, network path.
- **Credential fingerprints.** `runo enroll` records a salted hash of each voice's credential *reference* — a path in the secret manager, never the secret. Matching fingerprints prove two voices share a reference. Different references can still point at one account, so this detects declared sharing, not proven independence.
- **Diversity check.** `runo init` refuses a chorus in which every voice shares a host, a credential reference or a provider, and refuses one without at least one voice outside the lanes' common domains.
- **Weighted witnesses.** A vouch from a voice sharing your declared domains counts less than one from a voice outside them.
- **The sentinel.** The cheapest independent voice has no model at all: a small deterministic singer on separate hardware. It fails in different ways from an LLM lane, which diversifies the chorus by implementation as well as by machine.
- **Later: verified independence.** Provider-side account identity, where a provider exposes it, replaces the fingerprint for that domain.

### Setting up a chorus

Runo never creates or holds provider credentials; those come from the control plane, and no voice requests access. **Core.**

| Command | Run by | Does |
| --- | --- | --- |
| `runo init` | The owner, once | Reads the chorus manifest, checks diversity, sets durability levels and retention windows, names the fence allocator, and writes the genesis roster for the owner to sign |
| `runo enroll` | Each host, per voice | Generates the voice's key pair on the host, fingerprints its credential reference, and submits the entry for the owner to add to the roster |

The **chorus manifest** lists, for each voice: role and profile, host, model provider (or none), a credential reference, its network path, and the resources it writes that must be fenced.

| Host type | Where the voice key lives |
| --- | --- |
| Owner | Hardware token |
| Laptop or desktop | OS keychain |
| VPS or Pi | A file readable only by the Runo service user |
| Container | A mounted secret, scoped to that one container |

A private key never leaves the host that generated it.

## 7. Profiles

A profile describes how a voice behaves in the song and nothing more: its **role**, its **state machine**, its **listening graph** and its **cadence source**. Policy — what a role may do, how it is configured, what it is paid — comes from the consumer's agent configuration, never from the profile. **Core.**

|  | Dwarven Mine voice | Goliath node |
| --- | --- | --- |
| Roles | Lane, Smith, Foreman, Loremaster, sentinel, owner | Machine, sentinel, owner |
| Lane state machine | `claimed → building → package → folded \| exited`; `waiting` reachable from any state | `online → (sleep ↔ wake, docked ↔ undocked) → going-offline`; `state-saved`, `state-restored` as boundaries |
| Listening graph | Foreman ↔ Smith, Foreman ↔ lanes, Loremaster → Smith, sentinel → all, owner → all | Every machine → every other; sentinel → all |
| Refrain interval | About a minute | A quarter hour while awake; none asleep |
| Fixed cadences (Core) | Building 30 min; waiting on CI 60 min; waiting on human: the owner's presence | Hours to days by state |
| Cadence source (Later) | Aegloss, per unit type | Aegloss, per boundary pair |
| Fenced resources | Song store, merge gate, task worktree | Song store, state vessel in Taso |
| What stuck means | A lane refraining without a new head | A restore that never finished |

A verse that breaks its role's state machine — `package` before `claimed` — is itself evidence and raises suspicion.

**The mine in the song's terms.** Tickets are the seams. The Foreman launches the ones that are unlocked; the lanes are the miners; the Smith is assayer and forge, because his rulings decide what enters the hoard. The owner is the audience and the one who decides which seams exist. The fence allocator is the mine's control plane; today that is Paperclip, which already does atomic task checkout and would sing `fence` verses through a small adapter.

**Provenance.** Every change admitted to the hoard names the verses behind it: the lane's `package` verse with its head, and the Smith's `ruling` with his verdict. When a merged change later fails, its tome says which lane dug it, from which ticket, and which checks it passed. The voice that rules on a cart is never the voice that dug it.

### Who watches the Smith

No voice rules unruled. The Smith is watched for reachability and progress like any voice; the **Loremaster** checks whether his rulings are right. It audits after merge and never stands in the merge path, re-assays from the ticket and the diff with fresh context, and sings a `ruling` of `concur` or `dissent`. Cert-critical paths, large changes and anything later reverted are always audited; the rest at a measured rate. A `dissent` undoes nothing and reports to no one above: every voice hears it, and the Smith answers in the open, upholding with reasons or retracting by appending. The Smith's overturn rate and the share of the Loremaster's dissents that led to a retraction are both computed from the song.

## 8. Non-goals

- **Not the ledger.** The ledger is the narrative; the song is the pulse.
- **Not a scheduler.** It assigns no work in Core; the intent family (Later) offers work, it never commands it.
- **Not a message bus.** Verses are records; calls and probes go through the consumer's direct channel.
- **Not consensus.** The Song never decides who is dead or which value wins; it supplies evidence, and the fence allocator acts.
- **Not an authority.** Fences, rosters and rulings are enforced by the resources and voices that own them; the song only records that they happened.
- **Not a state store.** Large state lives in storage; a verse holds a reference by hash.
- **Not the admission policy.** The Smith decides what enters the hoard; the Song records that he did.

## 9. Design lineage: limits as inspiration

Most of the design came from taking impossibility results seriously. A proof that a problem cannot be solved names the assumptions it rests on; each assumption the Song gives up becomes a feature. v3 corrects two rows that v2 overstated.

| Limit | Assumption it rests on | What the Song does instead |
| --- | --- | --- |
| FLP: deterministic consensus cannot guarantee termination in an asynchronous system with one crash | Termination must be guaranteed | The Song never seeks a verdict; it keeps suspicion levels and states the conditions under which recovery eventually happens (§5) |
| A suspicion can be wrong, because slow and crashed look alike from outside | Acting on a suspicion is safe | Fencing enforced at the resource, so a wrong relaunch costs work within the fence and is listed as a risk outside it (§6) |
| A detector needs an observer | An observer above the observed | None above: every voice observes every other, including the watchdog and the owner |
| Two parties cannot tell a crash from a cut link | Only two observers | Three independent voices corroborate and probe indirectly; they still do not diagnose |
| Fixed timeouts are too eager or too slow | One cadence fits all | A promised cadence per task, fixed in Core and learned Later |
| Reachability says nothing about work (gray failure) | Health is one signal | Four clocks and four levels; the refrain carries reachability so progress can be judged on its own |
| Estimates on little data mislead | An estimate must always be given | Below five runs, no learned promise; the fixed cadence applies |

**The limit the Song cannot escape alone: common-mode failure.** Mutual liveness works only if voices fail independently. The Song checks declared independence, weights correlated witnesses down, and requires one voice outside the lanes' domains; it cannot prove independence, and says so (§6).

**Method, for future limits.** State the impossibility in one sentence; list its assumptions; negate each in turn; for each negation, ask what system you would build if it held for you. Most rows are dead ends; usually one is a design.

## 10. Properties, proof and open questions

The safety and liveness properties, the usefulness definition and the three-step proof plan live in the Usefulness and Safety tab. Their assumptions are stated here, because a property without its assumptions is a claim.

| Property | Holds when |
| --- | --- |
| S1 No write from a stale epoch | Every protected resource enforces its persisted fence atomically on every write path, grants are allocator-signed, and the replacement started only after every path acknowledged |
| S2 No unsigned or invalid verse appended | Every node verifies against the roster version the verse names |
| S3 Every tome covers its chains | Closure is authorized by the control plane, expected chains come from the grants it issued, and late verses become addenda |
| S4 Every `cause` points to an existing verse | Verses with a missing cause are held pending until it arrives; a verse is never appended with a dangling one |
| L1 Every claim ends in a tome or a `suspect` | A majority of the roster is alive and reaches storage; the scribe is running |
| L2 Every offer is claimed or withdrawn | Later: the intent family |
| L3 Every ruling is applied or contested | The resource a ruling changes sings its change with the ruling as `cause`; a ruling unanswered past its cadence is called like silence, and after two calls the resource's owner is suspected |

**The failures the tests must drive.** Simultaneous relaunches of one lane; a paused old worker resuming after its replacement; a storage replica partitioned during a fence install; a resource restarting after it acknowledged a fence; verses arriving out of order; a replayed or delayed refrain; a worker that keeps refraining and acknowledging while its height never moves; an external effect retried across epochs with a lost acknowledgement; the allocator unreachable during a suspicion; an unchanged signed verse arriving after a tome is sealed.

**Replay result, 2 November 2026.** The first proof step ran over the HODD Bob pipeline's four days (Runo Replay Report). With v0.1 cadences the Core clocks did not beat Paperclip's own detection: Paperclip flagged 533 lanes to Runo's 289 and was first on 134 of the 224 lanes both flagged. But 668 of 778 stall episodes were never followed by a human action, and Paperclip's 1,326 signals had no route to a person. The gap is delivery, not detection, which is principle 3 broken in practice. The success criterion for the first step was changed on that evidence, deliberately and on record: from "detects earlier" to "reaches a person". Consequences: the routing layer — existing signals in, call ladder and owner's view out — is built before Runo's own clocks; the task and waiting clocks come first among the clocks because Paperclip lacks them; the reachability clock is fed by refrains and output timestamps and **never switches off**: a lane that is `waiting` is still expected to answer, so waiting pauses the task clock and relaxes progress expectations while reachability keeps asking its question; only a declared sleep changes the reachability expectation, and where Runo has no direct evidence (Watch mode, board events only) reachability is shown as *unknown* rather than silently dropped. The first fixed reachability cadence is 45 minutes. Two signals join the Usefulness tab, review loops from the forge and idle polling. The gray-work signals held: 32 spawning tasks, 12 PRs with ten or more reviews, 12 lanes over four hours to a first head. The hypotheses below are kept; the first two rows now have measured starting points.

**Hypotheses to measure** (Magos principle: estimate, measure, understand, implement).

| Hypothesis | Estimate | How to measure |
| --- | --- | --- |
| Durable verses are cheap | A `Host`-level verse every 30 s costs under 1% of a laptop's flush capacity | Append latency on the jsonl and Taso backends |
| Refrains are cheaper still | A refrain a minute per lane adds under 1% to store load at 15 lanes | Store load with and without refrains |
| Verification is cheap | Checking `sig`, `sum` and `prev` on ingress costs under 5% of replication throughput | Replay a recorded song through ingress with and without checks |
| Four clocks beat one | Separate reachability and progress clocks raise fewer false calls than v0.1's single cadence, with no later detection of real stalls | Replay the HODD Bob corpus through both |
| Fencing holds under relaunch races | Zero stale-epoch writes across the simulation seeds | Deterministic simulation with the failure list above |
| Three voices corroborate a cut | A one-way cut is recorded as a path problem, not a dead voice, in over 90% of injected cuts | Drop one direction between two voices in a test chorus |

**Open questions**

- [ ] Which external side effects in the mine can be made idempotent by `(task, epoch, seq)`, and which stay listed as unfenced?
- [ ] How does the Paperclip adapter map atomic checkout onto `fence` verses and fence installation at the merge gate?
- [ ] What is the right refrain interval for a lane, measured against store load and detection delay?
- [ ] Later: the roster state machine for quorum admission and pre-rotation.
- [ ] Later: the intent family's claim race — settled by a conditional append in Taso, or by the offerer's ack?
