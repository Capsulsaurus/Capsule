//! The signed documents (sidecar, album record, stack record) over arbitrary bytes: decoding
//! never panics, and a document that decodes re-encodes to bytes that decode to itself.
#![no_main]

use capsule_core::sidecar::{
    ALBUM_DOC_SCHEMA_V1, AlbumDocV1, SIDECAR_SCHEMA_V1, STACK_DOC_SCHEMA_V1, SidecarV1, StackDocV1,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(doc) = SidecarV1::from_canonical_slice(data, SIDECAR_SCHEMA_V1) {
        let bytes = doc.to_canonical_vec();
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1)
            .expect("a re-encoded sidecar decodes");
        assert_eq!(back.to_canonical_vec(), bytes);
    }
    if let Ok(doc) = AlbumDocV1::from_canonical_slice(data, ALBUM_DOC_SCHEMA_V1) {
        let bytes = doc.to_canonical_vec();
        let back = AlbumDocV1::from_canonical_slice(&bytes, ALBUM_DOC_SCHEMA_V1)
            .expect("a re-encoded album record decodes");
        assert_eq!(back.to_canonical_vec(), bytes);
    }
    if let Ok(doc) = StackDocV1::from_canonical_slice(data, STACK_DOC_SCHEMA_V1) {
        let bytes = doc.to_canonical_vec();
        let back = StackDocV1::from_canonical_slice(&bytes, STACK_DOC_SCHEMA_V1)
            .expect("a re-encoded stack record decodes");
        assert_eq!(back.to_canonical_vec(), bytes);
    }
});
