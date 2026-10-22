//! A metadata-blob wire string from the server: opening it under a fixed key never panics, and
//! arbitrary bytes never authenticate.
#![no_main]

use capsule_core::crypto::encryption::open_blob;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        open_blob(&[0x5A; 32], data).is_err(),
        "arbitrary bytes opened as a metadata blob"
    );
});
