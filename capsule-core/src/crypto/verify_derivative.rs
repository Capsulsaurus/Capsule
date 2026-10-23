//! The derivative-manifest arm of the acknowledgement chokepoint (SSoT: [Cryptography —
//! Provenance § Derivative Provenance]).
//!
//! [`verify_derivative`] is the one path by which a client trusts a received
//! [`DerivativeManifest`] — the manifest that names the epoch, the source asset and the nonce
//! prefix a thumbnail or preview is decrypted with. "A derivative whose manifest fails
//! verification is surfaced, never silently applied": it answers with the same three-way
//! [`VerifyOutcome`] [`verify_asset`](crate::crypto::verify_asset::verify_asset) does, and holds a
//! derivative to the same two signatures:
//!
//! - `device_sig` under the generating device's entry in the published directory, dispatched on
//!   the entry's declared classical algorithm;
//! - `write_sig` under the album's write-tier key for the epoch `amk_version` selects — through
//!   the very helper `verify_asset` uses, so a derivative can never be authorized by a weaker
//!   rule than an asset.
//!
//! Two bindings an asset manifest does not need are added: `source_asset_id` must name the asset
//! the derivative is being opened for (a validly signed derivative of *another* asset is a
//! transplant), and `prior_provenance_hash` must chain to the receiver's head for
//! `(source_asset_id, role)` — the derivative-poisoning defense.
//!
//! [Cryptography — Provenance § Derivative Provenance]: https://docs/design/cryptography/provenance/#derivative-provenance

use uuid::Uuid;

use crate::crypto::authority::AlbumAuthority;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::DeviceDirectory;
use crate::crypto::primitives::SuiteId;
use crate::crypto::provenance::{DERIVATIVE_MANIFEST_VERSION, DerivativeManifest};
use crate::crypto::verify_asset::{
    PendingReason, RejectReason, VerifyOutcome, authorize_write, rfc3339_le,
};

/// Verify a received derivative manifest for the asset `asset_id`.
///
/// - `directory` is the published directory of the account whose device generated the
///   derivative; `generated_by_device` is resolved in it.
/// - `role_head` is the [`link_hash`](DerivativeManifest::link_hash) of the manifest this
///   receiver already trusts as the head of `(asset_id, role)`, or `None` when it trusts none
///   yet. A manifest whose `prior_provenance_hash` is `None` starts the role's chain and is only
///   placed when there is no head; any other must name the head exactly.
///
/// The derivative's ciphertext is not an argument: its content address is checked where the
/// bytes arrive (the blob fetch recomputes it against the address the manifest names), and a
/// manifest is verified before any byte is fetched.
///
/// Steps, in order — the first that fails decides the [`RejectReason`]:
///
/// 1. the authority's admin chain verifies ([`RejectReason::UntrustedAuthority`]);
/// 2. `version` is `derivative-manifest/v1`, and the two fields that are wire-optional only on
///    pre-binding fixtures — `amk_version` and `protocol_version` — are present
///    ([`RejectReason::Structural`]): without `amk_version` there is no key to check `write_sig`
///    under;
/// 3. `crypto_suite_id` is implemented by this build ([`RejectReason::SuiteDowngrade`]);
/// 4. `source_asset_id` is `asset_id` ([`RejectReason::WrongAsset`]);
/// 5. `generated_by_device` is in `directory` ([`RejectReason::UnknownDevice`]) and was added no
///    later than `generated_at` ([`RejectReason::DeviceAddedAfter`] /
///    [`RejectReason::BadTimestamp`]);
/// 6. `device_sig` verifies under that entry's key and declared algorithm
///    ([`RejectReason::BadDeviceSig`]);
/// 7. the epoch is within the attested ceiling and `write_sig` verifies under its write-tier key
///    ([`RejectReason::WrongEpoch`] / [`RejectReason::BadWriteSig`]);
/// 8. chain placement against `role_head` ([`RejectReason::Replayed`] for a chain start when a
///    head exists, [`RejectReason::ForgedChain`] for any other mismatch);
/// 9. accept iff the epoch's AMK is held locally, else
///    [`VerifyOutcome::Pending`]`(`[`PendingReason::AmkNotYetLocal`]`)`.
#[tracing::instrument(
    level = "debug",
    skip_all,
    fields(%asset_id, role = ?manifest.core.role, device = %manifest.core.generated_by_device)
)]
pub fn verify_derivative(
    manifest: &DerivativeManifest,
    asset_id: Uuid,
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    role_head: Option<Hash32>,
) -> VerifyOutcome {
    let outcome = verify_steps(manifest, asset_id, directory, authority, role_head);
    match outcome {
        VerifyOutcome::Accept => tracing::trace!("derivative manifest accepted"),
        VerifyOutcome::Pending(reason) => tracing::debug!(?reason, "derivative manifest pending"),
        VerifyOutcome::TerminalReject(reason) => {
            tracing::debug!(?reason, "derivative manifest rejected");
        }
    }
    outcome
}

fn verify_steps(
    manifest: &DerivativeManifest,
    asset_id: Uuid,
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    role_head: Option<Hash32>,
) -> VerifyOutcome {
    use RejectReason::{
        BadDeviceSig, BadTimestamp, DeviceAddedAfter, ForgedChain, Replayed, Structural,
        SuiteDowngrade, UnknownDevice, UntrustedAuthority, WrongAsset,
    };
    use VerifyOutcome::TerminalReject as Reject;
    let core = &manifest.core;

    // 1. Never trust an unsigned ledger.
    if !authority.admin_chain_verifies() {
        return Reject(UntrustedAuthority);
    }
    // 2. Schema, and the two fields every real write carries.
    let Some(amk_version) = core.amk_version else {
        return Reject(Structural);
    };
    if core.version != DERIVATIVE_MANIFEST_VERSION || core.protocol_version.is_none() {
        return Reject(Structural);
    }
    // 3. Fail closed on a suite this build does not implement.
    if SuiteId::from_u16(core.crypto_suite_id).is_none() {
        return Reject(SuiteDowngrade);
    }
    // 4. A derivative of another asset is a transplant, however well it is signed.
    if core.source_asset_id != asset_id {
        return Reject(WrongAsset);
    }
    // 5. The generating device is published, and was published before it generated this.
    let Some(entry) = directory.device(&core.generated_by_device) else {
        return Reject(UnknownDevice);
    };
    match rfc3339_le(&entry.added_at, &core.generated_at) {
        None => return Reject(BadTimestamp),
        Some(false) => return Reject(DeviceAddedAfter),
        Some(true) => {}
    }
    // 6. Provenance: the device signature, under the entry's declared classical algorithm.
    let signing_bytes = manifest.signing_bytes();
    if manifest.device_sig.classical_algorithm() != entry.dsk_public.classical_algorithm()
        || !entry
            .dsk_public
            .verify(&signing_bytes, &manifest.device_sig)
    {
        return Reject(BadDeviceSig);
    }
    // 7. Authorization: the epoch write-tier signature, by the rule assets are held to.
    if let Some(reason) =
        authorize_write(amk_version, &signing_bytes, &manifest.write_sig, authority)
    {
        return Reject(reason);
    }
    // 8. Chain placement per (asset, role): the derivative-poisoning defense.
    match (core.prior_provenance_hash, role_head) {
        (None, None) => {}
        (None, Some(_)) => return Reject(Replayed),
        (Some(prior), Some(head)) if prior == head => {}
        (Some(_), _) => return Reject(ForgedChain),
    }
    // 9. Everything authorizes; accept iff the epoch's AMK is here.
    if authority.has_amk(amk_version) {
        VerifyOutcome::Accept
    } else {
        VerifyOutcome::Pending(PendingReason::AmkNotYetLocal)
    }
}

#[cfg(test)]
mod tests;
