use super::*;

/// A script that derives a key from a record it read must spell the prefix the Rust key
/// functions use, or the two halves silently address different keys.
#[test]
fn scripts_derive_keys_with_the_prefixes_rust_writes() {
    let sid = SessionId::new("x");
    let uid = UserId::new("x");
    let up = UploadId::new("x");
    let strip = |key: String| key.trim_end_matches('x').to_owned();
    for (script, prefix) in [
        (&OPEN_SESSION, strip(user_sessions_key(&uid))),
        (&CLOSE_SESSION, strip(user_sessions_key(&uid))),
        (&SESSIONS_FOR_USER, strip(session_key(&sid))),
        (&OPEN_UPLOAD, strip(uploader_key(&uid))),
        (&OPEN_UPLOAD, "capsule:upload:pending:".to_owned()),
        (&OPEN_UPLOAD, strip(album_key(&AlbumId::new("x")))),
        (&UPLOADS_FOR_UPLOADER, strip(upload_key(&up))),
        (&UPLOADS_FOR_UPLOADER, strip(chunks_key(&up))),
        (&DISCARD_UPLOAD, strip(uploader_key(&uid))),
        (&DISCARD_UPLOAD, "capsule:upload:pending:".to_owned()),
        (&DISCARD_UPLOAD, strip(album_key(&AlbumId::new("x")))),
        (&SET_STATUS, "capsule:upload:pending:".to_owned()),
        (&SET_STATUS, strip(album_key(&AlbumId::new("x")))),
        (&PENDING_FOR_ADDRESS, strip(upload_key(&up))),
        (&PENDING_FOR_ADDRESS, strip(chunks_key(&up))),
        (&IN_FLIGHT_FOR_ALBUM, strip(upload_key(&up))),
        (&LEAST_RECENTLY_PROGRESSED, strip(upload_key(&up))),
        (&CLAIMED_UPLOADS, strip(upload_key(&up))),
        (&CLAIMED_UPLOADS, strip(chunks_key(&up))),
        (
            &REDEEM_ENROLLMENT,
            strip(enrollment_key(&EnrollmentCode::new("x"))),
        ),
    ] {
        let prefix = prefix.trim_end_matches(':');
        assert!(
            script.source.contains(&format!("'{prefix}:'")),
            "a script does not spell {prefix}: {}",
            script.source
        );
    }
    assert_eq!(
        pending_key(&UserId::new("o"), "h"),
        "capsule:upload:pending:o:h"
    );
    assert!(
        SET_STATUS
            .source
            .contains("'capsule:upload:pending:' .. uploader .. ':' .. hash"),
        "the pending key's two-part suffix is spelled the same way"
    );
}

#[test]
fn every_script_compiles_to_a_hash_once() {
    let first = OPEN_SESSION.script().get_hash().to_owned();
    assert_eq!(OPEN_SESSION.script().get_hash(), first);
    assert_ne!(
        OPEN_SESSION.script().get_hash(),
        READ_RECORD.script().get_hash()
    );
}

#[test]
fn a_session_round_trips_through_its_hash_encoding() {
    let record = SessionRecord {
        session_id: SessionId::new("sid"),
        user_id: UserId::new("uid"),
        created_at: Timestamp::UNIX_EPOCH,
        authenticated_at: deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_nanos(1)),
        signed_in_at: deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_nanos(-5)),
        last_active_at: Timestamp::UNIX_EPOCH,
        user_agent: None,
        ip_address: Some("203.0.113.7".to_owned()),
        cohort_hash: None,
        device_id: Some(Uuid::from_u128(7)),
    };
    let flat = encode_session(&record, Timestamp::UNIX_EPOCH);
    assert!(flat.contains(&"expires_at".to_owned()));
    assert!(flat.contains(&"signed_in_at".to_owned()));
    assert!(
        !flat.contains(&"user_agent".to_owned()),
        "absent stays absent"
    );
    assert_eq!(decode_session(flat).expect("decodes"), record);
}

#[test]
fn a_session_written_before_signed_in_at_existed_still_decodes() {
    // A live record from the previous release has no `signed_in_at` field. It must decode — a
    // deploy may not sign anyone out — with the lineage start taken from the record's own
    // `created_at`, never the older `authenticated_at`, so no lineage is already past its cap
    // the moment it is read.
    let created_at = deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_hours(48));
    let authenticated_at = deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_hours(1));
    let legacy: Vec<String> = [
        ("session_id", "sid".to_owned()),
        ("user_id", "uid".to_owned()),
        ("created_at", created_at.to_string()),
        ("authenticated_at", authenticated_at.to_string()),
        ("last_active_at", created_at.to_string()),
        ("expires_at", "0".to_owned()),
    ]
    .into_iter()
    .flat_map(|(name, value)| [name.to_owned(), value])
    .collect();

    let record = decode_session(legacy).expect("a legacy record decodes");
    assert_eq!(record.signed_in_at, created_at);
    assert_eq!(record.authenticated_at, authenticated_at);
}

#[test]
fn a_pending_authorization_round_trips_through_its_hash_encoding() {
    let record = PendingAuthorization {
        nonce: OidcNonce::new("nonce"),
        verifier: PkceVerifier::new("verifier"),
        redirect_uri: "http://127.0.0.1:4242/cb?x=1".to_owned(),
        binding_challenge: "binding".to_owned(),
        issued_at: deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_nanos(1)),
    };
    let flat = encode_authorization(&record, Timestamp::UNIX_EPOCH);
    assert!(flat.contains(&"expires_at".to_owned()));
    assert_eq!(decode_authorization(flat).expect("decodes"), record);
    assert_eq!(
        oidc_authorization_key(&OidcState::new("s")),
        "capsule:oidc:authorization:s"
    );
}

#[test]
fn a_pending_device_authorization_round_trips_through_its_hash_encoding() {
    let record = PendingDeviceAuthorization {
        device_code: DeviceCode::new("device-code"),
        binding_challenge: "binding".to_owned(),
        interval: SignedDuration::from_secs(10),
        issued_at: deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_nanos(1)),
    };
    let flat = encode_device(&record, Timestamp::UNIX_EPOCH, Timestamp::UNIX_EPOCH);
    for field in ["expires_at", "polled_at_ms", "interval_secs"] {
        assert!(flat.contains(&field.to_owned()), "{field} is encoded");
    }
    assert_eq!(decode_device(flat).expect("decodes"), record);
    assert_eq!(
        oidc_device_key(&OidcDeviceHandle::new("h")),
        "capsule:oidc:device:h"
    );
    // The script reads the two schedule fields by these names.
    for field in ["'interval_secs'", "'polled_at_ms'"] {
        assert!(CLAIM_DEVICE.source.contains(field), "{field}");
    }
    assert!(SLOW_DOWN_DEVICE.source.contains("'interval_secs'"));
}

#[test]
fn a_claim_reply_decodes_to_its_slot() {
    let record = PendingDeviceAuthorization {
        device_code: DeviceCode::new("device-code"),
        binding_challenge: "binding".to_owned(),
        interval: SignedDuration::from_secs(5),
        issued_at: Timestamp::UNIX_EPOCH,
    };
    let mut due = vec!["due".to_owned()];
    due.extend(encode_device(
        &record,
        Timestamp::UNIX_EPOCH,
        Timestamp::UNIX_EPOCH,
    ));
    assert_eq!(
        decode_claim(Some(due)).expect("decodes"),
        DevicePollSlot::Due(record)
    );
    assert_eq!(
        decode_claim(Some(vec!["too_soon".to_owned(), "10".to_owned()])).expect("decodes"),
        DevicePollSlot::TooSoon {
            interval: SignedDuration::from_secs(10)
        }
    );
    assert_eq!(
        decode_claim(None).expect("decodes"),
        DevicePollSlot::Unknown
    );
    for broken in [
        vec![],
        vec!["too_soon".to_owned()],
        vec!["too_soon".to_owned(), "1.5".to_owned()],
        vec!["later".to_owned()],
    ] {
        assert!(
            matches!(decode_claim(Some(broken)), Err(StoreError::Corrupt { .. })),
            "an unreadable claim is corrupt, not a slot"
        );
    }
}

#[test]
fn a_pending_authorization_missing_a_field_is_corrupt() {
    let mut flat = encode_authorization(
        &PendingAuthorization {
            nonce: OidcNonce::new("nonce"),
            verifier: PkceVerifier::new("verifier"),
            redirect_uri: "http://127.0.0.1:4242/cb".to_owned(),
            binding_challenge: "binding".to_owned(),
            issued_at: Timestamp::UNIX_EPOCH,
        },
        Timestamp::UNIX_EPOCH,
    );
    let at = flat
        .iter()
        .position(|field| field == "verifier")
        .expect("the verifier is encoded");
    flat.drain(at..at + 2);
    assert!(matches!(
        decode_authorization(flat),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn an_upload_session_round_trips_through_its_hash_encoding() {
    let record = UploadSessionRecord {
        upload_id: UploadId::new("up"),
        asset_id: AssetId::new("asset"),
        owner_id: OwnerId::new("owner"),
        upload_user_id: UserId::new("uploader"),
        album_id: Some(AlbumId::new("album")),
        content_type: None,
        expected_hash: "a".repeat(64),
        crypto_suite_id: 3,
        protocol_version: "2026-08-29".to_owned(),
        blob_role: BlobRole::Provenance,
        intent_id: Some("intent".to_owned()),
        manifest_envelope: "{\"tag\":\"x\"}".to_owned(),
        received_bytes: 12,
        total_size: 34,
        status: UploadSessionStatus::FailedProcessing,
        failure_code: Some("error.upload.content_hash_mismatch".to_owned()),
        created_at: Timestamp::UNIX_EPOCH,
        last_progress_at: deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_secs(5)),
    };
    let flat = encode_upload(&record, Timestamp::UNIX_EPOCH);
    assert_eq!(decode_upload(flat).expect("decodes"), record);
}

#[test]
fn a_chunk_round_trips_through_its_packed_encoding() {
    let chunk = AcceptedChunk {
        offset: 4096,
        chunk_hash: "b".repeat(64),
        next_offset: 8192,
        accepted_at: deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_nanos(123)),
    };
    assert_eq!(
        decode_chunk(4096, &encode_chunk(&chunk)).expect("decodes"),
        chunk
    );
}

#[test]
fn a_field_that_will_not_parse_is_corrupt_not_absent() {
    let flat = vec![
        "session_id".to_owned(),
        "sid".to_owned(),
        "user_id".to_owned(),
        "uid".to_owned(),
        "created_at".to_owned(),
        "not a timestamp".to_owned(),
    ];
    let error = decode_session(flat).expect_err("refuses");
    assert!(
        matches!(error, StoreError::Corrupt { store: "auth", .. }),
        "{error:?}"
    );

    let error = decode_chunk(0, "hash-only").expect_err("refuses");
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
}

#[test]
fn the_collector_never_runs_ahead_of_the_logical_lifetime() {
    // Whole milliseconds, rounded up: a 1 ns lifetime collects after 1 ms, a 1.5 ms one
    // after 2 ms — never before the microsecond `expires_at` the read gate compares.
    assert_eq!(ttl_millis(SignedDuration::from_nanos(1)), "1");
    assert_eq!(ttl_millis(SignedDuration::from_micros(1_500)), "2");
    assert_eq!(ttl_millis(SignedDuration::from_millis(1)), "1");
    assert_eq!(ttl_millis(SignedDuration::from_secs(2)), "2000");
    assert_eq!(ttl_millis(SignedDuration::ZERO), "1");
}

#[test]
fn a_pending_key_has_exactly_two_variable_segments() {
    assert_eq!(
        pending_key(&UserId::new("o"), "h"),
        "capsule:upload:pending:o:h"
    );
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "must contain no `:`")]
fn a_key_segment_with_a_colon_is_refused_in_debug() {
    let _ = pending_key(&UserId::new("not:an:id"), "h");
}

/// The classification the port's three variants promise, per driver error.
#[test]
fn driver_errors_are_classified_by_whether_the_command_could_have_run() {
    let unavailable = [
        ErrorKind::Server(ServerErrorKind::NoScript),
        ErrorKind::Server(ServerErrorKind::BusyLoading),
        ErrorKind::Server(ServerErrorKind::TryAgain),
        ErrorKind::Server(ServerErrorKind::MasterDown),
        ErrorKind::Server(ServerErrorKind::ClusterDown),
        ErrorKind::ClusterConnectionNotFound,
    ];
    for kind in unavailable {
        let error = classify(AUTH, "x", RedisError::from((kind, "declined")));
        assert!(
            matches!(error, StoreError::Unavailable { .. }),
            "{kind:?}: {error:?}"
        );
    }
    // The server may have run it: a plain error reply, a read-only replica, an aborted
    // transaction — and, by the driver's own account, a timeout or a dropped connection.
    for kind in [
        ErrorKind::Server(ServerErrorKind::ResponseError),
        ErrorKind::Server(ServerErrorKind::ReadOnly),
        ErrorKind::Io,
    ] {
        let error = classify(AUTH, "x", RedisError::from((kind, "after the fact")));
        assert!(
            matches!(error, StoreError::Rejected { .. }),
            "{kind:?}: {error:?}"
        );
    }
    let error = classify(
        CHALLENGES,
        "RevokeAllChallenge",
        RedisError::from((
            ErrorKind::UnexpectedReturnType,
            "Response was of incompatible type: the-secret",
        )),
    );
    match error {
        StoreError::Corrupt { detail, .. } => assert!(
            !detail.contains("the-secret"),
            "a corrupt reply's text never reaches the error: {detail}"
        ),
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

#[test]
fn a_microsecond_count_round_trips() {
    let at = deadline(
        Timestamp::UNIX_EPOCH,
        SignedDuration::from_micros(1_234_567),
    );
    assert_eq!(from_micros(AUTH, "x", micros(at)).expect("in range"), at);
}

#[tokio::test]
async fn an_unreachable_server_is_unavailable_not_rejected() {
    // Port 1 on loopback: nothing listens there, so the refusal is immediate and the retry
    // budget is what bounds the wait.
    let error = Valkey::connect("redis://127.0.0.1:1")
        .await
        .expect_err("nothing listens on port 1");
    assert!(
        matches!(
            error,
            StoreError::Unavailable {
                store: "valkey",
                ..
            }
        ),
        "{error:?}"
    );
    let error = Valkey::connect("not a url").await.expect_err("refuses");
    assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
    assert!(!format!("{error}").contains("not a url"), "never the URL");
}
