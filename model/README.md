# TLA+ model

`Runo.tla` is milestone 1. Run with TLC:

    java -jar tla2tools.jar -config Runo.cfg Runo.tla

Every action name in the model is also a `runo-sim` event name, so a counterexample trace from TLC
is replayable as a scenario under `crates/runo-sim/scenarios/`.
