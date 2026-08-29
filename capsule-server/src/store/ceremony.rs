//! The four typed ceremony stores that replace `save_temp_data<T>` / `get_temp_data<T>` /
//! `delete_temp_data`.
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
//! | `passkey_reg:{id}`, `passkey_auth:{id}` | WebAuthn ceremony state | an inline `Duration::from_secs(300)` |
//!
//! Three things went wrong with that, and each is closed by construction below.
//!
//! - **Type safety ended at the boundary.** `get_temp_data::<PasskeyAuthentication>` against a
//!   key written as `PasskeyRegistration` compiled fine and failed at runtime as a
//!   deserialization error. Here each ceremony's store names its own record type, and a
//!   registration and an authentication are different types even though they share an id
//!   space — so the confusion has no way to be written.
//! - **Collisions were prevented by convention.** A prefix typo put two ceremonies in one
//!   namespace. Here the key space is the store's, and a store holds exactly one kind of thing.
//! - **A record's lifetime was an argument.** Every call site restated the TTL, and the
//!   WebAuthn one restated it as a bare literal in two routes. Here `ttl()` is a property of
//!   the store, fixed at construction, and no operation accepts one.
//!
//! # Single-use is also a property, not a convention
//!
//! Three of these four ceremonies are one-shot. The generic store made that the caller's job:
//! `get_temp_data` then `delete_temp_data`, two calls, and a route that forgot the second left
//! a replayable credential. Here the read *is* the removal — [`ChallengeStore::consume`],
//! [`EnrollmentStore::redeem`], [`WebauthnCeremonyStore::finish_registration`] and
//! [`WebauthnCeremonyStore::finish_authentication`] have no non-destructive counterpart, so a
//! replay window cannot be left open by omission.

use jiff::{SignedDuration, Timestamp};

use super::{CeremonyId, ChallengeToken, ChannelId, EnrollmentCode, StoreFuture, UserId};

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
// WebAuthn ceremonies
// -------------------------------------------------------------------------------------------

/// In-flight state for one WebAuthn ceremony, as the WebAuthn library serializes it.
///
/// Opaque here by necessity: the state belongs to `webauthn-rs`, whose representation is that
/// crate's business and changes with it. This is a *named* type on a *named* operation, which
/// is what the contract requires — the store cannot be handed anything else, and cannot hand
/// this to a route expecting something else — as distinct from the generic `T: Serialize` it
/// replaces, which could carry anything anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeremonyState(String);

impl CeremonyState {
    /// Wrap the WebAuthn library's serialized ceremony state.
    pub fn new(state: impl Into<String>) -> Self {
        Self(state.into())
    }

    /// The serialized state, to hand back to the WebAuthn library.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A passkey **registration** ceremony awaiting its finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationCeremony {
    /// The account registering a passkey. Bound here so the finish route cannot be told which
    /// account it is completing by the request it is completing it with.
    pub user_id: UserId,
    /// The library's in-flight registration state.
    pub state: CeremonyState,
}

/// A passkey **authentication** ceremony awaiting its finish.
///
/// A separate type from [`RegistrationCeremony`] with a separate pair of operations, so the
/// `passkey_reg:` / `passkey_auth:` prefix convention has nothing left to enforce: a
/// registration cannot be finished as an authentication even under the same ceremony id.
/// It carries no `user_id` because a discoverable-credential login learns the account *from*
/// the credential — recording an expected account here would be inventing an authorization
/// input the ceremony does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationCeremony {
    /// The library's in-flight authentication state.
    pub state: CeremonyState,
}

/// How long a started WebAuthn ceremony may take to finish.
///
/// Five minutes, the value the Salvo passkey routes restated as a bare
/// `Duration::from_secs(300)` literal at each of their two start handlers. Declared once here
/// because it is a property of the ceremony, not of a route.
pub const WEBAUTHN_CEREMONY_TTL: SignedDuration = SignedDuration::from_mins(5);

/// WebAuthn ceremony state for the passkey routes.
pub trait WebauthnCeremonyStore: std::fmt::Debug + Send + Sync {
    /// How long a started ceremony may take to finish.
    fn ttl(&self) -> SignedDuration;

    /// Record a started registration ceremony under `ceremony`.
    fn begin_registration<'a>(
        &'a self,
        ceremony: &'a CeremonyId,
        record: RegistrationCeremony,
    ) -> StoreFuture<'a, ()>;

    /// Take the registration ceremony `ceremony`, or `None` if it is unknown, already
    /// finished, expired — or is an *authentication* ceremony under that id.
    fn finish_registration<'a>(
        &'a self,
        ceremony: &'a CeremonyId,
    ) -> StoreFuture<'a, Option<RegistrationCeremony>>;

    /// Record a started authentication ceremony under `ceremony`.
    fn begin_authentication<'a>(
        &'a self,
        ceremony: &'a CeremonyId,
        record: AuthenticationCeremony,
    ) -> StoreFuture<'a, ()>;

    /// Take the authentication ceremony `ceremony`, or `None` if it is unknown, already
    /// finished, expired — or is a *registration* ceremony under that id.
    fn finish_authentication<'a>(
        &'a self,
        ceremony: &'a CeremonyId,
    ) -> StoreFuture<'a, Option<AuthenticationCeremony>>;
}

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
