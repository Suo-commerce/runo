//! Escalation thresholds and the wait-one-cadence rule (Song spec §5, listening step 4).
//! A crossing becomes a call only after the lane's own next event fails to arrive within one
//! cadence, except for kinds that never self-clear.
