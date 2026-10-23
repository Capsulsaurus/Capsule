use capsule_core::derivative_format::DerivativeFormat;

use super::*;

#[test]
fn the_default_window_admits_the_protocol_version_core_speaks() {
    let policy = UploadPolicy::default();
    let current = capsule_core::crypto::primitives::PROTOCOL_VERSION;
    assert!(
        policy.protocol_min() <= current && current <= policy.protocol_max(),
        "a server that refused the protocol its own core speaks could accept no upload"
    );
}

#[test]
fn no_cutoff_is_the_build_every_client_satisfies() {
    assert_eq!(UploadPolicy::default().min_client_build(), "0.0.0");
}

#[test]
fn the_allow_list_carries_the_opaque_blob_type() {
    // Metadata, provenance and backup blobs all declare `application/octet-stream`; an
    // allow-list without it would refuse every blob but the original.
    assert!(
        UploadPolicy::default()
            .content_types()
            .contains(&"application/octet-stream")
    );
}

#[test]
fn every_committed_derivative_format_is_accepted() {
    // #470: the ladder encodes the thumbnail tier as JXL, the SDK uploads each derivative
    // with `content_type = derivative.format`, and the server answered
    // `400 error.upload.unsupported_content_type` — so every still larger than the 256 px
    // cap failed. The defect was not the missing string, it was that nothing tied the
    // accept-list to the closed set it is supposed to mirror. This is that tie.
    // The set is core's, not a restatement: a format core adds to the ladder fails here
    // until the server accepts it.
    let policy = UploadPolicy::default();
    let accepted = policy.content_types();
    for format in DerivativeFormat::STILL_DELIVERY_ORDER.map(DerivativeFormat::mime) {
        assert!(
            accepted.contains(&format),
            "{format} is a committed derivative format the ladder uploads, and the closed \
                 content-type enum refuses it"
        );
    }
}

#[test]
fn the_original_sentinel_is_not_a_content_type() {
    // It is a recognised `DerivativeManifest.format` value and nothing more: a tier that
    // references the original carries no bytes, opens no upload session, and presents no
    // `content_type`. Admitting it would widen the closed enum for a blob that cannot exist.
    let original = DerivativeFormat::Original.mime();
    assert!(
        !DerivativeFormat::STILL_DELIVERY_ORDER
            .map(DerivativeFormat::mime)
            .contains(&original)
    );
    assert!(!UploadPolicy::default().content_types().contains(&original));
}

#[test]
fn a_deployment_can_narrow_every_tunable() {
    let policy = UploadPolicy::default()
        .with_protocol_window("2026-06-01", "2026-06-30")
        .with_min_client_build("1.2.3")
        .with_content_types(["image/jpeg"])
        .with_max_file_bytes(1024)
        .with_drift_days(1);

    assert_eq!(policy.protocol_min(), "2026-06-01");
    assert_eq!(policy.protocol_max(), "2026-06-30");
    assert_eq!(policy.min_client_build(), "1.2.3");
    assert_eq!(policy.content_types(), vec!["image/jpeg"]);
    assert_eq!(policy.max_file_bytes(), 1024);
    assert_eq!(policy.drift_days(), 1);
}

#[test]
fn the_server_clock_renders_as_the_predicates_read_it() {
    let rendered = as_rfc3339(Timestamp::UNIX_EPOCH);
    assert_eq!(
        rendered.parse::<Timestamp>().expect("round trips"),
        Timestamp::UNIX_EPOCH
    );
}
