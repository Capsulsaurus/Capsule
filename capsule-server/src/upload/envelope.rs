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
//! # The projection is never manifest bytes
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
//! into an attestation. That re-serialization is the defect the verbatim-manifest rule exists
//! to kill, and this port does not carry it: nothing in this crate produces manifest bytes.
//!
//! # What the gate cannot answer alone
//!
//! Two invariants are facts about durable state rather than about the request — the album's
//! write capability and pin (6) and the device's directory `added_at` (7). They arrive
//! through [`WriteAuthority`](super::WriteAuthority) and are *arguments* to this module, which
//! keeps it pure and directly unit-testable.

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::AmkVersion;
use capsule_core::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, DerivativeRole, KeyMode, ManifestCore,
};
use capsule_core::derivative_format::verify_still_role_format;
use capsule_core::validation::{
    EnvelopeContext, EnvelopeReject, HandshakeReject, check_manifest_envelope, check_suite,
    content_type_allowed, hash_length_ok, protocol_gate, size_in_bounds,
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
    ///
    /// **This names the blob this session is uploading, not the manifest's own
    /// `ciphertext_hash`.** For the original the two coincide; for a metadata or provenance
    /// session they do not, and the projection reuses the manifest's field name for a per-blob
    /// declaration. Invisible for a `create`, because the bundle is assembled in a pending row
    /// nobody can see and no member has to name another. It is not invisible for a `replace`,
    /// which is why [`Self::original_blob_hash`] exists.
    pub ciphertext_hash: String,
    /// The plaintext length the manifest commits to.
    pub plaintext_size: u64,
    /// The STREAM plaintext chunk size.
    pub chunk_size: u32,
    /// `derived` or `wrapped`.
    pub key_mode: String,
    /// The content hash of the bundle's metadata blob, when the manifest commits to one.
    pub metadata_blob_hash: Option<String>,
    /// The content hash of the bundle's **original** blob, when the manifest commits to one.
    ///
    /// The manifest's own `ciphertext_hash`, under a name that cannot be confused with
    /// [`Self::ciphertext_hash`]'s per-session meaning. Optional on the wire and **required on a
    /// `replace`**: a replace re-points roles that already have bytes, so it has to be applied
    /// as one act, and the only member of the bundle that can carry the whole change is the
    /// manifest — which therefore has to be able to name the original it commits to.
    ///
    /// A `create` may omit it. Its bundle is assembled incrementally in a row nobody can see,
    /// so no member needs to name another and requiring it would be a wire change for no gain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_blob_hash: Option<String>,
    /// The derivative's role — `thumbnail`, `preview`, or `embedding` — copied from its signed
    /// `DerivativeManifest.role`. **Required on a `derivative` blob and refused on every other**.
    ///
    /// The server cannot read the signed derivative manifest (it is key-free), so the closed
    /// still-format check the tier table requires of every receiver runs over this declared
    /// pair instead. A forged pair buys nothing: the client that verifies the signed manifest
    /// runs the same check over the signed values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivative_role: Option<String>,
    /// The derivative's format — `DerivativeManifest.format`, e.g. `image/jxl`. Required and
    /// refused exactly as [`Self::derivative_role`] is, and must equal the blob's declared
    /// `content_type`. For a `thumbnail` or `preview` an unrecognised value is
    /// [`GateReject::UnknownDerivativeFormat`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivative_format: Option<String>,
    /// The account that created the asset.
    pub created_by_user: String,
    /// The device that created it, as a UUID — invariant 7's subject.
    pub created_by_device: String,
    /// The client build that wrote the manifest.
    pub client_version: String,
    /// The manifest's self-asserted RFC3339 timestamp — invariants 7 and 8's subject.
    pub timestamp: String,
    /// The lifecycle action. `create` or `replace` on this surface — the two that move blob
    /// bytes — and see [`GateReject::ActionNotAllowed`] for the rest.
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
    /// The album's own protocol pin — never the request's own value.
    pub album_pin: &'a str,
    /// When the writing device entered the uploader's published directory (invariant 7).
    pub device_added_at: Timestamp,
    /// The server's trusted clock (invariant 8).
    pub server_clock: Timestamp,
}

/// Why the gate refused.
///
/// One variant per row of the [upload protocol's error
/// taxonomy](../../../capsule-docs/src/content/docs/design/transfer/upload.md) that
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
    /// A `replace` did not chain: `prior_provenance_hash` is absent.
    ///
    /// Distinct from a stale chain, which is a `409` decided by the index against stored state.
    /// This is a `400`: the manifest never claimed to follow anything, and every non-create
    /// action chains by definition.
    ReplaceDoesNotChain,
    /// A `replace`'s manifest does not name the bundle it commits to. Carries the
    /// field that is missing.
    ReplaceIncomplete(&'static str),
    /// A `thumbnail` or `preview` derivative declares a format outside the closed still set —
    /// the tier table's structural rejection, never a future codec to ignore. Carries the
    /// unrecognised value.
    UnknownDerivativeFormat(String),
    /// A provenance blob is not a manifest the device it names signed, under the key the
    /// uploader's published directory holds for that device ([`super::signer`]). Decided at
    /// finalization, the one moment the server holds the manifest's bytes.
    SignatureUnverified,
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
    check_shared(declared, envelope, context)?;

    // Invariants 2, 6, 7, 8, 17, 18 over the reconstructed core.
    let core = build_manifest_core(envelope)?;
    if !core.action.is_create() {
        return Err(GateReject::ActionNotAllowed);
    }
    run_battery(&core, context)
}

/// Invariants 1–5 and the 15-family consistency checks: everything an upload owes whatever it
/// is *for*.
///
/// Shared by [`check_create`] and [`check_replace`] rather than duplicated, because a second
/// copy of a battery is a second answer to a question the threat model allows one answer to —
/// the same reason `capsule_core::validation` owns the invariants rather than this crate.
fn check_shared(
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

    // The closed still-derivative format set, on receipt.
    check_derivative(declared, envelope)?;

    check_derivative_manifest(declared)
}

/// The two shape rules a `derivative_manifest` blob owes (#556), both key-free.
///
/// - It is signed CBOR with no media type of its own, so it declares the opaque type, exactly as
///   the asset's `provenance` blob does. A media type would claim the bytes are something a
///   viewer could render.
/// - It fits under the sync feed's inline cap, because the feed is where a receiver gets it: a
///   manifest the feed would omit is one no receiver could open its derivative with, so it is
///   refused here with a `413` rather than accepted and silently dropped from every page.
///
/// What it does **not** check is the manifest's content. The server holds no key, and every
/// field a receiver relies on is under the manifest's own signatures, which the receiver
/// verifies; a server-side parse would be a second, weaker reader of a signed document.
fn check_derivative_manifest(declared: &DeclaredBlob<'_>) -> Result<(), GateReject> {
    if declared.blob_role != BlobRole::DerivativeManifest {
        return Ok(());
    }
    if declared.content_type != OPAQUE_CONTENT_TYPE {
        return Err(GateReject::UnsupportedContentType);
    }
    if declared.size > crate::sync::MAX_MANIFEST_BYTES {
        return Err(GateReject::FileTooLarge);
    }
    Ok(())
}

/// The content type a blob the server may not parse declares.
const OPAQUE_CONTENT_TYPE: &str = "application/octet-stream";

/// The tier table's closed-format rule, run by the receiver: a `thumbnail` or `preview`
/// derivative whose declared format is outside the closed still set is a structural rejection.
///
/// The check is [`capsule_core::derivative_format::verify_still_role_format`] — the same one
/// the producer runs before shipping, through its key-free entry point, because the server
/// holds the envelope's declared `(role, format)` pair and never the signed manifest.
///
/// The pair is **required on a derivative and refused on anything else**. Optional on a
/// derivative would make the closed set bypassable by omission, and a pair on an original or a
/// metadata blob contradicts the role the request declares. The format must also equal the
/// declared `content_type`, which is the same value on the wire, so the two cannot disagree
/// about what the bytes are.
fn check_derivative(
    declared: &DeclaredBlob<'_>,
    envelope: &ManifestEnvelope,
) -> Result<(), GateReject> {
    if declared.blob_role != BlobRole::Derivative {
        if envelope.derivative_role.is_some() {
            return Err(GateReject::EnvelopeMismatch("derivative_role"));
        }
        if envelope.derivative_format.is_some() {
            return Err(GateReject::EnvelopeMismatch("derivative_format"));
        }
        return Ok(());
    }

    let role: DerivativeRole = envelope
        .derivative_role
        .as_deref()
        .and_then(parse_wire_enum)
        .ok_or(GateReject::EnvelopeMismatch("derivative_role"))?;
    let format = envelope
        .derivative_format
        .as_deref()
        .ok_or(GateReject::EnvelopeMismatch("derivative_format"))?;
    if format != declared.content_type {
        return Err(GateReject::EnvelopeMismatch("derivative_format"));
    }

    match verify_still_role_format(role, format) {
        Ok(_) => Ok(()),
        // `Err(format)` is an unrecognised still format: a structural rejection, not a future
        // codec.
        Err(format) => {
            tracing::debug!(
                ?role,
                %format,
                "upload gate: still-role derivative names a format outside the closed set"
            );
            Err(GateReject::UnknownDerivativeFormat(format))
        }
    }
}

/// Run the same battery for a `replace`, plus the three things a replace must say.
///
/// Everything invariants 1–8 and 15 check is identical — a replace is an upload, and the
/// keyless battery does not care what the write is *for*. What differs is entirely about
/// chaining and about the bundle:
///
/// - the action must be `replace`, because this is the surface's other accepted action and
///   admitting the rest would let a `delete` move bytes;
/// - `prior_provenance_hash` must be present, because a replace chains by definition. Absent is
///   a `400` and not a `409`: a `409` tells a client to re-read and rebase, and a manifest that
///   never claimed to follow anything cannot be rebased;
/// - and on the **provenance** session the manifest must name the bundle it commits to, because
///   that session is the one that applies the whole change.
///
/// Invariants 17 and 18 stay vacuous here, exactly as they are for a lifecycle op: the battery
/// is handed the manifest's own claims and the **index** is the authority, because a check taken
/// outside the write's critical section is a check on facts that can change before the write
/// lands. Two concurrent replaces reading the same chain head would both pass a gate-side check
/// and double-apply, which is the stale revival invariant 17 exists to catch, reintroduced by
/// the code enforcing it.
pub fn check_replace(
    declared: &DeclaredBlob<'_>,
    envelope: &ManifestEnvelope,
    context: &GateContext<'_>,
) -> Result<(), GateReject> {
    check_shared(declared, envelope, context)?;

    let core = build_manifest_core(envelope)?;
    if core.action != Action::Replace {
        return Err(GateReject::ActionNotAllowed);
    }
    if envelope.prior_provenance_hash.is_none() {
        return Err(GateReject::ReplaceDoesNotChain);
    }
    if declared.blob_role == BlobRole::Provenance {
        // The commit member. It has to be able to say what the asset will hold, because the
        // moment it lands is the moment every role moves.
        if envelope.original_blob_hash.is_none() {
            return Err(GateReject::ReplaceIncomplete("original_blob_hash"));
        }
        if envelope.metadata_blob_hash.is_none() {
            return Err(GateReject::ReplaceIncomplete("metadata_blob_hash"));
        }
    }

    // The pass-through `check_op` makes, for the same reason and written the same way: 17 and 18
    // are handed the manifest's own claims so they pass here unconditionally, and the index
    // decides them where the comparison and the write are one operation. `check_create` leaves
    // both `None` and is *correct* to — a create has no predecessor and no prior epoch, so the
    // two predicates are vacuous rather than skipped. The moment `replace` is accepted that
    // stops being true, which is the trap: leaving them `None` here would
    // compare a real `prior_provenance_hash` against nothing and refuse every replace.
    let device_added_at = as_rfc3339(context.device_added_at);
    let server_clock = as_rfc3339(context.server_clock);
    let mut ctx = envelope_context(context, &device_added_at, &server_clock);
    ctx.stored_chain_head = core.prior_provenance_hash;
    ctx.stored_amk_version = Some(core.amk_version.0);
    map_reject(check_manifest_envelope(&core, &ctx))
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
    if core.action.is_create() {
        return run_battery(&core, context);
    }
    // A `replace` re-runs the same battery under the same layering as its open-time gate: 17 and
    // 18 are handed the manifest's own claims, and the index decides them. Without the
    // pass-through this would refuse every replace at finalization — the two `None`s being
    // correct for a create and wrong the moment a second action rides this surface, which is the
    // trap, and the shape it takes when only half of it is fixed.
    let device_added_at = as_rfc3339(context.device_added_at);
    let server_clock = as_rfc3339(context.server_clock);
    let mut ctx = envelope_context(context, &device_added_at, &server_clock);
    ctx.stored_chain_head = core.prior_provenance_hash;
    ctx.stored_amk_version = Some(core.amk_version.0);
    map_reject(check_manifest_envelope(&core, &ctx))
}

/// The gate for a **lifecycle write**: everything a `POST /albums/{id}/ops` manifest
/// must satisfy before the index is asked to apply it.
///
/// Returns the action, so the caller does not re-parse a string the gate has already resolved.
///
/// # Which invariants this decides, and which it deliberately does not
///
/// It decides 1, 2, 6, 7, 8, the 15 family, and **16** — the last by refusing every action that
/// moves blob bytes, because those are uploads by definition (`create` rides the upload
/// route and `replace` the lifecycle-op route). It also refuses the derivative pair
/// (`derivative_role`, `derivative_format`), which only a derivative upload may carry.
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

    // The derivative pair describes an uploaded derivative blob, and a lifecycle write uploads
    // none, so it is refused here exactly as `check_derivative` refuses it on a non-derivative
    // upload. Accepting it silently would leave a declaration nothing ever checks.
    if envelope.derivative_role.is_some() {
        return Err(GateReject::EnvelopeMismatch("derivative_role"));
    }
    if envelope.derivative_format.is_some() {
        return Err(GateReject::EnvelopeMismatch("derivative_format"));
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
        upgraded_from: None,
        unknown: Default::default(),
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
mod tests;
