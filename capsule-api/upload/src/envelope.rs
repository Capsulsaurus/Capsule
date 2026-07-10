//! The refuse-by-default envelope gate — the seam wiring `capsule_core::validation` into
//! every write path (slice `S-C1` in the repo-root `SLICES.md`; SSoT:
//! <https://docs/design/threat-model/validation/>).
//!
//! Two layers live here:
//!
//! 1. [`EnvelopeGate`] — a salvo middleware running the fail-closed protocol handshake
//!    (invariant 1 and the universal-headers rules) ahead of every upload handler. It
//!    reads `X-Capsule-Protocol`, rejects a version outside `[min, max]` with `426` +
//!    `error.protocol.version_unsupported`, and advertises the accepted range on every
//!    response (errors included).
//! 2. [`validate_create_envelope`] / [`revalidate_envelope`] — the manifest-envelope
//!    checks (invariants 2–8, 15) built on `capsule_core::validation`'s pure predicates
//!    plus [`check_manifest_envelope`], run ahead of the pending-row write at session
//!    creation and again inside the finalization transaction.
//!
//! [`check_manifest_envelope`]: capsule_core::validation::check_manifest_envelope

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::AmkVersion;
use capsule_core::crypto::provenance::action::Action;
use capsule_core::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
use capsule_core::validation::protocol::check_suite;
use capsule_core::validation::structural::{content_type_allowed, hash_length_ok, size_in_bounds};
use capsule_core::validation::{
    EnvelopeContext, EnvelopeReject, HandshakeReject, check_manifest_envelope, protocol_gate,
};
use salvo::prelude::*;
use uuid::Uuid;

use crate::config::UploadServerConfig;
use crate::error::UploadError;
use crate::models::requests::{CreateUploadRequest, ManifestEnvelope};

/// Salvo middleware running the fail-closed protocol handshake (invariant 1 and the
/// universal-headers rules) ahead of every write handler it is hooped onto.
pub(crate) struct EnvelopeGate {
    /// The lowest protocol version this server accepts (`X-Capsule-Protocol-Min`).
    pub min_protocol: String,
    /// The highest protocol version this server accepts (`X-Capsule-Protocol-Max`).
    pub max_protocol: String,
}

impl EnvelopeGate {
    pub(crate) fn new(min_protocol: String, max_protocol: String) -> Self {
        Self {
            min_protocol,
            max_protocol,
        }
    }
}

#[async_trait]
impl Handler for EnvelopeGate {
    #[tracing::instrument(skip_all)]
    async fn handle(
        &self,
        req: &mut Request,
        depot: &mut Depot,
        res: &mut Response,
        ctrl: &mut FlowCtrl,
    ) {
        // Advertise the accepted range on *every* response, errors included.
        res.add_header("X-Capsule-Protocol-Min", self.min_protocol.clone(), true)
            .ok();
        res.add_header("X-Capsule-Protocol-Max", self.max_protocol.clone(), true)
            .ok();

        // CORS preflight carries no protocol header and writes no state — let it pass.
        if *req.method() == salvo::http::Method::OPTIONS {
            return;
        }

        let client = req.header::<String>("X-Capsule-Protocol");
        let reject = match &client {
            Some(v) => match protocol_gate(v, &self.min_protocol, &self.max_protocol) {
                Ok(()) => {
                    tracing::trace!(protocol = %v, "protocol handshake ok");
                    None
                }
                Err(HandshakeReject::ProtocolOutOfRange) => {
                    Some(UploadError::ProtocolUnsupported {
                        min: self.min_protocol.clone(),
                        max: self.max_protocol.clone(),
                    })
                }
                Err(_) => Some(UploadError::InvalidUpload(
                    "X-Capsule-Protocol is not a YYYY-MM-DD date".to_string(),
                )),
            },
            None => Some(UploadError::InvalidUpload(
                "missing X-Capsule-Protocol header".to_string(),
            )),
        };

        if let Some(err) = reject {
            tracing::info!(
                protocol = ?client,
                code = err.code(),
                "upload protocol handshake rejected"
            );
            err.write(req, depot, res).await;
            ctrl.skip_rest();
        }
    }
}

/// Validate a `POST /upload` request's envelope ahead of any write (invariants 2–8 and the
/// top-level↔envelope consistency check, invariant-15 family). `uploader_added_at` is the
/// RFC3339 lower bound for the uploader's device authorization (the account-creation time,
/// standing in for the device-directory `added_at` until the directory table lands), and
/// `server_clock` is the server's trusted clock.
///
/// Album write-capability (the DB half of invariant 6) is checked separately against
/// Postgres in the create transaction; this function is pure over the request + config.
pub(crate) fn validate_create_envelope(
    request: &CreateUploadRequest,
    cfg: &UploadServerConfig,
    uploader_added_at: &str,
    server_clock: &str,
) -> Result<(), UploadError> {
    // Invariant 1: the session's pinned protocol version is within the window. (The wire
    // handshake header is gated by `EnvelopeGate`; this pins the body's declared version.)
    match protocol_gate(
        &request.protocol_version,
        &cfg.protocol_min,
        &cfg.protocol_max,
    ) {
        Ok(()) => {}
        Err(HandshakeReject::ProtocolOutOfRange) => {
            return Err(UploadError::ProtocolUnsupported {
                min: cfg.protocol_min.clone(),
                max: cfg.protocol_max.clone(),
            });
        }
        Err(_) => {
            return Err(UploadError::InvalidUpload(
                "protocol_version is not a YYYY-MM-DD date".to_string(),
            ));
        }
    }

    // Invariant 2: crypto suite is in the inventory.
    check_suite(request.crypto_suite_id).map_err(|_| UploadError::UnknownCryptoSuite)?;

    // Invariant 3: hash is lowercase hex of the suite's digest length.
    if !is_lower_hex(&request.hash)
        || !hash_length_ok(request.crypto_suite_id, request.hash.len() / 2)
    {
        return Err(UploadError::InvalidHash);
    }

    // Invariant 4: size is in (0, max_file_size].
    if request.size == 0 {
        return Err(UploadError::InvalidSize);
    }
    if !size_in_bounds(request.size, cfg.max_file_size as u64) {
        return Err(UploadError::FileTooLarge);
    }

    // Invariant 5: content type is in the closed enum.
    let allowed: Vec<&str> = cfg
        .allowed_content_types
        .iter()
        .map(String::as_str)
        .collect();
    if !content_type_allowed(&request.content_type, &allowed) {
        return Err(UploadError::UnsupportedContentType);
    }

    // Invariant 15 family: the strict top-level fields must agree with the envelope.
    check_envelope_consistency(request)?;

    // Invariants 2, 7 (time half), 8, 17, 18: the keyless envelope battery over the
    // reconstructed manifest core. `album_pin` = the request's protocol version (a create
    // pins the album to the version it is written under), so the pin check is a no-op here
    // and album write-capability is enforced separately in the DB transaction.
    let core = build_manifest_core(&request.manifest_envelope)?;
    let ctx = EnvelopeContext {
        album_pin: &request.protocol_version,
        device_added_at: uploader_added_at,
        server_clock,
        drift_days: cfg.timestamp_drift_days,
        stored_chain_head: None,
        stored_amk_version: None,
    };
    map_envelope_reject(check_manifest_envelope(&core, &ctx))
}

/// Re-run the keyless envelope battery at finalization (invariant 15). Deserializes the
/// envelope stored verbatim on the session and re-applies the create-time checks, so a
/// change since creation (envelope tampering, an out-of-drift clock) is caught inside the
/// finalization transaction. Maps a failure to [`UploadError::EnvelopeRejected`].
pub(crate) fn revalidate_envelope(
    envelope_json: &str,
    protocol_version: &str,
    cfg: &UploadServerConfig,
    uploader_added_at: &str,
    server_clock: &str,
) -> Result<(), UploadError> {
    let envelope: ManifestEnvelope = serde_json::from_str(envelope_json)
        .map_err(|e| UploadError::EnvelopeRejected(format!("undecodable envelope: {e}")))?;
    let core =
        build_manifest_core(&envelope).map_err(|e| UploadError::EnvelopeRejected(e.to_string()))?;
    let ctx = EnvelopeContext {
        album_pin: protocol_version,
        device_added_at: uploader_added_at,
        server_clock,
        drift_days: cfg.timestamp_drift_days,
        stored_chain_head: None,
        stored_amk_version: None,
    };
    check_manifest_envelope(&core, &ctx)
        .map_err(|r| UploadError::EnvelopeRejected(format!("{r:?}")))
}

/// Invariant-15-family consistency: the strict top-level request fields must not contradict
/// the signed manifest envelope the client also submits.
fn check_envelope_consistency(request: &CreateUploadRequest) -> Result<(), UploadError> {
    let env = &request.manifest_envelope;
    if env.crypto_suite_id != request.crypto_suite_id {
        return Err(UploadError::EnvelopeMismatch("crypto_suite_id"));
    }
    if env.protocol_version != request.protocol_version {
        return Err(UploadError::EnvelopeMismatch("protocol_version"));
    }
    if env.album_id != request.album_id {
        return Err(UploadError::EnvelopeMismatch("album_id"));
    }
    if env.ciphertext_hash != request.hash {
        return Err(UploadError::EnvelopeMismatch("ciphertext_hash"));
    }
    Ok(())
}

/// Reconstruct a [`ManifestCore`] from the wire envelope so the shared keyless battery in
/// `capsule_core` can run over it. The battery inspects only `crypto_suite_id`,
/// `protocol_version`, `timestamp`, `action`, `prior_provenance_hash`, and `amk_version`
/// (plus the `ctx`); every other field is set to a canonical placeholder. The load-bearing
/// wire fields are parsed here (a bad `action`, `key_mode`, or `prior_provenance_hash` is
/// `envelope_mismatch`); server identifiers (`album_id`/`file_id`) are opaque nanoids the
/// battery never reads, so they are not parsed as UUIDs.
fn build_manifest_core(env: &ManifestEnvelope) -> Result<ManifestCore, UploadError> {
    let key_mode: KeyMode =
        parse_json_enum(&env.key_mode).ok_or(UploadError::EnvelopeMismatch("key_mode"))?;
    let action: Action =
        parse_json_enum(&env.action).ok_or(UploadError::EnvelopeMismatch("action"))?;
    let prior_provenance_hash = match &env.prior_provenance_hash {
        Some(h) => Some(
            Hash32::from_hex(h)
                .map_err(|_| UploadError::EnvelopeMismatch("prior_provenance_hash"))?,
        ),
        None => None,
    };

    Ok(ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: env.crypto_suite_id,
        protocol_version: env.protocol_version.clone(),
        file_id: Uuid::nil(),
        album_id: Uuid::nil(),
        amk_version: AmkVersion(env.amk_version),
        ciphertext_hash: Hash32([0u8; 32]),
        plaintext_size: env.plaintext_size,
        chunk_size: env.chunk_size,
        nonce_prefix: [0u8; 7],
        key_mode,
        wrapped_file_key: None,
        metadata_blob_hash: None,
        created_by_user: Uuid::nil(),
        created_by_device: Uuid::nil(),
        client_version: env.client_version.clone(),
        timestamp: env.timestamp.clone(),
        action,
        prior_provenance_hash,
        retention_until: env.retention_until.clone(),
    })
}

/// Map a [`EnvelopeReject`] from the shared keyless battery to the upload error taxonomy.
fn map_envelope_reject(result: Result<(), EnvelopeReject>) -> Result<(), UploadError> {
    match result {
        Ok(()) => Ok(()),
        Err(EnvelopeReject::UnknownSuite) => Err(UploadError::UnknownCryptoSuite),
        Err(EnvelopeReject::AlbumPinMismatch) => Err(UploadError::AlbumAccessDenied),
        Err(EnvelopeReject::DeviceAddedAfter) => Err(UploadError::DeviceNotAuthorized),
        Err(EnvelopeReject::TimestampUnsane) => Err(UploadError::TimestampOutOfRange),
        // A create whose envelope carries a stale/advancing chain is a structural
        // contradiction of the create action — surfaced as an envelope mismatch.
        Err(EnvelopeReject::StaleChain) => {
            Err(UploadError::EnvelopeMismatch("prior_provenance_hash"))
        }
        Err(EnvelopeReject::AmkRegressed) => Err(UploadError::EnvelopeMismatch("amk_version")),
    }
}

/// Parse a bare wire enum value (e.g. `"create"`, `"derived"`) into its serde type.
fn parse_json_enum<T: serde::de::DeserializeOwned>(value: &str) -> Option<T> {
    serde_json::from_str::<T>(&format!("\"{value}\"")).ok()
}

/// True if `s` is non-empty, even-length, all lowercase hex.
fn is_lower_hex(s: &str) -> bool {
    !s.is_empty()
        && s.len().is_multiple_of(2)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
