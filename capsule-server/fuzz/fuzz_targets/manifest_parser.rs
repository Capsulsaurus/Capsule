//! The server's one manifest parser over arbitrary bytes: decoding never panics, and a manifest
//! that decodes re-encodes to bytes that decode to the same signed core.
#![no_main]

use capsule_server::ledger::manifest;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(parsed) = manifest::parse(data) {
        let _ = manifest::check_structure(&parsed);
        let bytes = capsule_core::cbor::to_canonical_vec(&parsed).expect("a manifest encodes");
        let back = manifest::parse(&bytes).expect("a re-encoded manifest decodes");
        assert_eq!(back.core.signing_bytes(), parsed.core.signing_bytes());
    }
});
