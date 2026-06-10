//! A deterministic, admin-signature-backed [`AlbumAuthority`] used in place of a live
//! OpenMLS group (deferred). It is an **admin-signed epoch ledger**: each epoch entry
//! binds `(album_id, epoch, write_tier_pubkey)` under the album's admin-tier signing key,
//! plus a local-only flag for whether that epoch's AMK content key has arrived.
//!
//! Guardrails that keep the deferral honest:
//! - The admin signature on every entry is **mandatory** and verified by
//!   [`admin_chain_verifies`](AlbumAuthority::admin_chain_verifies); a forged or unsigned
//!   entry makes the whole authority untrusted.
//! - The epoch ceiling is **data, not honor system**: it is the max attested epoch and
//!   never regresses across `attest_epoch` calls.
//! - Minting an epoch sets the write-tier key and (optionally) the AMK presence **together**
//!   — the design's "AMK epoch bump + write-tier rotation are one commit" atomicity.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::AlbumAuthority;
use crate::cbor;
use crate::crypto::keys::{AmkVersion, HybridSignature, HybridSigningKey, HybridVerifyingKey};

/// The canonical bytes an admin signs to attest one epoch.
#[derive(Serialize)]
struct EpochAttestation {
    album_id: Uuid,
    epoch: u32,
    #[serde(with = "serde_bytes")]
    write_tier_pub: Vec<u8>,
}

struct Entry {
    write_tier_pub: HybridVerifyingKey,
    admin_sig: HybridSignature,
    amk_present: bool,
}

/// The portable, admin-signed epoch ledger: every attested `(epoch, write_tier_pub)` with its
/// admin signature. `amk_present` is **local-only** state and deliberately is *not* part of this
/// signed artifact — on reload it is restored from the epochs a device actually holds AMKs for.
#[derive(Clone, Serialize, Deserialize)]
pub struct SignedEpochLedger {
    /// Album this ledger speaks for.
    pub album_id: Uuid,
    /// The admin-tier public key every entry is signed under.
    pub admin_pub: HybridVerifyingKey,
    /// The attested epoch ceiling (must equal the max attested epoch).
    pub ceiling: u32,
    /// One entry per attested epoch.
    pub entries: Vec<LedgerEntry>,
}

/// One epoch's signed attestation within a [`SignedEpochLedger`].
#[derive(Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Epoch number (`amk_version`).
    pub epoch: u32,
    /// The write-tier public key attested for this epoch.
    pub write_tier_pub: HybridVerifyingKey,
    /// The admin signature over `(album_id, epoch, write_tier_pub)`.
    pub admin_sig: HybridSignature,
}

/// A reference album authority backed by an admin-signed epoch ledger.
pub struct ReferenceAuthority {
    album_id: Uuid,
    admin_pub: HybridVerifyingKey,
    ceiling: AmkVersion,
    entries: BTreeMap<u32, Entry>,
}

fn attestation_bytes(
    album_id: Uuid,
    epoch: AmkVersion,
    write_tier_pub: &HybridVerifyingKey,
) -> Vec<u8> {
    cbor::to_canonical_vec(&EpochAttestation {
        album_id,
        epoch: epoch.0,
        write_tier_pub: write_tier_pub.to_bytes(),
    })
    .expect("attestation serializes")
}

impl ReferenceAuthority {
    /// An empty authority for `album_id` whose attestations are signed by `admin`.
    pub fn new(album_id: Uuid, admin_pub: HybridVerifyingKey) -> Self {
        Self {
            album_id,
            admin_pub,
            ceiling: AmkVersion(0),
            entries: BTreeMap::new(),
        }
    }

    /// Attest an epoch: bind `(album_id, epoch, write_tier_pub)` with an admin signature and
    /// record whether this epoch's AMK is locally held. Advances the ceiling monotonically.
    /// The `admin` key's public half must match this authority's `admin_pub`.
    pub fn attest_epoch(
        &mut self,
        admin: &HybridSigningKey,
        epoch: AmkVersion,
        write_tier_pub: &HybridVerifyingKey,
        amk_present: bool,
    ) {
        debug_assert_eq!(
            admin.verifying_key(),
            self.admin_pub,
            "attesting admin key must match the authority's admin public key"
        );
        let admin_sig = admin.sign(&attestation_bytes(self.album_id, epoch, write_tier_pub));
        self.entries.insert(
            epoch.0,
            Entry {
                write_tier_pub: write_tier_pub.clone(),
                admin_sig,
                amk_present,
            },
        );
        if epoch > self.ceiling {
            self.ceiling = epoch;
        }
    }

    /// Builder-style variant of [`attest_epoch`](Self::attest_epoch).
    pub fn with_epoch(
        mut self,
        admin: &HybridSigningKey,
        epoch: AmkVersion,
        write_tier_pub: &HybridVerifyingKey,
        amk_present: bool,
    ) -> Self {
        self.attest_epoch(admin, epoch, write_tier_pub, amk_present);
        self
    }

    /// Mark an epoch's AMK content key as now locally held (e.g. an in-flight
    /// `AlbumKeyDistribution` arrived), flipping a *pending* asset to verifiable.
    pub fn mark_amk_present(&mut self, epoch: AmkVersion) {
        if let Some(e) = self.entries.get_mut(&epoch.0) {
            e.amk_present = true;
        }
    }

    /// Export the admin-signed epoch ledger for persistence or transport. The local-only
    /// AMK-presence flags are excluded — they are not part of the signed history.
    pub fn to_ledger(&self) -> SignedEpochLedger {
        SignedEpochLedger {
            album_id: self.album_id,
            admin_pub: self.admin_pub.clone(),
            ceiling: self.ceiling.0,
            entries: self
                .entries
                .iter()
                .map(|(epoch, e)| LedgerEntry {
                    epoch: *epoch,
                    write_tier_pub: e.write_tier_pub.clone(),
                    admin_sig: e.admin_sig.clone(),
                })
                .collect(),
        }
    }

    /// Rebuild an authority from a serialized ledger, **re-verifying the entire admin chain**.
    /// `held_epochs` are the epochs whose AMK content key this device actually holds, restoring
    /// the local-only `amk_present` state. Returns `None` if the admin chain does not verify (a
    /// tampered, forged, or rewound ledger).
    pub fn from_ledger(ledger: &SignedEpochLedger, held_epochs: &BTreeSet<u32>) -> Option<Self> {
        let entries = ledger
            .entries
            .iter()
            .map(|le| {
                (
                    le.epoch,
                    Entry {
                        write_tier_pub: le.write_tier_pub.clone(),
                        admin_sig: le.admin_sig.clone(),
                        amk_present: held_epochs.contains(&le.epoch),
                    },
                )
            })
            .collect();
        let authority = Self {
            album_id: ledger.album_id,
            admin_pub: ledger.admin_pub.clone(),
            ceiling: AmkVersion(ledger.ceiling),
            entries,
        };
        authority.admin_chain_verifies().then_some(authority)
    }
}

impl AlbumAuthority for ReferenceAuthority {
    fn album_id(&self) -> Uuid {
        self.album_id
    }

    fn epoch_ceiling(&self) -> AmkVersion {
        self.ceiling
    }

    fn write_tier_pubkey(&self, epoch: AmkVersion) -> Option<HybridVerifyingKey> {
        self.entries.get(&epoch.0).map(|e| e.write_tier_pub.clone())
    }

    fn has_amk(&self, epoch: AmkVersion) -> bool {
        self.entries.get(&epoch.0).is_some_and(|e| e.amk_present)
    }

    fn admin_chain_verifies(&self) -> bool {
        // Ceiling must equal the highest attested epoch (no fabricated/rewound ceiling)...
        let max = self.entries.keys().copied().max().unwrap_or(0);
        if self.ceiling.0 != max {
            return false;
        }
        // ...and every entry's admin signature must verify against admin_pub.
        self.entries.iter().all(|(epoch, e)| {
            let bytes = attestation_bytes(self.album_id, AmkVersion(*epoch), &e.write_tier_pub);
            self.admin_pub.verify(&bytes, &e.admin_sig)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Uuid, HybridSigningKey, HybridSigningKey, HybridSigningKey) {
        // album, admin key, write-tier epoch 1 key, write-tier epoch 2 key
        (
            Uuid::from_u128(0xA1),
            HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]),
            HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]),
            HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]),
        )
    }

    #[test]
    fn lookups_reflect_attested_epochs() {
        let (album, admin, w1, w2) = setup();
        let auth = ReferenceAuthority::new(album, admin.verifying_key())
            .with_epoch(&admin, AmkVersion(1), &w1.verifying_key(), true)
            .with_epoch(&admin, AmkVersion(2), &w2.verifying_key(), false);

        assert!(auth.admin_chain_verifies());
        assert_eq!(auth.epoch_ceiling(), AmkVersion(2));
        assert_eq!(
            auth.write_tier_pubkey(AmkVersion(1)),
            Some(w1.verifying_key())
        );
        assert_eq!(
            auth.write_tier_pubkey(AmkVersion(2)),
            Some(w2.verifying_key())
        );
        assert_eq!(auth.write_tier_pubkey(AmkVersion(3)), None);
        // Epoch 1 AMK held; epoch 2 not yet (pending territory).
        assert!(auth.has_amk(AmkVersion(1)));
        assert!(!auth.has_amk(AmkVersion(2)));
    }

    #[test]
    fn mark_amk_present_flips_pending() {
        let (album, admin, w1, _) = setup();
        let mut auth = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &w1.verifying_key(),
            false,
        );
        assert!(!auth.has_amk(AmkVersion(1)));
        auth.mark_amk_present(AmkVersion(1));
        assert!(auth.has_amk(AmkVersion(1)));
    }

    #[test]
    fn forged_attestation_fails_admin_chain() {
        let (album, admin, w1, _) = setup();
        let mut auth = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &w1.verifying_key(),
            true,
        );
        // Tamper an entry's signature: the admin chain must no longer verify.
        auth.entries.get_mut(&1).unwrap().admin_sig =
            HybridSigningKey::from_seed_bytes(&[9; 32], &[9; 32]).sign(b"not the attestation");
        assert!(!auth.admin_chain_verifies());
    }

    #[test]
    fn attestation_signed_by_wrong_admin_fails() {
        let (album, admin, w1, _) = setup();
        let imposter = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
        // Build with the imposter signing, but claim the real admin's public key.
        let mut auth = ReferenceAuthority::new(album, admin.verifying_key());
        // Bypass the debug_assert by inserting a wrongly-signed entry directly.
        let wt = w1.verifying_key();
        let sig = imposter.sign(&attestation_bytes(album, AmkVersion(1), &wt));
        auth.entries.insert(
            1,
            Entry {
                write_tier_pub: wt,
                admin_sig: sig,
                amk_present: true,
            },
        );
        auth.ceiling = AmkVersion(1);
        assert!(
            !auth.admin_chain_verifies(),
            "an attestation not signed by the declared admin must be rejected"
        );
    }

    #[test]
    fn ceiling_inconsistent_with_entries_fails() {
        let (album, admin, w1, _) = setup();
        let mut auth = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &w1.verifying_key(),
            true,
        );
        // Fabricate a higher ceiling than any attested epoch.
        auth.ceiling = AmkVersion(5);
        assert!(!auth.admin_chain_verifies());
    }

    #[test]
    fn ledger_round_trip_reverifies_and_restores_amk_presence() {
        let (album, admin, w1, w2) = setup();
        let auth = ReferenceAuthority::new(album, admin.verifying_key())
            .with_epoch(&admin, AmkVersion(1), &w1.verifying_key(), true)
            .with_epoch(&admin, AmkVersion(2), &w2.verifying_key(), false);

        // Serialize → canonical CBOR → deserialize preserves the ledger.
        let bytes = cbor::to_canonical_vec(&auth.to_ledger()).unwrap();
        let decoded: SignedEpochLedger = cbor::from_slice(&bytes).unwrap();

        // Rebuild holding only epoch 1's AMK locally (presence is restored out-of-band).
        let restored = ReferenceAuthority::from_ledger(&decoded, &BTreeSet::from([1])).unwrap();
        assert!(restored.admin_chain_verifies());
        assert_eq!(restored.epoch_ceiling(), AmkVersion(2));
        assert_eq!(
            restored.write_tier_pubkey(AmkVersion(2)),
            Some(w2.verifying_key())
        );
        assert!(restored.has_amk(AmkVersion(1)));
        assert!(!restored.has_amk(AmkVersion(2)));
    }

    #[test]
    fn tampered_ledger_is_rejected_on_reload() {
        let (album, admin, w1, w2) = setup();
        let auth = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &w1.verifying_key(),
            true,
        );
        // Swap in a different write-tier key with no matching admin re-signature.
        let mut ledger = auth.to_ledger();
        ledger.entries[0].write_tier_pub = w2.verifying_key();
        assert!(ReferenceAuthority::from_ledger(&ledger, &BTreeSet::from([1])).is_none());
    }
}
