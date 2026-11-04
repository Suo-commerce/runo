//! The human-facing channel: one message per episode and level, never one per signal (technical
//! spec §2, "Episodes"). The caller words the message; a channel only delivers it, idempotently by
//! [`Notice::key`], so a notice resent after a crash reaches the person once.
//!
//! [`Terminal`] is the first channel: it prints. Comments on the lane (behind `--apply`) and push
//! notifications are further implementations of [`HumanChannel`].

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::CallError;
use crate::episodes::Notice;

/// Somewhere a person reads.
#[async_trait]
pub trait HumanChannel: Send + Sync {
    /// Deliver `text` for `notice`. Delivering the same key twice must reach the person once.
    ///
    /// # Errors
    /// The channel could not deliver; the notice stays owed and is retried.
    async fn deliver(&self, notice: &Notice, text: &str) -> Result<(), CallError>;
}

/// Prints each message, once per key, to a writer (standard output for `runo watch`).
pub struct Terminal<W> {
    out: Mutex<W>,
    sent: Mutex<BTreeSet<String>>,
}

impl<W: Write + Send> Terminal<W> {
    /// A terminal channel over `out`.
    pub fn new(out: W) -> Self {
        Self {
            out: Mutex::new(out),
            sent: Mutex::new(BTreeSet::new()),
        }
    }

    /// The writer back, for tests.
    ///
    /// # Panics
    /// A previous writer panicked while holding the lock.
    pub fn into_inner(self) -> W {
        self.out.into_inner().expect("terminal writer poisoned")
    }
}

#[async_trait]
impl<W: Write + Send> HumanChannel for Terminal<W> {
    async fn deliver(&self, notice: &Notice, text: &str) -> Result<(), CallError> {
        let key = notice.key();
        let fresh = self
            .sent
            .lock()
            .map_err(|_| CallError::Channel("terminal state poisoned".into()))?
            .insert(key);
        if fresh {
            let mut out = self
                .out
                .lock()
                .map_err(|_| CallError::Channel("terminal writer poisoned".into()))?;
            writeln!(out, "{text}")
                .and_then(|()| out.flush())
                .map_err(|e| CallError::Channel(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{HumanChannel, Terminal};
    use crate::episodes::{EpisodeId, Lane, Notice};
    use runo_clock::Level;
    use runo_core::{Clock, Hash, SingerId, TaskId, VerseId};

    #[tokio::test]
    async fn prints_each_key_once() {
        let n = Notice {
            id: EpisodeId {
                lane: Lane {
                    singer: SingerId("bob".into()),
                    task: TaskId("HOD-43".into()),
                },
                clock: Clock::Task,
                opening: VerseId(Hash([7; 32])),
            },
            level: Level::Called,
            resolved: None,
            reason: "stalled".into(),
        };
        let t = Terminal::new(Vec::new());
        t.deliver(&n, "HOD-43 is stalled").await.unwrap();
        t.deliver(&n, "HOD-43 is stalled").await.unwrap();
        let raised = Notice {
            level: Level::Suspected,
            ..n
        };
        t.deliver(&raised, "HOD-43 is still stalled").await.unwrap();
        assert_eq!(
            String::from_utf8(t.into_inner()).unwrap(),
            "HOD-43 is stalled\nHOD-43 is still stalled\n"
        );
    }
}
