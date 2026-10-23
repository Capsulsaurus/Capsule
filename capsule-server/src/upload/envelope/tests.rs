use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};

use super::*;

/// A hash that is what invariant 3 demands: 64 lowercase hex characters.
const CIPHERTEXT_HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60";
const DEVICE: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f";
const WRITTEN_AT: &str = "2026-05-31T00:00:00Z";

fn at(text: &str) -> Timestamp {
    text.parse().expect("the literal is a timestamp")
}

fn envelope() -> ManifestEnvelope {
    ManifestEnvelope {
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.to_owned(),
        album_id: Some(ALBUM.to_owned()),
        file_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61".to_owned(),
        amk_version: 1,
        ciphertext_hash: CIPHERTEXT_HASH.to_owned(),
        plaintext_size: 10,
        chunk_size: 65_536,
        key_mode: "derived".to_owned(),
        original_blob_hash: None,
        derivative_role: None,
        derivative_format: None,
        metadata_blob_hash: None,
        created_by_user: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e62".to_owned(),
        created_by_device: DEVICE.to_owned(),
        client_version: "capsule-cli/0.1.0".to_owned(),
        timestamp: WRITTEN_AT.to_owned(),
        action: "create".to_owned(),
        prior_provenance_hash: None,
        retention_until: None,
    }
}

fn declared(envelope: &ManifestEnvelope, role: BlobRole) -> DeclaredBlob<'_> {
    DeclaredBlob {
        size: 4096,
        hash: &envelope.ciphertext_hash,
        content_type: "image/jpeg",
        crypto_suite_id: envelope.crypto_suite_id,
        protocol_version: &envelope.protocol_version,
        album_id: envelope.album_id.as_deref(),
        blob_role: role,
    }
}

fn policy() -> UploadPolicy {
    UploadPolicy::default()
}

fn context(policy: &UploadPolicy) -> GateContext<'_> {
    GateContext {
        policy,
        album_pin: PROTOCOL_VERSION,
        device_added_at: at("2026-05-30T00:00:00Z"),
        server_clock: at("2026-05-31T01:00:00Z"),
    }
}

/// Run the create battery over a request whose top-level declaration the test adjusted.
///
/// [`DeclaredBlob`] borrows the envelope, so the envelope is built first and the view is
/// adjusted after — which is also the shape a route has: one body, two views of it.
fn check_declared(mutate: impl FnOnce(&mut DeclaredBlob<'_>)) -> Result<(), GateReject> {
    let envelope = envelope();
    let policy = policy();
    let mut view = declared(&envelope, BlobRole::Original);
    mutate(&mut view);
    check_create(&view, &envelope, &context(&policy))
}

#[test]
fn a_well_formed_create_passes_every_invariant() {
    assert_eq!(check_declared(|_| {}), Ok(()));
}

#[test]
fn invariant_1_refuses_a_protocol_version_it_does_not_speak() {
    let policy = policy();
    let envelope = envelope();

    let mut out_of_range = envelope.clone();
    out_of_range.protocol_version = "2020-01-01".to_owned();
    let mut view = declared(&out_of_range, BlobRole::Original);
    view.protocol_version = &out_of_range.protocol_version;
    assert_eq!(
        check_create(&view, &out_of_range, &context(&policy)),
        Err(GateReject::ProtocolOutOfRange)
    );

    let mut malformed = envelope;
    malformed.protocol_version = "2026-5-31".to_owned();
    let mut view = declared(&malformed, BlobRole::Original);
    view.protocol_version = &malformed.protocol_version;
    assert_eq!(
        check_create(&view, &malformed, &context(&policy)),
        Err(GateReject::ProtocolMalformed)
    );
}

#[test]
fn invariant_2_refuses_a_suite_it_does_not_implement() {
    let policy = policy();
    let mut envelope = envelope();
    envelope.crypto_suite_id = 0x9999;
    let view = declared(&envelope, BlobRole::Original);
    assert_eq!(
        check_create(&view, &envelope, &context(&policy)),
        Err(GateReject::UnknownCryptoSuite)
    );
}

#[test]
fn invariant_3_refuses_a_hash_that_is_not_the_suites_digest() {
    let policy = policy();
    for bad in [
        "1111",
        "111111111111111111111111111111111111111111111111111111111111111",
        "AAAA111111111111111111111111111111111111111111111111111111111111",
        "zzzz111111111111111111111111111111111111111111111111111111111111",
        "",
    ] {
        let mut envelope = envelope();
        envelope.ciphertext_hash = bad.to_owned();
        let view = declared(&envelope, BlobRole::Original);
        assert_eq!(
            check_create(&view, &envelope, &context(&policy)),
            Err(GateReject::InvalidHash),
            "{bad:?} is not a content hash"
        );
    }
}

#[test]
fn invariant_4_refuses_zero_and_the_over_large() {
    assert_eq!(
        check_declared(|view| view.size = 0),
        Err(GateReject::InvalidSize)
    );
    assert_eq!(
        check_declared(|view| view.size = super::super::policy::DEFAULT_MAX_FILE_BYTES),
        Ok(()),
        "the ceiling is inclusive"
    );
    assert_eq!(
        check_declared(|view| view.size = super::super::policy::DEFAULT_MAX_FILE_BYTES + 1),
        Err(GateReject::FileTooLarge)
    );
}

#[test]
fn invariant_5_refuses_a_content_type_outside_the_closed_enum() {
    assert_eq!(
        check_declared(|view| view.content_type = "application/x-evil"),
        Err(GateReject::UnsupportedContentType)
    );
}

#[test]
fn invariant_15_refuses_every_contradiction_of_the_envelope() {
    let policy = policy();

    // crypto_suite_id: both are in the inventory, so only the disagreement is caught.
    let mut envelope = envelope();
    envelope.crypto_suite_id = CRYPTO_SUITE_ID;
    let mut view = declared(&envelope, BlobRole::Original);
    view.crypto_suite_id = CRYPTO_SUITE_ID + 1;
    assert!(matches!(
        check_create(&view, &envelope, &context(&policy)),
        Err(GateReject::UnknownCryptoSuite | GateReject::EnvelopeMismatch("crypto_suite_id"))
    ));

    // protocol_version: both inside the window, but not each other.
    let mut envelope = self::envelope();
    envelope.protocol_version = "2026-02-02".to_owned();
    let view = declared(&envelope, BlobRole::Original);
    assert_eq!(
        check_create(
            &DeclaredBlob {
                protocol_version: PROTOCOL_VERSION,
                ..view
            },
            &envelope,
            &context(&policy)
        ),
        Err(GateReject::EnvelopeMismatch("protocol_version"))
    );

    // album_id.
    let envelope = self::envelope();
    let view = declared(&envelope, BlobRole::Original);
    assert_eq!(
        check_create(
            &DeclaredBlob {
                album_id: Some("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff"),
                ..view
            },
            &envelope,
            &context(&policy)
        ),
        Err(GateReject::EnvelopeMismatch("album_id"))
    );

    // ciphertext_hash.
    const OTHER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    assert_eq!(
        check_create(
            &DeclaredBlob {
                hash: OTHER,
                ..view
            },
            &envelope,
            &context(&policy)
        ),
        Err(GateReject::EnvelopeMismatch("ciphertext_hash"))
    );
}

#[test]
fn invariant_25_binds_a_metadata_upload_to_the_hash_its_manifest_signed() {
    let policy = policy();

    // The manifest commits to this blob's address: accepted.
    let mut envelope = envelope();
    envelope.metadata_blob_hash = Some(CIPHERTEXT_HASH.to_owned());
    let mut view = declared(&envelope, BlobRole::Metadata);
    view.content_type = "application/octet-stream";
    assert_eq!(check_create(&view, &envelope, &context(&policy)), Ok(()));

    // The manifest commits to *a different* blob: refused.
    let mut wrong = self::envelope();
    wrong.metadata_blob_hash =
        Some("3333333333333333333333333333333333333333333333333333333333333333".to_owned());
    let mut view = declared(&wrong, BlobRole::Metadata);
    view.content_type = "application/octet-stream";
    assert_eq!(
        check_create(&view, &wrong, &context(&policy)),
        Err(GateReject::EnvelopeMismatch("metadata_blob_hash"))
    );

    // The manifest commits to nothing: a bundle carrying the blob must be committed to.
    let uncommitted = self::envelope();
    let mut view = declared(&uncommitted, BlobRole::Metadata);
    view.content_type = "application/octet-stream";
    assert_eq!(
        check_create(&view, &uncommitted, &context(&policy)),
        Err(GateReject::EnvelopeMismatch("metadata_blob_hash"))
    );

    // A non-metadata blob is not bound by it — the original's manifest may commit to a
    // metadata blob that is a different object entirely.
    let original = self::envelope();
    let view = declared(&original, BlobRole::Original);
    assert_eq!(check_create(&view, &original, &context(&policy)), Ok(()));
}

/// A derivative upload with its declared pair, as the SDK sends it: the format rides both
/// the top-level `content_type` and the envelope's `derivative_format`.
fn derivative_envelope(role: &str, format: &str) -> ManifestEnvelope {
    let mut envelope = envelope();
    envelope.derivative_role = Some(role.to_owned());
    envelope.derivative_format = Some(format.to_owned());
    envelope
}

/// The default policy with one more content type admitted, so a value reaches the
/// derivative check rather than stopping at invariant 5.
fn policy_admitting(extra: &'static str) -> UploadPolicy {
    UploadPolicy::default().with_content_types(
        super::super::policy::DEFAULT_CONTENT_TYPES
            .iter()
            .copied()
            .chain([extra]),
    )
}

fn check_derivative_upload(envelope: &ManifestEnvelope) -> Result<(), GateReject> {
    let policy = policy();
    let mut view = declared(envelope, BlobRole::Derivative);
    view.content_type = envelope.derivative_format.as_deref().unwrap_or("image/jxl");
    check_create(&view, envelope, &context(&policy))
}

#[test]
fn a_still_derivative_in_the_closed_set_is_accepted() {
    for role in ["thumbnail", "preview"] {
        assert_eq!(
            check_derivative_upload(&derivative_envelope(role, "image/jxl")),
            Ok(()),
            "a {role} in JPEG XL is the format this build produces"
        );
    }
}

#[test]
fn a_still_derivative_outside_the_closed_set_is_a_structural_rejection() {
    // Invariant 5 runs first and its enum is the policy's, so each value is admitted there
    // and reaches the derivative check; the refusal is the closed set's, not the content
    // type's.
    let unknown = "image/future-codec";
    let policy = policy_admitting(unknown);
    for role in ["thumbnail", "preview"] {
        let envelope = derivative_envelope(role, unknown);
        let mut view = declared(&envelope, BlobRole::Derivative);
        view.content_type = unknown;
        assert_eq!(
            check_create(&view, &envelope, &context(&policy)),
            Err(GateReject::UnknownDerivativeFormat(unknown.to_owned())),
            "a {role} in an unrecognised format must be refused on receipt"
        );
        let mut replace = envelope.clone();
        replace.action = "replace".to_owned();
        replace.prior_provenance_hash = Some(CIPHERTEXT_HASH.to_owned());
        assert_eq!(
            check_replace(&view, &replace, &context(&policy)),
            Err(GateReject::UnknownDerivativeFormat(unknown.to_owned())),
            "a replace ships derivatives through the same gate"
        );
    }

    // Under the default policy: `image/jpeg` is an admitted *original* content type, so
    // invariant 5 passes it, and only the closed still set stands between a JPEG thumbnail
    // and storage.
    assert_eq!(
        check_derivative_upload(&derivative_envelope("thumbnail", "image/jpeg")),
        Err(GateReject::UnknownDerivativeFormat("image/jpeg".to_owned()))
    );
}

#[test]
fn a_derivative_must_declare_its_pair_and_nothing_else_may() {
    // Omission would make the closed set bypassable.
    let mut no_role = derivative_envelope("thumbnail", "image/jxl");
    no_role.derivative_role = None;
    assert_eq!(
        check_derivative_upload(&no_role),
        Err(GateReject::EnvelopeMismatch("derivative_role"))
    );
    let mut no_format = derivative_envelope("thumbnail", "image/jxl");
    no_format.derivative_format = None;
    assert_eq!(
        check_derivative_upload(&no_format),
        Err(GateReject::EnvelopeMismatch("derivative_format"))
    );

    // The role is a closed wire enum.
    assert_eq!(
        check_derivative_upload(&derivative_envelope("poster", "image/jxl")),
        Err(GateReject::EnvelopeMismatch("derivative_role"))
    );

    // The pair may not contradict the content type the request declares.
    let envelope = derivative_envelope("preview", "image/jxl");
    let policy = policy();
    let mut view = declared(&envelope, BlobRole::Derivative);
    view.content_type = "image/jpeg";
    assert_eq!(
        check_create(&view, &envelope, &context(&policy)),
        Err(GateReject::EnvelopeMismatch("derivative_format"))
    );

    // A non-derivative blob carrying the pair contradicts its declared role.
    let carried = derivative_envelope("thumbnail", "image/jxl");
    let view = declared(&carried, BlobRole::Original);
    assert_eq!(
        check_create(&view, &carried, &context(&policy)),
        Err(GateReject::EnvelopeMismatch("derivative_role"))
    );
    let mut format_only = self::envelope();
    format_only.derivative_format = Some("image/jxl".to_owned());
    let view = declared(&format_only, BlobRole::Original);
    assert_eq!(
        check_create(&view, &format_only, &context(&policy)),
        Err(GateReject::EnvelopeMismatch("derivative_format"))
    );
}

#[test]
fn a_derivative_manifest_is_opaque_and_fits_the_feed() {
    // #556: the signed manifest that opens a derivative. Opaque CBOR, and small enough for
    // the feed to inline, because the feed is the only way a receiver gets it.
    let envelope = self::envelope();
    let policy = policy();
    let mut view = declared(&envelope, BlobRole::DerivativeManifest);
    view.content_type = OPAQUE_CONTENT_TYPE;
    assert_eq!(check_create(&view, &envelope, &context(&policy)), Ok(()));

    // At the cap is accepted; one byte past it would be dropped from every page, so it is
    // refused instead.
    view.size = crate::sync::MAX_MANIFEST_BYTES;
    assert_eq!(check_create(&view, &envelope, &context(&policy)), Ok(()));
    view.size = crate::sync::MAX_MANIFEST_BYTES + 1;
    assert_eq!(
        check_create(&view, &envelope, &context(&policy)),
        Err(GateReject::FileTooLarge)
    );

    // A media type would claim the bytes are something a viewer could render.
    let mut typed = declared(&envelope, BlobRole::DerivativeManifest);
    typed.content_type = "image/jxl";
    assert_eq!(
        check_create(&typed, &envelope, &context(&policy)),
        Err(GateReject::UnsupportedContentType)
    );

    // It is not a derivative, so it may not carry the derivative pair.
    let carried = derivative_envelope("thumbnail", "image/jxl");
    let mut view = declared(&carried, BlobRole::DerivativeManifest);
    view.content_type = OPAQUE_CONTENT_TYPE;
    assert_eq!(
        check_create(&view, &carried, &context(&policy)),
        Err(GateReject::EnvelopeMismatch("derivative_role"))
    );

    // And the cap is this role's alone: an original of the same size is unaffected.
    let mut original = declared(&envelope, BlobRole::Original);
    original.size = crate::sync::MAX_MANIFEST_BYTES + 1;
    assert_eq!(
        check_create(&original, &envelope, &context(&policy)),
        Ok(())
    );
}

#[test]
fn an_embedding_derivative_is_outside_the_still_set_not_a_violation_of_it() {
    let unknown = "embedding/mobileclip-b";
    let policy = policy_admitting(unknown);
    let envelope = derivative_envelope("embedding", unknown);
    let mut view = declared(&envelope, BlobRole::Derivative);
    view.content_type = unknown;
    assert_eq!(check_create(&view, &envelope, &context(&policy)), Ok(()));
}

#[test]
fn a_field_the_envelope_schema_fixes_is_named_when_it_is_wrong() {
    for (field, mutate) in [
        (
            "key_mode",
            (|e: &mut ManifestEnvelope| e.key_mode = "borrowed".to_owned())
                as fn(&mut ManifestEnvelope),
        ),
        ("action", |e: &mut ManifestEnvelope| {
            e.action = "obliterate".to_owned();
        }),
        ("prior_provenance_hash", |e: &mut ManifestEnvelope| {
            e.prior_provenance_hash = Some("not-a-hash".to_owned());
        }),
        ("created_by_device", |e: &mut ManifestEnvelope| {
            e.created_by_device = "00000000-0000-0000-0000-000000000000".to_owned();
        }),
    ] {
        let policy = policy();
        let mut envelope = envelope();
        mutate(&mut envelope);
        let view = declared(&envelope, BlobRole::Original);
        assert_eq!(
            check_create(&view, &envelope, &context(&policy)),
            Err(GateReject::EnvelopeMismatch(field)),
            "a bad {field} must be named"
        );
    }
}

#[test]
fn only_a_create_or_a_replace_may_open_an_upload_session() {
    // The two actions that move blob bytes, and no others. An action that only
    // re-points blobs the server already holds is a lifecycle op, and admitting one here
    // would let a `delete` arrive carrying bytes.
    let policy = policy();
    let mut envelope = envelope();
    // A well-formed, in-the-closed-set action that is not this surface's.
    envelope.action = "metadata-update".to_owned();
    envelope.prior_provenance_hash = Some(CIPHERTEXT_HASH.to_owned());
    let view = declared(&envelope, BlobRole::Original);
    assert_eq!(
        check_create(&view, &envelope, &context(&policy)),
        Err(GateReject::ActionNotAllowed)
    );
    assert_eq!(
        check_replace(&view, &envelope, &context(&policy)),
        Err(GateReject::ActionNotAllowed)
    );
}

#[test]
fn a_replace_must_chain_and_must_name_its_bundle() {
    let policy = policy();
    let mut envelope = envelope();
    envelope.action = "replace".to_owned();

    // Every non-create action chains by definition, and a manifest that never claimed to
    // follow anything cannot be rebased — so this is a `400` rather than the `409` a stale
    // chain gets.
    assert_eq!(
        check_replace(
            &declared(&envelope, BlobRole::Original),
            &envelope,
            &context(&policy)
        ),
        Err(GateReject::ReplaceDoesNotChain)
    );
    envelope.prior_provenance_hash = Some(CIPHERTEXT_HASH.to_owned());

    // A non-manifest member names only itself, and is accepted on that basis: it commits
    // bytes and applies nothing.
    assert_eq!(
        check_replace(
            &declared(&envelope, BlobRole::Original),
            &envelope,
            &context(&policy)
        ),
        Ok(())
    );

    // The manifest is the member that applies the whole change, so it is the one that has
    // to be able to say what the asset will hold.
    assert_eq!(
        check_replace(
            &declared(&envelope, BlobRole::Provenance),
            &envelope,
            &context(&policy)
        ),
        Err(GateReject::ReplaceIncomplete("original_blob_hash"))
    );
    envelope.original_blob_hash = Some(CIPHERTEXT_HASH.to_owned());
    assert_eq!(
        check_replace(
            &declared(&envelope, BlobRole::Provenance),
            &envelope,
            &context(&policy)
        ),
        Err(GateReject::ReplaceIncomplete("metadata_blob_hash"))
    );
    envelope.metadata_blob_hash = Some(CIPHERTEXT_HASH.to_owned());
    assert_eq!(
        check_replace(
            &declared(&envelope, BlobRole::Provenance),
            &envelope,
            &context(&policy)
        ),
        Ok(())
    );
}

#[test]
fn a_create_may_not_carry_a_chain_position() {
    let policy = policy();
    let mut envelope = envelope();
    envelope.prior_provenance_hash = Some(CIPHERTEXT_HASH.to_owned());
    let view = declared(&envelope, BlobRole::Original);
    assert_eq!(
        check_create(&view, &envelope, &context(&policy)),
        Err(GateReject::EnvelopeMismatch("prior_provenance_hash"))
    );
}

#[test]
fn invariant_6_compares_the_request_against_the_albums_pin_not_its_own() {
    let policy = policy();
    let envelope = envelope();
    let view = declared(&envelope, BlobRole::Original);

    // The album was provisioned under an older protocol date. The Salvo gate passed the
    // request's own version as the pin, so this case could not fail; here it does.
    let drifted = GateContext {
        album_pin: "2026-02-02",
        ..context(&policy)
    };
    assert_eq!(
        check_create(&view, &envelope, &drifted),
        Err(GateReject::AlbumPinMismatch)
    );
}

#[test]
fn invariant_7_refuses_a_device_the_directory_admitted_later() {
    let policy = policy();
    let envelope = envelope();
    let view = declared(&envelope, BlobRole::Original);

    let admitted_after = GateContext {
        device_added_at: at("2026-06-01T00:00:00Z"),
        ..context(&policy)
    };
    assert_eq!(
        check_create(&view, &envelope, &admitted_after),
        Err(GateReject::DeviceNotAuthorized)
    );
}

#[test]
fn invariant_8_refuses_a_grossly_drifted_timestamp() {
    let policy = policy();
    let envelope = envelope();
    let view = declared(&envelope, BlobRole::Original);

    let far_future = GateContext {
        server_clock: at("2026-12-01T00:00:00Z"),
        ..context(&policy)
    };
    assert_eq!(
        check_create(&view, &envelope, &far_future),
        Err(GateReject::TimestampOutOfRange)
    );

    let mut unparseable = self::envelope();
    unparseable.timestamp = "sometime last tuesday".to_owned();
    let view = declared(&unparseable, BlobRole::Original);
    assert_eq!(
        check_create(&view, &unparseable, &context(&policy)),
        Err(GateReject::TimestampOutOfRange)
    );
}

#[test]
fn finalization_re_runs_the_battery_against_the_moment_it_runs_at() {
    let policy = policy();
    let envelope = envelope();

    assert_eq!(check_finalize(&envelope, &context(&policy)), Ok(()));

    // The album's pin moved, or the session was created against a different album, since
    // creation: the write does not land.
    let closed = GateContext {
        album_pin: "2026-02-02",
        ..context(&policy)
    };
    assert_eq!(
        check_finalize(&envelope, &closed),
        Err(GateReject::AlbumPinMismatch)
    );

    // The device left the directory since creation — the authority answers with a floor
    // that postdates the manifest.
    let revoked = GateContext {
        device_added_at: at("2026-06-01T00:00:00Z"),
        ..context(&policy)
    };
    assert_eq!(
        check_finalize(&envelope, &revoked),
        Err(GateReject::DeviceNotAuthorized)
    );
}

#[test]
fn the_envelope_is_strict_about_fields_it_does_not_know() {
    let json = serde_json::to_string(&envelope()).expect("the projection serializes");
    let with_extra = json.replace("{\"", "{\"surprise\":1,\"");
    assert!(
        serde_json::from_str::<ManifestEnvelope>(&with_extra).is_err(),
        "an unknown envelope field is a client bug, not a value to ignore"
    );
    assert_eq!(
        serde_json::from_str::<ManifestEnvelope>(&json).expect("round trips"),
        envelope(),
        "the projection round-trips, which is what the session record stores"
    );
}

#[test]
fn the_device_the_manifest_names_is_parsed_once() {
    assert_eq!(
        created_by_device(&envelope()),
        Ok(Uuid::parse_str(DEVICE).expect("the literal is a uuid"))
    );

    let mut nil = envelope();
    nil.created_by_device = "00000000-0000-0000-0000-000000000000".to_owned();
    assert_eq!(
        created_by_device(&nil),
        Err(GateReject::EnvelopeMismatch("created_by_device"))
    );
}
