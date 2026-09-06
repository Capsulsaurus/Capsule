//! The deterministic doubles: [`InMemoryCapabilities`] and [`InMemoryPeers`].
//!
//! One mutex each, which is what makes every multi-step operation — revoke-and-publish,
//! refresh — one critical section, exactly as the Postgres adapter's transaction is.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;

use super::PeerId;
use super::peers::{BlockOutcome, PeerRecord, PeerStore, UnblockOutcome};
use super::store::{
    CapabilityFilter, CapabilityRecord, CapabilityStore, RefreshOutcome, RevokeOutcome,
};
use crate::discovery::revocation::{
    MAX_TOKEN_TTL, PublishedRevocations, RevocationError, RevocationList, RevokeFuture,
    RevokedToken,
};
use crate::store::{Clock, StoreError, StoreFuture};

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The deterministic capability store, and the revocation list it publishes.
#[derive(Debug)]
pub struct InMemoryCapabilities {
    inner: Mutex<Inner>,
    clock: Arc<dyn Clock>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Every capability issued here, by `jti`.
    records: BTreeMap<String, CapabilityRecord>,
    /// Every published revocation, by `jti`, with the token's own expiry for pruning.
    published: BTreeMap<String, Timestamp>,
}

impl Inner {
    /// Revoke `jti` at `at` if a record backs it, and publish it either way.
    ///
    /// The one place both halves happen, so no path can do one without the other. The expiry
    /// the entry is published under is the **record's** when there is one — a caller's shorter
    /// `expires_at` would prune the entry while the token still verifies, which is a peer
    /// honouring a revoked token — and an entry already published is never shortened.
    fn revoke(&mut self, jti: &str, expires_at: Timestamp, at: Timestamp) {
        let mut expires_at = expires_at;
        if let Some(record) = self.records.get_mut(jti) {
            if record.revoked_at.is_none() {
                record.revoked_at = Some(at);
            }
            expires_at = record.expires_at;
        }
        let entry = self.published.entry(jti.to_owned()).or_insert(expires_at);
        *entry = (*entry).max(expires_at);
    }

    /// Refuse a record whose lifetime the published list could not stay bounded under.
    fn admissible(record: &CapabilityRecord) -> Result<(), StoreError> {
        if record.expires_at.duration_since(record.issued_at) > MAX_TOKEN_TTL {
            return Err(StoreError::Rejected {
                store: "capabilities",
                detail: format!(
                    "capability {} would live past the {MAX_TOKEN_TTL} ceiling",
                    record.jti
                ),
            });
        }
        Ok(())
    }
}

impl InMemoryCapabilities {
    /// An empty store reading `clock` for pruning and for `generated_at`.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            clock,
        }
    }
}

impl RevocationList for InMemoryCapabilities {
    fn revoke(&self, token: RevokedToken) -> RevokeFuture<'_> {
        Box::pin(async move {
            let now = self.clock.now();
            let ceiling = crate::store::deadline(now, MAX_TOKEN_TTL);
            if token.expires_at > ceiling {
                tracing::warn!(
                    jti = %token.jti,
                    expires_at = %token.expires_at,
                    "a revocation was refused: its expiry is beyond the capability TTL ceiling"
                );
                return Err(RevocationError::BeyondTtlCeiling {
                    expires_at: token.expires_at,
                    ceiling: MAX_TOKEN_TTL,
                }
                .into());
            }
            let mut inner = lock(&self.inner);
            inner.revoke(&token.jti, token.expires_at, now);
            tracing::info!(
                jti = %token.jti,
                expires_at = %token.expires_at,
                published = inner.published.len(),
                "a federation capability token was revoked"
            );
            Ok(())
        })
    }

    fn published(&self) -> StoreFuture<'_, PublishedRevocations> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut inner = lock(&self.inner);
            // Pruned on read *and* retained pruned, so a list nobody fetches does not grow
            // forever holding entries that already mean nothing.
            inner.published.retain(|_, expires_at| *expires_at > now);
            let mut revoked: Vec<RevokedToken> = inner
                .published
                .iter()
                .map(|(jti, expires_at)| RevokedToken {
                    jti: jti.clone(),
                    expires_at: *expires_at,
                })
                .collect();
            revoked.sort_by_key(|token| (token.expires_at, token.jti.clone()));
            Ok(PublishedRevocations {
                generated_at: now,
                revoked,
            })
        })
    }
}

impl CapabilityStore for InMemoryCapabilities {
    fn issue(&self, record: CapabilityRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            Inner::admissible(&record)?;
            let mut inner = lock(&self.inner);
            if inner.records.contains_key(&record.jti) {
                return Err(StoreError::Rejected {
                    store: "capabilities",
                    detail: format!("a capability with jti {} is already recorded", record.jti),
                });
            }
            tracing::info!(
                jti = %record.jti,
                peer = %record.peer_id,
                album = %record.album_id,
                member = %record.member,
                scope = %record.scope,
                granted_epoch = record.granted_epoch,
                expires_at = %record.expires_at,
                "a federation capability was recorded"
            );
            inner.records.insert(record.jti.clone(), record);
            Ok(())
        })
    }

    fn find<'a>(&'a self, jti: &'a str) -> StoreFuture<'a, Option<CapabilityRecord>> {
        Box::pin(async move { Ok(lock(&self.inner).records.get(jti).cloned()) })
    }

    fn live<'a>(
        &'a self,
        filter: &'a CapabilityFilter,
        now: Timestamp,
    ) -> StoreFuture<'a, Vec<CapabilityRecord>> {
        Box::pin(async move {
            Ok(lock(&self.inner)
                .records
                .values()
                .filter(|record| record.is_live(now))
                .filter(|record| match filter {
                    CapabilityFilter::Album(album) => &record.album_id == album,
                    CapabilityFilter::Peer(peer) => &record.peer_id == peer,
                })
                .cloned()
                .collect())
        })
    }

    fn revoke_issued<'a>(&'a self, jti: &'a str, at: Timestamp) -> StoreFuture<'a, RevokeOutcome> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(record) = inner.records.get(jti) else {
                return Ok(RevokeOutcome::Unknown);
            };
            if record.revoked_at.is_some() {
                return Ok(RevokeOutcome::AlreadyRevoked);
            }
            let expires_at = record.expires_at;
            inner.revoke(jti, expires_at, at);
            tracing::info!(%jti, published = inner.published.len(), "an issued capability was revoked");
            Ok(RevokeOutcome::Revoked)
        })
    }

    fn refresh<'a>(
        &'a self,
        predecessor: &'a str,
        successor: CapabilityRecord,
        at: Timestamp,
    ) -> StoreFuture<'a, RefreshOutcome> {
        Box::pin(async move {
            Inner::admissible(&successor)?;
            let mut inner = lock(&self.inner);
            let Some(old) = inner.records.get(predecessor) else {
                return Ok(RefreshOutcome::Unknown);
            };
            if successor.peer_id != old.peer_id
                || successor.album_id != old.album_id
                || successor.member != old.member
            {
                return Err(StoreError::Rejected {
                    store: "capabilities",
                    detail: format!(
                        "a successor of {predecessor} must carry its peer, album and member"
                    ),
                });
            }
            if let Some(next) = &old.refreshed_to {
                let existing =
                    inner
                        .records
                        .get(next)
                        .cloned()
                        .ok_or_else(|| StoreError::Corrupt {
                            store: "capabilities",
                            record: "CapabilityRecord",
                            detail: format!(
                                "{predecessor} was refreshed to {next}, which is not recorded"
                            ),
                        })?;
                return Ok(RefreshOutcome::AlreadyRefreshed(existing));
            }
            if old.revoked_at.is_some() {
                return Ok(RefreshOutcome::Revoked);
            }
            if inner.records.contains_key(&successor.jti) {
                return Err(StoreError::Rejected {
                    store: "capabilities",
                    detail: format!(
                        "a capability with jti {} is already recorded",
                        successor.jti
                    ),
                });
            }
            let old_expires_at = old.expires_at;
            inner.revoke(predecessor, old_expires_at, at);
            if let Some(old) = inner.records.get_mut(predecessor) {
                old.refreshed_to = Some(successor.jti.clone());
            }
            tracing::info!(
                predecessor = %predecessor,
                successor = %successor.jti,
                peer = %successor.peer_id,
                "a federation capability was refreshed"
            );
            inner
                .records
                .insert(successor.jti.clone(), successor.clone());
            Ok(RefreshOutcome::Issued(successor))
        })
    }
}

/// The deterministic peer store.
#[derive(Debug, Default)]
pub struct InMemoryPeers {
    peers: Mutex<BTreeMap<PeerId, PeerRecord>>,
}

impl InMemoryPeers {
    /// An empty store: no peer pinned, no peer blocked.
    pub fn new() -> Self {
        Self::default()
    }
}

impl PeerStore for InMemoryPeers {
    fn pin<'a>(
        &'a self,
        peer: &'a PeerId,
        signing_key: [u8; 32],
        at: Timestamp,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let mut peers = lock(&self.peers);
            match peers.get_mut(peer) {
                Some(record) => record.signing_key = Some(signing_key),
                None => {
                    peers.insert(
                        peer.clone(),
                        PeerRecord {
                            server_id: peer.clone(),
                            signing_key: Some(signing_key),
                            first_seen_at: at,
                            blocked_at: None,
                            note: None,
                        },
                    );
                }
            }
            tracing::info!(%peer, "a peer's signing key was pinned");
            Ok(())
        })
    }

    fn read<'a>(&'a self, peer: &'a PeerId) -> StoreFuture<'a, Option<PeerRecord>> {
        Box::pin(async move { Ok(lock(&self.peers).get(peer).cloned()) })
    }

    fn block<'a>(
        &'a self,
        peer: &'a PeerId,
        at: Timestamp,
        note: Option<String>,
    ) -> StoreFuture<'a, BlockOutcome> {
        Box::pin(async move {
            let mut peers = lock(&self.peers);
            let record = peers.entry(peer.clone()).or_insert_with(|| PeerRecord {
                server_id: peer.clone(),
                signing_key: None,
                first_seen_at: at,
                blocked_at: None,
                note: None,
            });
            if record.blocked_at.is_some() {
                return Ok(BlockOutcome::AlreadyBlocked);
            }
            record.blocked_at = Some(at);
            record.note = note;
            tracing::warn!(%peer, "a peer server was blocked");
            Ok(BlockOutcome::Blocked)
        })
    }

    fn unblock<'a>(&'a self, peer: &'a PeerId) -> StoreFuture<'a, UnblockOutcome> {
        Box::pin(async move {
            let mut peers = lock(&self.peers);
            match peers.get_mut(peer) {
                Some(record) if record.blocked_at.is_some() => {
                    record.blocked_at = None;
                    record.note = None;
                    tracing::info!(%peer, "a peer server was unblocked");
                    Ok(UnblockOutcome::Unblocked)
                }
                _ => Ok(UnblockOutcome::NotBlocked),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::conformance::{self, Harness};
    use super::{InMemoryCapabilities, InMemoryPeers};
    use crate::federation::{CapabilityStore, PeerStore};
    use crate::store::memory::ManualClock;

    #[derive(Debug)]
    struct MemoryHarness {
        clock: Arc<ManualClock>,
        capabilities: InMemoryCapabilities,
        peers: InMemoryPeers,
    }

    impl Harness for MemoryHarness {
        fn capabilities(&self) -> &dyn CapabilityStore {
            &self.capabilities
        }

        fn peers(&self) -> &dyn PeerStore {
            &self.peers
        }

        fn clock(&self) -> &ManualClock {
            &self.clock
        }
    }

    #[tokio::test]
    async fn the_in_memory_stores_conform() {
        let clock = Arc::new(ManualClock::default());
        let harness = MemoryHarness {
            capabilities: InMemoryCapabilities::new(clock.clone()),
            peers: InMemoryPeers::new(),
            clock,
        };
        conformance::run_all(&harness).await;
    }
}
