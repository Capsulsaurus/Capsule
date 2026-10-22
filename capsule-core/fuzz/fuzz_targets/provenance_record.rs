//! A provenance record off the sync feed: decoding never panics, and a record that decodes
//! round-trips through its canonical encoding.
#![no_main]

use capsule_core::cbor;
use capsule_core::crypto::provenance::ProvenanceRecord;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(record) = ProvenanceRecord::decode(data) {
        let bytes = cbor::to_canonical_vec(&record).expect("a decoded record encodes");
        let back = ProvenanceRecord::decode(&bytes).expect("a re-encoded record decodes");
        assert_eq!(back, record);
    }
});
