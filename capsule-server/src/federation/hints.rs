//! The pull hint: the one thing a peer may push, and what verifies one.
//!
//! design/federation.md, Pull-Only Federation: "The one thing a peer may push is an advisory
//! hint ... Capsule then fetches and validates on its own terms", and hints "ride a separate
//! low-trust channel and never feed validation". This module is that channel's contract; the
//! route is [`crate::routes::hints::submit_pull_hint`] and what acts on a hint is
//! [`PullWorker::hint`].
//!
//! # What a hint names, and what it cannot
//!
//! An album, by the id its **home server** gives it, and that home server — which is the peer
//! that signs the hint. The job a hint touches is [`JobKey`]`(home_server, album_id)`, so a peer
//! can only ever hint about albums this server pulls *from that peer*: a hint naming another
//! server's album is a hint about a job that does not exist. It carries no data, no cursor and
//! no claim about what changed, so there is nothing in it for validation to read. All it can do
//! is make a job this server already holds due sooner, never sooner than
//! [`HINT_FLOOR`](super::pull::HINT_FLOOR) after the job's last pull, and never for a peer on
//! probation or blocked. It cannot add a job, change what is pulled or from where, or reach past
//! a peer's back-off or open circuit: the worker decides what runs exactly as it would have.
//!
//! # Signed like a report, for the same reason
//!
//! A hinting peer holds no credential of this server's — the capability it would present is one
//! *this* server presents to *it* — so, as with [report intake](super::report), the hint carries
//! its own Ed25519 signature and is verified against the key an operator **pinned** for the
//! peer. A peer nobody pinned is refused; intake is not the moment a peer becomes trusted.
//!
//! # What is signed
//!
//! The canonical CBOR of [`HintClaim`]'s fields plus a fixed `context` of [`HINT_CONTEXT`], so a
//! signature this key makes for a hint cannot be mistaken for any other it makes. The fields are
//! the body's, whitespace-trimmed and otherwise exactly as sent.
//!
//! # Replay
//!
//! Bound to one recipient, one moment and one use, as a report is: the claim names the server it
//! is addressed to, carries the instant it was signed (inside [`HINT_FRESHNESS`] of this server's
//! clock either way), and a nonce this server accepts once per peer inside
//! [`HINT_NONCE_WINDOW`]. A replayed hint could only make a job due sooner, which the floor
//! already bounds; what the replay memory prevents is a party that captured one hint spending the
//! real peer's hint budget with it.
//!
//! # No oracle
//!
//! Whether this server holds the job a hint names is never said: a verified hint is answered the
//! same `202` whether it moved a job, found it already due, named a job this server does not
//! hold, or came from a peer on probation. The receiver does the same work for each — one peer
//! read and one schedule operation — so the answer's timing does not say either.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use super::pull::{HintOutcome, JobKey, PullWorker};

/// The `context` every hint signature covers.
///
/// Fixed text in the signed map so a signature over a hint can never verify as a report, a
/// request signature, or anything else the peer's operational key signs.
pub const HINT_CONTEXT: &str = "capsule-federation-hint-v1";

/// How far a hint's `sent_at` may sit from this server's clock, either way.
///
/// Five minutes, the [report's](super::REPORT_FRESHNESS) tolerance and for its reasons.
pub const HINT_FRESHNESS: SignedDuration = SignedDuration::from_mins(5);

/// How long a hint's nonce is remembered: every instant at which it could still be fresh.
pub const HINT_NONCE_WINDOW: SignedDuration = SignedDuration::from_mins(10);

/// Whether a hint signed at `sent_at` is fresh at `now`.
pub fn is_fresh(sent_at: Timestamp, now: Timestamp) -> bool {
    super::report::is_within(sent_at, now, HINT_FRESHNESS)
}

/// A hint's signed payload: every field of the body except the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintClaim {
    /// The album's home server, which sends and signs the hint, as its own `server-info` names
    /// it.
    pub home_server: String,
    /// The server the hint is addressed to, as its own `server-info` names it.
    pub recipient_server: String,
    /// A value the peer never signs twice. At least 16 bytes.
    pub nonce: String,
    /// The album that has news, as its home server names it.
    pub album_id: String,
    /// When the peer signed the hint, RFC 3339. Must be inside [`HINT_FRESHNESS`].
    pub sent_at: String,
}

/// The map a peer signs: the claim and the [`HINT_CONTEXT`].
///
/// Canonical CBOR sorts a map's keys, so the encoding depends on these names and values and not
/// on the order they are declared in. Renaming one is a breaking change to every peer, and
/// `tests::the_signing_bytes_are_stable` pins the encoding.
#[derive(Serialize)]
struct Signed<'a> {
    context: &'static str,
    home_server: &'a str,
    recipient_server: &'a str,
    nonce: &'a str,
    album_id: &'a str,
    sent_at: &'a str,
}

/// Why a hint was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HintError {
    /// The claim could not be encoded, which can only be this server's own fault.
    #[error("the hint's signing bytes could not be produced")]
    Unencodable,
    /// The signature does not verify under the peer's pinned key.
    #[error("the hint's signature does not verify")]
    NotAuthentic,
}

impl HintClaim {
    /// The exact bytes a peer signs.
    ///
    /// # Errors
    ///
    /// Returns [`HintError::Unencodable`] if the map does not serialize, which nothing a peer can
    /// send causes: every field is a string.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, HintError> {
        capsule_core::cbor::to_canonical_vec(&Signed {
            context: HINT_CONTEXT,
            home_server: &self.home_server,
            recipient_server: &self.recipient_server,
            nonce: &self.nonce,
            album_id: &self.album_id,
            sent_at: &self.sent_at,
        })
        .map_err(|error| {
            tracing::error!(%error, "a pull hint's claim did not serialize");
            HintError::Unencodable
        })
    }

    /// Whether `signature` is this claim's, under `key`.
    ///
    /// # Errors
    ///
    /// Returns [`HintError::NotAuthentic`] when it is not. Nothing of the signature or the key is
    /// logged.
    pub fn verify(&self, signature: &[u8], key: &[u8; 32]) -> Result<(), HintError> {
        let bytes = self.signing_bytes()?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.as_slice())
            .verify(&bytes, signature)
            .map_err(|_| {
                tracing::info!(
                    from = %self.home_server,
                    "a pull hint's signature did not verify under the pinned key"
                );
                HintError::NotAuthentic
            })
    }
}

/// What acts on a verified hint: in production, the [`PullWorker`].
///
/// A port rather than the worker itself so the route can be driven against a double, and so a
/// deployment that assembled no worker answers exactly as one that did.
pub trait HintReceiver: std::fmt::Debug + Send + Sync {
    /// Bring `job`'s next pull earlier, if its peer's hints are honoured and the job is held.
    fn hint<'a>(
        &'a self,
        job: &'a JobKey,
    ) -> Pin<Box<dyn Future<Output = HintOutcome> + Send + 'a>>;
}

impl HintReceiver for PullWorker {
    fn hint<'a>(
        &'a self,
        job: &'a JobKey,
    ) -> Pin<Box<dyn Future<Output = HintOutcome> + Send + 'a>> {
        Box::pin(PullWorker::hint(self, job))
    }
}

/// A [`HintReceiver`] double: records every job it was hinted and answers a set outcome.
#[derive(Debug)]
pub struct RecordingHints {
    received: Mutex<Vec<JobKey>>,
    outcome: Mutex<HintOutcome>,
}

impl Default for RecordingHints {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingHints {
    /// A receiver that answers [`HintOutcome::Unchanged`] until told otherwise.
    pub fn new() -> Self {
        Self {
            received: Mutex::new(Vec::new()),
            outcome: Mutex::new(HintOutcome::Unchanged),
        }
    }

    /// Answer `outcome` to every hint from now on.
    pub fn answer(&self, outcome: HintOutcome) {
        *self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = outcome;
    }

    /// Every job hinted so far, oldest first.
    pub fn received(&self) -> Vec<JobKey> {
        self.received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl HintReceiver for RecordingHints {
    fn hint<'a>(
        &'a self,
        job: &'a JobKey,
    ) -> Pin<Box<dyn Future<Output = HintOutcome> + Send + 'a>> {
        self.received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(job.clone());
        let outcome = *self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Box::pin(async move { outcome })
    }
}

#[cfg(test)]
mod tests;
