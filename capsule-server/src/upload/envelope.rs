//! The envelope gate — the refuse-by-default battery every upload write passes through.
//!
//! # It is key-free, and that is the whole point
//!
//! The server holds no key, so it cannot verify a signature. What it *can* do is check
//! structure, and [`capsule_core::validation`] is where those checks live: they are pure
//! predicates, shared with the client's own `verify_asset`, and this module is a **seam onto
//! them**, not a second implementation. Every invariant below is either a call into that
//! module or a comparison between two fields of the request; nothing here decrypts, and
//! nothing here needs to.
//!
//! # The projection is never manifest bytes (`S-C30`)
//!
//! [`ManifestEnvelope`] is the *server-visible projection* of the signed manifest — the
//! unencrypted fields the server validates. It is **not** the manifest. The signed manifest
//! reaches the server as one more blob in the bundle, under
//! [`BlobRole::Provenance`](crate::store::BlobRole), is stored verbatim at its content
//! address, and is served back byte-for-byte.
//!
//! So this type is serialized in exactly one direction and for exactly one purpose: onto the
//! session record, so finalization can re-run the battery against what creation validated. It
//! is never re-encoded into CBOR, never handed to a client as a manifest, and never hashed
//! into an attestation. That re-serialization is the defect `S-C30` and `S-C31` exist to kill,
//! and this port does not carry it: nothing in this crate produces manifest bytes.
//!
//! # What the gate cannot answer alone
//!
//! Two invariants are facts about durable state rather than about the request — the album's
//! write capability and pin (6) and the device's directory `added_at` (7). They arrive
//! through [`WriteAuthority`](super::WriteAuthority) and are *arguments* to this module, which
//! keeps it pure and directly unit-testable.

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::AmkVersion;
use capsule_core::crypto::provenance::action::Action;
use capsule_core::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
use capsule_core::validation::protocol::check_suite;
use capsule_core::validation::structural::{content_type_allowed, hash_length_ok, size_in_bounds};
use capsule_core::validation::{
    EnvelopeContext, EnvelopeReject, HandshakeReject, check_manifest_envelope, protocol_gate,
};
use jiff::Timestamp;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::policy::{UploadPolicy, as_rfc3339};
use crate::store::BlobRole;

/// The server-visible mirror of the signed manifest's envelope fields, as declared at
/// `POST /v1/upload`.
///
/// Strict (`deny_unknown_fields`) like the rest of the transport JSON. The Postel asymmetry
/// the design draws — tolerant inside documents that outlive us, strict on the wire we own —
/// puts unknown-key tolerance in the *signed CBOR interiors*, never in this JSON projection.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestEnvelope {
    /// The crypto suite the blob was sealed under. Must equal the top-level declaration.
    pub crypto_suite_id: u16,
    /// The protocol date the manifest was written under (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// The album the asset belongs to. Must equal the top-level declaration.
    pub album_id: Option<String>,
    /// The asset this blob belongs to — the same id across the bundle's members.
    pub file_id: String,
    /// The album-key epoch the manifest was written under.
    pub amk_version: u32,
    /// The ciphertext content hash, lowercase hex. Must equal the top-level `hash`.
    pub ciphertext_hash: String,
    /// The plaintext length the manifest commits to.
    pub plaintext_size: u64,
    /// The STREAM plaintext chunk size.
    pub chunk_size: u32,
    /// `derived` or `wrapped`.
    pub key_mode: String,
    /// The content hash of the bundle's metadata blob, when the manifest commits to one.
    pub metadata_blob_hash: Option<String>,
    /// The account that created the asset.
    pub created_by_user: String,
    /// The device that created it, as a UUID — invariant 7's subject.
    pub created_by_device: String,
    /// The client build that wrote the manifest.
    pub client_version: String,
    /// The manifest's self-asserted RFC3339 timestamp — invariants 7 and 8's subject.
    pub timestamp: String,
    /// The lifecycle action. `create` on this surface; see [`GateReject::ActionNotAllowed`].
    pub action: String,
    /// The provenance chain position this write continues from.
    pub prior_provenance_hash: Option<String>,
    /// The retention floor the manifest carries, when it carries one.
    pub retention_until: Option<String>,
}

/// The top-level declaration a `POST /v1/upload` body makes about the blob it is opening a
/// session for.
///
/// Borrowed rather than owned: the gate reads the request, it does not take it.
#[derive(Debug, Clone, Copy)]
pub struct DeclaredBlob<'a> {
    /// The declared ciphertext length in bytes.
    pub size: u64,
    /// The declared ciphertext content hash, lowercase hex.
    pub hash: &'a str,
    /// The declared media type.
    pub content_type: &'a str,
    /// The declared crypto suite.
    pub crypto_suite_id: u16,
    /// The declared protocol date.
    pub protocol_version: &'a str,
    /// The album the blob is filed into, when it names one.
    pub album_id: Option<&'a str>,
    /// The blob's role in its bundle.
    pub blob_role: BlobRole,
}

/// The server-known state the battery is decided against.
#[derive(Debug, Clone, Copy)]
pub struct GateContext<'a> {
    /// The tunable half of the contract.
    pub policy: &'a UploadPolicy,
    /// The album's own protocol pin — never the request's own value (`S-C19`).
    pub album_pin: &'a str,
    /// When the writing device entered the uploader's published directory (invariant 7).
    pub device_added_at: Timestamp,
    /// The server's trusted clock (invariant 8).
    pub server_clock: Timestamp,
}

/// Why the gate refused.
///
/// One variant per row of the [upload protocol's error
/// taxonomy](../../../capsule-docs/src/content/docs/design/import/upload-protocol.md) that
/// this battery can decide. The HTTP status and the `error.*` code are attached where the
/// rejection becomes a response — this enum stays framework-free so it can be unit-tested
/// without a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateReject {
    /// Invariant 1: the declared `protocol_version` is not a `YYYY-MM-DD` date.
    ProtocolMalformed,
    /// Invariant 1: the declared `protocol_version` is outside the accepted window.
    ProtocolOutOfRange,
    /// Invariant 2: `crypto_suite_id` is not in the primitives inventory.
    UnknownCryptoSuite,
    /// Invariant 3: the declared hash is not lowercase hex of the suite's digest length.
    InvalidHash,
    /// Invariant 4: the declared size is zero.
    InvalidSize,
    /// Invariant 4: the declared size is past the server's per-blob ceiling.
    FileTooLarge,
    /// Invariant 5: `content_type` is outside the closed enum.
    UnsupportedContentType,
    /// Invariant 15 family: a top-level field contradicts the envelope, or an envelope field
    /// is not the shape its own schema fixes. Carries the field's name.
    EnvelopeMismatch(&'static str),
    /// Invariant 6: the album's pin is not the version this write is under.
    AlbumPinMismatch,
    /// Invariant 7: the writing device's `added_at` postdates the manifest.
    DeviceNotAuthorized,
    /// Invariant 8: the manifest timestamp is unparseable or grossly drifted.
    TimestampOutOfRange,
    /// The manifest's `action` is not one this surface accepts.
    ActionNotAllowed,
}

/// Run the create-time battery: invariants 1–8 and the 15-family consistency checks.
///
/// Ordered as the error taxonomy reads, so the *first* thing wrong with a request is the
/// thing the client is told about — a client that sent an unsupported protocol version learns
/// that, rather than learning about a content type its next release would have changed anyway.
pub fn check_create(
    declared: &DeclaredBlob<'_>,
    envelope: &ManifestEnvelope,
    context: &GateContext<'_>,
) -> Result<(), GateReject> {
    // Invariant 1. The wire handshake gates the header; this pins the body's own declaration,
    // which is what the session is created under and what finalization re-checks.
    match protocol_gate(
        declared.protocol_version,
        context.policy.protocol_min(),
        context.policy.protocol_max(),
    ) {
        Ok(()) => {}
        Err(HandshakeReject::ProtocolOutOfRange) => return Err(GateReject::ProtocolOutOfRange),
        Err(_) => return Err(GateReject::ProtocolMalformed),
    }

    // Invariant 2.
    check_suite(declared.crypto_suite_id).map_err(|_| GateReject::UnknownCryptoSuite)?;

    // Invariant 3. The length check is the suite's, so a future suite with a different digest
    // moves this without the route being touched.
    if !is_lower_hex(declared.hash)
        || !hash_length_ok(declared.crypto_suite_id, declared.hash.len() / 2)
    {
        return Err(GateReject::InvalidHash);
    }

    // Invariant 4. Zero and over-the-ceiling are different answers — `400` versus `413` —
    // because one is a nonsensical declaration and the other is a legitimate one this
    // deployment will not serve.
    if declared.size == 0 {
        return Err(GateReject::InvalidSize);
    }
    if !size_in_bounds(declared.size, context.policy.max_file_bytes()) {
        return Err(GateReject::FileTooLarge);
    }

    // Invariant 5.
    if !content_type_allowed(declared.content_type, &context.policy.content_types()) {
        return Err(GateReject::UnsupportedContentType);
    }

    // Invariant 15 family.
    check_consistency(declared, envelope)?;

    // Invariants 2, 6, 7, 8, 17, 18 over the reconstructed core.
    let core = build_manifest_core(envelope)?;
    if !core.action.is_create() {
        return Err(GateReject::ActionNotAllowed);
    }
    run_battery(&core, context)
}

/// Re-run the battery at finalization (invariant 15), against the envelope the session stored
/// and the clock and authority *now*.
///
/// Same checks, different moment: a device revoked since creation, an album closed since
/// creation, or a session that sat long enough for its timestamp to drift out of the sanity
/// window is caught here rather than committed.
pub fn check_finalize(
    envelope: &ManifestEnvelope,
    context: &GateContext<'_>,
) -> Result<(), GateReject> {
    let core = build_manifest_core(envelope)?;
    run_battery(&core, context)
}

/// The gate for a **lifecycle write** (`S-C16`): everything a `POST /albums/{id}/ops` manifest
/// must satisfy before the index is asked to apply it.
///
/// Returns the action, so the caller does not re-parse a string the gate has already resolved.
///
/// # Which invariants this decides, and which it deliberately does not
///
/// It decides 1, 2, 6, 7, 8, the 15 family, and **16** — the last by refusing every action that
/// moves blob bytes, because those are uploads by definition (`create` is `S-C1`'s surface and
/// `replace` is `S-C43`'s).
///
/// It does **not** decide **17** or **18**, and the way it does not is worth stating plainly:
/// the shared battery is handed the manifest's *own* claims as the stored values, so both
/// predicates pass here unconditionally. That is not a hole, it is the layering. Those two are
/// the only invariants whose answer depends on stored state, so answering them from a read
/// taken outside the index's critical section would be answering them on facts that can change
/// before the write lands — which is exactly the double-apply the chain check exists to catch.
/// [`crate::index::AssetIndex::apply_op`] decides them where the comparison and the write are
/// one operation.
///
/// # Errors
///
/// Returns the first [`GateReject`] the manifest fails.
pub fn check_op(
    envelope: &ManifestEnvelope,
    context: &GateContext<'_>,
) -> Result<Action, GateReject> {
    match protocol_gate(
        &envelope.protocol_version,
        context.policy.protocol_min(),
        context.policy.protocol_max(),
    ) {
        Ok(()) => {}
        Err(HandshakeReject::ProtocolOutOfRange) => return Err(GateReject::ProtocolOutOfRange),
        Err(_) => return Err(GateReject::ProtocolMalformed),
    }

    // Invariant 2, which `check_create` gets from the top-level declaration a lifecycle write
    // does not carry.
    check_suite(envelope.crypto_suite_id).map_err(|_| GateReject::UnknownCryptoSuite)?;

    let core = build_manifest_core(envelope)?;
    // Invariant 16, this surface's half. `create` and `replace` both move blob bytes and are
    // therefore uploads; every other action in the closed enum belongs here. Written as an
    // allow-list so that a new action added to core's enum fails here rather than silently
    // becoming a lifecycle op nobody decided it was.
    if !matches!(
        core.action,
        Action::Delete
            | Action::TrashRestore
            | Action::MetadataUpdate
            | Action::DerivativeAdd
            | Action::DerivativeReplace
    ) {
        return Err(GateReject::ActionNotAllowed);
    }

    let device_added_at = as_rfc3339(context.device_added_at);
    let server_clock = as_rfc3339(context.server_clock);
    let mut ctx = envelope_context(context, &device_added_at, &server_clock);
    // The pass-through described above. Written as an assignment rather than folded into
    // `envelope_context` so that it is visible at the one call site it applies to.
    ctx.stored_chain_head = core.prior_provenance_hash;
    ctx.stored_amk_version = Some(core.amk_version.0);
    map_reject(check_manifest_envelope(&core, &ctx))?;
    Ok(core.action)
}

/// The 15-family check: the strict top-level fields must not contradict the envelope.
///
/// A contradiction is a client bug that would otherwise be silently resolved in the server's
/// favour, and the resolution would be wrong half the time.
fn check_consistency(
    declared: &DeclaredBlob<'_>,
    envelope: &ManifestEnvelope,
) -> Result<(), GateReject> {
    if envelope.crypto_suite_id != declared.crypto_suite_id {
        return Err(GateReject::EnvelopeMismatch("crypto_suite_id"));
    }
    if envelope.protocol_version != declared.protocol_version {
        return Err(GateReject::EnvelopeMismatch("protocol_version"));
    }
    if envelope.album_id.as_deref() != declared.album_id {
        return Err(GateReject::EnvelopeMismatch("album_id"));
    }
    if envelope.ciphertext_hash != declared.hash {
        return Err(GateReject::EnvelopeMismatch("ciphertext_hash"));
    }

    // Invariant 25, available here for free and checked nowhere else on this surface: when the
    // blob being uploaded *is* the metadata blob, its content address is the value the
    // manifest committed to. The server never holds the bytes and never decrypts them — it
    // compares the address it is about to verify against the address the manifest signed over,
    // and finalization's hash check turns that comparison into a fact about the stored bytes.
    if declared.blob_role == BlobRole::Metadata
        && envelope.metadata_blob_hash.as_deref() != Some(declared.hash)
    {
        return Err(GateReject::EnvelopeMismatch("metadata_blob_hash"));
    }

    Ok(())
}

/// The shared battery's context, rendered from this module's own.
///
/// The two instants are rendered by the caller and borrowed here, because
/// [`EnvelopeContext`] borrows its text: the predicates it runs compare timestamps that
/// arrive as text on the wire, so the server's own `jiff` readings are rendered once, at this
/// seam, and nowhere else.
fn envelope_context<'a>(
    context: &GateContext<'a>,
    device_added_at: &'a str,
    server_clock: &'a str,
) -> EnvelopeContext<'a> {
    EnvelopeContext {
        album_pin: context.album_pin,
        device_added_at,
        server_clock,
        drift_days: context.policy.drift_days(),
        stored_chain_head: None,
        stored_amk_version: None,
    }
}

/// Run the shared keyless battery over `core`, rendering the context's instants.
fn run_battery(core: &ManifestCore, context: &GateContext<'_>) -> Result<(), GateReject> {
    let device_added_at = as_rfc3339(context.device_added_at);
    let server_clock = as_rfc3339(context.server_clock);
    map_reject(check_manifest_envelope(
        core,
        &envelope_context(context, &device_added_at, &server_clock),
    ))
}

/// Reconstruct the [`ManifestCore`] the shared battery reads.
///
/// The battery inspects `crypto_suite_id`, `protocol_version`, `timestamp`, `action`,
/// `prior_provenance_hash` and `amk_version`; every other field is a canonical placeholder,
/// because a value the battery never reads must not look like one the server knows. The
/// load-bearing wire fields are *parsed* here, so a malformed `action`, `key_mode` or
/// `prior_provenance_hash` is a named `envelope_mismatch` rather than a silent default.
fn build_manifest_core(envelope: &ManifestEnvelope) -> Result<ManifestCore, GateReject> {
    let key_mode: KeyMode =
        parse_wire_enum(&envelope.key_mode).ok_or(GateReject::EnvelopeMismatch("key_mode"))?;
    let action: Action =
        parse_wire_enum(&envelope.action).ok_or(GateReject::EnvelopeMismatch("action"))?;
    let prior_provenance_hash = match &envelope.prior_provenance_hash {
        Some(hex) => Some(
            Hash32::from_hex(hex)
                .map_err(|_| GateReject::EnvelopeMismatch("prior_provenance_hash"))?,
        ),
        None => None,
    };
    // Parsed but not carried into the core: the battery does not read it, and invariant 7
    // resolves the device through the authority long before this point. Parsing it here is
    // what makes "the manifest names a device" a checkable claim rather than a string.
    let _ = created_by_device(envelope)?;

    Ok(ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: envelope.crypto_suite_id,
        protocol_version: envelope.protocol_version.clone(),
        file_id: Uuid::nil(),
        album_id: Uuid::nil(),
        amk_version: AmkVersion(envelope.amk_version),
        ciphertext_hash: Hash32([0u8; 32]),
        plaintext_size: envelope.plaintext_size,
        chunk_size: envelope.chunk_size,
        nonce_prefix: [0u8; 7],
        key_mode,
        wrapped_file_key: None,
        metadata_blob_hash: None,
        created_by_user: Uuid::nil(),
        created_by_device: Uuid::nil(),
        client_version: envelope.client_version.clone(),
        timestamp: envelope.timestamp.clone(),
        action,
        prior_provenance_hash,
        retention_until: envelope.retention_until.clone(),
    })
}

/// The device the manifest says wrote it, as a `Uuid`.
///
/// Public because invariant 7's *other* half — asking the directory when that device was
/// added — happens in the route, and both halves must name the same device.
pub fn created_by_device(envelope: &ManifestEnvelope) -> Result<Uuid, GateReject> {
    Uuid::parse_str(&envelope.created_by_device)
        .ok()
        .filter(|parsed| !parsed.is_nil())
        .ok_or(GateReject::EnvelopeMismatch("created_by_device"))
}

/// Map the shared battery's verdict onto this surface's taxonomy.
fn map_reject(result: Result<(), EnvelopeReject>) -> Result<(), GateReject> {
    match result {
        Ok(()) => Ok(()),
        Err(EnvelopeReject::UnknownSuite) => Err(GateReject::UnknownCryptoSuite),
        Err(EnvelopeReject::AlbumPinMismatch) => Err(GateReject::AlbumPinMismatch),
        Err(EnvelopeReject::DeviceAddedAfter) => Err(GateReject::DeviceNotAuthorized),
        Err(EnvelopeReject::TimestampUnsane) => Err(GateReject::TimestampOutOfRange),
        // A create carrying a chain position is a contradiction of the action it declares:
        // there is nothing for a first write to continue from.
        Err(EnvelopeReject::StaleChain) => {
            Err(GateReject::EnvelopeMismatch("prior_provenance_hash"))
        }
        Err(EnvelopeReject::AmkRegressed) => Err(GateReject::EnvelopeMismatch("amk_version")),
        // Unreachable from this surface: the metadata-blob predicate runs over bytes, and this
        // battery is never handed any. The consistency check above covers invariant 25 here.
        Err(EnvelopeReject::MetadataBlobHashMismatch) => {
            Err(GateReject::EnvelopeMismatch("metadata_blob_hash"))
        }
    }
}

/// Parse a bare wire enum value (`"create"`, `"derived"`) into its serde type.
fn parse_wire_enum<T: serde::de::DeserializeOwned>(value: &str) -> Option<T> {
    serde_json::from_str::<T>(&format!("\"{value}\"")).ok()
}

/// True if `text` is a non-empty, even-length, all-lowercase-hex string.
fn is_lower_hex(text: &str) -> bool {
    !text.is_empty()
        && text.len().is_multiple_of(2)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use capsule_core::crypto::CRYPTO_SUITE_ID;
    use capsule_core::crypto::primitives::PROTOCOL_VERSION;

    use super::*;

    /// A hash that is what invariant 3 demands: 64 lowercase hex characters.
    const CIPHERTEXT_HASH: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    const ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60";
    const DEVICE: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f";
    const WRITTEN_AT: &str = "2026-05-31T00:00:00Z";

    fn at(text: &str) -> Timestamp {
        text.parse().expect("the literal is a timestamp")
    }

    fn envelope() -> ManifestEnvelope {
        ManifestEnvelope {
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.to_owned(),
            album_id: Some(ALBUM.to_owned()),
            file_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61".to_owned(),
            amk_version: 1,
            ciphertext_hash: CIPHERTEXT_HASH.to_owned(),
            plaintext_size: 10,
            chunk_size: 65_536,
            key_mode: "derived".to_owned(),
            metadata_blob_hash: None,
            created_by_user: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e62".to_owned(),
            created_by_device: DEVICE.to_owned(),
            client_version: "capsule-cli/0.1.0".to_owned(),
            timestamp: WRITTEN_AT.to_owned(),
            action: "create".to_owned(),
            prior_provenance_hash: None,
            retention_until: None,
        }
    }

    fn declared(envelope: &ManifestEnvelope, role: BlobRole) -> DeclaredBlob<'_> {
        DeclaredBlob {
            size: 4096,
            hash: &envelope.ciphertext_hash,
            content_type: "image/jpeg",
            crypto_suite_id: envelope.crypto_suite_id,
            protocol_version: &envelope.protocol_version,
            album_id: envelope.album_id.as_deref(),
            blob_role: role,
        }
    }

    fn policy() -> UploadPolicy {
        UploadPolicy::default()
    }

    fn context(policy: &UploadPolicy) -> GateContext<'_> {
        GateContext {
            policy,
            album_pin: PROTOCOL_VERSION,
            device_added_at: at("2026-05-30T00:00:00Z"),
            server_clock: at("2026-05-31T01:00:00Z"),
        }
    }

    /// Run the create battery over a request whose top-level declaration the test adjusted.
    ///
    /// [`DeclaredBlob`] borrows the envelope, so the envelope is built first and the view is
    /// adjusted after — which is also the shape a route has: one body, two views of it.
    fn check_declared(mutate: impl FnOnce(&mut DeclaredBlob<'_>)) -> Result<(), GateReject> {
        let envelope = envelope();
        let policy = policy();
        let mut view = declared(&envelope, BlobRole::Original);
        mutate(&mut view);
        check_create(&view, &envelope, &context(&policy))
    }

    #[test]
    fn a_well_formed_create_passes_every_invariant() {
        assert_eq!(check_declared(|_| {}), Ok(()));
    }

    #[test]
    fn invariant_1_refuses_a_protocol_version_it_does_not_speak() {
        let policy = policy();
        let envelope = envelope();

        let mut out_of_range = envelope.clone();
        out_of_range.protocol_version = "2020-01-01".to_owned();
        let mut view = declared(&out_of_range, BlobRole::Original);
        view.protocol_version = &out_of_range.protocol_version;
        assert_eq!(
            check_create(&view, &out_of_range, &context(&policy)),
            Err(GateReject::ProtocolOutOfRange)
        );

        let mut malformed = envelope;
        malformed.protocol_version = "2026-5-31".to_owned();
        let mut view = declared(&malformed, BlobRole::Original);
        view.protocol_version = &malformed.protocol_version;
        assert_eq!(
            check_create(&view, &malformed, &context(&policy)),
            Err(GateReject::ProtocolMalformed)
        );
    }

    #[test]
    fn invariant_2_refuses_a_suite_it_does_not_implement() {
        let policy = policy();
        let mut envelope = envelope();
        envelope.crypto_suite_id = 0x9999;
        let view = declared(&envelope, BlobRole::Original);
        assert_eq!(
            check_create(&view, &envelope, &context(&policy)),
            Err(GateReject::UnknownCryptoSuite)
        );
    }

    #[test]
    fn invariant_3_refuses_a_hash_that_is_not_the_suites_digest() {
        let policy = policy();
        for bad in [
            "1111",
            "111111111111111111111111111111111111111111111111111111111111111",
            "AAAA111111111111111111111111111111111111111111111111111111111111",
            "zzzz111111111111111111111111111111111111111111111111111111111111",
            "",
        ] {
            let mut envelope = envelope();
            envelope.ciphertext_hash = bad.to_owned();
            let view = declared(&envelope, BlobRole::Original);
            assert_eq!(
                check_create(&view, &envelope, &context(&policy)),
                Err(GateReject::InvalidHash),
                "{bad:?} is not a content hash"
            );
        }
    }

    #[test]
    fn invariant_4_refuses_zero_and_the_over_large() {
        assert_eq!(
            check_declared(|view| view.size = 0),
            Err(GateReject::InvalidSize)
        );
        assert_eq!(
            check_declared(|view| view.size = super::super::policy::DEFAULT_MAX_FILE_BYTES),
            Ok(()),
            "the ceiling is inclusive"
        );
        assert_eq!(
            check_declared(|view| view.size = super::super::policy::DEFAULT_MAX_FILE_BYTES + 1),
            Err(GateReject::FileTooLarge)
        );
    }

    #[test]
    fn invariant_5_refuses_a_content_type_outside_the_closed_enum() {
        assert_eq!(
            check_declared(|view| view.content_type = "application/x-evil"),
            Err(GateReject::UnsupportedContentType)
        );
    }

    #[test]
    fn invariant_15_refuses_every_contradiction_of_the_envelope() {
        let policy = policy();

        // crypto_suite_id: both are in the inventory, so only the disagreement is caught.
        let mut envelope = envelope();
        envelope.crypto_suite_id = CRYPTO_SUITE_ID;
        let mut view = declared(&envelope, BlobRole::Original);
        view.crypto_suite_id = CRYPTO_SUITE_ID + 1;
        assert!(matches!(
            check_create(&view, &envelope, &context(&policy)),
            Err(GateReject::UnknownCryptoSuite | GateReject::EnvelopeMismatch("crypto_suite_id"))
        ));

        // protocol_version: both inside the window, but not each other.
        let mut envelope = self::envelope();
        envelope.protocol_version = "2026-02-02".to_owned();
        let view = declared(&envelope, BlobRole::Original);
        assert_eq!(
            check_create(
                &DeclaredBlob {
                    protocol_version: PROTOCOL_VERSION,
                    ..view
                },
                &envelope,
                &context(&policy)
            ),
            Err(GateReject::EnvelopeMismatch("protocol_version"))
        );

        // album_id.
        let envelope = self::envelope();
        let view = declared(&envelope, BlobRole::Original);
        assert_eq!(
            check_create(
                &DeclaredBlob {
                    album_id: Some("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff"),
                    ..view
                },
                &envelope,
                &context(&policy)
            ),
            Err(GateReject::EnvelopeMismatch("album_id"))
        );

        // ciphertext_hash.
        const OTHER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
        assert_eq!(
            check_create(
                &DeclaredBlob {
                    hash: OTHER,
                    ..view
                },
                &envelope,
                &context(&policy)
            ),
            Err(GateReject::EnvelopeMismatch("ciphertext_hash"))
        );
    }

    #[test]
    fn invariant_25_binds_a_metadata_upload_to_the_hash_its_manifest_signed() {
        let policy = policy();

        // The manifest commits to this blob's address: accepted.
        let mut envelope = envelope();
        envelope.metadata_blob_hash = Some(CIPHERTEXT_HASH.to_owned());
        let mut view = declared(&envelope, BlobRole::Metadata);
        view.content_type = "application/octet-stream";
        assert_eq!(check_create(&view, &envelope, &context(&policy)), Ok(()));

        // The manifest commits to *a different* blob: refused.
        let mut wrong = self::envelope();
        wrong.metadata_blob_hash =
            Some("3333333333333333333333333333333333333333333333333333333333333333".to_owned());
        let mut view = declared(&wrong, BlobRole::Metadata);
        view.content_type = "application/octet-stream";
        assert_eq!(
            check_create(&view, &wrong, &context(&policy)),
            Err(GateReject::EnvelopeMismatch("metadata_blob_hash"))
        );

        // The manifest commits to nothing: a bundle carrying the blob must be committed to.
        let uncommitted = self::envelope();
        let mut view = declared(&uncommitted, BlobRole::Metadata);
        view.content_type = "application/octet-stream";
        assert_eq!(
            check_create(&view, &uncommitted, &context(&policy)),
            Err(GateReject::EnvelopeMismatch("metadata_blob_hash"))
        );

        // A non-metadata blob is not bound by it — the original's manifest may commit to a
        // metadata blob that is a different object entirely.
        let original = self::envelope();
        let view = declared(&original, BlobRole::Original);
        assert_eq!(check_create(&view, &original, &context(&policy)), Ok(()));
    }

    #[test]
    fn a_field_the_envelope_schema_fixes_is_named_when_it_is_wrong() {
        for (field, mutate) in [
            (
                "key_mode",
                (|e: &mut ManifestEnvelope| e.key_mode = "borrowed".to_owned())
                    as fn(&mut ManifestEnvelope),
            ),
            ("action", |e: &mut ManifestEnvelope| {
                e.action = "obliterate".to_owned();
            }),
            ("prior_provenance_hash", |e: &mut ManifestEnvelope| {
                e.prior_provenance_hash = Some("not-a-hash".to_owned());
            }),
            ("created_by_device", |e: &mut ManifestEnvelope| {
                e.created_by_device = "00000000-0000-0000-0000-000000000000".to_owned();
            }),
        ] {
            let policy = policy();
            let mut envelope = envelope();
            mutate(&mut envelope);
            let view = declared(&envelope, BlobRole::Original);
            assert_eq!(
                check_create(&view, &envelope, &context(&policy)),
                Err(GateReject::EnvelopeMismatch(field)),
                "a bad {field} must be named"
            );
        }
    }

    #[test]
    fn only_a_create_may_open_an_upload_session() {
        let policy = policy();
        let mut envelope = envelope();
        // A well-formed, in-the-closed-set action that is not this surface's.
        envelope.action = "metadata-update".to_owned();
        envelope.prior_provenance_hash = Some(CIPHERTEXT_HASH.to_owned());
        let view = declared(&envelope, BlobRole::Original);
        assert_eq!(
            check_create(&view, &envelope, &context(&policy)),
            Err(GateReject::ActionNotAllowed)
        );
    }

    #[test]
    fn a_create_may_not_carry_a_chain_position() {
        let policy = policy();
        let mut envelope = envelope();
        envelope.prior_provenance_hash = Some(CIPHERTEXT_HASH.to_owned());
        let view = declared(&envelope, BlobRole::Original);
        assert_eq!(
            check_create(&view, &envelope, &context(&policy)),
            Err(GateReject::EnvelopeMismatch("prior_provenance_hash"))
        );
    }

    #[test]
    fn invariant_6_compares_the_request_against_the_albums_pin_not_its_own() {
        let policy = policy();
        let envelope = envelope();
        let view = declared(&envelope, BlobRole::Original);

        // The album was provisioned under an older protocol date. The Salvo gate passed the
        // request's own version as the pin, so this case could not fail; here it does.
        let drifted = GateContext {
            album_pin: "2026-02-02",
            ..context(&policy)
        };
        assert_eq!(
            check_create(&view, &envelope, &drifted),
            Err(GateReject::AlbumPinMismatch)
        );
    }

    #[test]
    fn invariant_7_refuses_a_device_the_directory_admitted_later() {
        let policy = policy();
        let envelope = envelope();
        let view = declared(&envelope, BlobRole::Original);

        let admitted_after = GateContext {
            device_added_at: at("2026-06-01T00:00:00Z"),
            ..context(&policy)
        };
        assert_eq!(
            check_create(&view, &envelope, &admitted_after),
            Err(GateReject::DeviceNotAuthorized)
        );
    }

    #[test]
    fn invariant_8_refuses_a_grossly_drifted_timestamp() {
        let policy = policy();
        let envelope = envelope();
        let view = declared(&envelope, BlobRole::Original);

        let far_future = GateContext {
            server_clock: at("2026-12-01T00:00:00Z"),
            ..context(&policy)
        };
        assert_eq!(
            check_create(&view, &envelope, &far_future),
            Err(GateReject::TimestampOutOfRange)
        );

        let mut unparseable = self::envelope();
        unparseable.timestamp = "sometime last tuesday".to_owned();
        let view = declared(&unparseable, BlobRole::Original);
        assert_eq!(
            check_create(&view, &unparseable, &context(&policy)),
            Err(GateReject::TimestampOutOfRange)
        );
    }

    #[test]
    fn finalization_re_runs_the_battery_against_the_moment_it_runs_at() {
        let policy = policy();
        let envelope = envelope();

        assert_eq!(check_finalize(&envelope, &context(&policy)), Ok(()));

        // The album's pin moved, or the session was created against a different album, since
        // creation: the write does not land.
        let closed = GateContext {
            album_pin: "2026-02-02",
            ..context(&policy)
        };
        assert_eq!(
            check_finalize(&envelope, &closed),
            Err(GateReject::AlbumPinMismatch)
        );

        // The device left the directory since creation — the authority answers with a floor
        // that postdates the manifest.
        let revoked = GateContext {
            device_added_at: at("2026-06-01T00:00:00Z"),
            ..context(&policy)
        };
        assert_eq!(
            check_finalize(&envelope, &revoked),
            Err(GateReject::DeviceNotAuthorized)
        );
    }

    #[test]
    fn the_envelope_is_strict_about_fields_it_does_not_know() {
        let json = serde_json::to_string(&envelope()).expect("the projection serializes");
        let with_extra = json.replace("{\"", "{\"surprise\":1,\"");
        assert!(
            serde_json::from_str::<ManifestEnvelope>(&with_extra).is_err(),
            "an unknown envelope field is a client bug, not a value to ignore"
        );
        assert_eq!(
            serde_json::from_str::<ManifestEnvelope>(&json).expect("round trips"),
            envelope(),
            "the projection round-trips, which is what the session record stores"
        );
    }

    #[test]
    fn the_device_the_manifest_names_is_parsed_once() {
        assert_eq!(
            created_by_device(&envelope()),
            Ok(Uuid::parse_str(DEVICE).expect("the literal is a uuid"))
        );

        let mut nil = envelope();
        nil.created_by_device = "00000000-0000-0000-0000-000000000000".to_owned();
        assert_eq!(
            created_by_device(&nil),
            Err(GateReject::EnvelopeMismatch("created_by_device"))
        );
    }
}
