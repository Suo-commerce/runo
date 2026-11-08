# Decisions, 4 November 2026

Taken by Jarmo while milestone 5 (fencing) was being built. Each changes the v3 wire format;
nothing outside this repository speaks v3 yet, so the protocol version stays 3.

## A `Closing` verse authorizes the closed fences

**Problem.** Technical spec §8 installed `Fence::Closed` at step 3 of closure and sang `Close` at
step 5, yet said the `Close` verse authorizes the closed fence. That verse names the closure
snapshot, which is captured only after every closed fence is acknowledged, so it cannot exist at
step 3.

**Decision.** A new allocator record, `Closing { task }`, is journaled and sung at step 1. It is
the authority every resource checks before installing `Fence::Closed`, and it tells every voice
the task is shutting down. `Close { task, snapshot }` still ends closure and lands as a control
record.

**Rejected.** `Close` first with the snapshot in a later verse (`Close` would no longer mean
sealed); a signed request kept in the allocator's journal (closing would be invisible in the song
until the end); building relaunch only and deferring the decision.

## `Activation` carries `FenceAck`

**Problem.** `Activation.acks` used `ResourceAck { resource, epoch, durable: bool }`, while every
resource returns `FenceAck { resource, grant, fence, durability }`.

**Decision.** The wire carries the `FenceAck` each resource gave, unchanged. Ingress accepts an
activation only if every resource its grant lists has an ack naming that grant, the grant's epoch
fence, and `Host` durability or better.
