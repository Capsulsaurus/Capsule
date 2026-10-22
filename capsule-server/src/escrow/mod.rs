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
//!
//! # The sealed identity rides beside it
//!
//! A recovered device needs two things to open as the account it recovers: the master key, which
//! the escrow above carries, and the account's **user identity key**. The IK is drawn at random,
//! not derived from the master key — rotating the master key must not rotate the identity — and
//! the server pins it on the first directory publish (`S-C42`), so a recovered device cannot mint
//! its own. `capsule_core`'s `Workspace::sealed_identity` seals it under the master key, and this
//! port holds that blob in a second slot beside the escrow ([`EscrowStore::store_identity`],
//! [`EscrowStore::fetch_identity`]).
//!
//! It adds no second root and no new trust in the server. The sealed identity opens only under
//! the master key, and the master key comes out of the escrow only under the recovery secret, so
//! whoever can open one could already open the other; the server holds two ciphertexts it can
//! read neither of. It obeys every rule the escrow does — one per account, opaque, replaced in
//! one operation, never versioned — and the two slots are independent: storing one never
//! touches the other, because the escrow is re-wrapped on every recovery-secret rotation and
//! the identity is not.
//!
//! # Album keys ride beside it too, one slot per album (#741)
//!
//! The master key and the identity open a recovered library as the account, but its albums'
//! content keys (AMKs) are random per album and epoch, so a library holding only those two can
//! open none of the account's assets. Each album's whole key state — every epoch's AMK, its
//! signing seeds and its authority — is therefore held here as well, sealed by the client under
//! the master key ([`EscrowStore::store_album_keys`], [`EscrowStore::list_album_keys`]).
//!
//! The same argument as the sealed identity's: whoever can open one could open the escrow
//! already, so this is a wrap reachable from the master key and not a second root. It differs
//! in one respect: there is one slot **per album**, not per account, because albums are created
//! and rotated independently and one device rewriting a single blob for every album would drop
//! the albums another device had escrowed since. Each slot is still replaced by one operation,
//! never versioned, and opaque; a client re-stores an album's slot whenever its state changes.
//!
//! # Adapters
//!
//! [`InMemoryEscrow`] is the deterministic double and [`PostgresEscrow`] the durable adapter;
//! both pass the one suite in [`conformance`].

pub mod conformance;
pub mod postgres;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;

pub use self::postgres::PostgresEscrow;
use crate::store::{AlbumId, StoreFuture, UserId};

/// The largest escrow blob this server will accept.
///
/// A wrapped 32-byte master key with a KDF header and an AEAD tag is a few hundred bytes; 64 KiB
/// is orders of magnitude past any version of that and still small enough that storing one
/// costs nothing. Coarse on purpose — a tight bound would be a format check, and the server has
/// no business knowing the format.
pub const MAX_ESCROW_BYTES: usize = 64 * 1024;

/// The largest album key escrow this server will accept.
///
/// Larger than [`MAX_ESCROW_BYTES`] because an album's escrow grows with its history: each
/// epoch adds an AMK, a hybrid write-tier key and an admin signature over it, a few KiB in all,
/// so 512 KiB holds on the order of a hundred epochs. Still coarse — the server does not know
/// the format — and below the bounded body cap the store route is mounted under, so a breach
/// is this module's coded `400` rather than the transport's uncoded `413`.
pub const MAX_ALBUM_KEY_ESCROW_BYTES: usize = 512 * 1024;

const _: () = assert!(
    (MAX_ALBUM_KEY_ESCROW_BYTES as u64) < crate::limits::MAX_BOUNDED_BODY_BYTES,
    "an album key escrow at the ceiling must fit under the bounded body cap"
);

/// The most album key escrows one account may hold.
///
/// Album key slots are never deleted (a delete would be a way to remove an album's last
/// recovery path with a stolen session token), so without a bound an account could grow its
/// rows without limit, one canonical album id at a time. Far past any personal library's album
/// count; a store that would open slot `MAX_ESCROWED_ALBUMS + 1` is refused, while replacing an
/// existing slot never is on count.
pub const MAX_ESCROWED_ALBUMS: usize = 4096;

/// The most bytes one account's album key escrows may hold together.
///
/// Bounds the listing a client fetches before every push as well as the storage: 32 MiB of blobs
/// is about 43 MiB of base64 JSON, and a typical album escrow is a few KiB, so a library reaches
/// it only with thousands of long-lived, often-rotated albums. Counted after the store — the
/// slot being replaced counts at its new size, so shrinking one always fits.
pub const MAX_ALBUM_KEY_ESCROW_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// Whether an account whose **other** album key slots number `other_slots` and hold
/// `other_bytes` may store a slot of `new_bytes`: the count and total after the store stay
/// within [`MAX_ESCROWED_ALBUMS`] and [`MAX_ALBUM_KEY_ESCROW_TOTAL_BYTES`].
pub fn album_keys_fit(other_slots: usize, other_bytes: usize, new_bytes: usize) -> bool {
    other_slots < MAX_ESCROWED_ALBUMS
        && other_bytes.saturating_add(new_bytes) <= MAX_ALBUM_KEY_ESCROW_TOTAL_BYTES
}

/// What storing an album key escrow did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumKeysStored {
    /// The slot holds the new blob; whether it displaced an earlier one.
    Stored(Replaced),
    /// Refused, with nothing written: the account's album key escrows would pass
    /// [`MAX_ESCROWED_ALBUMS`] or [`MAX_ALBUM_KEY_ESCROW_TOTAL_BYTES`].
    Full,
}

/// One account's escrowed master key, or its sealed user identity key — the two blobs share
/// one shape, and which one a record is follows from the [`EscrowStore`] method it went through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscrowRecord {
    /// The account it belongs to.
    pub user_id: UserId,
    /// The wrapped master key (or the sealed identity), exactly as the client sent it.
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
    #[error("the escrow blob is {size} bytes, past the {ceiling}-byte ceiling")]
    TooLarge {
        /// How large it was.
        size: usize,
        /// The ceiling it was held to.
        ceiling: usize,
    },
}

/// Check the one thing the server is entitled to check.
///
/// # Errors
///
/// Returns [`MalformedEscrow`] for an empty body or one past [`MAX_ESCROW_BYTES`].
pub fn admissible(blob: &[u8]) -> Result<(), MalformedEscrow> {
    admissible_under(blob, MAX_ESCROW_BYTES)
}

/// [`admissible`] for an album key escrow, under [`MAX_ALBUM_KEY_ESCROW_BYTES`].
///
/// # Errors
///
/// Returns [`MalformedEscrow`] for an empty body or one past [`MAX_ALBUM_KEY_ESCROW_BYTES`].
pub fn admissible_album_keys(blob: &[u8]) -> Result<(), MalformedEscrow> {
    admissible_under(blob, MAX_ALBUM_KEY_ESCROW_BYTES)
}

/// Refuse an empty blob or one past `ceiling`.
fn admissible_under(blob: &[u8], ceiling: usize) -> Result<(), MalformedEscrow> {
    if blob.is_empty() {
        return Err(MalformedEscrow::Empty);
    }
    if blob.len() > ceiling {
        return Err(MalformedEscrow::TooLarge {
            size: blob.len(),
            ceiling,
        });
    }
    Ok(())
}

/// One album's key escrow: the album's whole key state, sealed by the client under the
/// account's master key. Opaque here, like every blob this module holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumKeyRecord {
    /// The account it belongs to.
    pub user_id: UserId,
    /// The album it is the keys of, as a canonical lowercase hyphenated UUID.
    pub album_id: AlbumId,
    /// The sealed album keys, exactly as the client sent them.
    pub blob: Vec<u8>,
    /// When the server accepted it.
    pub stored_at: Timestamp,
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

    /// Store `record` as the account's **sealed identity**, replacing whatever sealed identity it
    /// had, and report whether one was replaced.
    ///
    /// The same one-operation replace [`store`](Self::store) owes, in a slot of its own: it never
    /// reads, writes or replaces the account's escrow.
    fn store_identity(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced>;

    /// The sealed identity currently held for `user`, or `None`. Independent of whether an
    /// escrow is held.
    fn fetch_identity<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>>;

    /// Store `record` as the key escrow of its album, replacing whatever that album's slot held,
    /// and report whether one was replaced.
    ///
    /// The same one-operation replace [`store`](Self::store) owes, in a slot per
    /// `(user, album)`: it never touches another album's slot, the escrow or the sealed
    /// identity.
    ///
    /// Bounded per account: a store that would take the account's slots past
    /// [`album_keys_fit`] is [`AlbumKeysStored::Full`] and writes nothing. The bound is checked
    /// and the slot written in one critical section per account, so two concurrent stores cannot
    /// both pass a check only one of them fits.
    fn store_album_keys(&self, record: AlbumKeyRecord) -> StoreFuture<'_, AlbumKeysStored>;

    /// Every album key escrow held for `user`, sorted by album id; empty when there is none.
    fn list_album_keys<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<AlbumKeyRecord>>;
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
    identities: Mutex<BTreeMap<UserId, EscrowRecord>>,
    album_keys: Mutex<BTreeMap<(UserId, String), AlbumKeyRecord>>,
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

    fn store_identity(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced> {
        Box::pin(async move {
            let mut identities = lock(&self.identities);
            let user = record.user_id.clone();
            let bytes = record.blob.len();
            if identities.insert(user.clone(), record).is_some() {
                tracing::info!(
                    %user,
                    bytes,
                    "a sealed identity was replaced; the previous one is gone"
                );
                return Ok(Replaced::Yes);
            }
            tracing::info!(%user, bytes, "a sealed identity was stored for the first time");
            Ok(Replaced::No)
        })
    }

    fn fetch_identity<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>> {
        Box::pin(async move { Ok(lock(&self.identities).get(user).cloned()) })
    }

    fn store_album_keys(&self, record: AlbumKeyRecord) -> StoreFuture<'_, AlbumKeysStored> {
        Box::pin(async move {
            let mut album_keys = lock(&self.album_keys);
            let user = record.user_id.clone();
            let album = record.album_id.as_str().to_owned();
            let bytes = record.blob.len();
            let (other_slots, other_bytes) = album_keys
                .iter()
                .filter(|((owner, slot), _)| *owner == user && *slot != album)
                .fold((0, 0), |(slots, total), (_, held)| {
                    (slots + 1, total + held.blob.len())
                });
            if !album_keys_fit(other_slots, other_bytes, bytes) {
                tracing::warn!(
                    %user,
                    %album,
                    bytes,
                    other_slots,
                    other_bytes,
                    "an album key escrow was refused: the account's slots are full"
                );
                return Ok(AlbumKeysStored::Full);
            }
            if album_keys
                .insert((user.clone(), album.clone()), record)
                .is_some()
            {
                tracing::info!(%user, %album, bytes, "an album key escrow was replaced");
                return Ok(AlbumKeysStored::Stored(Replaced::Yes));
            }
            tracing::info!(%user, %album, bytes, "an album key escrow was stored for the first time");
            Ok(AlbumKeysStored::Stored(Replaced::No))
        })
    }

    fn list_album_keys<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<AlbumKeyRecord>> {
        Box::pin(async move {
            // Keyed `(user, album)`, so one account's slots are contiguous and in album order.
            Ok(lock(&self.album_keys)
                .iter()
                .filter(|((owner, _), _)| owner == user)
                .map(|(_, record)| record.clone())
                .collect())
        })
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
