//! Device-enrollment codes + relay channel — the short-lived Valkey state behind the
//! cross-device-add ceremony (slice `S-C7`; SSoT: <https://docs/design/device-enrollment/>).
//!
//! An existing signed-in device **issues** a single-use [`IssuedCode`]; the new device
//! **redeems** it to open an opaque relay [`channel`](relay_send). Everything here is
//! ephemeral state that fits Valkey exactly (like sessions and rate limits): each code is
//! **single-use, valid 10 minutes, rate-limited, and deleted on redemption *and* on
//! expiry**. Two expiry guards run together: the storage TTL evicts an abandoned code, and
//! an explicit `expires_at` inside the record lets [`redeem`] deterministically refuse *and
//! delete* an expired code even before the TTL fires — so expiry is testable without sleeps.
//!
//! The **relay channel** carries opaque ceremony messages (the ephemeral-ECDH transcript,
//! the wrapped master key, B's key bundle for signing) between the two devices. The crypto
//! rides the messages opaquely: the server stores every payload verbatim and never inspects
//! it — it just relays, authorized by possession of the opaque channel handle.
//!
//! The **directory update** that lands B's entry is *not* implemented here: device A
//! completes the ceremony by re-publishing its signed [`DeviceDirectory`] with B's new entry
//! through the existing S-C9 `POST /devices/directory` monotonic publish path — S-C7 reuses
//! that surface rather than forking a second directory writer.
//!
//! Identifiers mirror the share-link opaque-id discipline: every code and channel handle is a
//! fresh OS-CSPRNG draw, non-structured and non-sequential — never a UUIDv7 or any
//! time-ordered id.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use model::errors::InternalServerError;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::session::SessionManager;

/// Enrollment-code lifetime: single-use, valid 10 minutes (doc step 1).
pub const CODE_TTL: Duration = Duration::from_mins(10);
/// Relay-channel lifetime — the ceremony window that opens on redemption.
pub const CHANNEL_TTL: Duration = Duration::from_mins(10);
/// Fresh-auth window for issuance: the server-visible proxy for the doc's *fresh local
/// device authorization*. The access token must have been minted within this window, so a
/// stolen **stale** token cannot start an add — a valid session token alone is not enough.
pub const LOCAL_AUTH_FRESHNESS: Duration = Duration::from_mins(2);
/// Per-user enrollment-code issuance budget within [`CODE_TTL`].
pub const MAX_ISSUE_PER_WINDOW: i64 = 5;
/// Per-source redemption budget within [`REDEEM_WINDOW`] (anti-brute-force; a refusal from
/// exceeding it is indistinguishable from any other refusal).
pub const MAX_REDEEM_PER_WINDOW: i64 = 10;
/// Redemption rate-limit window.
pub const REDEEM_WINDOW: Duration = Duration::from_secs(60);
/// Upper bound on a single relayed opaque payload (base64 chars). The ceremony messages
/// (ephemeral DH, a wrapped 32-byte master key, a device key bundle) are small; anything
/// larger is a structural reject before buffering.
pub const MAX_RELAY_PAYLOAD_LEN: usize = 16 * 1024;

/// Full-entropy code: 32 CSPRNG bytes (256 bits) → URL-safe base64, no padding. Far above
/// the doc's ≥64-bit floor for the QR payload.
const FULL_CODE_BYTES: usize = 32;
/// Opaque relay-channel handle: 32 CSPRNG bytes, same discipline as the code.
const CHANNEL_ID_BYTES: usize = 32;
/// Numeric text fallback length (doc: a friendly 8–10-digit transcribable form). Safe only
/// because redemption is single-use, expiring, and rate-limited, and channel integrity never
/// rests on the code.
const FALLBACK_DIGITS: usize = 10;

fn now_secs() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// Draw `n` CSPRNG bytes as a URL-safe, unpadded base64 string.
fn random_b64(n: usize) -> String {
    let mut buf = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut buf)
        .expect("OS CSPRNG must be available");
    URL_SAFE_NO_PAD.encode(buf)
}

/// Draw an `n`-digit numeric string from the CSPRNG. The per-byte modulo-10 reduction is
/// marginally biased, which is immaterial: the fallback is rate-limited, single-use, and
/// expiring, and the ceremony's MITM defense is the safety-code check, never this code.
fn random_digits(n: usize) -> String {
    let mut buf = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut buf)
        .expect("OS CSPRNG must be available");
    buf.iter().map(|b| char::from(b'0' + (b % 10))).collect()
}

fn code_key(code: &str) -> String {
    format!("enroll:code:{code}")
}

fn channel_key(id: &str) -> String {
    format!("enroll:channel:{id}")
}

fn mbox_key(id: &str, dir: Direction) -> String {
    format!("enroll:mbox:{id}:{}", dir.as_str())
}

/// The pending-enrollment record. Stored under **both** redeemable representations (full code
/// and text fallback) so redemption deletes every key for the enrollment in one shot.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnrollmentRecord {
    /// The issuing (device-A) account id — the initiator the relay channel binds to.
    user_id: String,
    /// The full-entropy code; kept so redemption by either form deletes this key.
    code: String,
    /// The numeric fallback; kept for the same reason.
    text_fallback: String,
    /// Explicit expiry (epoch seconds) — the deterministic, TTL-independent expiry guard.
    expires_at: i64,
}

/// The relay-channel record — bound to the initiator, gated by its own expiry window.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelState {
    /// The issuing (device-A) account id.
    initiator_user_id: String,
    /// Channel expiry (epoch seconds).
    expires_at: i64,
}

/// A freshly issued enrollment code, returned to the initiating device to display.
#[derive(Debug, Clone)]
pub struct IssuedCode {
    /// The full-entropy code carried by the QR payload.
    pub code: String,
    /// The shorter transcribable numeric fallback.
    pub text_fallback: String,
    /// Expiry as epoch seconds (the route renders it RFC 3339).
    pub expires_at: i64,
}

/// The outcome of a redemption attempt.
#[derive(Debug)]
pub enum RedeemOutcome {
    /// A live code was redeemed; the opaque relay-channel handle to continue on.
    Established { channel_id: String },
    /// No live code was redeemed. Every variant is surfaced identically by the route
    /// (indistinguishable by design); the reason is retained only for tracing and tests.
    Refused(RefusedReason),
}

/// Why a redemption was refused. Never leaked across the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedReason {
    /// Unknown or already-redeemed code (single-use consumed it).
    NotFound,
    /// The code existed but had passed its `expires_at`; it was deleted on this attempt.
    Expired,
    /// The source exceeded its redemption budget for the window.
    RateLimited,
}

/// Which mailbox a relay message flows into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Toward device A (the initiator).
    ToInitiator,
    /// Toward device B (the enrollee).
    ToEnrollee,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::ToInitiator => "a",
            Direction::ToEnrollee => "b",
        }
    }

    /// Parse the wire direction token (`"a"` = toward initiator, `"b"` = toward enrollee).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "a" => Some(Direction::ToInitiator),
            "b" => Some(Direction::ToEnrollee),
            _ => None,
        }
    }
}

/// Result of relaying a message into a mailbox.
#[derive(Debug, PartialEq, Eq)]
pub enum RelaySend {
    /// Payload enqueued.
    Ok,
    /// The channel is unknown or expired.
    NoChannel,
}

/// Result of draining a mailbox.
#[derive(Debug, PartialEq, Eq)]
pub enum RelayRecv {
    /// The (possibly empty) opaque payloads pending in the mailbox, drained on read.
    Messages(Vec<String>),
    /// The channel is unknown or expired.
    NoChannel,
}

/// Issue a fresh single-use enrollment code for `user_id`, live for `ttl`.
///
/// The code is stored under both its full and fallback forms so either redeems the same
/// pending enrollment. Callers set `ttl` to [`CODE_TTL`] in production; tests pass a shorter
/// (even zero) `ttl` to exercise the expiry path deterministically.
#[tracing::instrument(skip(sm), fields(user_id = %user_id, ttl_secs = ttl.as_secs()))]
pub async fn issue(
    sm: &SessionManager,
    user_id: &str,
    ttl: Duration,
) -> Result<IssuedCode, InternalServerError> {
    // Collision-checked at generation. A 256-bit draw never collides in practice; the guard
    // is cheap insurance and documents the invariant.
    let code = loop {
        let candidate = random_b64(FULL_CODE_BYTES);
        let existing: Option<EnrollmentRecord> = sm.get_temp_data(&code_key(&candidate)).await?;
        if existing.is_none() {
            break candidate;
        }
        tracing::warn!("enrollment code collision on generation — redrawing");
    };
    let text_fallback = random_digits(FALLBACK_DIGITS);
    let expires_at = now_secs() + i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);

    let record = EnrollmentRecord {
        user_id: user_id.to_string(),
        code: code.clone(),
        text_fallback: text_fallback.clone(),
        expires_at,
    };

    sm.save_temp_data(&code_key(&code), &record, ttl).await?;
    sm.save_temp_data(&code_key(&text_fallback), &record, ttl)
        .await?;

    tracing::debug!(expires_at, "issued enrollment code");
    Ok(IssuedCode {
        code,
        text_fallback,
        expires_at,
    })
}

/// Delete every stored key for an enrollment (single-use: consumed on redemption or expiry).
async fn delete_record(
    sm: &SessionManager,
    record: &EnrollmentRecord,
) -> Result<(), InternalServerError> {
    sm.delete_temp_data(&code_key(&record.code)).await?;
    sm.delete_temp_data(&code_key(&record.text_fallback))
        .await?;
    Ok(())
}

/// Redeem `submitted_code` (either representation) from the new device.
///
/// `source_key` buckets the anti-brute-force redemption rate limit (best-effort client IP).
/// On success the code is consumed and a relay channel opens; on any refusal the caller must
/// treat the outcome as opaque.
#[tracing::instrument(skip(sm, submitted_code), fields(source = %source_key))]
pub async fn redeem(
    sm: &SessionManager,
    submitted_code: &str,
    source_key: &str,
) -> Result<RedeemOutcome, InternalServerError> {
    // Rate-limit redemption per source. Exceeding it refuses without an oracle.
    let rl = sm
        .check_rate_limit(
            &format!("enroll_redeem:{source_key}"),
            MAX_REDEEM_PER_WINDOW,
            REDEEM_WINDOW.as_secs(),
        )
        .await?;
    if rl.count > MAX_REDEEM_PER_WINDOW {
        tracing::warn!("enrollment redemption rate-limited");
        return Ok(RedeemOutcome::Refused(RefusedReason::RateLimited));
    }

    let record: Option<EnrollmentRecord> = sm.get_temp_data(&code_key(submitted_code)).await?;
    let Some(record) = record else {
        return Ok(RedeemOutcome::Refused(RefusedReason::NotFound));
    };

    // Single-use + deleted-on-expiry: consume the code before deciding, so neither a valid
    // redemption nor an expired one can be replayed.
    delete_record(sm, &record).await?;

    if now_secs() >= record.expires_at {
        tracing::debug!("enrollment code expired — deleted on redemption attempt");
        return Ok(RedeemOutcome::Refused(RefusedReason::Expired));
    }

    let channel_id = random_b64(CHANNEL_ID_BYTES);
    let state = ChannelState {
        initiator_user_id: record.user_id,
        expires_at: now_secs() + i64::try_from(CHANNEL_TTL.as_secs()).unwrap_or(i64::MAX),
    };
    sm.save_temp_data(&channel_key(&channel_id), &state, CHANNEL_TTL)
        .await?;

    tracing::info!("enrollment code redeemed — relay channel established");
    Ok(RedeemOutcome::Established { channel_id })
}

/// Whether a relay channel exists and has not expired.
async fn channel_live(sm: &SessionManager, channel_id: &str) -> Result<bool, InternalServerError> {
    let state: Option<ChannelState> = sm.get_temp_data(&channel_key(channel_id)).await?;
    Ok(state.is_some_and(|s| now_secs() < s.expires_at))
}

/// Relay one opaque `payload` into the `dir` mailbox of `channel_id`. The payload is stored
/// verbatim — the server never decodes it.
#[tracing::instrument(skip(sm, payload), fields(channel = %channel_id, dir = dir.as_str(), payload_len = payload.len()))]
pub async fn relay_send(
    sm: &SessionManager,
    channel_id: &str,
    dir: Direction,
    payload: String,
) -> Result<RelaySend, InternalServerError> {
    if !channel_live(sm, channel_id).await? {
        return Ok(RelaySend::NoChannel);
    }
    let key = mbox_key(channel_id, dir);
    let mut queue: Vec<String> = sm.get_temp_data(&key).await?.unwrap_or_default();
    queue.push(payload);
    sm.save_temp_data(&key, &queue, CHANNEL_TTL).await?;
    tracing::debug!(depth = queue.len(), "relayed opaque enrollment payload");
    Ok(RelaySend::Ok)
}

/// Drain the `dir` mailbox of `channel_id`, returning any pending opaque payloads.
#[tracing::instrument(skip(sm), fields(channel = %channel_id, dir = dir.as_str()))]
pub async fn relay_recv(
    sm: &SessionManager,
    channel_id: &str,
    dir: Direction,
) -> Result<RelayRecv, InternalServerError> {
    if !channel_live(sm, channel_id).await? {
        return Ok(RelayRecv::NoChannel);
    }
    let key = mbox_key(channel_id, dir);
    let queue: Vec<String> = sm.get_temp_data(&key).await?.unwrap_or_default();
    if !queue.is_empty() {
        sm.delete_temp_data(&key).await?;
    }
    tracing::debug!(drained = queue.len(), "drained enrollment relay mailbox");
    Ok(RelayRecv::Messages(queue))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{InMemorySessionStorage, SessionManager};

    fn manager() -> SessionManager {
        SessionManager::new_with_storage(
            Box::new(InMemorySessionStorage::new()),
            Duration::from_secs(3600),
        )
    }

    // The in-memory storage ignores TTL, so these tests exercise the *explicit* `expires_at`
    // guard rather than storage eviction — that is exactly the deterministic, no-sleep path.

    #[tokio::test]
    async fn issue_then_redeem_establishes_channel() {
        let sm = manager();
        let issued = issue(&sm, "user-a", CODE_TTL).await.unwrap();
        assert!(!issued.code.is_empty());

        let outcome = redeem(&sm, &issued.code, "src").await.unwrap();
        match outcome {
            RedeemOutcome::Established { channel_id } => assert!(!channel_id.is_empty()),
            other => panic!("expected Established, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn redeem_is_single_use() {
        let sm = manager();
        let issued = issue(&sm, "user-a", CODE_TTL).await.unwrap();

        assert!(matches!(
            redeem(&sm, &issued.code, "src").await.unwrap(),
            RedeemOutcome::Established { .. }
        ));
        // Second redemption of the same code is refused — it was consumed.
        assert!(matches!(
            redeem(&sm, &issued.code, "src").await.unwrap(),
            RedeemOutcome::Refused(RefusedReason::NotFound)
        ));
    }

    #[tokio::test]
    async fn expired_code_is_refused_and_deleted() {
        let sm = manager();
        // A zero-TTL code is already past its explicit expiry the instant it is issued.
        let issued = issue(&sm, "user-a", Duration::from_secs(0)).await.unwrap();

        assert!(matches!(
            redeem(&sm, &issued.code, "src").await.unwrap(),
            RedeemOutcome::Refused(RefusedReason::Expired)
        ));
        // Deleted on expiry: a follow-up sees nothing at all (not even "expired").
        assert!(matches!(
            redeem(&sm, &issued.code, "src").await.unwrap(),
            RedeemOutcome::Refused(RefusedReason::NotFound)
        ));
        // The fallback form was deleted too.
        assert!(matches!(
            redeem(&sm, &issued.text_fallback, "src").await.unwrap(),
            RedeemOutcome::Refused(RefusedReason::NotFound)
        ));
    }

    #[tokio::test]
    async fn text_fallback_redeems_and_consumes_full_code() {
        let sm = manager();
        let issued = issue(&sm, "user-a", CODE_TTL).await.unwrap();

        // Redeeming by the fallback establishes the channel...
        assert!(matches!(
            redeem(&sm, &issued.text_fallback, "src").await.unwrap(),
            RedeemOutcome::Established { .. }
        ));
        // ...and consumes the full-code representation as well (single-use across both forms).
        assert!(matches!(
            redeem(&sm, &issued.code, "src").await.unwrap(),
            RedeemOutcome::Refused(RefusedReason::NotFound)
        ));
    }

    #[tokio::test]
    async fn codes_are_high_entropy_and_nonstructured() {
        let sm = manager();
        let a = issue(&sm, "user-a", CODE_TTL).await.unwrap();
        let b = issue(&sm, "user-a", CODE_TTL).await.unwrap();

        assert_ne!(a.code, b.code, "two CSPRNG codes must not collide");
        assert!(a.code.len() >= 43, "256-bit base64 code is ~43 chars");
        assert_eq!(a.text_fallback.len(), FALLBACK_DIGITS);
        assert!(a.text_fallback.chars().all(|c| c.is_ascii_digit()));
    }

    #[tokio::test]
    async fn relay_roundtrips_opaque_payloads() {
        let sm = manager();
        let issued = issue(&sm, "user-a", CODE_TTL).await.unwrap();
        let RedeemOutcome::Established { channel_id } =
            redeem(&sm, &issued.code, "src").await.unwrap()
        else {
            panic!("expected channel");
        };

        // Device A pushes toward B; the server stores it verbatim.
        assert_eq!(
            relay_send(&sm, &channel_id, Direction::ToEnrollee, "opaque-1".into())
                .await
                .unwrap(),
            RelaySend::Ok
        );
        assert_eq!(
            relay_send(&sm, &channel_id, Direction::ToEnrollee, "opaque-2".into())
                .await
                .unwrap(),
            RelaySend::Ok
        );

        // B drains its mailbox and gets the payloads back byte-for-byte, in order.
        assert_eq!(
            relay_recv(&sm, &channel_id, Direction::ToEnrollee)
                .await
                .unwrap(),
            RelayRecv::Messages(vec!["opaque-1".into(), "opaque-2".into()])
        );
        // A second drain is empty (messages consumed on read).
        assert_eq!(
            relay_recv(&sm, &channel_id, Direction::ToEnrollee)
                .await
                .unwrap(),
            RelayRecv::Messages(vec![])
        );
        // The other direction is independent.
        assert_eq!(
            relay_recv(&sm, &channel_id, Direction::ToInitiator)
                .await
                .unwrap(),
            RelayRecv::Messages(vec![])
        );
    }

    #[tokio::test]
    async fn relay_on_unknown_channel_reports_no_channel() {
        let sm = manager();
        assert_eq!(
            relay_send(&sm, "nope", Direction::ToEnrollee, "x".into())
                .await
                .unwrap(),
            RelaySend::NoChannel
        );
        assert_eq!(
            relay_recv(&sm, "nope", Direction::ToEnrollee)
                .await
                .unwrap(),
            RelayRecv::NoChannel
        );
    }

    #[test]
    fn direction_parse_round_trips() {
        assert_eq!(Direction::parse("a"), Some(Direction::ToInitiator));
        assert_eq!(Direction::parse("b"), Some(Direction::ToEnrollee));
        assert_eq!(Direction::parse("x"), None);
    }
}
