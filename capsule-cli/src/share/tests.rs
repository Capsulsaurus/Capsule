use super::*;

fn at(raw: &str) -> jiff::Timestamp {
    raw.parse().expect("a timestamp")
}

#[test]
fn an_expiry_is_normalized_to_utc() {
    let now = at("2026-01-01T00:00:00Z");
    assert_eq!(
        normalize_expiry("2026-12-31T23:59:59+02:00", now).expect("valid"),
        "2026-12-31T21:59:59Z"
    );
    assert_eq!(
        normalize_expiry(" 2026-06-01T00:00:00Z\n", now).expect("valid"),
        "2026-06-01T00:00:00Z"
    );
}

#[test]
fn an_expiry_that_does_not_parse_is_refused_naming_what_was_typed() {
    let now = at("2026-01-01T00:00:00Z");
    for raw in ["tomorrow", "2026-12-31", ""] {
        assert!(
            matches!(
                normalize_expiry(raw, now),
                Err(ShareCommandError::InvalidExpiry(ref typed)) if typed == raw
            ),
            "{raw:?} is not RFC 3339"
        );
    }
}

#[test]
fn an_expiry_at_or_before_now_is_refused() {
    let now = at("2026-01-01T00:00:00Z");
    for raw in ["2026-01-01T00:00:00Z", "2025-12-31T23:59:59Z"] {
        assert!(matches!(
            normalize_expiry(raw, now),
            Err(ShareCommandError::ExpiryNotInFuture(_))
        ));
    }
}

#[test]
fn an_opaque_id_is_32_lowercase_hex_characters() {
    let id = "0123456789abcdef0123456789abcdef";
    assert_eq!(validate_opaque_id(id).expect("valid"), id);
    assert_eq!(validate_opaque_id(&format!(" {id}\n")).expect("valid"), id);
    for raw in [
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0",
        "0123456789abcdef0123456789abcdeg",
        "https://capsule.example.com/s/0123456789abcdef0123456789abcdef",
    ] {
        assert!(
            matches!(
                validate_opaque_id(raw),
                Err(ShareCommandError::InvalidOpaqueId(_))
            ),
            "{raw:?} is not an opaque id"
        );
    }
}

#[test]
fn the_metadata_record_is_never_reported_missing_and_every_unheld_ciphertext_is() {
    let record = Hash32([1; 32]);
    let held = Hash32([2; 32]);
    let unheld = Hash32([3; 32]);
    let on_server: HashSet<String> = [held.to_hex()].into_iter().collect();
    assert_eq!(
        missing_ciphertexts(&[record, held, unheld], &on_server),
        vec![unheld.to_hex()]
    );
    assert!(missing_ciphertexts(&[record, held], &on_server).is_empty());
    assert!(missing_ciphertexts(&[record], &HashSet::<String>::new()).is_empty());
}

#[test]
fn every_local_refusal_is_described_in_its_own_words() {
    let cases = [
        ShareCommandError::InvalidExpiry("tomorrow".to_owned()),
        ShareCommandError::ExpiryNotInFuture("2020-01-01T00:00:00Z".to_owned()),
        ShareCommandError::InvalidOpaqueId("nope".to_owned()),
        ShareCommandError::EmptyScope,
        ShareCommandError::NotHeld {
            missing: vec!["ab".to_owned(), "cd".to_owned()],
        },
        ShareCommandError::Sharing(SharingError::ScopeUnavailable),
        ShareCommandError::Remote(RemoteError::NotAuthenticated),
    ];
    let mut seen = HashSet::new();
    for error in &cases {
        let described = describe_error(error);
        assert!(
            !described.is_empty() && seen.insert(described.clone()),
            "{error:?} renders a distinct sentence: {described}"
        );
    }
    assert!(describe_error(&cases[0]).contains("tomorrow"));
    assert!(describe_error(&cases[4]).starts_with("2 shared assets are"));
    assert!(
        describe_error(&ShareCommandError::NotHeld {
            missing: vec!["ab".to_owned()],
        })
        .starts_with("1 shared asset is")
    );
}

#[test]
fn a_request_never_prints_its_link_passphrase() {
    let request = CreateRequest {
        scope: ShareScope::Asset(uuid::Uuid::nil()),
        expires_at: None,
        passphrase: Some("hunter2-correct-horse".to_owned()),
    };
    let printed = format!("{request:?}");
    assert!(!printed.contains("hunter2"), "{printed}");
    assert!(printed.contains("<redacted>"), "{printed}");
}

/// A fast-cost workspace with one album holding `count` imported assets, in a scratch
/// directory removed on drop, plus a CLI state that has never signed in: a `create` that
/// got as far as the session would fail with `NotAuthenticated` and write nothing.
struct EmptyScopeFixture {
    dir: std::path::PathBuf,
    ws: Workspace,
    album: uuid::Uuid,
    ids: Vec<uuid::Uuid>,
    remote: RemoteConfig,
    store: SessionStore,
}

impl EmptyScopeFixture {
    fn with_assets(count: usize) -> Self {
        const FAST_KDF: capsule_core::crypto::primitives::Argon2Params =
            capsule_core::crypto::primitives::Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            };
        let dir = std::env::temp_dir().join(format!("capsule-cli-share-{}", nanoid::nanoid!()));
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).expect("scratch library dir");
        let mut ws =
            Workspace::create_with_params(&lib, b"pw", FAST_KDF).expect("create workspace");
        let album = ws.create_album("Shared").expect("create album");
        let ids = (0..count)
            .map(|n| {
                let mut bytes = b"\xFF\xD8\xFF".to_vec();
                bytes.extend_from_slice(format!(" asset {n}").as_bytes());
                ws.import_bytes(album, &format!("photo-{n}.jpg"), &bytes)
                    .expect("import")
            })
            .collect();
        // Nothing listens here; a request that went out would fail, not hang.
        let unreachable = "http://127.0.0.1:9".to_owned();
        let remote = RemoteConfig {
            auth_endpoint: unreachable.clone(),
            sync_endpoint: unreachable.clone(),
            upload_endpoint: unreachable.clone(),
            albums_endpoint: unreachable,
            protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
        };
        let store = SessionStore::new(dir.join("session.json"));
        Self {
            dir,
            ws,
            album,
            ids,
            remote,
            store,
        }
    }

    async fn create_album_link(&mut self) -> Result<IssuedShare, ShareCommandError> {
        let request = CreateRequest {
            scope: ShareScope::Album(self.album),
            expires_at: None,
            passphrase: None,
        };
        create(
            &self.remote,
            &self.store,
            &mut self.ws,
            &request,
            at("2026-01-01T00:00:00Z"),
        )
        .await
    }
}

impl Drop for EmptyScopeFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn an_album_with_no_asset_is_refused_as_empty_before_any_request() {
    let mut fixture = EmptyScopeFixture::with_assets(0);
    let outcome = fixture.create_album_link().await;
    assert!(
        matches!(outcome, Err(ShareCommandError::EmptyScope)),
        "{outcome:?}"
    );
    assert!(
        !fixture.dir.join("session.json").exists(),
        "an empty scope never reaches the session"
    );
}

#[tokio::test]
async fn an_album_whose_every_asset_is_in_trash_is_refused_as_empty_before_any_request() {
    let mut fixture = EmptyScopeFixture::with_assets(2);
    for id in fixture.ids.clone() {
        fixture.ws.soft_delete(&id, 30).expect("trash");
    }
    let outcome = fixture.create_album_link().await;
    assert!(
        matches!(outcome, Err(ShareCommandError::EmptyScope)),
        "{outcome:?}"
    );
    assert!(
        !fixture.dir.join("session.json").exists(),
        "an empty scope never reaches the session"
    );
}

#[tokio::test]
async fn an_album_with_one_asset_outside_trash_gets_past_the_scope_check() {
    // The falsifier for the two refusals above: the same fixture with one asset left
    // outside trash is not empty, so `create` goes on to the session and stops there.
    let mut fixture = EmptyScopeFixture::with_assets(2);
    let trashed = fixture.ids[0];
    fixture.ws.soft_delete(&trashed, 30).expect("trash");
    let outcome = fixture.create_album_link().await;
    assert!(
        matches!(
            outcome,
            Err(ShareCommandError::Remote(RemoteError::NotAuthenticated))
        ),
        "{outcome:?}"
    );
}

#[test]
fn a_server_refusal_is_described_by_its_error_code() {
    let error = ShareCommandError::Share(ShareError::Unavailable {
        code: Some("error.share.unavailable".to_owned()),
    });
    assert_eq!(error.error_code(), Some("error.share.unavailable"));
    assert_eq!(
        describe_error(&error),
        "Capsule couldn't reach that share. Please try again."
    );
}
