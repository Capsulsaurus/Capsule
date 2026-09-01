//! [`AccountRegistry`] — bringing an account into existence (slice `S-C53`).
//!
//! # Why this is not a method on [`AccountDirectory`](super::AccountDirectory)
//!
//! That port's own docs said so before this existed: *"a directory that also listed accounts,
//! created them or changed passwords would be the grab-bag `S-C29` deleted, rebuilt one method
//! at a time; registration and password change are their own surfaces and will bring their own
//! contracts."* This is that contract, and keeping it separate is what stops the one operation
//! login asks from growing into six.
//!
//! It matters more than tidiness here. `authenticate` is designed so that *no caller can tell an
//! unknown account from a wrong password* — one `Refused` value, a timing-equalized miss, no
//! branch to reintroduce an oracle through. Creation cannot have that property: a registration
//! has to say whether it happened. Putting the two behind one trait would put the operation that
//! must not disclose beside the operation that must, and eventually somebody would reach for the
//! wrong one.
//!
//! # The disclosure this operation does make, stated plainly
//!
//! A taken address answers `409 error.auth.user_already_exists`, which **is** an account
//! oracle: anyone may ask whether an address has an account here. That is the contract the
//! message catalog already fixed, and the alternative — answering success and creating nothing —
//! is worse in every way, because a client that believed it would then fail to sign in with no
//! explanation.
//!
//! What bounds it is a rate limiter, and **there is none**. The counter port exists (`S-C32`)
//! and the key it needs does not: limiting registration means limiting *a source*, and this
//! server has no trusted client address behind an unconfigured proxy chain. That is the same
//! missing fact [`CounterKey::ShareSource`](crate::counter::CounterKey::ShareSource) and
//! [`DropSource`](crate::counter::CounterKey::DropSource) are waiting on, and it is why
//! [`CounterKey::RegistrationSource`](crate::counter::CounterKey::RegistrationSource) is
//! declared here and consumed nowhere: the shape is written down so the day the fact arrives is
//! a one-line day. An email-keyed limiter was considered and rejected — it would bound repeated
//! probes against *one* address while doing nothing about a sweep across many, which is the
//! actual attack, and a limiter that reads as protection without providing it is worse than an
//! absent one.
//!
//! # No adapter here
//!
//! Same reason [`AccountDirectory`](super::AccountDirectory) has none: the real one is Postgres,
//! the test one is a double, and a double in `src/` is a fake account registry shipped inside
//! the server binary. The suite's lives in `tests/support/`.

use std::fmt;

use jiff::Timestamp;
use uuid::Uuid;

use super::directory::DirectoryFuture;
use crate::store::UserId;

/// The shortest password this server will accept.
///
/// Twelve, and it is deliberately a **length** floor with no composition rule. The password
/// authenticates a *session* and nothing else — the master key never derives from it and is
/// never visible to the credential verifier
/// ([Authentication](../../../capsule-docs/src/content/docs/design/authentication.md)) — so this
/// is ordinary sign-in security rather than key strength. Composition rules ("one digit, one
/// symbol") measurably push people towards shorter, more guessable passwords; a length floor
/// does not.
pub const MIN_PASSWORD_LENGTH: usize = 12;

/// What creating an account did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// The account is new, and this is its id.
    Created(UserId),
    /// An account already exists for that address, and nothing was written.
    ///
    /// The one disclosure this surface makes; see the module docs.
    AlreadyExists,
}

/// Bringing an account into existence.
///
/// One operation, for the reason [`AccountDirectory`](super::AccountDirectory) has one: a port
/// with a second method is a port that will have six.
pub trait AccountRegistry: fmt::Debug + Send + Sync {
    /// Create an account for `email` with `password`, under the id `user`.
    ///
    /// **The check and the write are one operation**, exactly as
    /// [`AlbumStore::provision`](crate::album::AlbumStore::provision) and
    /// [`DeviceDirectoryStore::publish`](crate::directory::DeviceDirectoryStore::publish) are: a
    /// caller that read, saw nothing, and then wrote has a window in which a second registration
    /// for the same address lands, and both would believe they own it.
    ///
    /// The id is minted **above** this port and passed in, because it is a fact about the
    /// server's clock rather than about the backend — and because an adapter that minted its own
    /// would make two adapters two different id schemes.
    ///
    /// `password` is borrowed and must not be retained, logged, or included in any error. The
    /// adapter owns hashing end to end, for the same reason the directory owns verification: a
    /// hash that crossed this boundary would put Argon2id's parameters in the routing layer.
    fn create<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
        user: &'a UserId,
        at: Timestamp,
    ) -> DirectoryFuture<'a, Registration>;
}

/// A fresh account id.
///
/// UUIDv7, per the Identifiers rule's default for a newly introduced identifier: an account id
/// is written to a database, referenced by every manifest, and read back in ranges, so index
/// locality is worth having. The v4 carve-out is for ids whose *creation time must not leak*,
/// and an account's does not — it is visible to every federated peer the moment the account
/// federates anything, and it is not a secret in any threat this design names.
#[must_use]
pub fn new_user_id() -> UserId {
    UserId::new(Uuid::now_v7().to_string())
}
