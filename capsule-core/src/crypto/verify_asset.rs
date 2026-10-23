//! The single asset-acknowledgement chokepoint (SSoT: [Keys — Write Authorization]).
//!
//! `verify_asset` is the **only** path by which a client accepts an asset into its trusted
//! set. It returns one of three outcomes — never a silent drop, never a silent accept:
//!
//! - [`VerifyOutcome::Accept`] — both signatures valid, epoch within the MLS-attested
//!   ceiling, chain head matches, AMK locally held.
//! - [`VerifyOutcome::TerminalReject`] — reader-signed / removed-writer / wrong-epoch /
//!   forged-chain / replayed / suite-downgrade / bad device sig … → quarantine.
//! - [`VerifyOutcome::Pending`] — epoch is within the attested range but its AMK content
//!   key has not arrived yet → hold and retry, never quarantine.
//!
//! Authorization authority is the album's admin-signed MLS commit chain (the
//! [`AlbumAuthority`]), never the server. It ships with an exhaustive negative-case test
//! surface, since every negative is a real damage scenario.
//!
//! [Keys — Write Authorization]: https://docs/design/cryptography/keys/#write-authorization

use uuid::Uuid;

use crate::crypto::authority::AlbumAuthority;
use crate::crypto::encryption::{blob_ciphertext_hash, open_blob};
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::{AmkVersion, DeviceDirectory, HybridSignature};
use crate::crypto::primitives::SuiteId;
use crate::crypto::provenance::AssetManifest;

/// Why a manifest was terminally rejected. Each maps to a damage scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The album authority's admin chain does not verify — untrusted state.
    UntrustedAuthority,
    /// The manifest names a different album than this authority speaks for.
    WrongAlbum,
    /// `crypto_suite_id` is not in the current inventory (downgrade / unknown).
    SuiteDowngrade,
    /// Structural rule violated (e.g. non-create with null prior; retention off a delete).
    Structural,
    /// Recomputed ciphertext hash does not match the manifest's declared hash.
    CiphertextHashMismatch,
    /// The signing device is not in the user's published directory.
    UnknownDevice,
    /// The device's `added_at` postdates the manifest timestamp (key older than itself).
    DeviceAddedAfter,
    /// A timestamp field was not valid RFC3339.
    BadTimestamp,
    /// The device signature (`device_sig`) did not verify.
    BadDeviceSig,
    /// `amk_version` exceeds the MLS-attested epoch ceiling (fabricated future epoch).
    WrongEpoch,
    /// The write-tier signature did not verify for the claimed epoch (reader-signed,
    /// removed-writer, or wrong-epoch signature).
    BadWriteSig,
    /// `prior_provenance_hash` does not match the local chain head (stale / forked / replay).
    ForgedChain,
    /// A `create` for an asset that already exists locally (replay).
    Replayed,
    /// A derivative manifest names a `source_asset_id` other than the asset it is being
    /// opened for — a derivative transplanted from another asset
    /// ([`verify_derivative`](crate::crypto::verify_derivative::verify_derivative)).
    WrongAsset,
}

/// Why a manifest is pending rather than accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReason {
    /// The epoch is attested but its AMK content key has not arrived locally yet.
    AmkNotYetLocal,
}

/// The outcome of [`verify_asset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Acknowledge the asset.
    Accept,
    /// Reject and quarantine, with a structured reason.
    TerminalReject(RejectReason),
    /// Hold and retry as MLS state catches up.
    Pending(PendingReason),
}

impl VerifyOutcome {
    /// Convenience: did verification accept?
    pub fn is_accept(self) -> bool {
        matches!(self, VerifyOutcome::Accept)
    }
}

/// `a <= b` over two RFC 3339 timestamps; `None` when either does not parse.
pub(super) fn rfc3339_le(a: &str, b: &str) -> Option<bool> {
    let pa = a.parse::<jiff::Timestamp>().ok()?;
    let pb = b.parse::<jiff::Timestamp>().ok()?;
    Some(pa <= pb)
}

/// Verify a manifest against the device directory, the album's MLS-attested authority, and
/// the local provenance chain head. The one path by which an asset is acknowledged.
///
/// - `ciphertext` is the asset's ciphertext, used to confirm the content hash (integrity);
///   it need not be decryptable here.
/// - `local_chain_head` is the hash of the current head provenance record for this asset,
///   or `None` if the asset is unknown locally.
pub fn verify_asset(
    manifest: &AssetManifest,
    ciphertext: &[u8],
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    local_chain_head: Option<Hash32>,
) -> VerifyOutcome {
    verify_asset_hashed(
        manifest,
        &hash::hash_bytes(ciphertext),
        directory,
        authority,
        local_chain_head,
    )
}

/// [`verify_asset`] over the ciphertext's SHA-256 rather than its bytes, for a caller that
/// streams a ciphertext too large to hold. Step 5 compares `ciphertext_hash` with the manifest's
/// content address, so it vouches for the bytes only as far as the caller's digest does: a
/// caller that has not hashed them yet must hash what it then reads and refuse it unless the
/// result equals this argument. Every other step is [`verify_asset`]'s, unchanged.
pub fn verify_asset_hashed(
    manifest: &AssetManifest,
    ciphertext_hash: &Hash32,
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    local_chain_head: Option<Hash32>,
) -> VerifyOutcome {
    use RejectReason::{
        CiphertextHashMismatch, Structural, SuiteDowngrade, UntrustedAuthority, WrongAlbum,
    };
    use VerifyOutcome::TerminalReject as Reject;
    let core = &manifest.core;

    // 1. The authority's own admin chain must verify — never trust an unsigned ledger.
    if !authority.admin_chain_verifies() {
        return Reject(UntrustedAuthority);
    }
    // 2. The manifest must be for the album this authority speaks for.
    if core.album_id != authority.album_id() {
        return Reject(WrongAlbum);
    }
    // 3. The crypto suite must be one this build implements (fail-closed).
    if SuiteId::from_u16(core.crypto_suite_id).is_none() {
        return Reject(SuiteDowngrade);
    }
    // 4. Structural rules (prior-hash/create coupling; retention only on delete).
    if !manifest.structural_ok() {
        return Reject(Structural);
    }
    // 5. Content integrity: the ciphertext must hash to the declared content address.
    if *ciphertext_hash != core.ciphertext_hash {
        return Reject(CiphertextHashMismatch);
    }
    // 6–8. The signing device is in this user's directory, was added before it signed, and its
    //      key produced `device_sig`.
    if let Err(reason) = verify_device_signature(manifest, directory) {
        return Reject(reason);
    }
    let signing_bytes = manifest.signing_bytes();
    // 9–11. Epoch ceiling, write-tier signature, chain placement.
    if let Some(reason) = authorize_and_place(manifest, &signing_bytes, authority, local_chain_head)
    {
        return Reject(reason);
    }
    // 12. Everything authorizes; accept iff the AMK is locally held, else hold as pending.
    if authority.has_amk(core.amk_version) {
        VerifyOutcome::Accept
    } else {
        VerifyOutcome::Pending(PendingReason::AmkNotYetLocal)
    }
}

/// Steps 6–8 of [`verify_asset`]: the manifest's signer is a device of `directory`, added no
/// later than the manifest's timestamp, and its signing key produced `device_sig`.
///
/// Public because the server's index rebuild checks exactly this against the directory version
/// its owner ledger recorded, and a second implementation of the device check would be a second
/// answer to a question the threat model allows one answer to. Revocation is not judged here: a
/// device a later directory revoked still verifies the manifests it signed before, and whether a
/// revoked entry may sign at all is the caller's rule.
///
/// # Errors
///
/// [`RejectReason::UnknownDevice`] when `directory` is another user's or does not list the
/// device, [`RejectReason::BadTimestamp`] or [`RejectReason::DeviceAddedAfter`] for step 7, and
/// [`RejectReason::BadDeviceSig`] when the signature does not verify under the entry's key.
pub fn verify_device_signature(
    manifest: &AssetManifest,
    directory: &DeviceDirectory,
) -> Result<(), RejectReason> {
    let core = &manifest.core;
    // 6. The signing device must be in this user's published directory.
    if directory.core.user_id != core.created_by_user {
        return Err(RejectReason::UnknownDevice);
    }
    let Some(entry) = directory.device(&core.created_by_device) else {
        return Err(RejectReason::UnknownDevice);
    };
    // 7. The device cannot have been added after it claims to have signed.
    match rfc3339_le(&entry.added_at, &core.timestamp) {
        None => return Err(RejectReason::BadTimestamp),
        Some(false) => return Err(RejectReason::DeviceAddedAfter),
        Some(true) => {}
    }
    // 8. The device signature must verify under the entry's **declared classical algorithm**.
    //    The directory entry's `dsk_public` is algorithm-tagged — Ed25519 (software) or
    //    ECDSA-P256 (hardware secure elements) — so verification dispatches on that tag. A
    //    signature whose classical half is a *different* algorithm than the entry declares is a
    //    cross-algorithm forgery and is rejected before the (algorithm-specific) verify.
    if manifest.device_sig.classical_algorithm() != entry.dsk_public.classical_algorithm() {
        return Err(RejectReason::BadDeviceSig);
    }
    if !entry
        .dsk_public
        .verify(&manifest.signing_bytes(), &manifest.device_sig)
    {
        return Err(RejectReason::BadDeviceSig);
    }
    Ok(())
}

/// Steps 9–11 of [`verify_asset`]: epoch ceiling, write-tier signature, chain placement.
/// `None` when all three hold. Shared with [`diagnose_misattributed_signer`] so the diagnosis
/// can never hold a record to a weaker standard than the chokepoint does.
fn authorize_and_place(
    manifest: &AssetManifest,
    signing_bytes: &[u8],
    authority: &dyn AlbumAuthority,
    local_chain_head: Option<Hash32>,
) -> Option<RejectReason> {
    let core = &manifest.core;
    // 9–10. Epoch ceiling and write-tier signature.
    if let Some(reason) = authorize_write(
        core.amk_version,
        signing_bytes,
        &manifest.write_sig,
        authority,
    ) {
        return Some(reason);
    }
    // 11. Chain placement: a create must be new; a non-create must chain to the local head.
    if core.action.is_create() {
        if local_chain_head.is_some() {
            return Some(RejectReason::Replayed);
        }
    } else if core.prior_provenance_hash.is_none() || core.prior_provenance_hash != local_chain_head
    {
        return Some(RejectReason::ForgedChain);
    }
    None
}

/// Steps 9–10 of [`verify_asset`], shared with
/// [`verify_derivative`](crate::crypto::verify_derivative::verify_derivative) so a derivative
/// is authorized by exactly the rule an asset is: the epoch must be within the MLS-attested
/// ceiling (no fabricated future epoch), and `write_sig` must verify — both hybrid halves —
/// under that epoch's attested write-tier key. Reader-signed and removed-writer records both
/// fail the second check. `None` when both hold.
pub(super) fn authorize_write(
    amk_version: AmkVersion,
    signing_bytes: &[u8],
    write_sig: &HybridSignature,
    authority: &dyn AlbumAuthority,
) -> Option<RejectReason> {
    if amk_version > authority.epoch_ceiling() {
        return Some(RejectReason::WrongEpoch);
    }
    let Some(write_pub) = authority.write_tier_pubkey(amk_version) else {
        return Some(RejectReason::WrongEpoch);
    };
    if !write_pub.verify(signing_bytes, write_sig) {
        return Some(RejectReason::BadWriteSig);
    }
    None
}

/// Diagnose a [`RejectReason::BadDeviceSig`] as the **legacy inherited-device defect** (issue
/// #475) rather than a forgery, returning the directory device that actually signed the record.
///
/// Before the fix, `sign_lifecycle` built every continuation (`delete`, `trash-restore`,
/// `metadata-update`) with `created_by_user`/`created_by_device` copied from the chain head while
/// signing with the *current* device. A record written that way from a second device of the same
/// account names a real directory device that did not sign it, so [`verify_asset`] step 8 rejects
/// it — the same verdict a forged signature earns. This function separates the two, and returns
/// `Some(signer)` only when **every** fact below holds:
///
/// 1. [`verify_asset`] itself returns `TerminalReject(BadDeviceSig)` — this never reclassifies
///    any other verdict, and never turns a rejection into an acceptance;
/// 2. the record is a continuation (not a `create`): the create paths always named their signer;
/// 3. `device_sig` verifies under a **different** device of the same published directory, under
///    that entry's declared classical algorithm, and that device was added no later than the
///    record's timestamp (steps 7–8 hold for the true signer);
/// 4. steps 9–11 hold unchanged: the epoch is attested, `write_sig` verifies under its write-tier
///    key, and the record chains to `local_chain_head`.
///
/// So `Some` means "this record would have been accepted had it named its own signer" — a
/// signature by a device the account's IK vouches for, carrying album write authority, in the
/// right chain position. A forger holding none of this account's device keys cannot produce it.
///
/// The record is **still refused**: `created_by_device` is covered by both signatures, so the
/// misattribution cannot be corrected without re-signing history, and accepting the record would
/// teach the chokepoint to trust a field it cannot verify. The caller quarantines it with the
/// distinct signal this function enables; see [provenance.md § Legacy Inherited-Device Records].
///
/// [provenance.md § Legacy Inherited-Device Records]: https://docs/design/cryptography/provenance/#legacy-inherited-device-records
pub fn diagnose_misattributed_signer(
    manifest: &AssetManifest,
    ciphertext_hash: &Hash32,
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    local_chain_head: Option<Hash32>,
) -> Option<Uuid> {
    let core = &manifest.core;
    if verify_asset_hashed(
        manifest,
        ciphertext_hash,
        directory,
        authority,
        local_chain_head,
    ) != VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    {
        return None;
    }
    if core.action.is_create() {
        return None;
    }
    let signing_bytes = manifest.signing_bytes();
    let signer = directory.core.devices.iter().find(|entry| {
        entry.device_id != core.created_by_device
            && rfc3339_le(&entry.added_at, &core.timestamp) == Some(true)
            && manifest.device_sig.classical_algorithm() == entry.dsk_public.classical_algorithm()
            && entry
                .dsk_public
                .verify(&signing_bytes, &manifest.device_sig)
    })?;
    if authorize_and_place(manifest, &signing_bytes, authority, local_chain_head).is_some() {
        return None;
    }
    tracing::debug!(
        asset_id = %core.file_id,
        named_device = %core.created_by_device,
        signing_device = %signer.device_id,
        "verify_asset: BadDeviceSig diagnosed as a legacy inherited-device record"
    );
    Some(signer.device_id)
}

/// Why a metadata blob fails to bind to its manifest and the locally-signed sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingReject {
    /// The manifest commits to no `metadata_blob_hash` — a metadata-bearing action must
    /// (`create | replace | metadata-update`); a caller should not seek a binding otherwise.
    NoManifestCommitment,
    /// The blob's content hash does not equal the manifest's committed `metadata_blob_hash`.
    /// This is the key-free half — the same comparison the server runs as [invariant 25].
    ///
    /// [invariant 25]: crate::validation::check_metadata_blob_envelope
    BlobHashMismatch,
    /// The blob did not decrypt/authenticate under the derived blob key (tampered ciphertext,
    /// wrong key, or a truncated wire).
    Undecryptable,
    /// The blob decrypted, but its plaintext is not byte-identical to the locally-signed
    /// sidecar — the client would be persisting one sidecar while its manifest commits to a
    /// different one.
    SidecarMismatch,
}

/// The outcome of the metadata↔manifest round-trip binding check (client-side; SSoT:
/// [Metadata — Local and Server Metadata Equivalence]).
///
/// This is a **distinct class** from a [`verify_asset`] terminal-reject: a manifest's two
/// signatures can be perfectly valid while the metadata blob a client was handed diverges from
/// what the manifest commits to. Per the [client-side invariant] a divergence is
/// **quarantined** — surfaced, never silently persisted — exactly like the pending→timeout and
/// terminal-reject cases funnel into quarantine, but reached from a different check.
///
/// [Metadata — Local and Server Metadata Equivalence]: https://docs/design/metadata/#local-and-server-metadata-equivalence
/// [client-side invariant]: https://docs/design/threat-model/validation/#client-side-validation-invariants
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataBinding {
    /// The blob decrypts to the byte-identical signed sidecar and hashes to the manifest field.
    Bound,
    /// The blob diverges from the manifest or the signed sidecar — quarantine, never persist.
    Quarantine(BindingReject),
}

impl MetadataBinding {
    /// Convenience: did the metadata blob bind?
    pub fn is_bound(self) -> bool {
        matches!(self, MetadataBinding::Bound)
    }
}

/// Run the metadata↔manifest round-trip equivalence check for a metadata-bearing manifest
/// (SSoT: [Metadata — Local and Server Metadata Equivalence], [Encryption — Local–server
/// equivalence]). Two facts must hold, both funnelling a failure into a
/// [quarantine](MetadataBinding::Quarantine):
///
/// 1. **Content address** — the metadata blob's content hash equals the manifest's committed
///    `metadata_blob_hash` (the key-free half; the server enforces the same value at
///    [invariant 25](crate::validation::check_metadata_blob_envelope)).
/// 2. **Round-trip** — decrypting the blob under `blob_key` yields canonical CBOR
///    byte-identical to `signed_sidecar` (the sidecar the client persists locally).
///
/// A client therefore never persists a sidecar that does not round-trip to the committed
/// `metadata_blob_hash`.
///
/// [Metadata — Local and Server Metadata Equivalence]: https://docs/design/metadata/#local-and-server-metadata-equivalence
/// [Encryption — Local–server equivalence]: https://docs/design/cryptography/encryption/#metadata-encryption
pub fn verify_metadata_binding(
    manifest: &AssetManifest,
    metadata_blob: &[u8],
    blob_key: &[u8; 32],
    signed_sidecar: &[u8],
) -> MetadataBinding {
    use MetadataBinding::Quarantine;

    let Some(committed) = manifest.core.metadata_blob_hash else {
        return Quarantine(BindingReject::NoManifestCommitment);
    };
    // (1) Key-free content address: the ciphertext blob hashes to the committed value.
    if blob_ciphertext_hash(metadata_blob) != committed {
        return Quarantine(BindingReject::BlobHashMismatch);
    }
    // (2) Round-trip: the blob decrypts to the byte-identical signed sidecar.
    match open_blob(blob_key, metadata_blob) {
        Err(_) => Quarantine(BindingReject::Undecryptable),
        Ok(plaintext) if plaintext != signed_sidecar => Quarantine(BindingReject::SidecarMismatch),
        Ok(_) => MetadataBinding::Bound,
    }
}

#[cfg(test)]
mod tests;
