//! [`AuthStateStore`] — session records and the per-user session index, as one fact.
//!
//! # The bug this shape makes unrepresentable
//!
//! The Salvo `SessionStorage` exposed the record and the index as six independent operations:
//! `save_session`, `get_session`, `delete_session`, `add_user_session`, `get_user_sessions`,
//! `delete_user_sessions_key`. Opening a session called two of them; closing one called
//! exactly one. So `revoke_session` deleted the record and left the session id in
//! `capsule:user_sessions:<user>`, and `revoke_all_for_user` — which counted *index entries* —
//! told the user it had "signed out N devices" with one phantom device per prior refresh.
//!
//! It is not fixed here by remembering to delete both. It is fixed by removing the ability to
//! address them separately:
//!
//! - **No operation names the index.** There is no `add_user_session`, no `get_user_sessions`,
//!   no `delete_user_sessions_key`. The index is an adapter's internal derivative of the
//!   record set and has no lifetime, no key and no entry point of its own.
//! - **The write takes a whole [`SessionRecord`]**, which carries its own `user_id`. A caller
//!   cannot express "record without index entry", because it never supplies the index entry —
//!   the store derives it.
//! - **The removals take only a [`SessionId`]** and return the *records they removed*. The
//!   store looks the record up to learn its user, so it always has both halves. A caller
//!   cannot express "index entry without record" either.
//! - **Every read returns [`SessionRecord`] values, never ids.** An index entry whose record
//!   is gone is therefore not observable through this port, so nothing downstream can count
//!   one. [`AuthStateStore::close_all_for_user`] returns the removed records, so the number a
//!   revoke-all reports is `Vec::len()` of things that actually went away — there is no other
//!   number to report.

use std::fmt;

use jiff::{SignedDuration, Timestamp};
use uuid::Uuid;

use super::{SessionId, StoreFuture, UserId};

/// One open authentication session.
///
/// Deliberately derives no `serde`: how a session is encoded is the adapter's business (the
/// Valkey adapter's hash fields, a Postgres row's columns), not the record's. A record that
/// carried its own wire format is how the generic blob store started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// The session's own identifier — the value a refresh token names.
    pub session_id: SessionId,
    /// The account the session authenticates.
    pub user_id: UserId,
    /// When the session was opened.
    pub created_at: Timestamp,
    /// When the session was last seen. Refreshed by [`AuthStateStore::touch_session`].
    pub last_active_at: Timestamp,
    /// The `User-Agent` the opening ceremony carried, for the devices listing.
    pub user_agent: Option<String>,
    /// The address the opening ceremony came from, for the devices listing.
    pub ip_address: Option<String>,
    /// The advisory device-cohort hash asserted at session creation (slice `S-C13`).
    ///
    /// **Legibility metadata only.** No authorization path reads it — the JWT claims carry no
    /// cohort field — so it groups a physical device's re-enrollments in the devices view and
    /// nothing more.
    pub cohort_hash: Option<String>,
    /// The directory device the client claimed to be (slice `S-N3`).
    ///
    /// A *different* identifier space from [`Self::cohort_hash`]: the cohort groups
    /// re-enrollments of one physical device, this names one directory device. Typed as a
    /// [`Uuid`] rather than a string so the normalization the Salvo tree did by hand at every
    /// call site — parse, reject the nil uuid, re-render lowercase-hyphenated — happens once,
    /// above this port. It is client-asserted and unverified, so it must never gate anything.
    pub device_id: Option<Uuid>,
}

/// The default lifetime an open session is stored under.
///
/// A deployment may configure a different one when it constructs its adapter; what it may not
/// do is vary it per call, which is why no operation here takes a TTL.
pub const DEFAULT_SESSION_TTL: SignedDuration = SignedDuration::from_hours(24);

/// Session state: the records, and the per-user view of them.
///
/// See the module docs for why this port has six operations and not the Salvo trait's twelve,
/// and for why none of them mentions an index.
pub trait AuthStateStore: std::fmt::Debug + Send + Sync {
    /// How long an open session lives, from the moment it is opened.
    ///
    /// Absolute, not sliding: [`Self::touch_session`] records activity and deliberately does
    /// **not** extend the window. A sliding lifetime would make a session's life a function of
    /// its traffic — a caller-supplied TTL wearing a different hat.
    fn ttl(&self) -> SignedDuration;

    /// Open `record`'s session, making it visible to
    /// [`Self::read_session`] **and** to [`Self::sessions_for_user`] in one step.
    ///
    /// Re-opening an id that is already live replaces the record and does not duplicate the
    /// user's listing — an id is one session, however many times it is written.
    fn open_session(&self, record: SessionRecord) -> StoreFuture<'_, ()>;

    /// The live session `session`, or `None` if it never existed, was closed, or expired.
    fn read_session<'a>(&'a self, session: &'a SessionId)
    -> StoreFuture<'a, Option<SessionRecord>>;

    /// Refresh a live session's `last_active_at`, returning the updated record.
    ///
    /// `None` means there was no live session to refresh — which is the whole answer, since a
    /// refreshed session's TTL is a property of the store and not something the caller extends.
    fn touch_session<'a>(
        &'a self,
        session: &'a SessionId,
        last_active_at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>>;

    /// Close one session, returning the record that was removed.
    ///
    /// `None` means nothing was removed. There is no variant of this that removes the record
    /// but not the user's view of it: the store reads the record to learn its user, so both
    /// halves go together or neither does.
    fn close_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>>;

    /// Every live session for `user`, oldest first, ties broken by session id.
    ///
    /// The order is part of the contract, not an accident of the backend: the devices listing
    /// is a user-visible surface, and a set-backed adapter (Valkey `SMEMBERS`) must sort to
    /// conform rather than serve whatever order it happens to hold.
    fn sessions_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>>;

    /// Close every session for `user`, returning the records actually removed, oldest first.
    ///
    /// This is the revoke-all ceremony, and the returned length is the number a client is told.
    /// Global by construction: the caller's own session is among `user`'s sessions, so a
    /// revoke-all signs the caller out too (slice `S-C23`) — the point of the ceremony, not an
    /// oversight. Running it twice returns an empty second time; there is no residue to count.
    fn close_all_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>>;
}

// -------------------------------------------------------------------------------------------
// Device cohorts
// -------------------------------------------------------------------------------------------

/// One physical device's history with an account (slice `S-C13`).
///
/// The durable half of the cohort story. A session store forgets a cohort exactly when the
/// "have I seen this device before?" question becomes worth asking — a user reinstalls, gets a
/// new `device_id` by design, and the sessions that carried the old one have long expired. So
/// the map outlives sessions, and `first_seen` is what lets a client say *"a device you've used
/// before (last seen March)"* rather than presenting a stranger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortRecord {
    /// The account. Folded into the hash itself, so the same physical device under two accounts
    /// yields unlinkable values and this field is a scoping key rather than a correlation one.
    pub user_id: UserId,
    /// The advisory hash the client asserted.
    pub cohort_hash: String,
    /// The first time this account saw it.
    pub first_seen: Timestamp,
    /// The most recent time.
    pub last_seen: Timestamp,
}

/// The durable `device_cohorts(user_id, cohort_hash, first_seen, last_seen)` map.
///
/// **Advisory storage, structurally.** Nothing here is read by an authorization path, and the
/// port offers no lookup that could tempt one: there is no "is this cohort trusted", no
/// per-cohort flag, and no way to ask about a cohort across accounts. A client asserts the value
/// and the server records it; that is the whole contract.
pub trait CohortStore: fmt::Debug + Send + Sync {
    /// Record that `user` was seen under `cohort_hash` at `at`.
    ///
    /// Sets `first_seen` on the first sighting and moves `last_seen` on every one. Idempotent in
    /// the sense that matters: seeing the same cohort twice is one row, not two.
    fn observe<'a>(
        &'a self,
        user: &'a UserId,
        cohort_hash: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, CohortRecord>;

    /// Every cohort `user` has ever been seen under, oldest first seen first.
    ///
    /// The order is part of the contract for the same reason the session listing's is: this is a
    /// user-visible surface, and a set-backed adapter must sort to conform rather than the suite
    /// being loosened to accept whatever order it holds.
    fn cohorts_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<CohortRecord>>;
}
