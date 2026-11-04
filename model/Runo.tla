---------------------------- MODULE Runo ----------------------------
(* The Dwarven Song, Core: claim, grant, activation, relaunch, close, seal.

   Instance: three voices, one allocator, two tasks, epochs 0..MaxEpoch, a store with two
   replicas. Checks S1-S4 as invariants and L1 as liveness under weak fairness of the allocator
   and the scribe. L3 is NOT modelled (consumer behaviour). Every action name is also a runo-sim
   event name, so a TLC counterexample is replayable as a scenario.

   Technical specification sections 4, 7, 8, 9 and 12. Milestone 1.

   What is modelled and what is abstracted away:
   - A verse is a record [voice, task, epoch, seq, cause]; its identity is the record itself.
     Signatures are not modelled: S2 reads "every current verse on a fenced chain names an
     activated grant", which is the part of S2 the protocol (not the crypto) must guarantee.
   - Every write is admitted per replica. A replica admits a write as Current only if the
     write's epoch is at or above its installed fence, otherwise it archives it as History
     (the Record class of tech spec section 7). A write is tagged, at admission, with the set of
     epochs already activated for its chain: that is what lets S1 see "an old write landed after
     the replacement was already active" without a global clock.
   - A relaunch is of the same singer at the next epoch (tech spec section 8); the holder of a
     task never changes within the model.
   - The allocator's journal (reservation, pending grant, closure snapshot hash) is the
     variables reserved, pending and closure; crashes of the allocator are the interleavings
     between its steps, which TLC explores anyway.
   - Fences are persisted: ReplicaRestart leaves them alone. That is the whole content of the
     fence-restart scenario.
   - L1's "suspect" is sung by any other voice once the holder is silent. Silence is a
     nondeterministic event (Crash), not a timer. *)

EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS Voices, Tasks, MaxEpoch, MaxSeq, Replicas, Allocator, None,
          RequireAllAcks   \* TRUE is the protocol; FALSE breaks it to show S1 is not vacuous

ASSUME Allocator \in Voices
ASSUME MaxEpoch >= 1 /\ MaxSeq >= 1
ASSUME None \notin Voices /\ None \notin Tasks /\ None \notin Nat   \* a model value

Closed == MaxEpoch + 1              \* above every epoch: nothing writes after it
Epochs == 0..MaxEpoch
Fences == 0..Closed

VARIABLES
  holder,      \* [task] -> voice that claimed it, or None
  active,      \* [task] -> the epoch whose incarnation currently works, or None before any claim
  silent,      \* [task] -> TRUE once the active incarnation stopped singing (Crash)
  seqs,        \* [task][epoch] -> last seq sung on that chain
  verses,      \* set of verse records ever sung (the song, as the singer sees it)
  current,     \* [replica] -> set of [verse, actThen] admitted as Current at that replica
  archive,     \* [replica] -> set of verses admitted as History at that replica
  fences,      \* [replica][task] -> epoch fence for the holder's chain, or Closed
  reserved,    \* [task] -> highest epoch the allocator has reserved (journal)
  pending,     \* [task] -> a reserved epoch not yet granted, or None (journal)
  grants,      \* set of <<task, epoch>> for which a Grant verse was sung
  activations, \* set of <<task, epoch>> for which an Activation verse was sung
  taskState,   \* [task] -> "open" | "closing" | "closed"
  closure,     \* [task] -> the closure snapshot (set of verses) once captured, or None (journal)
  tomes,       \* [task] -> the sealed set of verses, or None
  suspects,    \* set of <<voice, task>>: voice sang Suspect about the task's holder
  refrains,    \* [task] -> refrain counter n (bounded)
  restarts     \* [replica] -> restarts taken (bounded; fences must survive them)

vars == <<holder, active, silent, seqs, verses, current, archive, fences, reserved, pending,
          grants, activations, taskState, closure, tomes, suspects, refrains, restarts>>

Verse(v, t, e, s, c) == [voice |-> v, task |-> t, epoch |-> e, seq |-> s, cause |-> c]

ActivatedEpochs(t) == {e \in Epochs : <<t, e>> \in activations}

(* The holder's chain of task t at epoch e is fenced once any grant exists for the task:
   epoch 0 is the original, unfenced claim. *)
Fenced(t, e) == e >= 1

(* The store's view: a verse is "in the song" at replica r if admitted as Current there. *)
CurrentVerses(r) == {w.verse : w \in current[r]}
TaskVerses(S, t) == {w \in S : w.task = t}

(* Admit a verse at every replica (the store service is one writer, tech spec section 7):
   Current where its epoch is at or above the replica's fence, History otherwise. The tag
   actThen records which epochs were already activated for the chain at admission. *)
Admit(w) ==
  /\ current' = [r \in Replicas |->
                  IF w.epoch >= fences[r][w.task]
                  THEN current[r] \cup {[verse |-> w, actThen |-> ActivatedEpochs(w.task)]}
                  ELSE current[r]]
  /\ archive' = [r \in Replicas |->
                  IF w.epoch >= fences[r][w.task] THEN archive[r] ELSE archive[r] \cup {w}]

(* A cause is any verse of the same task already in the song, or none. Ingress rejects a
   dangling cause, so the model never produces one; S4 states the resulting invariant. *)
Causes(t) == TaskVerses(verses, t) \cup {None}

----------------------------------------------------------------------------------------------
(* Actions *)

Init ==
  /\ holder = [t \in Tasks |-> None]
  /\ active = [t \in Tasks |-> None]
  /\ silent = [t \in Tasks |-> FALSE]
  /\ seqs = [t \in Tasks |-> [e \in Epochs |-> 0]]
  /\ verses = {}
  /\ current = [r \in Replicas |-> {}]
  /\ archive = [r \in Replicas |-> {}]
  /\ fences = [r \in Replicas |-> [t \in Tasks |-> 0]]
  /\ reserved = [t \in Tasks |-> 0]
  /\ pending = [t \in Tasks |-> None]
  /\ grants = {}
  /\ activations = {}
  /\ taskState = [t \in Tasks |-> "open"]
  /\ closure = [t \in Tasks |-> None]
  /\ tomes = [t \in Tasks |-> None]
  /\ suspects = {}
  /\ refrains = [t \in Tasks |-> 0]
  /\ restarts = [r \in Replicas |-> 0]

(* Claim: a voice other than the allocator claims an unclaimed task at epoch 0. *)
Claim(v, t) ==
  /\ v # Allocator
  /\ holder[t] = None
  /\ taskState[t] = "open"
  /\ LET w == Verse(v, t, 0, 1, None) IN
     /\ holder' = [holder EXCEPT ![t] = v]
     /\ active' = [active EXCEPT ![t] = 0]
     /\ seqs' = [seqs EXCEPT ![t][0] = 1]
     /\ verses' = verses \cup {w}
     /\ Admit(w)
  /\ UNCHANGED <<silent, fences, reserved, pending, grants, activations, taskState, closure,
                 tomes, suspects, refrains, restarts>>

(* Sing: the active incarnation sings the next verse of its chain while the task is open. *)
Sing(t, c) ==
  /\ holder[t] # None
  /\ taskState[t] = "open"
  /\ ~silent[t]
  /\ c \in Causes(t)
  /\ LET e == active[t]
         s == seqs[t][e] + 1
         w == Verse(holder[t], t, e, s, c) IN
     /\ s <= MaxSeq
     /\ seqs' = [seqs EXCEPT ![t][e] = s]
     /\ verses' = verses \cup {w}
     /\ Admit(w)
  /\ UNCHANGED <<holder, active, silent, fences, reserved, pending, grants, activations,
                 taskState, closure, tomes, suspects, refrains, restarts>>

(* Refrain: the active incarnation says "still here"; reachability only, no clocks reset. *)
Refrain(t) ==
  /\ holder[t] # None
  /\ ~silent[t]
  /\ refrains[t] < 1
  /\ refrains' = [refrains EXCEPT ![t] = @ + 1]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, reserved,
                 pending, grants, activations, taskState, closure, tomes, suspects, restarts>>

(* Crash: the active incarnation stops singing. Not a protocol action, the thing the protocol
   exists for. *)
Crash(t) ==
  /\ holder[t] # None
  /\ ~silent[t]
  /\ silent' = [silent EXCEPT ![t] = TRUE]
  /\ UNCHANGED <<holder, active, seqs, verses, current, archive, fences, reserved, pending,
                 grants, activations, taskState, closure, tomes, suspects, refrains, restarts>>

(* Resume: the incarnation was only paused, not dead, and sings again at its own epoch. Peers
   may already have suspected it and the allocator may already be relaunching: the fences,
   not the voice's own belief, decide what its writes become (paused-resumes). *)
Resume(t) ==
  /\ holder[t] # None
  /\ silent[t]
  /\ silent' = [silent EXCEPT ![t] = FALSE]
  /\ UNCHANGED <<holder, active, seqs, verses, current, archive, fences, reserved, pending,
                 grants, activations, taskState, closure, tomes, suspects, refrains, restarts>>

(* Suspect: another voice, seeing silence, sings Suspect about the holder. *)
Suspect(v, t) ==
  /\ holder[t] # None
  /\ v # holder[t]
  /\ silent[t]
  /\ <<v, t>> \notin suspects
  /\ suspects' = suspects \cup {<<v, t>>}
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, reserved,
                 pending, grants, activations, taskState, closure, tomes, refrains, restarts>>

(* Reserve: the allocator, on a suspicion, reserves the next epoch in its journal before
   anything else (section 8, steps 1-2). Relaunch is serialized per (singer, task): nothing is
   reserved while a reservation is pending or a grant is unactivated; a second request while one
   is in flight gets the pending grant, not a new epoch. An epoch, once reserved, is never
   reused. *)
Reserve(t) ==
  /\ taskState[t] = "open"
  /\ \E v \in Voices : <<v, t>> \in suspects
  /\ pending[t] = None
  /\ \A e \in Epochs : <<t, e>> \in grants => <<t, e>> \in activations
  /\ reserved[t] < MaxEpoch
  /\ reserved' = [reserved EXCEPT ![t] = @ + 1]
  /\ pending' = [pending EXCEPT ![t] = reserved[t] + 1]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, grants,
                 activations, taskState, closure, tomes, suspects, refrains, restarts>>

(* Grant: the allocator sings the Grant for the pending reservation (step 3). *)
Grant(t) ==
  /\ taskState[t] = "open"
  /\ pending[t] # None
  /\ grants' = grants \cup {<<t, pending[t]>>}
  /\ pending' = [pending EXCEPT ![t] = None]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, reserved,
                 activations, taskState, closure, tomes, suspects, refrains, restarts>>

(* Install(replica): one resource persists the fence for a granted, unactivated epoch (step 4).
   Never lowers. Each replica is installed on independently: this is where partitions live. *)
Install(r, t) ==
  /\ \E e \in Epochs :
       /\ <<t, e>> \in grants
       /\ <<t, e>> \notin activations
       /\ fences[r][t] < e
       /\ fences' = [fences EXCEPT ![r][t] = e]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, reserved, pending,
                 grants, activations, taskState, closure, tomes, suspects, refrains, restarts>>

(* Activate: with every replica's ack in hand the allocator sings Activation and starts the
   replacement, which is alive and takes over the chain at the new epoch (steps 6-7). *)
Activate(t) ==
  /\ taskState[t] = "open"
  /\ \E e \in Epochs :
       /\ <<t, e>> \in grants
       /\ <<t, e>> \notin activations
       /\ IF RequireAllAcks THEN \A r \in Replicas : fences[r][t] >= e
                           ELSE \E r \in Replicas : fences[r][t] >= e
       /\ activations' = activations \cup {<<t, e>>}
       /\ active' = [active EXCEPT ![t] = e]
       /\ silent' = [silent EXCEPT ![t] = FALSE]
       /\ suspects' = {s \in suspects : s[2] # t}
  /\ UNCHANGED <<holder, seqs, verses, current, archive, fences, reserved, pending, grants,
                 taskState, closure, tomes, refrains, restarts>>

(* OldWrite: an earlier incarnation, paused rather than dead, writes at its own epoch after a
   later one exists (step 8; paused-resumes). The store files it by the fence at each replica. *)
OldWrite(t, c) ==
  /\ holder[t] # None
  /\ c \in Causes(t)
  /\ \E e \in Epochs :
       /\ e < active[t]
       /\ LET s == seqs[t][e] + 1
              w == Verse(holder[t], t, e, s, c) IN
          /\ s <= MaxSeq
          /\ seqs' = [seqs EXCEPT ![t][e] = s]
          /\ verses' = verses \cup {w}
          /\ Admit(w)
  /\ UNCHANGED <<holder, active, silent, fences, reserved, pending, grants, activations,
                 taskState, closure, tomes, suspects, refrains, restarts>>

(* ReplicaRestart: a replica restarts. Fences and admitted records are persisted, so nothing
   changes but the count; the model would fail S1 if fences were volatile. *)
ReplicaRestart(r) ==
  /\ restarts[r] < 1
  /\ restarts' = [restarts EXCEPT ![r] = @ + 1]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, reserved,
                 pending, grants, activations, taskState, closure, tomes, suspects, refrains>>

(* Close, as the steps of section 8 so that the allocator can crash between any two of them.
   CloseBegin: the task goes Closing; no reservation or activation is accepted from here on
   (Reserve, Grant and Activate all require "open"). A pending reservation is settled as
   abandoned: its epoch stays reserved and is never reused. *)
CloseBegin(t) ==
  /\ holder[t] # None
  /\ taskState[t] = "open"
  /\ taskState' = [taskState EXCEPT ![t] = "closing"]
  /\ pending' = [pending EXCEPT ![t] = None]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, reserved,
                 grants, activations, closure, tomes, suspects, refrains, restarts>>

(* CloseInstall(replica): Fence::Closed is installed at a resource, through the same install
   path as an epoch fence. *)
CloseInstall(r, t) ==
  /\ taskState[t] = "closing"
  /\ fences[r][t] < Closed
  /\ fences' = [fences EXCEPT ![r][t] = Closed]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, reserved, pending,
                 grants, activations, taskState, closure, tomes, suspects, refrains, restarts>>

(* CaptureSnapshot: with every Closed ack in, the closure snapshot is captured once and its
   hash persisted in the journal before anything else. The snapshot is the store's view: what
   is Current at the replicas (set union, tech spec section 7). *)
CaptureSnapshot(t) ==
  /\ taskState[t] = "closing"
  /\ closure[t] = None
  /\ \A r \in Replicas : fences[r][t] = Closed
  /\ closure' = [closure EXCEPT ![t] = UNION {TaskVerses(CurrentVerses(r), t) : r \in Replicas}]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, reserved,
                 pending, grants, activations, taskState, tomes, suspects, refrains, restarts>>

(* Close: the Close verse is published naming the persisted snapshot, and the task is Closed.
   A crash before this step resumes here with the same snapshot (close-crash). *)
Close(t) ==
  /\ taskState[t] = "closing"
  /\ closure[t] # None
  /\ taskState' = [taskState EXCEPT ![t] = "closed"]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, reserved,
                 pending, grants, activations, closure, tomes, suspects, refrains, restarts>>

(* Seal: the scribe seals the tome from the snapshot the Close verse names, never from live
   heads. *)
Seal(t) ==
  /\ taskState[t] = "closed"
  /\ tomes[t] = None
  /\ tomes' = [tomes EXCEPT ![t] = closure[t]]
  /\ UNCHANGED <<holder, active, silent, seqs, verses, current, archive, fences, reserved,
                 pending, grants, activations, taskState, closure, suspects, refrains, restarts>>

(* LateVerse: a signed verse from any incarnation arrives after closure began. Every replica
   holds Closed, so it is archived unchanged everywhere; the tome, if sealed, does not change
   (late-verse, after-close, close-crash). *)
LateVerse(t, c) ==
  /\ holder[t] # None
  /\ taskState[t] # "open"
  /\ c \in Causes(t)
  /\ \E e \in Epochs :
       /\ e <= active[t]
       /\ LET s == seqs[t][e] + 1
              w == Verse(holder[t], t, e, s, c) IN
          /\ s <= MaxSeq
          /\ seqs' = [seqs EXCEPT ![t][e] = s]
          /\ verses' = verses \cup {w}
          /\ Admit(w)
  /\ UNCHANGED <<holder, active, silent, fences, reserved, pending, grants, activations,
                 taskState, closure, tomes, suspects, refrains, restarts>>

Next ==
  \/ \E v \in Voices, t \in Tasks : Claim(v, t) \/ Suspect(v, t)
  \/ \E t \in Tasks : \E c \in Causes(t) : Sing(t, c) \/ OldWrite(t, c) \/ LateVerse(t, c)
  \/ \E t \in Tasks : Refrain(t) \/ Crash(t) \/ Resume(t) \/ Reserve(t) \/ Grant(t) \/ Activate(t)
                      \/ CloseBegin(t) \/ CaptureSnapshot(t) \/ Close(t) \/ Seal(t)
  \/ \E r \in Replicas, t \in Tasks : Install(r, t) \/ CloseInstall(r, t)
  \/ \E r \in Replicas : ReplicaRestart(r)

(* Fairness: the allocator finishes what it starts and the scribe seals what is closed. The
   fence allocator's decision to relaunch is not fair: it may never relaunch, and L1 must hold
   anyway because a suspect was sung. *)
AllocatorFair ==
  /\ \A t \in Tasks : WF_vars(Grant(t)) /\ WF_vars(Activate(t))
                      /\ WF_vars(CloseBegin(t)) /\ WF_vars(CaptureSnapshot(t)) /\ WF_vars(Close(t))
  /\ \A r \in Replicas, t \in Tasks : WF_vars(Install(r, t)) /\ WF_vars(CloseInstall(r, t))
ScribeFair == \A t \in Tasks : WF_vars(Seal(t))
PeersFair == \A v \in Voices, t \in Tasks : WF_vars(Suspect(v, t))

Spec == Init /\ [][Next]_vars /\ AllocatorFair /\ ScribeFair /\ PeersFair

----------------------------------------------------------------------------------------------
(* Properties *)

TypeOK ==
  /\ \A t \in Tasks : taskState[t] \in {"open", "closing", "closed"}
  /\ \A r \in Replicas, t \in Tasks : fences[r][t] \in Fences
  /\ \A t \in Tasks : reserved[t] \in Epochs
  /\ grants \subseteq (Tasks \X Epochs)
  /\ activations \subseteq grants

(* S1: no write from a stale epoch is admitted as Current at any replica. Stale means: a later
   epoch of the same chain was already activated when the write was admitted. *)
S1 ==
  \A r \in Replicas : \A w \in current[r] :
    \A e \in w.actThen : e <= w.verse.epoch

(* S2: every Current verse on a fenced chain names an activated grant for its epoch
   (the model's half of "no unsigned or invalid verse is appended"). *)
S2 ==
  \A r \in Replicas : \A w \in current[r] :
    Fenced(w.verse.task, w.verse.epoch) => <<w.verse.task, w.verse.epoch>> \in activations

(* S3: every tome is exactly the closure snapshot the Close verse named, that snapshot is a
   set of verses that were sung, and a verse the store filed as History everywhere (a late
   verse, after Closed was installed) is never in it. A verse that is Current at one replica
   and History at another (admitted during a partial install) is work, and may be. *)
S3 ==
  \A t \in Tasks :
    /\ tomes[t] # None => tomes[t] = closure[t]
    /\ closure[t] # None =>
         /\ closure[t] \subseteq verses
         /\ \A w \in verses :
              (w.task = t /\ \A r \in Replicas : w \in archive[r]) => w \notin closure[t]

(* S4: every cause points to a verse that exists. *)
S4 == \A w \in verses : w.cause = None \/ w.cause \in verses

(* Closure never recomputes: once captured, the snapshot is fixed, and the fence is never
   lowered at any replica. Both are action properties over every step. *)
SnapshotFixed ==
  [][\A t \in Tasks : closure[t] # None => closure'[t] = closure[t]]_vars
FenceMonotone ==
  [][\A r \in Replicas, t \in Tasks : fences'[r][t] >= fences[r][t]]_vars

(* L1: every claim ends in a tome or a suspect. *)
L1 == \A t \in Tasks :
  (holder[t] # None) ~> (tomes[t] # None \/ \E v \in Voices : <<v, t>> \in suspects)

=====================================================================
