//! Canonical CBOR over arbitrary bytes: decoding never panics, and whatever decodes
//! canonicalizes to a fixed point.
#![no_main]

use capsule_core::cbor;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(once) = cbor::canonicalize(data) {
        let twice = cbor::canonicalize(&once).expect("canonical bytes decode");
        assert_eq!(twice, once, "canonical encoding is not a fixed point");
    }
});
