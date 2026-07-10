//! Federated moderation-report intake (slice `S-C8`, threat-model invariant 24).
//!
//! A report against `alice@other.tld`'s asset is routed to her home server's admins — the only
//! party that can act on her account. Three mechanics are fixed and enforced here (SSoT:
//! [Moderation — Federated Reporting](https://docs/design/moderation/#federated-reporting)):
//!
//! - **Authentication.** A report MUST be signed by the reporting server's classical Ed25519
//!   [operational key](https://docs/design/federation/#server-identity-and-key-rotation) and
//!   is verified against that peer's registered key before it reaches the admin queue. An
//!   unsigned, tampered, or unknown-peer report is dropped, never surfaced. This makes every
//!   report attributable — a server that submits false reports is itself identifiable and
//!   blockable.
//! - **Rate-limiting.** Reports are bounded per `(reporting_server, reported_user)`; exceeding
//!   the budget applies backpressure rather than amplifying. With signing, this defeats the
//!   false-flag / mass-report vector.
//! - **Content.** A report carries the alleged asset's **content hash and album pointer only —
//!   never plaintext or decryption material**. A report never widens who can read content.

use capsule_core::cbor;
use entity::{federation_peer, moderation_report};
use jiff::Timestamp;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use tracing::instrument;
use uuid::Uuid;

use super::blocklist::Blocklist;
use super::{ModerationError, ModerationLimits};

/// Schema version carried on every federated report core.
pub const FEDERATED_REPORT_VERSION: &str = "federated-report/v1";

/// The signed core of a federated moderation report. Canonical CBOR of this struct is exactly
/// the bytes the reporting server's Ed25519 key signs and this server verifies — deliberately
/// server-visible (content hashes only, never plaintext).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCore {
    /// Schema version (`federated-report/v1`).
    pub version: String,
    /// The peer server submitting the report (its canonical origin).
    pub reporting_server: String,
    /// The reported account on this (home) server.
    pub reported_user: String,
    /// The alleged asset's content hash (64-char lowercase hex).
    pub content_hash: String,
    /// An album pointer locating the asset for an admin with album access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_pointer: Option<String>,
    /// A free-form admin-facing reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The reporter's clock at issuance (RFC 3339) — binds the signature to a time.
    pub issued_at: String,
}

impl ReportCore {
    /// The canonical CBOR bytes the reporting server signs and this server verifies.
    fn signing_bytes(&self) -> Result<Vec<u8>, ModerationError> {
        cbor::to_canonical_vec(self)
            .map_err(|e| ModerationError::ReportMalformed(format!("report cbor: {e}")))
    }
}

/// A federated report as it arrives on the wire: the signed core plus the 64-byte Ed25519
/// signature over its canonical CBOR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedReport {
    /// The signed report body.
    pub core: ReportCore,
    /// Ed25519 signature over `core`'s canonical CBOR (64 bytes).
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

/// Federated-report intake and the admin-queue query surface.
pub struct Report;

impl Report {
    /// Register (or rotate) a peer server's 32-byte Ed25519 public signing key. Idempotent —
    /// a re-registration overwrites the stored key (key rotation).
    #[instrument(skip(db, public_key), fields(server_id = %server_id))]
    pub async fn register_peer<C: ConnectionTrait>(
        db: &C,
        server_id: &str,
        public_key: &[u8; 32],
    ) -> Result<(), ModerationError> {
        let existing = federation_peer::Entity::find_by_id(server_id)
            .one(db)
            .await?;
        match existing {
            Some(model) => {
                let mut am: federation_peer::ActiveModel = model.into();
                am.ed25519_public_key = Set(public_key.to_vec());
                am.update(db).await?;
            }
            None => {
                federation_peer::ActiveModel {
                    server_id: Set(server_id.to_string()),
                    ed25519_public_key: Set(public_key.to_vec()),
                    created_at: Set(entity::time::now_entity()),
                }
                .insert(db)
                .await?;
            }
        }
        tracing::info!(server_id, "registered federated peer signing key");
        Ok(())
    }

    /// Intake a federated report: verify the peer is not blocklisted, verify the signature
    /// against the peer's registered key, enforce the per-pair rate budget, and — only then —
    /// append it to the admin queue. Returns the new report id.
    ///
    /// Any failing gate drops the report **before** it reaches the queue (invariant 24): a
    /// blocked peer, an unsigned/invalid/unknown-peer signature, or an exhausted rate budget
    /// each return an [`Err`] and write nothing.
    #[instrument(skip(db, signed, limits), fields(
        reporting_server = %signed.core.reporting_server,
        reported_user = %signed.core.reported_user,
    ))]
    pub async fn intake<C: ConnectionTrait>(
        db: &C,
        signed: &SignedReport,
        limits: &ModerationLimits,
        now: Timestamp,
    ) -> Result<String, ModerationError> {
        let core = &signed.core;

        // Structural shape: the content hash is a 64-char lowercase hex digest.
        if core.content_hash.len() != 64
            || !core
                .content_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            tracing::warn!("federated report dropped: malformed content hash");
            return Err(ModerationError::ReportMalformed(
                "content_hash must be 64-char lowercase hex".to_string(),
            ));
        }

        // A blocked peer cannot report (reports ride the federation surface).
        Blocklist::ensure_server_allowed(db, &core.reporting_server).await?;

        // Verify the signature against the peer's registered key. No key ⇒ unknown peer ⇒
        // unverifiable ⇒ dropped.
        Self::verify_signature(db, signed).await?;

        // Per-(reporting_server, reported_user) rate budget: backpressure, never amplify.
        let window_start = now
            .checked_sub(limits.report_rate_window)
            .unwrap_or(Timestamp::UNIX_EPOCH);
        let recent = moderation_report::Entity::find()
            .filter(moderation_report::Column::ReportingServer.eq(&core.reporting_server))
            .filter(moderation_report::Column::ReportedUser.eq(&core.reported_user))
            .filter(
                moderation_report::Column::ReceivedAt.gte(entity::time::ts_to_entity(window_start)),
            )
            .count(db)
            .await?;
        if recent >= limits.report_rate_max {
            tracing::warn!(
                recent,
                max = limits.report_rate_max,
                "federated report dropped: per-pair rate budget exhausted (backpressure)"
            );
            return Err(ModerationError::ReportRateLimited {
                server: core.reporting_server.clone(),
                user: core.reported_user.clone(),
            });
        }

        let id = Uuid::now_v7().to_string();
        moderation_report::ActiveModel {
            id: Set(id.clone()),
            reporting_server: Set(core.reporting_server.clone()),
            reported_user: Set(core.reported_user.clone()),
            content_hash: Set(core.content_hash.clone()),
            album_pointer: Set(core.album_pointer.clone()),
            reason: Set(core.reason.clone()),
            received_at: Set(entity::time::ts_to_entity(now)),
        }
        .insert(db)
        .await?;
        tracing::info!(report_id = %id, "federated report accepted into admin queue");
        Ok(id)
    }

    /// Verify a report's Ed25519 signature against the reporting peer's registered key.
    /// An unknown peer, a wrong-length signature, or a bad signature all collapse to
    /// [`ModerationError::ReportUnsigned`] (indistinguishable — no oracle).
    async fn verify_signature<C: ConnectionTrait>(
        db: &C,
        signed: &SignedReport,
    ) -> Result<(), ModerationError> {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        let Some(peer) = federation_peer::Entity::find_by_id(&signed.core.reporting_server)
            .one(db)
            .await?
        else {
            tracing::warn!("federated report dropped: reporting peer is unknown (no key)");
            return Err(ModerationError::ReportUnsigned);
        };

        let key_bytes: [u8; 32] = peer
            .ed25519_public_key
            .as_slice()
            .try_into()
            .map_err(|_| ModerationError::ReportUnsigned)?;
        let sig_bytes: [u8; 64] = signed
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| ModerationError::ReportUnsigned)?;

        let verifying_key =
            VerifyingKey::from_bytes(&key_bytes).map_err(|_| ModerationError::ReportUnsigned)?;
        let signature = Signature::from_bytes(&sig_bytes);
        let message = signed.core.signing_bytes()?;

        verifying_key.verify(&message, &signature).map_err(|_| {
            tracing::warn!("federated report dropped: signature did not verify");
            ModerationError::ReportUnsigned
        })
    }

    /// The admin queue of reports against `reported_user`, newest first.
    #[instrument(skip(db), fields(reported_user = %reported_user))]
    pub async fn queue_for_user<C: ConnectionTrait>(
        db: &C,
        reported_user: &str,
    ) -> Result<Vec<moderation_report::Model>, ModerationError> {
        Ok(moderation_report::Entity::find()
            .filter(moderation_report::Column::ReportedUser.eq(reported_user))
            .order_by_desc(moderation_report::Column::ReceivedAt)
            .all(db)
            .await?)
    }
}
