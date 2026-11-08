# Runo — instructions for agents working in this repository

Read `README.md` first. The protocol is the Dwarven Song Specification v3; the build plan is the
Runo Core Technical Specification. This file holds the rules that keep the code matching them.

## Design rules

- **One crate, one reason to change.** core = wire format · profile = configuration · cadence =
  estimation · store = persistence · clock = detection policy · fence = recovery authority ·
  node = runtime · scribe = compaction. If one change touches two crates for one reason, the
  boundary is wrong: say so instead of making the change.
- **`Payload` is matched in exactly two places:** `runo_core::ingress::admit` and
  `runo_clock::Chorus::observe`. Everything else asks those two.
- **Extend by trait, not by match arm.** Later features arrive as implementations of
  `CadenceSource`, `RosterGovernance`, `SongStore`, `FencedResource`, `AllocatorJournal`,
  `IndependenceVerifier`.
- **Every trait has a conformance suite** in `runo-testkit` (`store_conformance!`,
  `fenced_resource_conformance!`, `cadence_source_conformance!`). A new implementation is not
  done until the suite passes against it.
- **Role traits over god traits.** Callers take `&dyn VerseReader`, `&dyn ResourceFenceStore`, never
  `&dyn SongStore` unless they need all of it.
- **Dependencies point down.** `runo-core` imports nothing from the workspace. Check the table in
  `README.md` before adding a `[dependencies]` line.
- **Errors are per crate** (`thiserror`), converted at boundaries with `From`. No shared
  mega-error enum.
- **No `unsafe`** (`forbid`). **No new external dependency** without a line in the technical
  specification §2 saying why.
- **Wire types change only under the version rule** (technical spec §3): new fields need a
  version bump; `Option` and defaults are for values within the existing field set.
- **Every duration is `Millis`** in protocol types, interfaces and persisted data; escalation
  factors are integer tenths; `std::time::Duration` appears only at a timer boundary.

## Ownership table — one home per concept

| Concept | Home |
| --- | --- |
| Canonical encoding, hashing, signing | `runo_core::codec`, `runo_core::hash`, `runo_core::sign` |
| Time (`Clock`, `Instant`) | `runo_core::time` |
| Ingress decisions | `runo_core::ingress` |
| Verse and refrain builders, fixture chorus, `SimClock`, `FailurePlan` | `runo_testkit` |
| Merkle tree | `runo_scribe::merkle` |
| Fencing types (`Fence`, `FenceAck`, `Durability`, `RecordClass`) | `runo_core::fence`, `runo_core::ingress` |
| The store's installed fences | `runo_store::resource_fences` (`ResourceFenceStore`) |
| The allocator's reservations and closure record | `runo_fence::AllocatorJournal` |
| Escalation thresholds | `runo_clock::escalate` |
| Human-facing delivery | `runo_node::channel::human` |

If you need one of these somewhere else, import it. If it does not exist, add it to its home.

## Before opening a PR

- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- `cargo deny check`; `npx jscpd crates`
- `cargo run --release -p runo-sim -- --seeds 1000` (the simulator sweep runs here, not in CI)
- A new trait implementation runs its conformance suite.
- A change to a Core rule cites the spec section it implements, in the PR body.

## The principle that overrides the others

The listener comes first (Song spec, principle 14). If a change makes `runo sing`, `runo watch`
or `runo view` need more explanation than its command, the change is wrong.
