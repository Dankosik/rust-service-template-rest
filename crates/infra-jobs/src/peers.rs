use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;

use crate::{Kinds, maintenance};

struct Peer {
    kinds: Vec<&'static str>,
    claim_wake: Arc<Notify>,
}

#[derive(Default)]
struct Peers {
    entries: Vec<Peer>,
    sample_revision: Arc<()>,
}

impl Peers {
    fn invalidate_sample(&mut self) {
        self.sample_revision = Arc::default();
        maintenance::invalidate_sample();
    }

    /// Sorted and deduplicated for registration's binary searches.
    fn registered_kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<_> = self
            .entries
            .iter()
            .flat_map(|peer| peer.kinds.iter().copied())
            .collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    }
}

#[derive(Default)]
pub(crate) struct PeerRegistry {
    peers: Mutex<Peers>,
}

impl PeerRegistry {
    /// The peer list is never held across an `.await`.
    fn lock(&self) -> MutexGuard<'_, Peers> {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        registry: &Kinds,
        claim_wake: &Arc<Notify>,
    ) -> PeerRegistration {
        let mut peers = self.lock();
        let previous = peers.registered_kinds();
        let kinds: Vec<_> = registry.names().collect();
        for &kind in &kinds {
            if previous.binary_search(&kind).is_err() {
                maintenance::reset_kind_metrics(kind);
            }
        }
        peers.entries.push(Peer {
            kinds,
            claim_wake: Arc::clone(claim_wake),
        });
        peers.invalidate_sample();
        PeerRegistration {
            registry: Arc::clone(self),
            claim_wake: Arc::clone(claim_wake),
        }
    }

    fn unregister(&self, claim_wake: &Arc<Notify>) {
        let mut peers = self.lock();
        let Some(index) = peers
            .entries
            .iter()
            .position(|peer| Arc::ptr_eq(&peer.claim_wake, claim_wake))
        else {
            return;
        };
        let removed = peers.entries.swap_remove(index);
        let remaining = peers.registered_kinds();
        let mut changed = false;
        for kind in removed.kinds {
            if remaining.binary_search(&kind).is_err() {
                maintenance::reset_kind_metrics(kind);
                changed = true;
            }
        }
        if changed {
            peers.invalidate_sample();
        }
    }

    /// Snapshot the kinds and the identity that authorizes publishing their sample.
    pub(crate) fn sampling_kinds(&self) -> (Vec<&'static str>, Arc<()>) {
        let peers = self.lock();
        (peers.registered_kinds(), Arc::clone(&peers.sample_revision))
    }

    /// A change can invalidate a sample even if the same kinds later return.
    pub(crate) fn publish_if_current(&self, revision: &Arc<()>, publish: impl FnOnce()) -> bool {
        let peers = self.lock();
        if !Arc::ptr_eq(&peers.sample_revision, revision) {
            return false;
        }
        publish();
        true
    }

    pub(crate) fn wake_all(&self) {
        self.wake_matching(None);
    }

    pub(crate) fn wake_kind(&self, kind: &str) {
        self.wake_matching(Some(kind));
    }

    fn wake_matching(&self, kind: Option<&str>) {
        for peer in &self.lock().entries {
            if kind.is_none_or(|kind| peer.kinds.contains(&kind)) {
                peer.claim_wake.notify_one();
            }
        }
    }
}

/// Owned by the unstarted engine, then by its claim task.
pub(crate) struct PeerRegistration {
    registry: Arc<PeerRegistry>,
    claim_wake: Arc<Notify>,
}

impl Drop for PeerRegistration {
    fn drop(&mut self) {
        self.registry.unregister(&self.claim_wake);
    }
}
