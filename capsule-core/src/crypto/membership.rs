//! The album roster attestation — the one membership fact a **key-free server** can verify
//! (slice `S-C51`).
//!
//! # What the server cannot see, and what this gives it instead
//!
//! Membership of a shared album is decided inside the MLS group, and every control message
//! that adds or removes a member is AEAD-protected under a group key the server never holds.
//! `crypto::authority` can classify a commit chain as behind, ahead or forked from a server's
//! view of it, but it cannot tell the server *who is in the group*. So the server has no
//! roster — and without one it cannot answer "may this account read this album's blobs", which
//! is why the blob route and the album write routes have been owner-only.
//!
//! The [`SignedAlbumRoster`] is the album owner's **statement** of the roster, signed by one of
//! the owner account's devices. The server verifies it against the owner's published
//! [`DeviceDirectory`] — the same trust anchor `S-C42` established and the same check
//! [`SignedUpgradeIntent::verify`](crate::crypto::upgrade::SignedUpgradeIntent::verify) runs
//! for the upgrade ceremony — and then holds it as a *transport control*: who may fetch which
//! bytes. It is **not** a confidentiality control. A former member who kept the AMK for an
//! epoch can still decrypt what they already downloaded; what the roster does is stop the
//! server handing them anything further, exactly as design/federation.md says an unshare cuts
//! read access to the historical photos at the transport level.
//!
//! # A full document, versioned, and the owner is implicit
//!
//! The roster is the **whole** member list every time, with a strictly monotonic
//! [`AlbumRoster::roster_version`] — the same shape as the device directory's
//! `directory_version` (invariant 23). One monotonic field gives idempotency, replay-safety and
//! ordering at once, and it needs no per-grant ids and no separate revocation artefact:
//! removal is *absence* at a higher version. The owner account is never listed, because the
//! owner's access is the album record's `owner_id` fact and a roster that could omit the owner
//! would be a roster that could lock the owner out.
//!
//! # Why this lives here and not in the server crate
//!
//! A client signs it. The DSK that signs a roster is on a device, so the type must be
//! constructible and signable without the server crate — which is the rule `crypto::upgrade`
//! records for the upgrade intent, for the same reason: a structure defined at both ends is one
//! added field away from a signature that stops verifying.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::keys::{AmkVersion, DeviceDirectory, HybridSignature, HybridSigningKey};

/// What went wrong encoding or verifying an album roster.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MembershipError {
    /// The roster could not be canonically encoded for signing.
    #[error("the album roster could not be encoded: {0}")]
    Encode(String),
    /// The signature did not verify, or the attesting device is not a live device of the album
    /// owner's account.
    #[error("the album roster's attester signature did not verify: {0}")]
    Attester(&'static str),
}

/// What a member may do with the album's contents, as far as the server is concerned.
///
/// Two values only. The finer MLS-side distinctions (admin, for one) never reach the server,
/// which needs exactly this: whether to serve bytes, and whether to accept them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    /// May read the album's blobs and its sync feed.
    Reader,
    /// May read, and may add or change assets under the album owner's namespace.
    Writer,
}

/// One member of an album, as the roster names them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterMember {
    /// The member's account.
    pub user_id: Uuid,
    /// What the member may do.
    pub role: MemberRole,
}

/// The signed-over content of a roster attestation. Every field is covered by the attesting
/// device's DSK hybrid signature in the enclosing [`SignedAlbumRoster`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumRoster {
    /// The album this roster is for.
    pub album_id: Uuid,
    /// Strictly monotonic per album. A server refuses a version at or below the one it holds
    /// unless the bytes are identical (a replay), so a roster can neither be rolled back nor
    /// silently replaced.
    pub roster_version: u64,
    /// The AMK epoch the group is at after the commit this roster reflects. Non-decreasing
    /// across versions; the server records the epoch at which a member was granted and the one
    /// at which they vanished.
    pub amk_epoch: AmkVersion,
    /// The album owner's account. The server anchors on this account's published device
    /// directory, and refuses a roster whose owner is not the album's.
    pub attested_by_user: Uuid,
    /// The owner-account device whose DSK signed this roster. Must be present and **not
    /// revoked** in the owner's directory.
    pub attested_by_device: Uuid,
    /// RFC 3339 time the client produced the roster. Audit-only: the server orders by
    /// `roster_version`, never by this.
    pub attested_at: String,
    /// Everyone other than the owner who may read the album, and what they may do. Absence at a
    /// higher version *is* removal.
    pub members: Vec<RosterMember>,
}

impl AlbumRoster {
    /// The canonical-CBOR signing bytes the attesting device's DSK covers.
    ///
    /// # Errors
    ///
    /// Returns [`MembershipError::Encode`] if the roster cannot be canonically encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, MembershipError> {
        crate::cbor::to_canonical_vec(self).map_err(|e| MembershipError::Encode(e.to_string()))
    }
}

/// An [`AlbumRoster`] plus the attesting device's DSK **hybrid** signature over it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAlbumRoster {
    /// The attested roster.
    pub roster: AlbumRoster,
    /// The attesting DSK's hybrid signature over [`AlbumRoster::signing_bytes`].
    pub attester_sig: HybridSignature,
}

impl SignedAlbumRoster {
    /// Sign `roster` with the attesting device's DSK.
    ///
    /// The caller is responsible for `roster.attested_by_device` naming the device `dsk`
    /// belongs to; [`verify`](Self::verify) is what checks it, on the other end.
    ///
    /// # Errors
    ///
    /// Returns [`MembershipError::Encode`] if the roster cannot be canonically encoded.
    pub fn sign(roster: AlbumRoster, dsk: &HybridSigningKey) -> Result<Self, MembershipError> {
        let attester_sig = dsk.sign(&roster.signing_bytes()?);
        Ok(Self {
            roster,
            attester_sig,
        })
    }

    /// Verify the attester's DSK hybrid signature (Ed25519 **and** ML-DSA) against the album
    /// owner's published device directory.
    ///
    /// Stricter than the upgrade intent's check in one respect: a device the directory has
    /// **revoked** may not attest a roster, whatever it signed before. The entry is retained so
    /// that older manifests stay verifiable; it is not a licence to keep issuing new documents.
    ///
    /// # Errors
    ///
    /// Returns [`MembershipError::Attester`] when the directory names a different account, does
    /// not hold the attesting device, holds it revoked, or the signature does not verify under
    /// its DSK.
    pub fn verify(&self, directory: &DeviceDirectory) -> Result<(), MembershipError> {
        if directory.core.user_id != self.roster.attested_by_user {
            return Err(MembershipError::Attester(
                "the roster's attested_by_user is not this directory's account",
            ));
        }
        let entry =
            directory
                .device(&self.roster.attested_by_device)
                .ok_or(MembershipError::Attester(
                    "the attesting device is not in the directory",
                ))?;
        if entry.revoked_at.is_some() {
            return Err(MembershipError::Attester("the attesting device is revoked"));
        }
        if !entry
            .dsk_public
            .verify(&self.roster.signing_bytes()?, &self.attester_sig)
        {
            return Err(MembershipError::Attester(
                "the attester's DSK signature does not verify",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::{DeviceEntry, DirectoryCore};

    const OWNER: Uuid = Uuid::from_u128(0xA11CE);
    const DEVICE: Uuid = Uuid::from_u128(0xD1);
    const ALBUM: Uuid = Uuid::from_u128(0xA1B);
    const BOB: Uuid = Uuid::from_u128(0xB0B);
    const CAROL: Uuid = Uuid::from_u128(0xCA501);

    fn ik() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
    }

    fn dsk() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
    }

    fn other_dsk() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32])
    }

    fn directory_for(user_id: Uuid, revoked: bool) -> DeviceDirectory {
        DirectoryCore {
            user_id,
            directory_version: 1,
            updated_at: "2026-09-01T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id: DEVICE,
                dsk_public: dsk().verifying_key(),
                dek_public: None,
                added_at: "2026-09-01T00:00:00Z".into(),
                revoked_at: revoked.then(|| "2026-09-02T00:00:00Z".to_owned()),
            }],
        }
        .sign(&ik())
    }

    fn roster() -> AlbumRoster {
        AlbumRoster {
            album_id: ALBUM,
            roster_version: 1,
            amk_epoch: AmkVersion(1),
            attested_by_user: OWNER,
            attested_by_device: DEVICE,
            attested_at: "2026-09-02T00:00:00Z".into(),
            members: vec![
                RosterMember {
                    user_id: BOB,
                    role: MemberRole::Writer,
                },
                RosterMember {
                    user_id: CAROL,
                    role: MemberRole::Reader,
                },
            ],
        }
    }

    #[test]
    fn a_roster_signed_by_a_live_owner_device_verifies() {
        let signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
        assert_eq!(signed.verify(&directory_for(OWNER, false)), Ok(()));
    }

    #[test]
    fn the_signed_roster_round_trips_through_canonical_cbor() {
        // The server stores the document verbatim and the SDK ships it base64-encoded, so the
        // bytes must decode back to a value that still verifies.
        let signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
        let bytes = crate::cbor::to_canonical_vec(&signed).expect("encodes");
        let decoded: SignedAlbumRoster = crate::cbor::from_slice(&bytes).expect("decodes");
        assert_eq!(decoded, signed);
        assert_eq!(decoded.verify(&directory_for(OWNER, false)), Ok(()));
        // And the signing bytes are stable: the same roster encodes to the same bytes.
        assert_eq!(
            roster().signing_bytes().expect("encodes"),
            decoded.roster.signing_bytes().expect("encodes")
        );
    }

    #[test]
    fn a_directory_of_another_account_is_refused() {
        let signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
        assert_eq!(
            signed.verify(&directory_for(BOB, false)),
            Err(MembershipError::Attester(
                "the roster's attested_by_user is not this directory's account"
            ))
        );
    }

    #[test]
    fn a_device_the_directory_does_not_hold_is_refused() {
        let mut unknown = roster();
        unknown.attested_by_device = Uuid::from_u128(0xD2);
        let signed = SignedAlbumRoster::sign(unknown, &dsk()).expect("signs");
        assert_eq!(
            signed.verify(&directory_for(OWNER, false)),
            Err(MembershipError::Attester(
                "the attesting device is not in the directory"
            ))
        );
    }

    #[test]
    fn a_revoked_device_may_not_attest_a_roster() {
        // Stricter than the upgrade intent: the entry is retained so old manifests verify, not
        // so the device can keep issuing new documents.
        let signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
        assert_eq!(
            signed.verify(&directory_for(OWNER, true)),
            Err(MembershipError::Attester("the attesting device is revoked"))
        );
    }

    #[test]
    fn a_tampered_member_role_does_not_verify() {
        let mut signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
        signed.roster.members[1].role = MemberRole::Writer;
        assert_eq!(
            signed.verify(&directory_for(OWNER, false)),
            Err(MembershipError::Attester(
                "the attester's DSK signature does not verify"
            ))
        );
    }

    #[test]
    fn a_tampered_version_or_epoch_does_not_verify() {
        let refused = Err(MembershipError::Attester(
            "the attester's DSK signature does not verify",
        ));
        let mut bumped = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
        bumped.roster.roster_version = 2;
        assert_eq!(bumped.verify(&directory_for(OWNER, false)), refused);

        let mut rolled = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
        rolled.roster.amk_epoch = AmkVersion(2);
        assert_eq!(rolled.verify(&directory_for(OWNER, false)), refused);
    }

    #[test]
    fn a_signature_by_the_wrong_key_does_not_verify() {
        // The right device id, the wrong DSK: what a member forging the owner's attestation
        // looks like.
        let signed = SignedAlbumRoster::sign(roster(), &other_dsk()).expect("signs");
        assert_eq!(
            signed.verify(&directory_for(OWNER, false)),
            Err(MembershipError::Attester(
                "the attester's DSK signature does not verify"
            ))
        );
    }

    #[test]
    fn roles_encode_as_their_snake_case_tokens() {
        let bytes = crate::cbor::to_canonical_vec(&MemberRole::Reader).expect("encodes");
        let value: ciborium::Value = ciborium::from_reader(bytes.as_slice()).expect("decodes");
        assert_eq!(value, ciborium::Value::Text("reader".into()));
    }
}
