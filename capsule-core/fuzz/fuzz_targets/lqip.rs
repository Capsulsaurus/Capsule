//! An LQIP payload read from a sidecar: parsing never panics, and a payload that parses
//! renders, as its contract promises.
#![no_main]

use capsule_core::lqip::Lqip;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(lqip) = Lqip::from_bytes(data) {
        let _ = lqip.decode_capped(64, 64);
    }
});
