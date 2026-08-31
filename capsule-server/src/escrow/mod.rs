//! The master-key escrow (`S-C12`) — the one blob a server holds that can reconstruct a
//! library, and cannot read.
//!
//! # What is stored, and what the server knows about it
//!
//! The account master key, wrapped under a key derived from the user's recovery secret. The
//! server stores **opaque bytes**: it never derives, never unwraps, never inspects the wrap
//! format, and cannot tell a real escrow from random noise of the same length. The
//! ≥128-bit entropy floor design/backup-recovery.md sets on the recovery secret is enforced
//! *client-side*, in `capsule_core`, because it is a property of a secret the server never
//! sees — a server-side check would be a check on something that has already been hashed away.
//!
//! The only judgement made here is a coarse size bound, and it is not a validation of the
//! format: it is a refusal to store something that cannot be an escrow at any version, so a
//! misdirected upload does not silently become an account's recovery blob.
//!
//! # Single active escrow, and why replacement is one operation
//!
//! There is exactly one escrow per account, and storing a new one **deletes the old in the same
//! operation**. That is the guided re-wrap contract: after a failed-verification threshold a
//! client mints a fresh recovery secret, re-wraps the *same* master key, and replaces the blob —
//! and the whole point is that the lost secret then unwraps nothing. A store that kept both, or
//! that deleted-then-wrote, would leave a window in which the old secret still works, or worse,
//! a window in which neither does and the account has no recovery path at all.
//!
//! So [`EscrowStore::store`] is a replace, not an insert plus a delete, and every adapter owes
//! atomicity across it. This is the same lesson as `S-C37`'s sequence mint and `S-C42`'s anchor,
//! in the place where getting it wrong loses a library rather than an entry.
//!
//! # It is not versioned, and that is deliberate
//!
//! Keeping the previous escrow "just in case" sounds prudent and is the opposite: rotation
//! happens precisely when the user believes the old secret is lost or compromised, and a server
//! that retained the blob it unwraps would preserve exactly the artifact the rotation exists to
//! destroy. Recovery from a bad rotation is the backup *artifact*, which the user holds.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;

use crate::store::{StoreFuture, UserId};

/// The largest escrow blob this server will accept.
///
/// A wrapped 32-byte master key with a KDF header and an AEAD tag is a few hundred bytes; 64 KiB
/// is orders of magnitude past any version of that and still small enough that storing one
/// costs nothing. Coarse on purpose — a tight bound would be a format check, and the server has
/// no business knowing the format.
pub const MAX_ESCROW_BYTES: usize = 64 * 1024;

/// One account's escrowed master key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscrowRecord {
    /// The account it belongs to.
    pub user_id: UserId,
    /// The wrapped master key, exactly as the client sent it.
    pub blob: Vec<u8>,
    /// When the server accepted it.
    ///
    /// Served back so a client can tell whether its cached copy is the current one — the
    /// stale-cache rule in design/backup-recovery.md, which exists because a rotation from
    /// another device would otherwise manufacture false verification failures on this one.
    pub stored_at: Timestamp,
}

/// Why an escrow was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MalformedEscrow {
    /// An empty body is not a blob.
    #[error("an escrow blob cannot be empty")]
    Empty,
    /// Past the coarse ceiling.
    #[error("the escrow blob is {size} bytes, past the {MAX_ESCROW_BYTES}-byte ceiling")]
    TooLarge {
        /// How large it was.
        size: usize,
    },
}

/// Check the one thing the server is entitled to check.
///
/// # Errors
///
/// Returns [`MalformedEscrow`] for an empty body or one past [`MAX_ESCROW_BYTES`].
pub fn admissible(blob: &[u8]) -> Result<(), MalformedEscrow> {
    if blob.is_empty() {
        return Err(MalformedEscrow::Empty);
    }
    if blob.len() > MAX_ESCROW_BYTES {
        return Err(MalformedEscrow::TooLarge { size: blob.len() });
    }
    Ok(())
}

/// Where escrowed master keys live.
pub trait EscrowStore: std::fmt::Debug + Send + Sync {
    /// Store `record`, replacing whatever this account had, and report whether one was replaced.
    ///
    /// **One operation.** An adapter that deleted and then inserted would leave a window with no
    /// escrow at all, and one that inserted before deleting would leave a window in which the
    /// secret the user is rotating *away from* still works. Either is a recovery path the user
    /// did not ask for.
    fn store(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced>;

    /// The escrow currently held for `user`, or `None`.
    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>>;
}

/// Whether storing an escrow displaced an earlier one.
///
/// Reported because the two are different events for a client: the first escrow completes
/// account setup, and a later one completes a *rotation* whose whole meaning is that the
/// previous secret has stopped working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replaced {
    /// This account had no escrow before.
    No,
    /// An earlier escrow was displaced and is gone.
    Yes,
}

/// A deterministic in-memory adapter.
///
/// One mutex over the whole map, which is what makes the replace atomic. A Postgres adapter gets
/// the same property from a single upsert — never a delete followed by an insert.
#[derive(Debug, Default)]
pub struct InMemoryEscrow {
    held: Mutex<BTreeMap<UserId, EscrowRecord>>,
}

impl InMemoryEscrow {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl EscrowStore for InMemoryEscrow {
    fn store(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced> {
        Box::pin(async move {
            let mut held = lock(&self.held);
            let user = record.user_id.clone();
            let bytes = record.blob.len();
            let replaced = held.insert(user.clone(), record);
            // The previous blob is dropped here and nowhere retained. A "previous escrow" table
            // would preserve exactly the artifact a rotation exists to destroy.
            if replaced.is_some() {
                tracing::info!(
                    %user,
                    bytes,
                    "an escrow was replaced; the previous wrapped master key is gone"
                );
                return Ok(Replaced::Yes);
            }
            tracing::info!(%user, bytes, "an escrow was stored for the first time");
            Ok(Replaced::No)
        })
    }

    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>> {
        Box::pin(async move { Ok(lock(&self.held).get(user).cloned()) })
    }
}

/// The escrow module's collaborators.
#[derive(Debug, Clone)]
pub struct EscrowContext {
    escrows: Arc<dyn EscrowStore>,
    clock: Arc<dyn crate::store::Clock>,
}

impl EscrowContext {
    /// Assembles the module.
    pub fn new(escrows: Arc<dyn EscrowStore>, clock: Arc<dyn crate::store::Clock>) -> Self {
        Self { escrows, clock }
    }

    /// Where escrows live.
    pub fn escrows(&self) -> &dyn EscrowStore {
        self.escrows.as_ref()
    }

    /// The clock a stored escrow is stamped from.
    pub fn clock(&self) -> &dyn crate::store::Clock {
        self.clock.as_ref()
    }
}

#[cfg(test)]
mod tests;
