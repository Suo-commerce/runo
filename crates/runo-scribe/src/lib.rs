//! Closure snapshots, tomes, Merkle roots and addenda (technical spec §9). Not a voice: holds no
//! key, sings nothing, and its output is reproducible by anyone with the store.
//!
//! [`seal`] reads only what the `Close` verse fixes: the snapshot it names and the verses that
//! snapshot reaches by `prev`. Live heads are never consulted, so a late verse cannot change a
//! tome; it becomes an addendum. [`verify`] recomputes everything and names the first difference.

pub mod merkle;

use std::collections::{BTreeMap, BTreeSet};

use runo_core::codec::{canonical, decode};
use runo_core::hash::{digest, display};
use runo_core::time::wall;
use runo_core::{
    ClosureSnapshot, Epoch, Hash, Millis, NetTime, Payload, SingerId, Summary, Timing,
    TomeManifest, Verse, VerseId,
};
use runo_store::{Record, ScribeRead, StoreError};

/// A sealed tome.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tome {
    /// The `Close` verse.
    pub close: VerseId,
    /// Hash of the manifest blob.
    pub manifest: Hash,
    /// Merkle root over the included verses.
    pub root: Hash,
}

/// A tome with its manifest bytes, for the runner to store as a blob.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sealed {
    /// The tome.
    pub tome: Tome,
    /// CBOR of the [`TomeManifest`]; `tome.manifest` is its hash.
    pub manifest: Vec<u8>,
    /// The included verses, in canonical order.
    pub included: Vec<VerseId>,
}

/// Sealing or verification failure.
#[derive(Debug, thiserror::Error)]
pub enum ScribeError {
    /// The closure snapshot could not be loaded.
    #[error("snapshot missing")]
    SnapshotMissing,
    /// A chain in the snapshot is incomplete in the store.
    #[error("chain {0} incomplete")]
    ChainIncomplete(String),
    /// Verification found a mismatch; names the first differing chain or fork.
    #[error("mismatch at {0}")]
    Mismatch(String),
    /// The store failed or holds something unreadable.
    #[error("store: {0}")]
    Store(String),
}

impl From<StoreError> for ScribeError {
    fn from(e: StoreError) -> Self {
        Self::Store(e.to_string())
    }
}

/// Seal the tome for `close` (steps 1–6 of §9). Deterministic: the same `Close` and the same
/// store contents give a byte-identical manifest and root.
///
/// # Errors
/// The snapshot or a verse it reaches is missing, or the store fails.
pub async fn seal(store: &dyn ScribeRead, close: &Verse) -> Result<Sealed, ScribeError> {
    // 1. The snapshot fixes the input.
    let bytes = store
        .snapshot(&close.seal.sum)
        .await?
        .ok_or(ScribeError::SnapshotMissing)?;
    let snapshot: ClosureSnapshot =
        decode(&bytes).map_err(|e| ScribeError::Store(format!("snapshot: {e}")))?;
    let records = store.task_records(&snapshot.task).await?;
    let by_id: BTreeMap<VerseId, &Verse> = records
        .iter()
        .map(|r| (r.verse.seal.sum, &r.verse))
        .collect();

    // 2. Walk each chain from its recorded end back to seq 0, and each fork branch by its own
    // hashes back to where it meets what is already included.
    let mut included: BTreeMap<VerseId, &Verse> = BTreeMap::new();
    for end in &snapshot.chains {
        if let Some(h) = end.last_hash {
            walk(
                &by_id,
                h,
                &mut included,
                &format!("{} epoch {}", end.singer.0, end.epoch.0),
            )?;
        }
    }
    for (key, ids) in &snapshot.forks {
        for id in ids {
            let name = format!("fork {} {} epoch {}", key.singer.0, key.task.0, key.epoch.0);
            walk(&by_id, *id, &mut included, &name)?;
        }
    }

    // 3. Canonical order.
    let mut ordered: Vec<&Verse> = included.into_values().collect();
    ordered.sort_by(|a, b| {
        let k = |v: &Verse| {
            (
                v.header.singer.clone(),
                v.header.epoch,
                v.header.seq,
                v.seal.sum,
            )
        };
        k(a).cmp(&k(b))
    });

    // 4. Merkle root.
    let sums: Vec<Hash> = ordered.iter().map(|v| v.seal.sum.0).collect();
    let root = merkle::root(&sums).unwrap_or(Hash([0; 32]));

    // 5. The summary.
    let summary = summarize(&ordered, close);

    // 6. The manifest.
    let manifest = TomeManifest {
        close: close.seal.sum,
        snapshot,
        summary,
    };
    let manifest = canonical(&manifest).map_err(|e| ScribeError::Store(e.to_string()))?;
    Ok(Sealed {
        tome: Tome {
            close: close.seal.sum,
            manifest: digest(&manifest),
            root,
        },
        manifest,
        included: ordered.iter().map(|v| v.seal.sum).collect(),
    })
}

/// Recompute the tome and compare. A mismatch names the first differing chain or fork, or what
/// else differs.
///
/// # Errors
/// [`ScribeError::Mismatch`], or what [`seal`] can return.
pub async fn verify(store: &dyn ScribeRead, tome: &Tome) -> Result<(), ScribeError> {
    let close = find_close(store, tome).await?;
    let again = seal(store, &close).await?;
    if again.tome == *tome {
        return Ok(());
    }
    let stored: Option<TomeManifest> = store
        .blob(&tome.manifest)
        .await?
        .and_then(|b| decode(&b).ok());
    let fresh: TomeManifest =
        decode(&again.manifest).map_err(|e| ScribeError::Store(e.to_string()))?;
    let Some(stored) = stored else {
        return Err(ScribeError::Mismatch(
            "the manifest blob is missing or unreadable".into(),
        ));
    };
    Err(ScribeError::Mismatch(first_difference(
        &stored,
        &fresh,
        tome,
        &again.tome,
    )))
}

/// The `Close` verse a tome names: its manifest says which task, and the task's records hold it.
async fn find_close(store: &dyn ScribeRead, tome: &Tome) -> Result<Verse, ScribeError> {
    let manifest: TomeManifest = store
        .blob(&tome.manifest)
        .await?
        .and_then(|b| decode(&b).ok())
        .ok_or_else(|| {
            ScribeError::Mismatch("the manifest blob is missing or unreadable".into())
        })?;
    store
        .task_records(&manifest.snapshot.task)
        .await?
        .into_iter()
        .map(|r: Record| r.verse)
        .find(|v| v.seal.sum == tome.close)
        .ok_or_else(|| ScribeError::Mismatch("the Close verse is not in the store".into()))
}

fn first_difference(stored: &TomeManifest, fresh: &TomeManifest, a: &Tome, b: &Tome) -> String {
    if stored.snapshot != fresh.snapshot {
        for (x, y) in stored.snapshot.chains.iter().zip(&fresh.snapshot.chains) {
            if x != y {
                return format!("chain {} epoch {}", x.singer.0, x.epoch.0);
            }
        }
        for (x, y) in stored.snapshot.forks.iter().zip(&fresh.snapshot.forks) {
            if x != y {
                return format!("fork {} epoch {} ", x.0.singer.0, x.0.epoch.0);
            }
        }
        return "the snapshot".into();
    }
    if stored.summary != fresh.summary {
        return "the summary".into();
    }
    if a.root != b.root {
        return format!("the root ({} vs {})", display(&a.root), display(&b.root));
    }
    "the manifest bytes".into()
}

/// Include `from` and everything before it on its chain, stopping at what is already included.
fn walk<'a>(
    by_id: &BTreeMap<VerseId, &'a Verse>,
    from: VerseId,
    included: &mut BTreeMap<VerseId, &'a Verse>,
    name: &str,
) -> Result<(), ScribeError> {
    let mut next = Some(from);
    while let Some(id) = next {
        if included.contains_key(&id) {
            break;
        }
        let v = by_id.get(&id).ok_or_else(|| {
            ScribeError::ChainIncomplete(format!("{name}: {} not in the store", display(&id.0)))
        })?;
        included.insert(id, v);
        if v.header.seq == 0 {
            break;
        }
        next = v.header.prev;
        if next.is_none() {
            return Err(ScribeError::ChainIncomplete(format!(
                "{name}: seq {} has no prev",
                v.header.seq
            )));
        }
    }
    Ok(())
}

fn summarize(ordered: &[&Verse], close: &Verse) -> Summary {
    let top: Option<Epoch> = ordered.iter().map(|v| v.header.epoch).max();
    let final_progress = ordered
        .iter()
        .filter(|v| Some(v.header.epoch) == top && v.header.singer != close.header.singer)
        .max_by_key(|v| (v.header.seq, v.seal.sum))
        .map(|v| v.header.progress.clone())
        .unwrap_or_default();
    let mut by_time: Vec<&&Verse> = ordered.iter().collect();
    by_time.sort_by(|a, b| {
        (a.header.sung_at.0.as_str(), a.seal.sum).cmp(&(b.header.sung_at.0.as_str(), b.seal.sum))
    });
    let outcome = by_time.iter().rev().find_map(|v| match &v.payload {
        Payload::Boundary { event, .. } => Some(event.clone()),
        _ => None,
    });
    let first_claimed = by_time
        .iter()
        .find(|v| matches!(v.payload, Payload::Claimed { .. }))
        .map(|v| v.header.sung_at.clone());
    let last_sung = by_time.last().map(|v| v.header.sung_at.clone());
    let waits = waits(ordered);
    let net = net_time(
        first_claimed.as_ref(),
        last_sung.as_ref(),
        waits,
        &close.header.sung_at,
    );
    Summary {
        verse_count: ordered.len() as u64,
        final_progress,
        outcome,
        timing: Timing {
            first_claimed,
            last_sung,
            waits,
            net,
        },
    }
}

/// Reported waits: per chain, from a boundary naming `blocked_on` to the next boundary without.
fn waits(ordered: &[&Verse]) -> Millis {
    let mut chains: BTreeMap<(SingerId, Epoch), Vec<&Verse>> = BTreeMap::new();
    for v in ordered {
        chains
            .entry((v.header.singer.clone(), v.header.epoch))
            .or_default()
            .push(v);
    }
    let mut total: u64 = 0;
    for verses in chains.values_mut() {
        verses.sort_by_key(|v| v.header.seq);
        let mut since: Option<u64> = None;
        for v in verses.iter() {
            let Payload::Boundary { blocked_on, .. } = &v.payload else {
                continue;
            };
            let at = wall::parse(&v.header.sung_at.0).map(|i| i.0 / 1_000_000);
            match (blocked_on, since, at) {
                (Some(_), None, Some(t)) => since = Some(t),
                (None, Some(s), Some(t)) => {
                    total = total.saturating_add(t.saturating_sub(s));
                    since = None;
                }
                _ => {}
            }
        }
    }
    Millis(total)
}

fn net_time(
    first: Option<&runo_core::Timestamp>,
    last: Option<&runo_core::Timestamp>,
    waits: Millis,
    close: &runo_core::Timestamp,
) -> NetTime {
    let ms = |t: Option<&runo_core::Timestamp>| {
        t.and_then(|t| wall::parse(&t.0)).map(|i| i.0 / 1_000_000)
    };
    let (Some(first), Some(last)) = (ms(first), ms(last)) else {
        return NetTime::Invalid("no claim, or a time that is not RFC 3339".into());
    };
    if let Some(close) = ms(Some(close))
        && last > close
    {
        return NetTime::Invalid("a verse claims to be sung after the task closed".into());
    }
    match last
        .checked_sub(first)
        .and_then(|span| span.checked_sub(waits.0))
    {
        Some(net) => NetTime::Valid(Millis(net)),
        None => NetTime::Invalid("waits or times out of order: the span is negative".into()),
    }
}

/// The verses a tome includes that are not in `seen`: a late verse becomes an addendum.
#[must_use]
pub fn late(included: &[VerseId], task_records: &[Record]) -> Vec<VerseId> {
    let set: BTreeSet<&VerseId> = included.iter().collect();
    task_records
        .iter()
        .filter(|r| {
            r.class == runo_core::ingress::RecordClass::History && !set.contains(&r.verse.seal.sum)
        })
        .map(|r| r.verse.seal.sum)
        .collect()
}
