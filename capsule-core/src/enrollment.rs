//! The wire form of the cross-device add ceremony (SSoT: [Device Enrollment § Cross-Device
//! Add]): what each device advertises, the payloads they relay, and the safety code over them.
//!
//! The server leg (code issue/redeem, the relay channel, close) carries **opaque strings**; this
//! module defines what goes inside them. The ceremony, in relay order:
//!
//! 1. enrollee B → initiator A: [`EnrollmentPayload::Commitment`] to B's [`EnrolleeKeys`];
//! 2. A → B: [`EnrollmentPayload::InitiatorKeys`], carrying a fresh nonce;
//! 3. B → A: [`EnrollmentPayload::EnrolleeKeys`], which A checks against the commitment
//!    ([`EnrolleeKeys::matches_commitment`]);
//! 4. both display [`enrollment_safety_code`] over what they hold and the user compares them;
//! 5. A admits B with `Workspace::cross_sign_device` (`capsule-library`) and sends
//!    [`EnrollmentPayload::Directory`], which B verifies under the user IK.
//!
//! The commit-before-reveal order in steps 1–3, over inputs that each carry a fresh per-ceremony
//! nonce, is what makes a twelve-digit code sound against a man in the middle (see
//! the safety-code module, `crypto::keys::safety_code`). Each side therefore draws its
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

use crate::crypto::keys::{
    DekPublic, DeviceDirectory, DeviceEntry, HybridVerifyingKey, KEY_COMMITMENT_LEN,
    commitment_matches, key_commitment, safety_code,
};

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

#[cfg(test)]
mod tests;
