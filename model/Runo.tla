---------------------------- MODULE Runo ----------------------------
(* The Dwarven Song, Core: claim, grant, activation, relaunch, close, seal.
   Instance: three voices, one allocator, two tasks, epochs 0..2, a store with two replicas.
   Checks S1-S4 as invariants and L1 as liveness under weak fairness of the allocator and the
   scribe. L3 is NOT modelled (consumer behaviour). Every action name is also a runo-sim event.
   Technical specification section 12. Milestone 1. *)

EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS Voices, Tasks, MaxEpoch, Replicas

VARIABLES
  chains,      \* [voice, task, epoch] -> sequence of verse ids
  fences,      \* [replica, voice, task] -> epoch or "closed"
  grants,      \* set of <<voice, task, epoch>> reserved by the allocator
  activations, \* set of grants whose every replica acknowledged
  taskState,   \* [task] -> "open" | "closing" | "closed"
  tomes        \* [task] -> snapshot or none

vars == <<chains, fences, grants, activations, taskState, tomes>>

\* Actions (milestone 1 fills these in):
\*   Claim, Sing, Refrain, Suspect, Reserve, Grant, Install(replica), Activate,
\*   OldWrite, ReplicaRestart, Close, Seal, LateVerse

Init == TRUE
Next == TRUE
Spec == Init /\ [][Next]_vars

\* S1: no write from a stale epoch is admitted at any replica
S1 == TRUE
\* S2: no unsigned or invalid verse appended (modelled as: every chain entry has a valid grant)
S2 == TRUE
\* S3: every tome covers exactly the chains in its closure snapshot
S3 == TRUE
\* S4: every cause points to an existing verse
S4 == TRUE
\* L1: every claim ends in a tome or a suspect, under fairness
L1 == TRUE

=====================================================================
