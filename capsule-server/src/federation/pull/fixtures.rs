//! Test fixtures for the pull's tests: really signed provenance records, and feed pages built
//! from them the way a home server serves them.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
use capsule_core::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, AssetManifest, KeyMode, ManifestCore, ProvenanceRecord,
};
use jiff::Timestamp;

use super::remote::rest::types::SyncPageResponse;
use crate::store::AlbumId;

/// The album every fixture lives in.
pub(crate) fn album_uuid() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x0190_a5b0_0000_7000_8000_0000_0000_0002)
}

/// [`album_uuid`] as the store names it.
pub(crate) fn album() -> AlbumId {
    AlbumId::new(album_uuid().to_string())
}

/// The album's pinned protocol date, inside the default window.
pub(crate) const PIN: &str = "2026-05-31";

/// The clock every fixture is validated at.
pub(crate) fn now() -> Timestamp {
    "2026-06-01T00:00:00Z".parse().expect("an instant")
}

/// The asset numbered `n`.
pub(crate) fn asset(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(0x0190_a5b0_0000_7000_8000_0000_0001_0000 + n)
}

/// The metadata blob every fixture's manifest commits to.
pub(crate) const METADATA: &[u8] = b"an encrypted metadata blob";

/// The original every fixture's manifest commits to.
pub(crate) fn original() -> Hash32 {
    hash_bytes(b"an original")
}

/// A manifest core for `asset`, taking `action` on top of `prior`, at `amk`.
pub(crate) fn core(
    asset: uuid::Uuid,
    action: Action,
    prior: Option<Hash32>,
    amk: u32,
) -> ManifestCore {
    ManifestCore {
        version: ASSET_MANIFEST_VERSION.to_owned(),
        crypto_suite_id: 1,
        protocol_version: PIN.to_owned(),
        file_id: asset,
        album_id: album_uuid(),
        amk_version: AmkVersion(amk),
        ciphertext_hash: original(),
        plaintext_size: 1024,
        chunk_size: 65_536,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: action.binds_metadata_blob().then(|| hash_bytes(METADATA)),
        created_by_user: uuid::Uuid::from_u128(7),
        created_by_device: uuid::Uuid::from_u128(8),
        client_version: "capsule-cli/0.1.0".to_owned(),
        timestamp: "2026-05-31T12:00:00Z".to_owned(),
        action,
        prior_provenance_hash: prior,
        retention_until: (action == Action::Delete).then(|| "2026-07-01T00:00:00Z".to_owned()),
        upgraded_from: None,
        unknown: Default::default(),
    }
}

/// The provenance blob a client would upload for `core`: its record, canonically encoded.
pub(crate) fn record(core: ManifestCore) -> Vec<u8> {
    let device = HybridSigningKey::from_seed64(&[1; 64]);
    let write_tier = HybridSigningKey::from_seed64(&[2; 64]);
    let asset = core.file_id;
    let prior = core.prior_provenance_hash;
    let manifest: AssetManifest = core.sign(&device, &write_tier).expect("a manifest signs");
    capsule_core::cbor::to_canonical_vec(&ProvenanceRecord::new(asset, manifest, prior))
        .expect("a record encodes")
}

/// One feed entry for `asset` at `sync_seq`, serving `record`, as an album page serves it to a
/// peer.
///
/// A `deleted` entry is the album page's tombstone: its record and no byte reference — no
/// metadata blob, an empty blob list and `original_held: false`. Only the owner's own feed names
/// the blobs a restore needs on a tombstone, and a peer never reads that feed.
pub(crate) fn entry(
    asset: uuid::Uuid,
    sync_seq: u64,
    change: &str,
    record: Option<&[u8]>,
) -> serde_json::Value {
    let deleted = change == "deleted";
    let blobs = if deleted {
        serde_json::json!([])
    } else {
        serde_json::json!([
            { "role": "original", "hash": original().to_hex(), "size": 2048 },
            { "role": "metadata", "hash": hash_bytes(METADATA).to_hex(), "size": METADATA.len() },
        ])
    };
    serde_json::json!({
        "asset_id": asset.to_string(),
        "album_id": album_uuid().to_string(),
        "protocol_version": PIN,
        "sync_seq": sync_seq,
        "change": change,
        "manifest_cbor": record.map(|bytes| BASE64.encode(bytes)),
        "metadata_blob": (!deleted).then(|| hash_bytes(METADATA).to_hex()),
        "blobs": blobs,
        "derivative_manifests": [],
        "original_held": !deleted,
        "changed_at": "2026-05-31T12:00:00Z",
    })
}

/// A page of `entries`, as the generated client decodes it.
pub(crate) fn page(
    entries: Vec<serde_json::Value>,
    next_cursor: &str,
    has_more: bool,
) -> SyncPageResponse {
    serde_json::from_value(page_json(entries, next_cursor, has_more)).expect("a page decodes")
}

/// A page of `entries`, as the wire carries it.
pub(crate) fn page_json(
    entries: Vec<serde_json::Value>,
    next_cursor: &str,
    has_more: bool,
) -> serde_json::Value {
    serde_json::json!({ "entries": entries, "next_cursor": next_cursor, "has_more": has_more })
}
