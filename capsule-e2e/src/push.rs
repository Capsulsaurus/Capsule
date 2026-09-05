//! Pushing a library asset to the server: the SDK's ladder plus the rung it omits, and the
//! lifecycle-op posting that chains onto what the rung established.
//!
//! **The provenance rung.** The SDK's `push_bundle` ships the sealed metadata blob, every
//! derivative and the original, and stops: it never uploads a `provenance` blob. The server
//! publishes an asset to other devices only once it holds both index-tier roles — provenance
//! *and* metadata (`capsule-server/src/upload/visibility.rs`) — and the head of the server-side
//! chain is the SHA-256 of the provenance blob's bytes. So the harness uploads one more blob per
//! asset: the canonical CBOR of the chain's head `ProvenanceRecord`, whose digest is by
//! definition core's `record_hash()`. That is what lets a later lifecycle op's
//! `prior_provenance_hash` — the client's record hash of the previous record — match the head
//! the server holds. The finding is filed against the SDK; the encoding decision is recorded in
//! the pull request that landed this crate.
//!
//! Every envelope here is projected from the head manifest's [`ManifestCore`] rather than from
//! an `UploadBundle`, because a bundle re-derives the original's ciphertext and an adopted
//! (wrapped-key) asset or a tombstone head has nothing to re-derive; the projection is the same
//! one `capsule_sdk::push::envelope_for` makes, field for field.

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::provenance::manifest::ManifestCore;
use capsule_core::lifecycle::UploadBundle;
use capsule_sdk::net::ConnectionClass;
pub use capsule_sdk::push::ensure_album;
use capsule_sdk::push::{AssetPushReport, push_bundle};
use capsule_sdk::rest;
use capsule_sdk::staged::StagedScheduler;
use capsule_sdk::upload::{
    BlobRole, CreateUploadRequest, ManifestEnvelope, UploadClient, UploadOutcome,
};
use uuid::Uuid;

use crate::{Device, PROTOCOL_VERSION, Server};

/// The content type every metadata, provenance and backup blob declares.
pub const OPAQUE_CONTENT_TYPE: &str = "application/octet-stream";

/// What one push left behind.
pub struct Pushed {
    /// The bundle the library produced for the asset's current head.
    pub bundle: UploadBundle,
    /// The SDK ladder's report — metadata, derivatives, original.
    pub report: AssetPushReport,
    /// The content address of the provenance blob the harness added: the server's chain head.
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

/// Upload `bytes` as one `role` blob of the asset whose head is `core`, returning the content
/// address it landed at.
pub async fn upload_blob(
    client: &UploadClient,
    core: &ManifestCore,
    role: BlobRole,
    content_type: &str,
    bytes: &[u8],
) -> String {
    let hash = hash_bytes(bytes).to_hex();
    let request = CreateUploadRequest {
        size: bytes.len() as u64,
        hash: hash.clone(),
        content_type: content_type.to_owned(),
        crypto_suite_id: core.crypto_suite_id,
        protocol_version: core.protocol_version.clone(),
        blob_role: role,
        manifest_envelope: sdk_envelope(core, &hash),
        album_id: Some(core.album_id.to_string()),
        owner_id: None,
        intent_id: None,
    };
    let outcome = client
        .upload(&request, bytes)
        .await
        .unwrap_or_else(|error| panic!("the {role:?} blob uploads: {error}"));
    assert!(
        matches!(
            outcome,
            UploadOutcome::Completed { .. } | UploadOutcome::AlreadyStored { .. }
        ),
        "the {role:?} blob finalizes"
    );
    hash
}

/// The canonical CBOR of the chain head — the bytes the provenance blob carries.
#[must_use]
pub fn provenance_bytes(device: &Device, asset_id: &Uuid) -> Vec<u8> {
    let record = device.head_record(asset_id);
    let bytes = capsule_core::cbor::to_canonical_vec(&record).expect("a record serializes");
    debug_assert_eq!(
        hash_bytes(&bytes),
        record.record_hash(),
        "record_hash is the digest of the canonical record bytes"
    );
    bytes
}

/// Upload the provenance rung for `asset_id`, returning its content address.
pub async fn push_provenance(client: &UploadClient, device: &Device, asset_id: &Uuid) -> String {
    let core = device.head_record(asset_id).manifest.core;
    let bytes = provenance_bytes(device, asset_id);
    upload_blob(
        client,
        &core,
        BlobRole::Provenance,
        OPAQUE_CONTENT_TYPE,
        &bytes,
    )
    .await
}

/// Upload the sealed metadata blob the head manifest binds, returning its content address.
pub async fn push_metadata(client: &UploadClient, device: &Device, asset_id: &Uuid) -> String {
    let core = device.head_record(asset_id).manifest.core;
    let bytes = device
        .workspace
        .asset(asset_id)
        .expect("the asset is in the library")
        .metadata_blob
        .clone();
    let hash = upload_blob(
        client,
        &core,
        BlobRole::Metadata,
        OPAQUE_CONTENT_TYPE,
        &bytes,
    )
    .await;
    assert_eq!(
        Some(hash.as_str()),
        core.metadata_blob_hash.map(|h| h.to_hex()).as_deref(),
        "the sealed metadata blob is the one the head manifest binds"
    );
    hash
}

/// Push `asset_id` in full: the SDK ladder under `UploadPolicy::Full` on an unmetered link,
/// then the provenance rung.
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
    let provenance_hash = push_provenance(&client, device, asset_id).await;
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

/// Publish an adopted asset whose original the server already holds: the metadata blob and
/// the provenance rung — the index tier — and nothing else.
pub async fn push_index_tier(device: &Device, server: &Server, asset_id: &Uuid) -> String {
    let client = device.upload_client(server);
    push_metadata(&client, device, asset_id).await;
    push_provenance(&client, device, asset_id).await
}

/// Post the library's current chain head for `asset_id` as a lifecycle op
/// (`POST /v1/albums/{album}/ops`) through the generated client.
///
/// The op carries the head record's canonical CBOR as `manifest_cbor` — so the server's new head
/// is that record's hash — and the sealed metadata blob whenever the head manifest binds one
/// (invariant 25: a hash without its bytes, or bytes without a hash, is a `400`).
pub async fn post_lifecycle_head(
    device: &Device,
    server: &Server,
    asset_id: &Uuid,
) -> rest::types::OpResponse {
    let asset = device
        .workspace
        .asset(asset_id)
        .expect("the asset is in the library");
    let core = &asset
        .chain
        .records()
        .last()
        .expect("a chain is never empty")
        .manifest
        .core;
    let manifest = provenance_bytes(device, asset_id);
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
