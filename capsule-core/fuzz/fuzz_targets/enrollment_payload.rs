//! A device-enrollment payload from the untrusted relay: decoding never panics, and a payload
//! that decodes is the one canonical spelling of its value.
#![no_main]

use capsule_core::enrollment::EnrollmentPayload;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(payload) = EnrollmentPayload::decode(text) {
        let encoded = payload.encode().expect("a decoded payload encodes");
        assert_eq!(encoded, text, "a non-canonical payload decoded");
    }
});
