//! The typed ceremony stores that replace `save_temp_data<T>` / `get_temp_data<T>` /
//! `delete_temp_data`.
//!
//! There were four. Passkeys are not in v1, so the WebAuthn ceremony store is gone — a port
//! nothing consumes, with an adapter and a conformance suite behind it, reads as live and is
//! not. Its contract is recoverable from the commit that removed it.
//!
//! # What was there
//!
//! One generic serialize-anything key-value store with a caller-supplied TTL, carrying four
//! unrelated typed things, namespaced by hand-formatted string prefixes:
//!
//! | key prefix | record | TTL passed at every call site |
//! |---|---|---|
//! | `revoke_all:challenge:{token}` | the revoke-all challenge | `CHALLENGE_TTL` |
//! | `enroll:code:{code}` | the pending enrollment, written twice under two spellings | `CODE_TTL` |
//! | `enroll:channel:{id}`, `enroll:mbox:{id}:{a\|b}` | the relay channel and its queues | `CHANNEL_TTL` |
//!
//! Three things went wrong with that, and each is closed by construction below.
//!
//! - **Type safety ended at the boundary.** `get_temp_data::<RelayChannel>` against a key
//!   written as a `PendingEnrollment` compiled fine and failed at runtime as a deserialization
//!   error. Here each ceremony's store names its own record type, so the confusion has no way to
//!   be written.
//! - **Collisions were prevented by convention.** A prefix typo put two ceremonies in one
//!   namespace. Here the key space is the store's, and a store holds exactly one kind of thing.
//! - **A record's lifetime was an argument.** Every call site restated the TTL, and the
//!   retired WebAuthn one restated it as a bare literal in two routes. Here `ttl()` is a
//!   property of the store, fixed at construction, and no operation accepts one.
//!
//! # Single-use is also a property, not a convention
//!
//! Three of these four ceremonies are one-shot. The generic store made that the caller's job:
//! `get_temp_data` then `delete_temp_data`, two calls, and a route that forgot the second left
//! a replayable credential. Here the read *is* the removal — [`ChallengeStore::consume`],
//! [`EnrollmentStore::redeem`] and [`OidcAuthorizationStore::consume`] have no non-destructive
//! counterpart, so a replay window cannot be left open by omission.

use jiff::{SignedDuration, Timestamp};

use super::{
    ChallengeToken, ChannelId, DeviceCode, EnrollmentCode, OidcDeviceHandle, OidcNonce, OidcState,
    PkceVerifier, StoreFuture, UserId,
};

// -------------------------------------------------------------------------------------------
// Revoke-all challenge
// -------------------------------------------------------------------------------------------

/// The account a live revoke-all challenge authorizes a global sign-out for.
///
/// No `expires_at` field. The Salvo record carried one *in addition* to the storage TTL, so
/// expiry was enforced twice, in two places, from two clocks — the belt-and-braces the generic
/// store needed because its in-memory double ignored TTL outright. Expiry is the store's job
/// here, and the store's double honours it, so the record carries only the fact it exists to
/// carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeAllChallenge {
    /// The account this challenge authorizes a global revoke for.
    pub user_id: UserId,
    /// When it was issued. The route renders `issued_at + ttl()` as the published expiry.
    pub issued_at: Timestamp,
}

/// How long an issued revoke-all challenge lives.
pub const CHALLENGE_TTL: SignedDuration = SignedDuration::from_mins(5);

/// Single-use challenges for the revoke-all ceremony.
pub trait ChallengeStore: std::fmt::Debug + Send + Sync {
    /// How long an issued challenge lives. A property of the ceremony, not of a call.
    fn ttl(&self) -> SignedDuration;

    /// Record a freshly issued challenge.
    fn issue<'a>(
        &'a self,
        token: &'a ChallengeToken,
        record: RevokeAllChallenge,
    ) -> StoreFuture<'a, ()>;

    /// Burn `token` and return what it authorized, or `None` if it is unknown, already
    /// consumed, or expired.
    ///
    /// Destructive on **every** attempt, successful or not: that is what stops an attacker
    /// grinding signatures against a live challenge, and it costs a legitimate user one extra
    /// request. There is deliberately no way to read a challenge without burning it.
    fn consume<'a>(
        &'a self,
        token: &'a ChallengeToken,
    ) -> StoreFuture<'a, Option<RevokeAllChallenge>>;
}

// -------------------------------------------------------------------------------------------
// Device enrollment
// -------------------------------------------------------------------------------------------

/// A pending device enrollment, redeemable under either of its two spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEnrollment {
    /// The issuing device-A account — the initiator the relay channel will bind to.
    pub user_id: UserId,
    /// The full-entropy code the QR payload carries.
    pub code: EnrollmentCode,
    /// The shorter transcribable numeric fallback.
    pub text_fallback: EnrollmentCode,
    /// When it was issued. The route renders `issued_at + ttl()` as the published expiry.
    pub issued_at: Timestamp,
}

/// How long an issued device-enrollment code lives.
pub const ENROLLMENT_CODE_TTL: SignedDuration = SignedDuration::from_mins(10);

/// Pending device-enrollment codes.
pub trait EnrollmentStore: std::fmt::Debug + Send + Sync {
    /// How long an issued code lives.
    fn ttl(&self) -> SignedDuration;

    /// Record a freshly issued enrollment under **both** its spellings, as one fact.
    ///
    /// The Salvo code did this with two `save_temp_data` calls and undid it with two
    /// `delete_temp_data` calls, so a failure between them left one spelling redeemable and
    /// the other not. Here the record carries both spellings and the store registers both, so
    /// a half-registered enrollment cannot be written and a half-burned one cannot be left.
    fn issue(&self, record: PendingEnrollment) -> StoreFuture<'_, ()>;

    /// Whether `code` currently names a live enrollment — the generator's collision check.
    ///
    /// This is the one non-destructive read here, and it deliberately returns a `bool` rather
    /// than the record: a code generator needs to know an id is taken, and must not be handed
    /// someone else's pending enrollment to learn it.
    fn is_taken<'a>(&'a self, code: &'a EnrollmentCode) -> StoreFuture<'a, bool>;

    /// Redeem by either spelling, burning both. `None` if unknown, already redeemed, or
    /// expired.
    fn redeem<'a>(&'a self, code: &'a EnrollmentCode)
    -> StoreFuture<'a, Option<PendingEnrollment>>;
}

// -------------------------------------------------------------------------------------------
// Enrollment relay channel
// -------------------------------------------------------------------------------------------

/// Which device a relayed payload is travelling toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    /// Toward device A, the initiator.
    ToInitiator,
    /// Toward device B, the enrollee.
    ToEnrollee,
}

impl Direction {
    /// The wire token (`"a"` toward the initiator, `"b"` toward the enrollee).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToInitiator => "a",
            Self::ToEnrollee => "b",
        }
    }

    /// Parse the wire token.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "a" => Some(Self::ToInitiator),
            "b" => Some(Self::ToEnrollee),
            _ => None,
        }
    }
}

/// One opaque relay payload.
///
/// The ceremony transcript — ephemeral-ECDH messages, the wrapped master key, device B's key
/// bundle — is end-to-end encrypted between the two devices. The server relays bytes and
/// never decodes them, so this is a newtype over the wire form and not a modelled message.
/// That opacity is the security property, not a shortcut: a payload the server could parse is
/// a payload it could tamper with meaningfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayPayload(String);

impl RelayPayload {
    /// Wrap a payload the transport already length-checked.
    pub fn new(payload: impl Into<String>) -> Self {
        Self(payload.into())
    }

    /// The payload's bytes, to hand back to the other device verbatim.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The payload's length, for the bound the route enforces and for tracing.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The live relay channel opened when an enrollment code is redeemed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayChannel {
    /// The issuing device-A account the channel is bound to.
    pub initiator_user_id: UserId,
    /// When the channel opened. The ceremony window is `opened_at + ttl()`.
    pub opened_at: Timestamp,
}

/// What happened to a payload handed to the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayOutcome {
    /// Appended. `depth` is the mailbox depth after the append, for the caller's tracing.
    Enqueued {
        /// The mailbox depth after this payload.
        depth: usize,
    },
    /// The channel is unknown, closed, or past its window. Nothing was stored.
    NoChannel,
}

/// What a drain found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainOutcome {
    /// The mailbox's pending payloads in arrival order, removed by this call. Possibly empty.
    Drained(Vec<RelayPayload>),
    /// The channel is unknown, closed, or past its window.
    NoChannel,
}

/// How long a relay channel — the ceremony window that opens on redemption — lives.
pub const RELAY_CHANNEL_TTL: SignedDuration = SignedDuration::from_mins(10);

/// The device-enrollment relay: a channel and its two directional mailboxes.
pub trait ChannelStore: std::fmt::Debug + Send + Sync {
    /// How long a channel — and therefore its mailboxes — lives.
    fn ttl(&self) -> SignedDuration;

    /// Open a channel bound to its initiator.
    fn open<'a>(&'a self, channel: &'a ChannelId, record: RelayChannel) -> StoreFuture<'a, ()>;

    /// The live channel, or `None`. Non-destructive: the routes need to authorize the
    /// initiator against the channel without ending the ceremony.
    fn lookup<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, Option<RelayChannel>>;

    /// Append `payload` to the `direction` mailbox of a live channel.
    ///
    /// One operation, on purpose. Salvo relayed by reading the whole queue, pushing, and
    /// writing it back — a read-modify-write that loses a payload whenever two devices post at
    /// once — after a *separate* liveness check that could pass an instant before the channel
    /// expired. This appends and checks liveness in the same operation, so neither race exists.
    fn enqueue<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
        payload: RelayPayload,
    ) -> StoreFuture<'a, RelayOutcome>;

    /// Take everything pending in the `direction` mailbox of a live channel.
    ///
    /// Destructive: a relayed payload is delivered once. Draining one direction leaves the
    /// other untouched.
    fn drain<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
    ) -> StoreFuture<'a, DrainOutcome>;

    /// Close a channel and drop **both** mailboxes with it, returning whether one was live.
    ///
    /// The queues have no lifetime of their own — they are the channel's, the same way a
    /// session's index entry is the session's.
    fn close<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, bool>;
}

// -------------------------------------------------------------------------------------------
// OIDC authorization
// -------------------------------------------------------------------------------------------

/// What the server holds between the two legs of an OIDC authorization-code ceremony.
///
/// Keyed by the [`OidcState`] the client carries to the identity provider and back. Everything
/// here exists to be checked **once**, at the callback: the nonce against the ID token, the
/// verifier against the token endpoint, the redirect URI byte-for-byte against the one the
/// authorization request named (RFC 6749 §4.1.3 requires the two to be identical), and the
/// client binding against the secret the callback presents.
///
/// No `expires_at` field, for the reason [`RevokeAllChallenge`] has none: expiry is the store's,
/// and the route publishes `issued_at + ttl()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAuthorization {
    /// The nonce the authorization request carried; the ID token must echo it.
    pub nonce: OidcNonce,
    /// The PKCE verifier whose S256 challenge the authorization request carried.
    pub verifier: PkceVerifier,
    /// The redirect URI the authorization request named, replayed verbatim to the token
    /// endpoint. Client-supplied and allow-listed by the route before it is stored here.
    pub redirect_uri: String,
    /// The S256 hash of the secret the *initiating client* holds (issue #517).
    ///
    /// The PKCE verifier above never leaves this server, so it binds the code to this server and
    /// not to the client that began the ceremony: anyone holding a `state` and its `code` could
    /// redeem them. This is the binding that closes that — the client keeps the secret, sends only
    /// its hash at the authorize, and the callback is refused unless it presents the secret. Not a
    /// secret itself: a hash of 256 random bits reveals nothing usable.
    pub binding_challenge: String,
    /// When the ceremony began. The route renders `issued_at + ttl()` as the published expiry.
    pub issued_at: Timestamp,
}

/// How long a begun OIDC authorization waits for its callback.
///
/// Ten minutes: long enough for a person to sign in at an identity provider that asks for a
/// second factor of its own, short enough that a state captured from a URL bar is not worth
/// keeping. The same figure as [`ENROLLMENT_CODE_TTL`], which bounds the same kind of thing —
/// a human completing a ceremony on another surface.
pub const OIDC_AUTHORIZATION_TTL: SignedDuration = SignedDuration::from_mins(10);

/// Pending OIDC authorizations, keyed by `state`.
///
/// A ceremony store and not a field on [`AuthStateStore`](super::AuthStateStore), which owns
/// durable session records and the record-plus-index atomicity its conformance suite is built
/// around. A pending authorization is a single-use, short-window ceremony credential — exactly
/// the shape this module exists for.
pub trait OidcAuthorizationStore: std::fmt::Debug + Send + Sync {
    /// How long a begun authorization waits for its callback. A property of the ceremony.
    fn ttl(&self) -> SignedDuration;

    /// Record a freshly begun authorization under its `state`.
    fn begin<'a>(
        &'a self,
        state: &'a OidcState,
        record: PendingAuthorization,
    ) -> StoreFuture<'a, ()>;

    /// Burn `state` and return what it holds, or `None` if it is unknown, already consumed, or
    /// expired.
    ///
    /// Destructive on **every** attempt, like [`ChallengeStore::consume`]: that is what makes a
    /// replayed `state` — and therefore a replayed authorization code arriving on a stolen
    /// redirect — unrepeatable, and it is why the nonce can never be checked twice. Two callbacks
    /// racing the same `state` resolve here: one gets the record, the other gets `None`.
    fn consume<'a>(&'a self, state: &'a OidcState)
    -> StoreFuture<'a, Option<PendingAuthorization>>;
}

// -------------------------------------------------------------------------------------------
// OIDC device authorization (RFC 8628, issue #635)
// -------------------------------------------------------------------------------------------

/// What the server holds while a person approves a device authorization on another device.
///
/// Keyed by the [`OidcDeviceHandle`] the client polls with. Unlike [`PendingAuthorization`] this
/// is read many times — once per poll — so it also carries the interval the server enforces.
/// *When* it was last polled is the store's, not the record's: the store stamps it from its own
/// clock at the begin and at every claim, so no caller can hand it a schedule.
///
/// No `expires_at` field, for the reason [`RevokeAllChallenge`] has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDeviceAuthorization {
    /// The provider's device code, redeemed at its token endpoint. Never sent to the client.
    pub device_code: DeviceCode,
    /// The S256 hash of the secret the initiating client holds, as on
    /// [`PendingAuthorization::binding_challenge`]: every poll must present the secret.
    pub binding_challenge: String,
    /// How long the client must wait between polls. Starts at the provider's interval and is
    /// raised by [`DEVICE_SLOW_DOWN_STEP`] on every `slow_down`, whoever answered it.
    pub interval: SignedDuration,
    /// When the ceremony began. The route renders the published expiry from it.
    pub issued_at: Timestamp,
}

/// How long a begun device authorization waits for its person.
///
/// Fifteen minutes: the device flow asks a person to pick up another device, open an address and
/// type a code before they sign in, which takes longer than following a redirect. Providers issue
/// device codes for five (dex) to thirty minutes; the route publishes whichever of the provider's
/// `expires_in` and this is sooner, so this is a ceiling on how long the server holds one.
pub const OIDC_DEVICE_TTL: SignedDuration = SignedDuration::from_mins(15);

/// How much a `slow_down` raises a ceremony's interval by (RFC 8628 §3.5: "increased by 5
/// seconds for this and all subsequent requests").
///
/// The client adds the same amount on every `slow_down` it is answered, so it and the server
/// stay in step without the new interval ever being sent.
pub const DEVICE_SLOW_DOWN_STEP: SignedDuration = SignedDuration::from_secs(5);

/// The shortest interval a device ceremony is ever polled at.
///
/// Five seconds, RFC 8628 §3.2's default when the provider names none. A provider naming less is
/// raised to it: a client polling slower than the provider allows costs nothing, and the server
/// is the one relaying every poll.
pub const DEVICE_POLL_FLOOR: SignedDuration = SignedDuration::from_secs(5);

/// What claiming a poll slot found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePollSlot {
    /// At least one interval has passed since the last poll (or the begin): the provider may be
    /// asked. This claim is now the last poll.
    Due(PendingDeviceAuthorization),
    /// The client polled sooner than one interval after the last poll (or the begin). The
    /// provider must not be asked; the interval was raised by [`DEVICE_SLOW_DOWN_STEP`], and
    /// this claim is now the last poll, so a client that keeps polling early keeps being slowed.
    TooSoon {
        /// The raised interval.
        interval: SignedDuration,
    },
    /// No live ceremony has this handle: unknown, finished, or expired.
    Unknown,
}

/// Pending OIDC device authorizations, keyed by handle (issue #635).
///
/// Beside [`OidcAuthorizationStore`] rather than a second record type in it: a pending device
/// authorization is polled, so it needs a non-destructive read with a schedule, which the
/// authorization store exists to not have.
pub trait OidcDeviceStore: std::fmt::Debug + Send + Sync {
    /// How long a begun device authorization lives. A property of the ceremony.
    fn ttl(&self) -> SignedDuration;

    /// Record a freshly begun device authorization under its handle. The begin counts as the
    /// last poll, so the first claim is due one interval after it (RFC 8628 §3.4).
    fn begin<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
        record: PendingDeviceAuthorization,
    ) -> StoreFuture<'a, ()>;

    /// Claim the next poll for `handle`, atomically.
    ///
    /// One operation, on purpose, for the reason [`ChannelStore::enqueue`] is one: a separate
    /// read of `polled_at` and write of the new one would let two polls racing one handle both
    /// see the slot free and both reach the provider. Here exactly one claims a slot; the other
    /// is [`DevicePollSlot::TooSoon`].
    fn claim<'a>(&'a self, handle: &'a OidcDeviceHandle) -> StoreFuture<'a, DevicePollSlot>;

    /// Raise `handle`'s interval by [`DEVICE_SLOW_DOWN_STEP`] because the provider said
    /// `slow_down`, returning the raised interval, or `None` if no live ceremony has the handle.
    fn slow_down<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
    ) -> StoreFuture<'a, Option<SignedDuration>>;

    /// End the ceremony and return what it held, or `None` if it was unknown, already finished,
    /// or expired.
    ///
    /// Destructive, like [`OidcAuthorizationStore::consume`]: the route calls it on every
    /// terminal answer, and on success *before* it opens a session, so two polls collecting one
    /// approval resolve here to one session.
    fn finish<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
    ) -> StoreFuture<'a, Option<PendingDeviceAuthorization>>;
}

// -------------------------------------------------------------------------------------------
// WebAuthn ceremonies
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_tokens_round_trip() {
        for direction in [Direction::ToInitiator, Direction::ToEnrollee] {
            assert_eq!(Direction::parse(direction.as_str()), Some(direction));
        }
        assert_eq!(Direction::parse("c"), None);
    }

    #[test]
    fn a_relay_payload_is_carried_verbatim() {
        let payload = RelayPayload::new("opaque\u{0}bytes");
        assert_eq!(payload.as_str(), "opaque\u{0}bytes");
        assert_eq!(payload.len(), 12);
        assert!(!payload.is_empty());
        assert!(RelayPayload::new("").is_empty());
    }
}
