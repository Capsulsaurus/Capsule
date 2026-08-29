//! The upload module: the envelope gate, the session lifecycle, and the collaborators both
//! need (slice `S-C1`).
//!
//! One cohesive module, per design/module-map.md's "Planned Server Modules" — the operations
//! themselves live in [`crate::routes::upload`], because a route is a description of a surface
//! and this is the machinery behind it. The same split [`crate::auth`] uses.
//!
//! # What it owns, and what it borrows
//!
//! | Concern | Lives | Why |
//! | --- | --- | --- |
//! | Session records, chunk replay, the finalize claim | [`crate::store::UploadSessionStore`] (`S-C29`) | landed already; consumed, not re-declared |
//! | Staged bytes and the content-addressed store | [`crate::blob::BlobStore`] (`S-C35`) | landed already; the **only** path to bytes |
//! | The keyless invariants | [`capsule_core::validation`] | shared with the client's own `verify_asset`; a second implementation would be a second answer |
//! | Album capability, album pin, device floor | [`WriteAuthority`] | the two facts the request cannot carry |
//! | The tunable half of the contract | [`UploadPolicy`] | protocol window, ceiling, closed content-type enum, drift |
//!
//! # The three things this port refuses to do
//!
//! - **It never produces manifest bytes.** The signed manifest arrives as a `provenance` blob
//!   and is stored verbatim (`S-C30`); [`envelope::ManifestEnvelope`] is the server's
//!   *projection* for validation and is never re-encoded into anything a client is handed.
//! - **It never touches the filesystem.** Staging, appending, verifying and committing all go
//!   through the blob port, which is why finalization reads the stage back through
//!   [`crate::blob::BlobStore::read_staged_at`] rather than opening a file.
//! - **It never splits chunk accounting.** Accepting a chunk is one call —
//!   [`crate::store::UploadSessionStore::record_progress`] — because the byte counter, the progress clock and the replay entry describe one event.
//!   The Salvo server issued three writes and a crash between them left all three disagreeing.
//!
//! # What this port does not carry
//!
//! There is **no durable asset index** in this crate yet, and three parts of the Salvo surface
//! rest on one:
//!
//! - the **pending asset row** a session reserves at creation, and the `uploaded` flip at
//!   finalization;
//! - the `409 error.upload.duplicate_blob` half of create-dedup, which must name the existing
//!   asset (`S-C22`'s structured `existing_asset` field) and cannot be answered from blob
//!   presence alone without leaking another account's holdings;
//! - the durable half of the visibility gate — [`visibility`] carries the *definition*, and
//!   the row it would flip does not exist here.
//!
//! Quota (`S-C6`), the sync feed (`S-C2`) and custody receipts (`S-C15`) are likewise absent
//! and owned elsewhere. Each is reported as a gap rather than approximated.

pub mod authority;
pub mod body;
pub mod chunk;
pub mod envelope;
pub mod finalize;
pub mod policy;
pub mod visibility;

use std::sync::Arc;

pub use self::authority::{AlbumWriteAccess, AuthorityError, AuthorityFuture, WriteAuthority};
pub use self::envelope::{DeclaredBlob, GateContext, GateReject, ManifestEnvelope};
pub use self::policy::UploadPolicy;
use crate::blob::BlobStore;
use crate::store::{Clock, UploadSessionStore};

/// Everything the upload operations reach for, as one injectable value.
///
/// `Clone` is cheap and required — a Kynos provider hands a value out per request — and every
/// field is an `Arc`, so cloning shares the one store, the one blob store and the one policy
/// the process was built with.
#[derive(Debug, Clone)]
pub struct UploadContext {
    sessions: Arc<dyn UploadSessionStore>,
    blobs: Arc<dyn BlobStore>,
    authority: Arc<dyn WriteAuthority>,
    clock: Arc<dyn Clock>,
    policy: Arc<UploadPolicy>,
}

impl UploadContext {
    /// Assembles the module from its collaborators.
    ///
    /// `clock` is passed in rather than read off one of the stores for the reason
    /// [`crate::auth::AuthContext`] records: a session's `created_at`, its progress clock and
    /// the drift bound the envelope is judged against must be the *same* instant source, and
    /// handing it in once makes that a fact about construction rather than a convention.
    pub fn new(
        sessions: Arc<dyn UploadSessionStore>,
        blobs: Arc<dyn BlobStore>,
        authority: Arc<dyn WriteAuthority>,
        clock: Arc<dyn Clock>,
        policy: UploadPolicy,
    ) -> Self {
        Self {
            sessions,
            blobs,
            authority,
            clock,
            policy: Arc::new(policy),
        }
    }

    /// The upload-session store (`S-C29`).
    pub fn sessions(&self) -> &dyn UploadSessionStore {
        self.sessions.as_ref()
    }

    /// The blob store (`S-C35`) — the only path to bytes.
    pub fn blobs(&self) -> &dyn BlobStore {
        self.blobs.as_ref()
    }

    /// The album and device authority invariants 6 and 7 are decided against.
    pub fn authority(&self) -> &dyn WriteAuthority {
        self.authority.as_ref()
    }

    /// The clock every record, deadline and drift bound is stamped from.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// The tunable half of the contract.
    pub fn policy(&self) -> &UploadPolicy {
        &self.policy
    }
}
