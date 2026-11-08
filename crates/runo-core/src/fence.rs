//! Shared fencing types (technical spec §7–§8; technical decisions 1 and 4). Every crate that
//! installs, acknowledges or reads a fence speaks these; the store persists them, the fence crate
//! orchestrates them, and nothing here does I/O.

use serde::{Deserialize, Serialize};

use crate::types::{Epoch, ResourceId, VerseId};

/// How durable a persisted fact must be before the write that made it returns.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Durability {
    /// Survives the writer's process crashing.
    Process,
    /// Survives power loss on the host: fsync has returned.
    Host,
    /// Survives loss of fewer than k replicas.
    Replicated(u8),
}

/// A fence as a resource stores it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fence {
    /// Writes at this epoch or above are admitted.
    Epoch(Epoch),
    /// Above every epoch: nothing writes after it.
    Closed,
}

impl Fence {
    /// `max(self, other)`: a fence never lowers, and `Closed` is above every epoch.
    #[must_use]
    pub fn raised_to(self, other: Fence) -> Fence {
        match (self, other) {
            (Fence::Closed, _) | (_, Fence::Closed) => Fence::Closed,
            (Fence::Epoch(a), Fence::Epoch(b)) => Fence::Epoch(a.max(b)),
        }
    }
}

/// A resource's acknowledgement of an installed fence. Explicit on purpose: it names the
/// resource, the grant it answers, the fence now enforced and the durability reached, so the
/// allocator never has to supply any of these from its own context.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FenceAck {
    /// The resource that persisted the fence.
    pub resource: ResourceId,
    /// The `Grant` verse (or, for `Fence::Closed`, the `Closing` verse) the install answers.
    pub grant: VerseId,
    /// The fence now enforced: the installed epoch, or `Closed`.
    pub fence: Fence,
    /// The durability the persisted fence reached before this ack.
    pub durability: Durability,
}

#[cfg(test)]
mod tests {
    use super::{Epoch, Fence};

    #[test]
    fn a_fence_never_lowers_and_closed_is_above_every_epoch() {
        assert_eq!(
            Fence::Epoch(Epoch(3)).raised_to(Fence::Epoch(Epoch(2))),
            Fence::Epoch(Epoch(3))
        );
        assert_eq!(
            Fence::Epoch(Epoch(2)).raised_to(Fence::Epoch(Epoch(3))),
            Fence::Epoch(Epoch(3))
        );
        assert_eq!(
            Fence::Closed.raised_to(Fence::Epoch(Epoch(9))),
            Fence::Closed
        );
        assert_eq!(
            Fence::Epoch(Epoch(9)).raised_to(Fence::Closed),
            Fence::Closed
        );
    }
}
