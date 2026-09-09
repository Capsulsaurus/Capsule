//! Pushing a library asset to the server through the SDK's own ladder, and the lifecycle-op
//! posting that chains onto what the ladder established.
//!
//! **Nothing here uploads a blob.** [`push_asset`] hands `capsule_core`'s [`UploadBundle`] to
//! [`capsule_sdk::push::push_bundle`] and reports what came back; the index tier it ships is
//! the provenance blob and then the sealed metadata blob, which is what makes the server
//! publish the asset (`capsule-server/src/upload/visibility.rs` requires both index-tier
//! roles) and what gives the server a chain head equal to the client's next
//! `prior_provenance_hash`. The harness used to add that provenance rung itself, because the
//! ladder omitted it; issues #464 and #465 fixed the ladder and the decode side, so the rung is
//! gone from here and the cases exercise the shipped path.
//!
//! The lifecycle-op envelope is projected from the head manifest's [`ManifestCore`] rather than
//! from an `UploadBundle`, because a bundle re-derives the original's ciphertext and an adopted
//! (wrapped-key) asset or a tombstone head has nothing to re-derive; the projection is the same
//! one `capsule_sdk::push::envelope_for` makes, field for field.

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::provenance::manifest::ManifestCore;
use capsule_core::lifecycle::UploadBundle;
use capsule_sdk::net::ConnectionClass;
pub use capsule_sdk::push::ensure_album;
use capsule_sdk::push::{AssetPushReport, bundle_blobs, push_bundle};
use capsule_sdk::rest;
use capsule_sdk::staged::StagedScheduler;
use capsule_sdk::upload::{BlobRole, ManifestEnvelope};
use uuid::Uuid;

use crate::{Device, PROTOCOL_VERSION, Server};

/// What one push left behind.
pub struct Pushed {
    /// The bundle the library produced for the asset's current head.
    pub bundle: UploadBundle,
    /// The SDK ladder's report — provenance, metadata, derivatives, original.
    pub report: AssetPushReport,
    /// The content address of the provenance blob the ladder shipped: the server's chain head.
    pub provenance_hash: String,
}

/// Serialize a wire enum (`Action`, `KeyMode`) to its bare protocol string.
fn wire_enum<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .expect("a wire enum serializes to a string")
}

/// The SDK's [`ManifestEnvelope`] for one blob of the asset whose head is `core`, with
/// `ciphertext_hash` naming **this** blob (the server's invariant-15 consistency rule).
#[must_use]
pub fn sdk_envelope(core: &ManifestCore, blob_hash: &str) -> ManifestEnvelope {
    ManifestEnvelope {
        crypto_suite_id: core.crypto_suite_id,
        protocol_version: core.protocol_version.clone(),
        album_id: Some(core.album_id.to_string()),
        file_id: core.file_id.to_string(),
        amk_version: core.amk_version.0,
        ciphertext_hash: blob_hash.to_owned(),
        plaintext_size: core.plaintext_size,
        chunk_size: core.chunk_size,
        key_mode: wire_enum(&core.key_mode),
        metadata_blob_hash: core.metadata_blob_hash.map(|h| h.to_hex()),
        created_by_user: core.created_by_user.to_string(),
        created_by_device: core.created_by_device.to_string(),
        client_version: core.client_version.clone(),
        timestamp: core.timestamp.clone(),
        action: wire_enum(&core.action),
        prior_provenance_hash: core.prior_provenance_hash.map(|h| h.to_hex()),
        retention_until: core.retention_until.clone(),
    }
}

/// The same projection in the generated type the lifecycle-op and adopt operations take.
#[must_use]
pub fn wire_envelope(core: &ManifestCore, blob_hash: &str) -> rest::types::ManifestEnvelope {
    let envelope = sdk_envelope(core, blob_hash);
    rest::types::ManifestEnvelope {
        crypto_suite_id: i64::from(envelope.crypto_suite_id),
        protocol_version: envelope.protocol_version,
        album_id: envelope.album_id,
        file_id: envelope.file_id,
        amk_version: i64::from(envelope.amk_version),
        ciphertext_hash: envelope.ciphertext_hash,
        plaintext_size: envelope.plaintext_size as i64,
        chunk_size: i64::from(envelope.chunk_size),
        key_mode: envelope.key_mode,
        metadata_blob_hash: envelope.metadata_blob_hash,
        original_blob_hash: None,
        created_by_user: envelope.created_by_user,
        created_by_device: envelope.created_by_device,
        client_version: envelope.client_version,
        timestamp: envelope.timestamp,
        action: envelope.action,
        prior_provenance_hash: envelope.prior_provenance_hash,
        retention_until: envelope.retention_until,
    }
}

/// Push `asset_id` in full through the SDK ladder under `UploadPolicy::Full` on an unmetered
/// link: the index tier (provenance, then the sealed metadata blob), every derivative, then the
/// original.
pub async fn push_asset(device: &Device, server: &Server, asset_id: &Uuid) -> Pushed {
    let bundle = device
        .workspace
        .upload_bundle(asset_id)
        .expect("the library builds an upload bundle for its own asset");
    let client = device.upload_client(server);
    let scheduler = StagedScheduler::new(
        capsule_core::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    let report = push_bundle(&client, &scheduler, &bundle, &HashSet::new(), false)
        .await
        .expect("the SDK ladder pushes the bundle");
    // The ladder's own content address for the provenance rung, read back off the same
    // function that decided what to upload rather than re-derived here.
    let provenance_hash = bundle_blobs(&bundle)
        .into_iter()
        .find_map(|(blob, hash)| (blob.role == BlobRole::Provenance).then_some(hash))
        .expect("the SDK ladder always carries a provenance rung");
    tracing::info!(
        asset_id = %asset_id,
        pushed = report.pushed.len(),
        %provenance_hash,
        "e2e push complete"
    );
    Pushed {
        bundle,
        report,
        provenance_hash,
    }
}

/// Post the library's current chain head for `asset_id` as a lifecycle op
/// (`POST /v1/albums/{album}/ops`) through the generated client.
///
/// The op carries the head record's canonical CBOR as `manifest_cbor` — so the server's new head
/// is that record's hash — and the sealed metadata blob whenever the head manifest binds one
/// (invariant 25: a hash without its bytes, or bytes without a hash, is a `400`).
///
/// The CBOR is encoded here rather than taken from [`UploadBundle::provenance_blob`] because a
/// bundle re-derives the original's ciphertext from the media file, and the heads this posts —
/// a tombstone, a trash-restore — need no such file to exist.
pub async fn post_lifecycle_head(
    device: &Device,
    server: &Server,
    asset_id: &Uuid,
) -> rest::types::OpResponse {
    let asset = device
        .workspace
        .asset(asset_id)
        .expect("the asset is in the library");
    let head_record = device.head_record(asset_id);
    let core = &head_record.manifest.core;
    let manifest = capsule_core::cbor::to_canonical_vec(&head_record).expect("a record serializes");
    debug_assert_eq!(
        capsule_core::crypto::hash::hash_bytes(&manifest),
        head_record.record_hash(),
        "record_hash is the digest of the canonical record bytes"
    );
    let request = rest::types::OpRequest {
        manifest_envelope: wire_envelope(core, &core.ciphertext_hash.to_hex()),
        manifest_cbor: BASE64.encode(&manifest),
        metadata_blob: core
            .metadata_blob_hash
            .map(|_| BASE64.encode(&asset.metadata_blob)),
    };
    let response = device
        .generated(server)
        .album_lifecycle_op(core.album_id.to_string(), PROTOCOL_VERSION, None, &request)
        .await
        .unwrap_or_else(|error| panic!("the {:?} op applies: {error}", core.action))
        .into_inner();
    tracing::info!(
        asset_id = %asset_id,
        action = %response.action,
        sync_seq = response.sync_seq,
        replayed = response.replayed,
        "e2e lifecycle op applied"
    );
    response
}
