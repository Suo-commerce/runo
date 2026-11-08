//! Canonical encoding over the real wire types (technical spec §3, §12 "property tests worth
//! naming"): a bijection on valid values, deterministic, strict on unknown fields, and the
//! verse's `sum` is a pure function of its fields.

use ed25519_dalek::SigningKey;
use proptest::prelude::*;
use runo_core::codec::{CodecError, canonical, decode, decode_canonical};
use runo_core::sign::{seal, verify};
use runo_core::*;

fn hash() -> impl Strategy<Value = Hash> {
    any::<[u8; 32]>().prop_map(Hash)
}
fn vid() -> impl Strategy<Value = VerseId> {
    hash().prop_map(VerseId)
}
fn name() -> impl Strategy<Value = String> {
    "[a-z0-9-]{1,12}"
}
fn reference() -> impl Strategy<Value = Ref> {
    (name(), ".{0,20}", hash()).prop_map(|(scheme, path, hash)| Ref { scheme, path, hash })
}
fn about() -> impl Strategy<Value = About> {
    (
        name(),
        proptest::option::of(name()),
        proptest::option::of(any::<u64>()),
        proptest::option::of(vid()),
    )
        .prop_map(|(s, t, e, h)| About {
            singer: SingerId(s),
            task: t.map(TaskId),
            epoch: e.map(Epoch),
            hash: h,
        })
}
fn clock() -> impl Strategy<Value = Clock> {
    prop_oneof![
        Just(Clock::Reachability),
        Just(Clock::Progress),
        Just(Clock::Task),
        Just(Clock::Waiting)
    ]
}
fn verdict() -> impl Strategy<Value = Verdict> {
    prop_oneof![
        Just(Verdict::Merge),
        Just(Verdict::Reject),
        Just(Verdict::Concur),
        Just(Verdict::Dissent),
        Just(Verdict::Accept),
        Just(Verdict::Retract),
        Just(Verdict::Permit),
        Just(Verdict::Deny)
    ]
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per payload kind, kept side by side"
)]
fn payload() -> impl Strategy<Value = Payload> {
    let promise =
        (name(), any::<[u64; 3]>(), any::<bool>(), name()).prop_map(|(u, c, k, s)| Promise {
            unit: u,
            cadence: Cadence {
                call: Millis(c[0]),
                again: Millis(c[1]),
                suspect: Millis(c[2]),
            },
            checkpoints: k,
            source: s,
        });
    let spec = (".{0,30}", proptest::collection::vec(name(), 0..3), name()).prop_map(
        |(deliver, scope, kind)| TaskSpec {
            deliver,
            scope,
            kind,
        },
    );
    prop_oneof![
        (spec, promise).prop_map(|(task, promise)| Payload::Claimed { task, promise }),
        (
            name(),
            proptest::option::of(reference()),
            proptest::option::of(name())
        )
            .prop_map(|(event, state, blocked_on)| Payload::Boundary {
                event,
                state,
                blocked_on
            }),
        (
            name(),
            proptest::collection::vec(reference(), 0..3),
            proptest::option::of(hash())
        )
            .prop_map(|(step, changed, head)| Payload::Work {
                step,
                changed,
                head
            }),
        about().prop_map(|about| Payload::Ack { about }),
        (
            about(),
            prop_oneof![
                Just(VouchHow::Direct),
                name().prop_map(|v| VouchHow::Indirect { via: SingerId(v) }),
                Just(VouchHow::Refrain)
            ]
        )
            .prop_map(|(about, how)| Payload::Vouch { about, how }),
        (about(), clock(), any::<u64>()).prop_map(|(about, clock, o)| Payload::Suspect {
            about,
            clock,
            overdue_by: Millis(o)
        }),
        (about(), verdict(), proptest::option::of(".{0,20}")).prop_map(
            |(about, verdict, reason)| Payload::Ruling {
                about,
                verdict,
                reason
            }
        ),
        (
            name(),
            name(),
            any::<u64>(),
            proptest::collection::vec(name(), 0..3)
        )
            .prop_map(|(h, t, e, r)| Payload::Grant {
                holder: SingerId(h),
                task: TaskId(t),
                epoch: Epoch(e),
                resources: r.into_iter().map(ResourceId).collect(),
            }),
        (
            vid(),
            proptest::collection::vec(
                (name(), vid(), proptest::option::of(any::<u64>()), 0u8..4),
                0..3
            )
        )
            .prop_map(|(grant, acks)| Payload::Activation {
                grant,
                acks: acks
                    .into_iter()
                    .map(|(r, g, e, d)| fence::FenceAck {
                        resource: ResourceId(r),
                        grant: g,
                        fence: e.map_or(fence::Fence::Closed, |e| fence::Fence::Epoch(Epoch(e))),
                        durability: match d {
                            0 => fence::Durability::Process,
                            1 => fence::Durability::Host,
                            k => fence::Durability::Replicated(k),
                        },
                    })
                    .collect(),
            }),
        name().prop_map(|t| Payload::Closing { task: TaskId(t) }),
        (name(), reference()).prop_map(|(t, snapshot)| Payload::Close {
            task: TaskId(t),
            snapshot
        }),
        (vid(), reference(), hash()).prop_map(|(close, manifest, root)| Payload::Tome {
            close,
            manifest,
            root
        }),
        (vid(), vid()).prop_map(|(tome, verse)| Payload::Addendum { tome, verse }),
        (any::<u32>(), reference()).prop_map(|(v, roster)| Payload::Roster {
            version: RosterVersion(v),
            roster
        }),
    ]
}

fn header() -> impl Strategy<Value = Header> {
    (
        (
            name(),
            any::<u32>(),
            name(),
            any::<u64>(),
            proptest::option::of(vid()),
        ),
        (
            any::<u64>(),
            proptest::option::of(vid()),
            proptest::option::of(vid()),
        ),
        (
            any::<u64>(),
            proptest::option::of(hash()),
            ".{0,25}",
            proptest::option::of(".{0,30}"),
        ),
    )
        .prop_map(
            |((s, r, t, e, g), (seq, prev, cause), (h, head, at, note))| Header {
                v: PROTOCOL_VERSION,
                singer: SingerId(s),
                roster: RosterVersion(r),
                task: TaskId(t),
                epoch: Epoch(e),
                grant: g,
                seq,
                prev,
                cause,
                progress: Progress { height: h, head },
                sung_at: Timestamp(at),
                note,
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_sealed_verse_round_trips_canonically_and_still_verifies(
        h in header(), p in payload(), seed in any::<[u8; 32]>()
    ) {
        let key = SigningKey::from_bytes(&seed);
        let verse = seal(&key, h, p).unwrap();
        let bytes = canonical(&verse).unwrap();
        let back: Verse = decode_canonical(&bytes).unwrap();
        prop_assert_eq!(&back, &verse);
        prop_assert_eq!(canonical(&back).unwrap(), bytes);
        prop_assert!(verify(&back, &key.verifying_key().to_bytes()).is_ok());
    }

    #[test]
    fn changing_any_header_field_changes_the_sum(h in header(), p in payload(), seed in any::<[u8; 32]>()) {
        let key = SigningKey::from_bytes(&seed);
        let a = seal(&key, h.clone(), p.clone()).unwrap();
        let mut h2 = h;
        h2.seq = h2.seq.wrapping_add(1);
        let b = seal(&key, h2, p).unwrap();
        prop_assert_ne!(a.seal.sum, b.seal.sum);
    }
}

#[test]
fn an_unknown_field_in_a_payload_is_refused() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let h = Header {
        v: PROTOCOL_VERSION,
        singer: SingerId("lane-1".into()),
        roster: RosterVersion(1),
        task: TaskId("HOD-42".into()),
        epoch: Epoch(0),
        grant: None,
        seq: 0,
        prev: None,
        cause: None,
        progress: Progress::default(),
        sung_at: Timestamp("2026-11-03T12:00:00Z".into()),
        note: None,
    };
    let verse = seal(
        &key,
        h,
        Payload::Ack {
            about: About {
                singer: SingerId("x".into()),
                task: None,
                epoch: None,
                hash: None,
            },
        },
    )
    .unwrap();
    let mut v = ciborium::Value::serialized(&verse).unwrap();
    let ciborium::Value::Map(top) = &mut v else {
        panic!("verse is a map")
    };
    let payload = top
        .iter_mut()
        .find(|(k, _)| k.as_text() == Some("payload"))
        .map(|(_, v)| v)
        .unwrap();
    let ciborium::Value::Map(fields) = payload else {
        panic!("payload is a map")
    };
    fields.push(("offer".into(), true.into()));
    let bytes = canonical(&v).unwrap();
    assert!(matches!(
        decode::<Verse>(&bytes),
        Err(CodecError::Decode(_))
    ));
}
