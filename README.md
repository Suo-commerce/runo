# Runo

Runo is the Dwarven Song: a protocol and library for mutual liveness. Every participant sings
signed, durable verses into a shared append-only record, repeats a refrain while it works, and
its peers read both to tell who is reachable, who is making progress, and whose work is any good.
The Song supplies evidence; the authority to act on it lives in a small fencing contract enforced
at the resources that work touches.

It began as a pacing layer for an agent software factory and is built for two consumers: the
Dwarven Mine (AI agents producing code) and Goliath (a fleet of personal machines).

## Three tiers

| Tier | Command | Gives |
| --- | --- | --- |
| Diagnose | `runo diagnose --paperclip <db-or-jsonl> [--forge <url>]` | The replay report on an existing pipeline. Read-only. |
| Watch | `runo watch --paperclip <db> --notify telegram:<chat>` | The triage view and the call ladder, as a chorus of one. |
| Chorus | `runo init` → `runo enroll` → `runo node` | Signed verses from every voice, refrains, timers, fencing, tomes. |

Until the Rust `diagnose` exists, the Python kit in `runo-replay/` is tier 1.

## Documents

- The Dwarven Song: Specification v3 — the protocol (`docs/song-spec.md` once exported)
- Runo Core: Technical Specification — the build plan (`docs/tech-spec.md` once exported)
- Runo Replay Report — the evidence from two pipelines (`docs/replay-report.md` once exported)

## Workspace

Eight library crates, a CLI and a simulator, layered so every crate depends only on crates below it
and `runo-core` depends on nothing in the workspace. See `CLAUDE.md` for the design rules and the
ownership table.

```
crates/runo-core      wire types, canonical CBOR, blake3, ed25519, pure ingress, time trait
crates/runo-profile   profile and chorus manifest TOML, load-time checks
crates/runo-cadence   CadenceSource: fixed and corpus-read cadences
crates/runo-store     SongStore and role traits, jsonl and in-memory backends, store service
crates/runo-clock     the four clocks, escalation, suspicion
crates/runo-fence     grants, activations, guard, task lifecycle, operation keys
crates/runo-node      the voice runtime: sing, refrain, probe, ladder
crates/runo-scribe    closure snapshots, tomes, Merkle roots, addenda
crates/runo-adapter-paperclip   the herald: Paperclip signals as peer verses
crates/runo-testkit   builders, fixture chorus, SimClock, conformance macros (dev only)
crates/runo-sim       the fourteen scenarios under a deterministic simulator
crates/runo-cli       the runo binary
model/                TLA+ model of claim, grant, activation, close, seal
profiles/             example profiles and chorus manifest
runo-replay/          the Python diagnostic kit
```

## Build order

replay (done twice) → TLA+ model → `runo-core` and `runo-store` → triage view → own clocks →
fencing and scribe → first real chorus → Later. Each milestone has a gate in the technical
specification §14; nothing starts the next milestone before its gate is green.

## Principle 14

The listener comes first. A dwarf sings with one line, a machine comes online with a sentence,
an owner reads a chorus in a minute and acts with a word. A step that needs more explanation
than its command is a step to remove.
