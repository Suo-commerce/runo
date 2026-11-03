# Runo Core Technical Decisions

3 November 2026

This record captures the design choices agreed during our review of the thirteen reported inconsistencies in the technical specification. Use it to reconcile the specification, code, profiles, and tests. These are approved directions, not a claim that the repository has already been updated or verified.

# Ownership and interfaces

## 1 Shared types belong in core

Define Fence, FenceAck, shared IDs, and persisted record classification in runo-core. Keep storage interfaces and persistence implementations in runo-store. Keep allocation, activation, closure orchestration, and resource enforcement in runo-fence.

## 2 Separate validation and archive reads

Split StoreView into IngressView and ScribeRead. IngressView provides synchronous validation facts for pure ingress. ScribeRead provides closure snapshots, blobs, and historical verses for sealing and verification. Their exact method signatures remain to be reconciled against the source.

## 3 Keep two fence installation interfaces

Retain FencedResource::install(...) for protected resources and the store’s install_fence(grant: &Verse) interface. Connect them through an explicit store adapter that resolves and validates the grant before delegating to storage. Both paths must support epoch fencing and permanent closure with matching durability semantics; the exact closure request representation remains an implementation check.

## 4 Make acknowledgements explicit

Use FenceAck { resource, grant, fence, durability } in runo-core. Each acknowledgement identifies the resource, the grant, the installed Epoch or Closed fence, and the achieved durability. Do not rely on caller context or a bare durable boolean to supply these facts.

## 5 Separate the persistence contracts

Replace the overloaded FenceLog name with ResourceFenceStore for installed resource fences and AllocatorJournal for epoch reservations, pending grants, and closure recovery state. The interfaces may share a backend, but their contracts remain separate.

# Representation and integrations

## 6 Use milliseconds consistently

Use Millis(u64) throughout protocol types, interfaces, and persisted data, including cadence thresholds and overdue durations. Convert to Rust Duration only at timer boundaries. Keep escalation factors as integer tenths: \[10, 15, 20\] represents 1.0, 1.5, and 2.0.

## 7 Round escalation thresholds upward

Calculate threshold_ms = ceil(base_ms × factor_tenths / 10). Use a wider integer intermediate and reject a result that exceeds u64. Fractional milliseconds round upward so escalation never precedes the calculated deadline. Overflow is a configuration error.

## 8 Use the Paperclip API first

Use reqwest for the primary Paperclip integration and JSONL imports for offline diagnosis. Do not include a direct database adapter in Core. Verify that the API exposes the required signals before implementation; pagination, incremental reads, and webhooks are capabilities to check, not assumed features.

## 9 Stage simulation coverage

Begin with protocol-first simulation using the real state machines, SimClock, MemStore, and a deterministic event queue. Progress later to runtime-inclusive simulation of async scheduling and networking. Defer the madsim versus turmoil choice until that stage.

Keep time, transport, storage, and task execution injectable. Preserve reusable scenario intentions and invariant assertions, with reproducible seeds and failure traces within each harness. Identical event interleavings across harnesses are not required.

## 10 Carry classification on stored records

Use Decision::Stored { class: RecordClass }. Storage classification and observer deduplication answer different questions: the observer cursor determines whether the reader has processed the record; the persisted class determines how it may affect live state. Archived evidence must not update live clocks merely because it is new to a reader.

# Standing decision retained

CorpusCadence belongs in Core, as established before this decision sequence. Update the crate ownership and dependency descriptions accordingly. This record does not select an estimator or resolve any remaining cadence-policy details.

# Reconciliation work still required

The current repository snapshot and Claude Doc revision 36 were not available for independent inspection. The following work is based on the supplied audit and must be checked against the actual files before edits are declared complete.

- Apply the approved choices consistently across docs/tech-spec.md, relevant crate interfaces, implementations, profiles, examples, and tests. Reconcile the Song specification where protocol contracts change.

- Correct obsolete Fence verse references to Grant followed by Activation, and replace runo-core::profile with runo-profile.

- Correct the scenario count, duplicate structural-check paragraph, install-versus-missing-ack step reference, and crate counts. Distinguish runtime libraries from dev-only support crates.

- Document diagnose, watch, and doctor in the CLI table, marking each command as implemented or planned rather than assuming implementation from documentation.

- Update dependency declarations and their rationale for reqwest, CorpusCadence, and testkit dependencies. Remove undecided simulator dependency claims until the runtime-stage selection is made.

- Resolve the missing second replay report reference using the real source, or explicitly mark the supporting report unavailable. Do not treat its results as independently verified.

# Details not yet decided

The approved architecture does not settle every Rust signature or wire field. In particular, verify the precise IngressView and ScribeRead methods, the installation adapter’s closure authorization, acknowledgement field types and serialization, and the RecordClass variants and persistence envelope. Bring any material alternative back for a decision rather than silently choosing one.

# Validation targets

During implementation, check that the explicit acknowledgement binds resource, grant, fence, and durability; archived Stored records cannot affect live clocks; installation and closure agree across both interfaces; millisecond arithmetic rounds upward and rejects overflow; and protocol scenarios remain reusable when runtime simulation is added.

# Status

Decisions approved in conversation on 3 November 2026. No repository edits, test runs, model results, or API capability verification are asserted by this record. It supplements the specifications and is intended to guide their next reconciliation.
