//! A signed storage attestation as served by an untrusted server: decoding never panics, and an
//! attestation that decodes round-trips through its canonical encoding.
#![no_main]

use capsule_core::crypto::storage_attestation::StorageAttestation;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(attestation) = StorageAttestation::from_canonical_cbor(data) {
        let bytes = attestation.to_canonical_cbor();
        let back = StorageAttestation::from_canonical_cbor(&bytes)
            .expect("a re-encoded attestation decodes");
        assert_eq!(back, attestation);
    }
});
