//! A custody receipt as served by an untrusted server: decoding never panics, and a receipt
//! that decodes round-trips through its canonical encoding.
#![no_main]

use capsule_core::crypto::receipts::CustodyReceipt;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(receipt) = CustodyReceipt::from_canonical_cbor(data) {
        let bytes = receipt.to_canonical_cbor();
        let back =
            CustodyReceipt::from_canonical_cbor(&bytes).expect("a re-encoded receipt decodes");
        assert_eq!(back, receipt);
    }
});
