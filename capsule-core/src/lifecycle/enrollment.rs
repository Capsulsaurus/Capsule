//! The client half of cross-device add (SSoT: [Device Enrollment § Cross-Device Add]): what each
//! device advertises, the payloads they relay, and the cross-sign that admits the new device.
//!
//! The server leg (code issue/redeem, the relay channel, close) carries **opaque strings**; this
//! module defines what goes inside them. The ceremony, in relay order:
//!
//! 1. enrollee B → initiator A: [`EnrollmentPayload::Commitment`] to B's [`EnrolleeKeys`];
//! 2. A → B: [`EnrollmentPayload::InitiatorKeys`], carrying a fresh nonce;
//! 3. B → A: [`EnrollmentPayload::EnrolleeKeys`], which A checks against the commitment
//!    ([`EnrolleeKeys::matches_commitment`]);
//! 4. both display [`enrollment_safety_code`] over what they hold and the user compares them;
//! 5. A admits B with [`Workspace::cross_sign_device`] and sends
//!    [`EnrollmentPayload::Directory`], which B verifies under the user IK.
//!
//! The commit-before-reveal order in steps 1–3, over inputs that each carry a fresh per-ceremony
//! nonce, is what makes a twelve-digit code sound against a man in the middle (see
//! [the safety-code module](mod@crate::crypto::keys::safety_code)). Each side therefore draws its
//! bundle **once per ceremony** and keeps the one it sent: a second call draws a new nonce, and
//! so a different code. The master-key transfer under a
//! channel key (design step 5) is not here: the channel's ephemeral key agreement is not yet
//! defined.
//!
//! ## Wire form
//!
//! A payload is canonical CBOR of `{ "v": 1, "payload": <tagged payload> }`, written as
//! lowercase hex — the relay's field is a JSON string, and hex needs no new dependency. Decoding
//! refuses anything that does not re-encode to the exact same string, so every payload has one
//! spelling and a key set hashes to one safety code.
//!
//! [Device Enrollment § Cross-Device Add]: https://docs/design/device-enrollment/#cross-device-add

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::{Result, Workspace, now_rfc3339};
use crate::crypto::keys::{
    DekPublic, DeviceDirectory, DeviceEntry, HybridVerifyingKey, KEY_COMMITMENT_LEN,
    commitment_matches, key_commitment, safety_code,
};
use crate::crypto::rng;

/// The payload format version this build writes and the only one it reads.
pub const ENROLLMENT_PAYLOAD_VERSION: u8 = 1;

/// The longest encoded payload this build writes or reads, in characters — the relay's own
/// per-payload ceiling, so a payload this side accepts is one the relay would have carried.
pub const MAX_ENROLLMENT_PAYLOAD_CHARS: usize = 64 * 1024;

/// The length of each side's per-ceremony nonce. Device keys are static — the initiator's are
/// already published — so without fresh randomness on both sides a man in the middle could
/// predict one side's input and grind the code offline.
pub const ENROLLMENT_NONCE_LEN: usize = 32;

/// What the **initiator** (the enrolled device A) advertises: the account it is adding to, the
/// identity key the directory it will send verifies under, and its own device signing key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitiatorKeys {
    /// The account's user id.
    pub user_id: Uuid,
    /// The user identity public key — the key B verifies the cross-signed directory under.
    pub user_ik_public: HybridVerifyingKey,
    /// The initiator's device id.
    pub device_id: Uuid,
    /// The initiator's device signing public key.
    pub dsk_public: HybridVerifyingKey,
    /// Fresh random bytes drawn for this ceremony alone, which the safety code covers. Revealed
    /// only after the enrollee has committed, so a man in the middle commits toward A before it
    /// can know what A's input to the code will be.
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; ENROLLMENT_NONCE_LEN],
}

impl InitiatorKeys {
    /// The canonical bytes the safety code is derived over.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        crate::cbor::to_canonical_vec(self).expect("initiator keys serialize")
    }
}

/// What the **enrollee** (the new device B) advertises: the public halves of the device keys it
/// generated, which A cross-signs into the directory.
///
/// The DEK is required, not optional: a device admitted without one could never be wrapped to,
/// so it could not join a single album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrolleeKeys {
    /// The enrollee's device id.
    pub device_id: Uuid,
    /// The enrollee's device signing public key.
    pub dsk_public: HybridVerifyingKey,
    /// The enrollee's device encryption public key.
    pub dek_public: DekPublic,
    /// Fresh random bytes drawn for this ceremony alone, which the commitment and the safety code
    /// cover. Hidden under the commitment until A has revealed, so B's input stays unpredictable
    /// even when its keys are known (a retried ceremony has already revealed them).
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; ENROLLMENT_NONCE_LEN],
}

impl EnrolleeKeys {
    /// The canonical bytes the safety code and the commitment are derived over.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        crate::cbor::to_canonical_vec(self).expect("enrollee keys serialize")
    }

    /// The commitment B sends before A reveals its keys.
    pub fn commitment(&self) -> [u8; KEY_COMMITMENT_LEN] {
        key_commitment(&self.canonical_bytes())
    }

    /// Whether these revealed keys are the ones `commitment` was made to.
    pub fn matches_commitment(&self, commitment: &[u8; KEY_COMMITMENT_LEN]) -> bool {
        commitment_matches(commitment, &self.canonical_bytes())
    }

    /// The directory entry A cross-signs for these keys. `added_at` is the initiator's clock,
    /// never the enrollee's claim: it bounds which manifests the new device can have signed.
    pub fn into_device_entry(self, added_at: String) -> DeviceEntry {
        DeviceEntry {
            device_id: self.device_id,
            dsk_public: self.dsk_public,
            dek_public: Some(self.dek_public),
            added_at,
            revoked_at: None,
            unknown: Default::default(),
        }
    }
}

/// The safety code both devices display, over the initiator's and the enrollee's advertised
/// keys as each side received them.
pub fn enrollment_safety_code(initiator: &InitiatorKeys, enrollee: &EnrolleeKeys) -> String {
    safety_code(&initiator.canonical_bytes(), &enrollee.canonical_bytes())
}

/// One message relayed between the two devices.
///
/// Variants are held inline rather than boxed: a ceremony relays four of them, once, so the
/// size spread clippy flags costs nothing and boxing would only burden every caller's match.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentPayload {
    /// B → A, first: the commitment to B's [`EnrolleeKeys`].
    Commitment(#[serde(with = "serde_bytes")] [u8; KEY_COMMITMENT_LEN]),
    /// A → B: what A advertises.
    InitiatorKeys(InitiatorKeys),
    /// B → A, after A's keys: the keys B committed to.
    EnrolleeKeys(EnrolleeKeys),
    /// A → B, last: the cross-signed directory that admits B.
    Directory(DeviceDirectory),
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u8,
    payload: EnrollmentPayload,
}

/// Why a relayed payload could not be encoded or decoded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EnrollmentPayloadError {
    /// Longer than [`MAX_ENROLLMENT_PAYLOAD_CHARS`], or empty.
    #[error("enrollment payload is {0} characters; it must be 1..={MAX_ENROLLMENT_PAYLOAD_CHARS}")]
    Size(usize),
    /// Not hex, not CBOR, or not a payload this build knows.
    #[error("enrollment payload is malformed: {0}")]
    Malformed(String),
    /// A payload version this build does not read.
    #[error("enrollment payload version {0} is not supported")]
    UnsupportedVersion(u8),
    /// Decodes, but is not the one canonical spelling of its value.
    #[error("enrollment payload is not canonically encoded")]
    NonCanonical,
}

impl EnrollmentPayload {
    /// Encode this payload as the string the relay carries.
    pub fn encode(&self) -> std::result::Result<String, EnrollmentPayloadError> {
        let envelope = Envelope {
            v: ENROLLMENT_PAYLOAD_VERSION,
            payload: self.clone(),
        };
        let bytes = crate::cbor::to_canonical_vec(&envelope)
            .map_err(|e| EnrollmentPayloadError::Malformed(e.to_string()))?;
        let encoded = hex::encode(bytes);
        if encoded.len() > MAX_ENROLLMENT_PAYLOAD_CHARS {
            tracing::warn!(
                chars = encoded.len(),
                "enrollment: payload exceeds the relay ceiling"
            );
            return Err(EnrollmentPayloadError::Size(encoded.len()));
        }
        tracing::trace!(chars = encoded.len(), "enrollment: encoded a payload");
        Ok(encoded)
    }

    /// Decode a relayed string. The relay is untrusted, so anything but a canonical payload of
    /// this build's version is refused rather than tolerated.
    pub fn decode(encoded: &str) -> std::result::Result<Self, EnrollmentPayloadError> {
        if encoded.is_empty() || encoded.len() > MAX_ENROLLMENT_PAYLOAD_CHARS {
            return Err(EnrollmentPayloadError::Size(encoded.len()));
        }
        let bytes =
            hex::decode(encoded).map_err(|e| EnrollmentPayloadError::Malformed(e.to_string()))?;
        let value: ciborium::Value = crate::cbor::from_slice(&bytes)
            .map_err(|e| EnrollmentPayloadError::Malformed(e.to_string()))?;
        // Read the version before the body, so a newer build's payload is reported as a version
        // mismatch rather than as whatever its unfamiliar body fails to parse as.
        let version = value
            .as_map()
            .and_then(|m| m.iter().find(|(k, _)| k.as_text() == Some("v")))
            .and_then(|(_, v)| v.as_integer())
            .and_then(|v| u8::try_from(v).ok())
            .ok_or_else(|| EnrollmentPayloadError::Malformed("missing version".into()))?;
        if version != ENROLLMENT_PAYLOAD_VERSION {
            tracing::warn!(version, "enrollment: unsupported payload version");
            return Err(EnrollmentPayloadError::UnsupportedVersion(version));
        }
        let envelope: Envelope = crate::cbor::from_slice(&bytes)
            .map_err(|e| EnrollmentPayloadError::Malformed(e.to_string()))?;
        if envelope.payload.encode()? != encoded {
            tracing::warn!("enrollment: refusing a non-canonical payload");
            return Err(EnrollmentPayloadError::NonCanonical);
        }
        Ok(envelope.payload)
    }
}

impl Workspace {
    /// What this device advertises as the **initiator** of a cross-device add, with a nonce
    /// freshly drawn by this call. Call it once per ceremony and keep the result: it is both what
    /// A sends and what A computes its safety code over.
    pub fn enrollment_initiator_keys(&self) -> InitiatorKeys {
        InitiatorKeys {
            user_id: self.account.user_id,
            user_ik_public: self.account.user_ik.verifying_key(),
            device_id: self.account.device.device_id,
            dsk_public: self.device_signer.verifying_key(),
            nonce: rng::random_array(),
        }
    }

    /// What this device advertises as the **enrollee** of a cross-device add: the signing key it
    /// actually signs with (a hardware signer's, when one was supplied), its DEK, and a nonce
    /// freshly drawn by this call. Call it once per ceremony and keep the result: B commits to,
    /// reveals, and computes its safety code over this one bundle.
    pub fn enrollment_enrollee_keys(&self) -> EnrolleeKeys {
        EnrolleeKeys {
            device_id: self.account.device.device_id,
            dsk_public: self.device_signer.verifying_key(),
            dek_public: DekPublic(self.account.device.dek.public_bytes()),
            nonce: rng::random_array(),
        }
    }

    /// **Cross-sign** a new device into this account's directory: append `entry` at the next
    /// `directory_version`, re-sign under the user IK, adopt it as this workspace's directory,
    /// and return it for publication and for relaying to the new device.
    ///
    /// Call this only after the safety codes matched. Refused with
    /// [`LifecycleError::Directory`](super::LifecycleError::Directory), leaving the directory
    /// unchanged, when the entry's id or signing key is already listed or the entry is revoked.
    ///
    /// The re-signed directory is persisted beside the account before it is adopted, so
    /// [`open`](Self::open) restores it and the next directory this device signs builds on it;
    /// a failed write is a [`LifecycleError::Io`](super::LifecycleError::Io) and also leaves the
    /// directory unchanged. The caller still publishes the returned directory.
    #[tracing::instrument(skip_all, fields(device_id = %entry.device_id))]
    pub fn cross_sign_device(&mut self, entry: DeviceEntry) -> Result<DeviceDirectory> {
        let mut core = self.directory.core.clone();
        core.add_device(entry, now_rfc3339()).inspect_err(|e| {
            tracing::warn!(error = %e, "enrollment: cross-sign refused");
        })?;
        self.adopt_directory(core.sign(&self.account.user_ik))?;
        tracing::info!(
            directory_version = self.directory.core.directory_version,
            devices = self.directory.core.devices.len(),
            "enrollment: device cross-signed into the directory"
        );
        Ok(self.directory.clone())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::super::{FAST_PARAMS, LifecycleError, fast_workspace};
    use super::*;
    use crate::crypto::keys::{DirectoryError, MasterKey};

    /// A and a second device B of the same account, B built from A's master key and sealed
    /// identity the way a recovered device is.
    fn two_devices() -> (TempDir, TempDir, Workspace, Workspace) {
        let lib_a = TempDir::new().unwrap();
        let lib_b = TempDir::new().unwrap();
        let a = fast_workspace(lib_a.path());
        let b = Workspace::create_from_master(
            lib_b.path(),
            b"device b passphrase",
            FAST_PARAMS,
            MasterKey::from_bytes(*a.account.master.as_bytes()),
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap();
        (lib_a, lib_b, a, b)
    }

    /// Relay a payload through its wire form, as the server would carry it.
    fn relay(payload: &EnrollmentPayload) -> EnrollmentPayload {
        EnrollmentPayload::decode(&payload.encode().unwrap()).unwrap()
    }

    /// **The ceremony, end to end, with nothing on the relay.** The commitment opens, both
    /// codes match, A cross-signs B, and B verifies the directory it receives under the identity
    /// key A advertised — finding itself and A in it.
    #[test]
    fn an_untampered_ceremony_admits_the_new_device() {
        let (_la, _lb, mut a, b) = two_devices();
        let before = a.device_directory().core.directory_version;

        let a_keys = a.enrollment_initiator_keys();
        let b_keys = b.enrollment_enrollee_keys();
        let EnrollmentPayload::Commitment(commitment) =
            relay(&EnrollmentPayload::Commitment(b_keys.commitment()))
        else {
            panic!("commitment did not round-trip");
        };
        let EnrollmentPayload::InitiatorKeys(a_seen_by_b) =
            relay(&EnrollmentPayload::InitiatorKeys(a_keys.clone()))
        else {
            panic!("initiator keys did not round-trip");
        };
        let EnrollmentPayload::EnrolleeKeys(b_seen_by_a) =
            relay(&EnrollmentPayload::EnrolleeKeys(b_keys.clone()))
        else {
            panic!("enrollee keys did not round-trip");
        };
        assert!(b_seen_by_a.matches_commitment(&commitment));

        assert_eq!(a_seen_by_b, a_keys);
        let code_on_a = enrollment_safety_code(&a_keys, &b_seen_by_a);
        let code_on_b = enrollment_safety_code(&a_seen_by_b, &b_keys);
        assert_eq!(code_on_a, code_on_b);

        let directory = a
            .cross_sign_device(b_seen_by_a.into_device_entry("2026-06-01T00:00:00Z".into()))
            .unwrap();
        assert_eq!(directory.core.directory_version, before + 1);
        assert_eq!(a.device_directory(), &directory);

        let EnrollmentPayload::Directory(received) =
            relay(&EnrollmentPayload::Directory(directory))
        else {
            panic!("directory did not round-trip");
        };
        assert!(received.verify(&a_seen_by_b.user_ik_public));
        assert_eq!(received.core.user_id, a_seen_by_b.user_id);
        let entry = received
            .device(&b.device_id())
            .expect("B is in the directory");
        assert_eq!(entry.dsk_public, b.enrollment_enrollee_keys().dsk_public);
        assert_eq!(
            entry.dek_public.as_ref().map(DekPublic::as_bytes),
            Some(b.device_dek_public().as_slice())
        );
        assert!(received.device(&a.device_id()).is_some());
        // A peer holding only the directory can now wrap a key to B.
        let (ciphertext, sent) = entry.encapsulate().unwrap();
        assert_eq!(b.device_dek_decapsulate(&ciphertext).unwrap(), sent);
    }

    /// **The MITM abort.** A relay that swaps in its own device's keys toward A leaves the two
    /// devices showing different codes.
    #[test]
    fn a_relay_that_swaps_the_enrollee_keys_diverges_the_codes() {
        let (_la, _lb, a, b) = two_devices();
        let lib_m = TempDir::new().unwrap();
        let mallory = fast_workspace(lib_m.path());

        let a_keys = a.enrollment_initiator_keys();
        let b_keys = b.enrollment_enrollee_keys();
        let swapped = mallory.enrollment_enrollee_keys();
        assert_ne!(
            enrollment_safety_code(&a_keys, &swapped),
            enrollment_safety_code(&a_keys, &b_keys)
        );
        // And keys revealed after the fact do not open B's commitment.
        assert!(!swapped.matches_commitment(&b_keys.commitment()));
        // Nor does a relay that swaps A's keys toward B go unnoticed.
        assert_ne!(
            enrollment_safety_code(&mallory.enrollment_initiator_keys(), &b_keys),
            enrollment_safety_code(&a_keys, &b_keys)
        );
    }

    /// **The offline-grinding defence.** A's device keys are already published, so a man in the
    /// middle knows them before it commits toward A. A fresh nonce per ceremony means it still
    /// cannot predict the code A will show: the same static keys under another nonce give
    /// another code. Likewise B's nonce rides under its commitment, so B's revealed keys from an
    /// earlier attempt do not predict the next attempt's code or open its commitment.
    #[test]
    fn each_ceremony_draws_fresh_nonces_that_the_code_and_commitment_cover() {
        let (_la, _lb, a, b) = two_devices();
        let a_first = a.enrollment_initiator_keys();
        let a_second = a.enrollment_initiator_keys();
        let b_first = b.enrollment_enrollee_keys();
        let b_second = b.enrollment_enrollee_keys();

        // The static keys are the same; only the nonce is fresh.
        assert_eq!(a_first.dsk_public, a_second.dsk_public);
        assert_eq!(b_first.dsk_public, b_second.dsk_public);
        assert_ne!(a_first.nonce, a_second.nonce);
        assert_ne!(b_first.nonce, b_second.nonce);

        // A man in the middle holding A's published keys and an old nonce mispredicts the code.
        assert_ne!(
            enrollment_safety_code(&a_first, &b_first),
            enrollment_safety_code(&a_second, &b_first)
        );
        assert_ne!(
            enrollment_safety_code(&a_first, &b_first),
            enrollment_safety_code(&a_first, &b_second)
        );
        // B's commitment binds its nonce, not just its keys.
        assert_ne!(b_first.commitment(), b_second.commitment());
        assert!(!b_second.matches_commitment(&b_first.commitment()));
    }

    /// A refused cross-sign surfaces the directory's reason and leaves A's directory as it was.
    #[test]
    fn cross_signing_a_listed_device_is_refused_and_changes_nothing() {
        let (_la, _lb, mut a, b) = two_devices();
        let entry = b
            .enrollment_enrollee_keys()
            .into_device_entry("2026-06-01T00:00:00Z".into());
        a.cross_sign_device(entry.clone()).unwrap();
        let admitted = a.device_directory().clone();

        let err = a.cross_sign_device(entry).unwrap_err();
        assert!(matches!(
            err,
            LifecycleError::Directory(DirectoryError::DeviceAlreadyListed(id)) if id == b.device_id()
        ));
        assert_eq!(a.device_directory(), &admitted);

        // A's own device cannot be re-enrolled either.
        let own = a
            .enrollment_enrollee_keys()
            .into_device_entry("2026-06-01T00:00:00Z".into());
        assert!(matches!(
            a.cross_sign_device(own),
            Err(LifecycleError::Directory(
                DirectoryError::DeviceAlreadyListed(_)
            ))
        ));
        assert_eq!(a.device_directory(), &admitted);
    }

    /// **#562.** A cross-signed device and the version it bumped survive a reopen of A, and the
    /// next device A cross-signs lands on top of it rather than on a rebuilt version-1 directory.
    #[test]
    fn a_cross_signed_device_survives_a_reopen() {
        let (la, _lb, mut a, b) = two_devices();
        let admitted = a
            .cross_sign_device(
                b.enrollment_enrollee_keys()
                    .into_device_entry("2026-06-01T00:00:00Z".into()),
            )
            .unwrap();
        let directory_path = Workspace::directory_path(la.path());
        assert!(
            !crate::utils::paths::tmp_path(&directory_path).exists(),
            "the staged directory was renamed into place, not left beside it"
        );
        let a_id = a.device_id();
        drop(a);

        let mut reopened = Workspace::open(la.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(reopened.device_id(), a_id);
        assert_eq!(reopened.device_directory(), &admitted);
        assert!(reopened.device_directory().device(&b.device_id()).is_some());

        let lib_c = TempDir::new().unwrap();
        let c = fast_workspace(lib_c.path());
        let next = reopened
            .cross_sign_device(
                c.enrollment_enrollee_keys()
                    .into_device_entry("2026-06-02T00:00:00Z".into()),
            )
            .unwrap();
        assert_eq!(
            next.core.directory_version,
            admitted.core.directory_version + 1
        );
        assert!(next.device(&b.device_id()).is_some(), "B was not forgotten");
        assert!(next.device(&c.device_id()).is_some());
    }

    /// **#562.** A cross-sign that cannot be persisted is refused and adopts nothing, so memory
    /// never runs ahead of the directory a reopen would restore.
    #[test]
    fn a_cross_sign_that_cannot_be_persisted_changes_nothing() {
        let (la, _lb, mut a, b) = two_devices();
        let before = a.device_directory().clone();
        // A directory where the file belongs makes the rename fail.
        let directory_path = Workspace::directory_path(la.path());
        std::fs::remove_file(&directory_path).unwrap();
        std::fs::create_dir(&directory_path).unwrap();

        let err = a
            .cross_sign_device(
                b.enrollment_enrollee_keys()
                    .into_device_entry("2026-06-01T00:00:00Z".into()),
            )
            .unwrap_err();
        assert!(matches!(err, LifecycleError::Io(_)), "{err:?}");
        assert_eq!(a.device_directory(), &before);
    }

    /// The entry A signs carries A's clock and no revocation, whatever B might have hoped for.
    #[test]
    fn the_cross_signed_entry_is_live_and_dated_by_the_initiator() {
        let (_la, _lb, _a, b) = two_devices();
        let entry = b
            .enrollment_enrollee_keys()
            .into_device_entry("2026-06-01T00:00:00Z".into());
        assert_eq!(entry.added_at, "2026-06-01T00:00:00Z");
        assert_eq!(entry.revoked_at, None);
        assert_eq!(entry.device_id, b.device_id());
    }

    // ── Wire form ───────────────────────────────────────────────────────────────

    fn sample() -> EnrollmentPayload {
        EnrollmentPayload::Commitment([7u8; KEY_COMMITMENT_LEN])
    }

    #[test]
    fn a_payload_is_lowercase_hex_and_round_trips() {
        let encoded = sample().encode().unwrap();
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(EnrollmentPayload::decode(&encoded).unwrap(), sample());
    }

    /// The commitment is a CBOR byte string, not an array of integers — the canonical form any
    /// other implementation must produce.
    #[test]
    fn the_commitment_encodes_as_a_byte_string() {
        let bytes = hex::decode(sample().encode().unwrap()).unwrap();
        let value: ciborium::Value = crate::cbor::from_slice(&bytes).unwrap();
        let payload = value
            .as_map()
            .unwrap()
            .iter()
            .find(|(k, _)| k.as_text() == Some("payload"))
            .map(|(_, v)| v.clone())
            .unwrap();
        let inner = payload.as_map().unwrap()[0].clone();
        assert_eq!(inner.0.as_text(), Some("commitment"));
        assert!(inner.1.is_bytes());
    }

    #[test]
    fn uppercase_hex_is_refused_as_non_canonical() {
        let encoded = sample().encode().unwrap().to_uppercase();
        assert_eq!(
            EnrollmentPayload::decode(&encoded),
            Err(EnrollmentPayloadError::NonCanonical)
        );
    }

    #[test]
    fn a_non_canonical_cbor_spelling_is_refused() {
        // The same envelope with its two keys in non-canonical order.
        let value = ciborium::Value::Map(vec![
            (
                ciborium::Value::Text("payload".into()),
                ciborium::Value::Map(vec![(
                    ciborium::Value::Text("commitment".into()),
                    ciborium::Value::Bytes(vec![7u8; KEY_COMMITMENT_LEN]),
                )]),
            ),
            (
                ciborium::Value::Text("v".into()),
                ciborium::Value::Integer(1.into()),
            ),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        assert_ne!(hex::encode(&bytes), sample().encode().unwrap());
        assert_eq!(
            EnrollmentPayload::decode(&hex::encode(&bytes)),
            Err(EnrollmentPayloadError::NonCanonical)
        );
    }

    #[test]
    fn another_version_is_refused_by_number() {
        let envelope = ciborium::Value::Map(vec![
            (
                ciborium::Value::Text("v".into()),
                ciborium::Value::Integer(2.into()),
            ),
            (
                ciborium::Value::Text("payload".into()),
                ciborium::Value::Text("from the future".into()),
            ),
        ]);
        let encoded = hex::encode(crate::cbor::value_to_canonical_vec(&envelope));
        assert_eq!(
            EnrollmentPayload::decode(&encoded),
            Err(EnrollmentPayloadError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn malformed_and_oversized_payloads_are_refused() {
        for bad in ["zz", "0", "a0", "ff"] {
            assert!(
                matches!(
                    EnrollmentPayload::decode(bad),
                    Err(EnrollmentPayloadError::Malformed(_))
                ),
                "{bad}"
            );
        }
        assert_eq!(
            EnrollmentPayload::decode(""),
            Err(EnrollmentPayloadError::Size(0))
        );
        let huge = "0".repeat(MAX_ENROLLMENT_PAYLOAD_CHARS + 2);
        assert_eq!(
            EnrollmentPayload::decode(&huge),
            Err(EnrollmentPayloadError::Size(huge.len()))
        );
    }

    /// A commitment of the wrong length is not a commitment.
    #[test]
    fn a_short_commitment_is_refused() {
        let envelope = ciborium::Value::Map(vec![
            (
                ciborium::Value::Text("v".into()),
                ciborium::Value::Integer(1.into()),
            ),
            (
                ciborium::Value::Text("payload".into()),
                ciborium::Value::Map(vec![(
                    ciborium::Value::Text("commitment".into()),
                    ciborium::Value::Bytes(vec![7u8; KEY_COMMITMENT_LEN - 1]),
                )]),
            ),
        ]);
        let encoded = hex::encode(crate::cbor::value_to_canonical_vec(&envelope));
        assert!(matches!(
            EnrollmentPayload::decode(&encoded),
            Err(EnrollmentPayloadError::Malformed(_))
        ));
    }
}
