//! A hybrid verifying key as published by a peer or a server: parsing never panics, and a key
//! that parses round-trips byte-identically.
#![no_main]

use capsule_core::crypto::keys::HybridVerifyingKey;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(key) = HybridVerifyingKey::from_bytes(data) {
        assert_eq!(key.to_bytes(), data, "a parsed key re-encodes differently");
    }
});
