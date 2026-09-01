//! [`WriteAuthority`] — the two durable facts the envelope gate cannot answer from the
//! request.
//!
//! # Why this is a port and not a lookup
//!
//! Eleven of the fifteen invariants the gate runs are pure functions of the request
//! ([`capsule_core::validation`] owns them). Two are not:
//!
//! - **Invariant 6** — the album must exist and the owner must have write capability on it,
//!   and the album's *pinned* protocol version is what the request is compared against. The
//!   Salvo gate compared the request's `protocol_version` against **itself**
//!   (`album_pin: &request.protocol_version`), so the first write to an album self-certified
//!   its own pin; `S-C19` exists to fix exactly that. Taking the pin from this port instead
//!   makes the self-check unrepresentable — there is no value in scope to compare a request
//!   with except the album's own.
//! - **Invariant 7** — the `created_by_device` on the envelope must be a device in the
//!   uploader's published directory, and its `added_at` must precede the manifest's
//!   timestamp. The Salvo gate used the *account*-creation time as the floor and
//!   `S-C20` owns replacing it with the directory row. The port asks for the row.
//!
//! # No adapter here, deliberately
//!
//! There is no implementation in `src/`, for the reason [`crate::auth`] records for
//! [`AccountDirectory`](crate::auth::AccountDirectory): the real one is Postgres — the album
//! table (`S-C25`), its pin column (`S-C19`) and the device directory (`S-C9`/`S-C20`) — and
//! the test one is a double, which belongs in a test binary rather than inside a server
//! binary. This slice declares what the gate needs and refuses by default without it; wiring
//! the adapters is the work of the slices that own those tables.
//!
//! # Two methods, one question
//!
//! It is one port rather than two because it answers one question — *may this write happen,
//! and from when was the writing device entitled to make it* — at one moment, ahead of one
//! write. Splitting it would mean two collaborators on every handler for two halves of a
//! single decision.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use jiff::Timestamp;
use uuid::Uuid;

use crate::store::{AlbumId, OwnerId, UserId};

/// The future every authority lookup returns.
///
/// Boxed rather than `async fn` in trait position for the reason [`crate::store`] boxes its
/// own: the application context holds an `Arc<dyn WriteAuthority>`, so the server does not
/// become generic over where albums are kept.
pub type AuthorityFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AuthorityError>> + Send + 'a>>;

/// What can go wrong asking the authority.
///
/// One variant: from a route's point of view every failure here has the same shape — the
/// question was not answered, so the write must not proceed. Which backend failed and how is
/// the adapter's log line, not the route's decision.
#[derive(Debug, thiserror::Error)]
#[error("the write authority could not answer: {detail}")]
pub struct AuthorityError {
    /// The adapter's own description of the failure, for the operator's log.
    pub detail: String,
}

impl AuthorityError {
    /// An authority that could not answer, with `detail` for the log.
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Whether an owner may add a blob to an album, and under which protocol pin.
///
/// A missing album and a forbidden one are **one variant**, deliberately: the error taxonomy
/// answers both with `403 error.upload.album_access_denied`, and telling a caller which
/// applied would turn the endpoint into an oracle for album ids it cannot otherwise see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlbumWriteAccess {
    /// The album exists, the owner may write to it, and this is its immutable protocol pin.
    Writable {
        /// The album's pinned protocol version (`YYYY-MM-DD`), set when it was provisioned.
        protocol_pin: String,
        /// The upgrade ceremony the album is quiescing under, if any (`S-C24`).
        ///
        /// Carried on the *write access* rather than fetched separately, for the reason the
        /// serving path carries a hold on the blob reference: the answer has to come from the
        /// same read that decided the access, or an upgrade that began between the two is one a
        /// write slips past. `None` covers both "no ceremony" and "the ceremony's deadline has
        /// passed" — an expired quiescence aborts the upgrade, so it is indistinguishable from
        /// none by design.
        quiescing_under: Option<uuid::Uuid>,
    },
    /// The album does not exist, or the owner may not write to it.
    Denied,
}

/// The durable facts invariants 6 and 7 are decided against.
pub trait WriteAuthority: fmt::Debug + Send + Sync {
    /// Whether `owner` may add a blob to `album`, and the album's protocol pin.
    fn album_write_access<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
    ) -> AuthorityFuture<'a, AlbumWriteAccess>;

    /// When `device` was added to `user`'s published device directory, or `None` when it is
    /// not in it.
    ///
    /// `None` is a refusal, not an absence to work around: an upload whose manifest names a
    /// device the directory does not carry is invariant 7's rejecting case. An adapter for an
    /// account with no directory yet may answer with the documented account-creation floor
    /// (`S-C20`), which is a decision about *that adapter*, not about this contract.
    fn device_added_at<'a>(
        &'a self,
        user: &'a UserId,
        device: Uuid,
    ) -> AuthorityFuture<'a, Option<Timestamp>>;
}
