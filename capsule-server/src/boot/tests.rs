use std::collections::BTreeMap;

use super::{App, BootError, assemble, assemble_on};
use crate::config::{Config, Demands, Overrides};
use crate::store::memory::ManualClock;

/// A PKCS#8 v1 Ed25519 key, base64. Signs nothing; see `config`'s own tests.
const EXAMPLE_DER: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";

fn memory_config(root: &std::path::Path) -> Config {
    memory_config_with(root, &[])
}

/// The same, with `extra` on top of the environment.
fn memory_config_with(root: &std::path::Path, extra: &[(&str, &str)]) -> Config {
    let mut environment: BTreeMap<String, String> = [
        ("BLOB_ROOT".to_owned(), root.display().to_string()),
        ("JWT_ED25519_DER".to_owned(), EXAMPLE_DER.to_owned()),
    ]
    .into_iter()
    .collect();
    for (key, value) in extra {
        environment.insert((*key).to_owned(), (*value).to_owned());
    }
    let overrides = Overrides {
        memory: true,
        ..Overrides::default()
    };
    Config::load(&environment, &overrides, Demands::Serve).expect("the configuration loads")
}

/// Register the account the auth cases sign in with, through the surface.
async fn register(client: &kynos::test::TestClient<App>, password: &str) {
    client
        .post("/v1/auth/register")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({ "email": "somebody@example.test", "password": password }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK);
}

/// Attempt a sign-in and return the status the route answered with.
async fn login(client: &kynos::test::TestClient<App>, password: &str) -> kynos::http::StatusCode {
    client
        .post("/v1/auth/login")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({ "email": "somebody@example.test", "password": password }))
        .send()
        .await
        .status()
}

#[tokio::test]
async fn the_memory_profile_assembles_a_server_whose_router_builds() {
    // The property nothing in this crate asserted before: seventeen module contexts, every
    // port filled, and a router Kynos will build out of them.
    let root = tempfile::tempdir().expect("a scratch directory");
    let assembled = assemble(&memory_config(root.path()))
        .await
        .expect("it assembles");
    assembled.service().expect("the router builds");
}

#[tokio::test]
async fn the_blob_root_is_created_rather_than_required_to_exist() {
    let parent = tempfile::tempdir().expect("a scratch directory");
    let root = parent.path().join("does/not/exist/yet");
    let assembled = assemble(&memory_config(&root)).await.expect("it assembles");
    assert!(root.join("blobs").is_dir(), "the store's tree is created");
    assert!(
        root.join("ledger").is_dir(),
        "and the owner ledgers live beside the blobs"
    );
    drop(assembled);
}

#[tokio::test]
async fn a_signing_key_that_is_not_ed25519_refuses_the_boot() {
    // `SigningKeyError` has always been documented as a startup failure. This is the startup
    // it fails.
    let root = tempfile::tempdir().expect("a scratch directory");
    let environment: BTreeMap<String, String> = [
        ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
        (
            "JWT_ED25519_DER".to_owned(),
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                b"not a PKCS#8 document",
            ),
        ),
    ]
    .into_iter()
    .collect();
    let overrides = Overrides {
        memory: true,
        ..Overrides::default()
    };
    let config = Config::load(&environment, &overrides, Demands::Serve).expect("it is well-formed");
    let error = assemble(&config).await.expect_err("it refuses");
    assert!(matches!(error, BootError::SigningKey { .. }), "{error:?}");
}

#[tokio::test]
async fn a_durable_backend_without_a_database_url_refuses_by_name() {
    // `Config::load` is the front door that demands `DATABASE_URL` for a durable boot (#520),
    // so this path's own demand is the backstop for a `Config` that reached it without one —
    // asserted to be made by name rather than discovered as a `None` somewhere further in.
    let root = tempfile::tempdir().expect("a scratch directory");
    let mut config = Config::load(
        &durable_environment(root.path()),
        &Overrides::default(),
        Demands::Serve,
    )
    .expect("it is well-formed");
    config.database_url = None;
    let error = assemble(&config).await.expect_err("it refuses");
    assert!(
        matches!(
            error,
            BootError::Missing {
                key: "DATABASE_URL"
            }
        ),
        "{error:?}"
    );
}

/// The environment a durable `serve` needs, with backend URLs nothing answers on.
///
/// `DATABASE_URL` names a port nothing listens on; a case that reaches a database replaces
/// it.
fn durable_environment(root: &std::path::Path) -> BTreeMap<String, String> {
    [
        ("BLOB_ROOT".to_owned(), root.display().to_string()),
        ("JWT_ED25519_DER".to_owned(), EXAMPLE_DER.to_owned()),
        (
            "DATABASE_URL".to_owned(),
            "postgres://127.0.0.1:1/capsule".to_owned(),
        ),
        ("VALKEY_URL".to_owned(), "redis://127.0.0.1:1".to_owned()),
        // A durable deployment supplies its own attestation identity rather than having one
        // derived from the token signer; `config` refuses without it.
        (
            "ATTESTATION_KEY_SEED".to_owned(),
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                [9_u8; 64].as_slice(),
            ),
        ),
    ]
    .into_iter()
    .collect()
}

/// What a durable boot does once it can actually reach a database.
mod postgres_conformance {
    use super::{BTreeMap, BootError, Config, Demands, Overrides, assemble, durable_environment};
    use crate::postgres::testing;

    /// A config pointing at `url`, with everything else a durable `serve` needs.
    fn config_for(root: &std::path::Path, url: &str) -> Config {
        let mut environment: BTreeMap<String, String> = durable_environment(root);
        environment.insert("DATABASE_URL".to_owned(), url.to_owned());
        Config::load(&environment, &Overrides::default(), Demands::Serve)
            .expect("it is well-formed")
    }

    /// A durable boot clears Postgres and refuses on Valkey rather than falling back.
    ///
    /// The property that matters is *which* refusal: falling back to the in-memory adapters
    /// is the one thing that must never happen, and refusing on the first port anybody
    /// happened to write would hide what is actually missing. Getting as far as the Valkey
    /// probe proves the pool opened and the schema check passed.
    ///
    /// This was two cases before #402 and #403 met. #403 asserted the Valkey refusal as a
    /// unit test, because nothing had yet demanded `DATABASE_URL` ahead of it; #402 asserted
    /// that the arm reached `AdapterUnavailable { key: "VALKEY_URL" }`, which was the
    /// refusal of a port *no adapter read*. Both halves have landed, so the durable arm now
    /// needs a real database before it dials Valkey at all, and the honest error at
    /// `redis://127.0.0.1:1` is [`BootError::Valkey`] — the adapter tried and nothing
    /// answered. One case, both lanes' assertions, and it needs a container to reach.
    #[tokio::test]
    async fn the_durable_arm_clears_postgres_and_refuses_on_an_unreachable_valkey() {
        let Some(database) = testing::start("the durable boot arm").await else {
            return;
        };
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = config_for(root.path(), database.url());
        let error = assemble(&config).await.expect_err("it refuses");
        assert!(matches!(error, BootError::Valkey { .. }), "{error:?}");
        let message = format!("{error}");
        assert!(message.contains("VALKEY_URL"), "{message}");
        assert!(!message.contains("127.0.0.1:1"), "never the URL: {message}");
    }

    /// A database that has not been migrated refuses, and names the command that fixes it.
    ///
    /// The whole reason `serve` reads `seaql_migrations` rather than running
    /// `Migrator::up`: the alternative is a rolling deploy in which every replica races to
    /// apply the same schema change.
    #[tokio::test]
    async fn an_unmigrated_database_refuses_and_names_the_migration_command() {
        let Some(database) = testing::start("an unmigrated durable boot").await else {
            return;
        };
        database.roll_back().await;
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = config_for(root.path(), database.url());
        let error = assemble(&config).await.expect_err("it refuses");
        assert!(matches!(error, BootError::Database { .. }), "{error:?}");
        let rendered = format!("{error}");
        assert!(
            rendered.contains("capsule-server-migration up"),
            "the refusal must name the command that fixes it, got {rendered}"
        );
        assert!(
            !rendered.contains(database.url()),
            "a startup error must never carry the connection URL: {rendered}"
        );
    }

    /// A Valkey for a case that drives the whole durable arm, or `None` — logged — when the
    /// Valkey tier is not admitted.
    ///
    /// The same gate and the same image tag as `tests/valkey.rs`, so one environment admits
    /// both. The container is returned so the caller holds it for the length of the case.
    async fn valkey(
        case: &str,
    ) -> Option<(
        String,
        testcontainers::ContainerAsync<testcontainers_modules::valkey::Valkey>,
    )> {
        use testcontainers::ImageExt as _;
        use testcontainers::runners::AsyncRunner as _;

        if std::env::var("CAPSULE_TEST_VALKEY").ok().as_deref() != Some("1") {
            eprintln!("skipping {case}: set CAPSULE_TEST_VALKEY=1 to admit the Valkey tier");
            return None;
        }
        let mut request = testcontainers_modules::valkey::Valkey::default().with_tag(VALKEY_TAG);
        if let Ok(userns) = std::env::var("CAPSULE_TEST_CONTAINER_USERNS") {
            request = request.with_userns_mode(&userns);
        }
        let container = request.start().await.expect("the Valkey container starts");
        let host = container.get_host().await.expect("a host");
        let port = container
            .get_host_port_ipv4(testcontainers_modules::valkey::VALKEY_PORT)
            .await
            .expect("a mapped port");
        Some((format!("redis://{host}:{port}"), container))
    }

    /// The image tag `capsule-server/compose.yaml` runs, as `tests/valkey.rs` pins it.
    const VALKEY_TAG: &str = "9.0.4";

    /// A durable `serve` assembles a whole server on Postgres and Valkey, and what it wrote
    /// is there for the next one.
    ///
    /// The property #446 exists for: the arm used to refuse after both probes because some
    /// ports had no durable adapter. Now it builds every module context, and an account
    /// registered through the first assembly signs in through a second one built over the same
    /// two backends — the in-process half of a restart. The process half is
    /// `tests/binary.rs`'s.
    #[tokio::test]
    async fn the_durable_arm_assembles_a_server_whose_state_outlives_it() {
        let Some(database) = testing::start("the durable boot arm, whole").await else {
            return;
        };
        let Some((valkey_url, _valkey)) = valkey("the durable boot arm, whole").await else {
            return;
        };
        let root = tempfile::tempdir().expect("a scratch directory");
        let mut config = config_for(root.path(), database.url());
        config.valkey_url = Some(valkey_url);

        let first = assemble(&config).await.expect("the durable arm assembles");
        let client = kynos::test::TestClient::new(first.service().expect("the router builds"));
        super::register(&client, "correct horse battery staple").await;
        assert_eq!(
            super::login(&client, "correct horse battery staple").await,
            kynos::http::StatusCode::OK
        );
        drop(client);
        drop(first);

        let second = assemble(&config).await.expect("it assembles again");
        let client = kynos::test::TestClient::new(second.service().expect("the router builds"));
        assert_eq!(
            super::login(&client, "correct horse battery staple").await,
            kynos::http::StatusCode::OK,
            "an account the first assembly registered is the second one's to sign in to"
        );
    }

    /// A durable `serve` that configures OIDC assembles on Postgres and Valkey (#460).
    ///
    /// It used to be refused by name before either probe, because its two stores had
    /// in-memory adapters only. Discovery is lazy, so an issuer nothing answers at still
    /// assembles; what this proves is that the durable arm now builds both OIDC ports.
    #[tokio::test]
    async fn the_durable_arm_assembles_with_a_relying_party() {
        let Some(database) = testing::start("the durable boot arm, with OIDC").await else {
            return;
        };
        let Some((valkey_url, _valkey)) = valkey("the durable boot arm, with OIDC").await else {
            return;
        };
        let root = tempfile::tempdir().expect("a scratch directory");
        let mut environment: BTreeMap<String, String> = durable_environment(root.path());
        environment.insert("DATABASE_URL".to_owned(), database.url().to_owned());
        environment.insert("VALKEY_URL".to_owned(), valkey_url);
        environment.insert(
            "OIDC_ISSUER".to_owned(),
            "http://127.0.0.1:9/nothing-listens-here".to_owned(),
        );
        environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
        let config = Config::load(&environment, &Overrides::default(), Demands::Serve)
            .expect("it is well-formed");
        assert!(config.oidc.is_some(), "the relying party is configured");
        let assembled = assemble(&config).await.expect("the durable arm assembles");
        assembled.service().expect("the router builds");
    }

    /// A durable `gc` keeps its marks between processes, so a later pass can sweep.
    ///
    /// The reason the maintenance arm exists: the collector marks a blob on one pass and
    /// sweeps it on a later one, so a mark store that forgets is a collector that can only
    /// ever mark. Each pass here assembles its own workers, as each `gc` invocation does.
    #[tokio::test]
    async fn a_durable_collection_sweeps_what_an_earlier_pass_marked() {
        use crate::gc::{Mode, collect};

        let Some(database) = testing::start("the durable maintenance arm").await else {
            return;
        };
        let Some((valkey_url, _valkey)) = valkey("the durable maintenance arm").await else {
            return;
        };
        let root = tempfile::tempdir().expect("a scratch directory");
        let address = "b".repeat(64);
        let shard = root.path().join("blobs").join("bb").join("bb");
        std::fs::create_dir_all(&shard).expect("the shard is created");
        let blob = shard.join(format!("{address}.bin"));
        std::fs::write(&blob, b"an orphan a crashed finalization left").expect("written");

        let mut environment: BTreeMap<String, String> = [
            ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
            ("DATABASE_URL".to_owned(), database.url().to_owned()),
            ("VALKEY_URL".to_owned(), valkey_url),
        ]
        .into_iter()
        .collect();
        // No grace at all, so the second pass is the first one past the window.
        environment.insert("GC_GRACE_WINDOW_HOURS".to_owned(), "0".to_owned());
        let config = Config::load(&environment, &Overrides::default(), Demands::Maintenance)
            .expect("it is well-formed");

        let first = super::super::assemble_maintenance(&config)
            .await
            .expect("the durable maintenance arm assembles");
        let report = collect(&first.collection, Mode::Apply)
            .await
            .expect("a pass runs");
        assert_eq!(report.marked.len(), 1, "{report:?}");
        assert!(report.swept.is_empty(), "{report:?}");
        drop(first);

        let second = super::super::assemble_maintenance(&config)
            .await
            .expect("it assembles again");
        let report = collect(&second.collection, Mode::Apply)
            .await
            .expect("a pass runs");
        assert_eq!(
            report.swept.len(),
            1,
            "a later process sweeps what an earlier one marked: {report:?}"
        );
        assert!(!blob.exists(), "the orphan is reclaimed");
    }
}

#[tokio::test]
async fn a_durable_backend_with_oidc_configured_goes_on_to_the_probes() {
    // Until #460 a durable boot with `OIDC_ISSUER` was refused before either probe, naming
    // the adapters it lacked. Both exist now, so it proceeds exactly as one without OIDC
    // does — to the first probe, which refuses here only because no database is named. The
    // name is dropped after loading, because `Config::load` demands it.
    let root = tempfile::tempdir().expect("a scratch directory");
    let environment: BTreeMap<String, String> = [
        ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
        ("JWT_ED25519_DER".to_owned(), EXAMPLE_DER.to_owned()),
        (
            "DATABASE_URL".to_owned(),
            "postgres://127.0.0.1:1/capsule".to_owned(),
        ),
        ("VALKEY_URL".to_owned(), "redis://127.0.0.1:6379".to_owned()),
        (
            "ATTESTATION_KEY_SEED".to_owned(),
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                [9_u8; 64].as_slice(),
            ),
        ),
        (
            "OIDC_ISSUER".to_owned(),
            "https://idp.example.test".to_owned(),
        ),
        ("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned()),
    ]
    .into_iter()
    .collect();
    let mut config = Config::load(&environment, &Overrides::default(), Demands::Serve)
        .expect("it is well-formed");
    config.database_url = None;
    assert!(config.oidc.is_some(), "the relying party is configured");
    let error = assemble(&config).await.expect_err("it refuses");
    assert!(
        matches!(
            error,
            BootError::Missing {
                key: "DATABASE_URL"
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_ca_bundle_is_read_at_boot_and_refused_by_name_when_unusable() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let bundle = root.path().join("idp-ca.pem");

    // Missing: refused, naming the path.
    let config = memory_config_with(
        root.path(),
        &[
            ("OIDC_ISSUER", "https://idp.example.test"),
            ("OIDC_CLIENT_ID", "capsule"),
            ("OIDC_CA_BUNDLE", &bundle.display().to_string()),
        ],
    );
    let error = assemble(&config).await.expect_err("it refuses");
    assert!(matches!(error, BootError::OidcClient { .. }), "{error:?}");
    assert!(format!("{error}").contains("idp-ca.pem"), "{error}");

    // Present and not a certificate: refused, and the contents are not echoed.
    std::fs::write(
        &bundle,
        "this is not a certificate, it is a secret-looking string",
    )
    .expect("writes");
    let error = assemble(&config).await.expect_err("it refuses");
    assert!(matches!(error, BootError::OidcClient { .. }), "{error:?}");
    assert!(!format!("{error}").contains("secret-looking"), "{error}");

    // A real CA certificate: the server boots, trusting it.
    let key = rcgen::KeyPair::generate().expect("a key");
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = params.self_signed(&key).expect("a CA");
    let body = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, ca.der());
    let pem = format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n");
    std::fs::write(&bundle, pem).expect("writes");
    let assembled = assemble(&config).await.expect("it assembles");
    assembled.service().expect("the router builds");
}

#[tokio::test]
async fn the_memory_profile_assembles_with_a_relying_party_without_reaching_it() {
    // Discovery is lazy: an issuer nothing answers at is still a server that boots.
    let root = tempfile::tempdir().expect("a scratch directory");
    let config = memory_config_with(
        root.path(),
        &[
            ("OIDC_ISSUER", "http://127.0.0.1:9/nothing-listens-here"),
            ("OIDC_CLIENT_ID", "capsule"),
        ],
    );
    let assembled = assemble(&config).await.expect("it assembles");
    assembled.service().expect("the router builds");
}

#[tokio::test]
async fn the_published_signing_key_is_the_one_the_tokens_verify_under() {
    // Not a coincidence to be re-checked at every deployment: `ServerInfo` is built from
    // `tokens.public_key()`, so there is no second copy for an operator to paste wrongly.
    use crate::auth::SessionTokens;
    use crate::store::SystemClock;

    let root = tempfile::tempdir().expect("a scratch directory");
    let config = memory_config(root.path());
    let assembled = assemble(&config).await.expect("it assembles");
    let expected = SessionTokens::from_pkcs8(
        config
            .signing_key_der
            .as_ref()
            .expect("the key is configured")
            .expose(),
        std::sync::Arc::new(SystemClock),
    )
    .expect("the key parses")
    .public_key()
    .to_vec();

    // Read back the way a client would, through the surface rather than through a field.
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
    let body: serde_json::Value = client
        .get("/.well-known/capsule/server-info")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK)
        .json();
    let published = body["signing_key"].as_str().expect("it is published");
    assert_eq!(
        published,
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &expected)
    );
}

#[tokio::test]
async fn the_capability_codec_signs_under_the_published_key() {
    // A peer verifies a capability against `server-info`'s `signing_key`. The codec is
    // built from the same DER as the session signer, so the two are one key — asserted
    // through the surface and through the codec, rather than assumed from the wiring.
    let root = tempfile::tempdir().expect("a scratch directory");
    let config = memory_config(root.path());
    let assembled = assemble(&config).await.expect("it assembles");
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
    let body: serde_json::Value = client
        .get("/.well-known/capsule/server-info")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK)
        .json();
    let published = body["signing_key"].as_str().expect("it is published");
    let codec = crate::federation::CapabilityCodec::from_pkcs8(
        config
            .signing_key_der
            .as_ref()
            .expect("the key is configured")
            .expose(),
        config.server_domain.clone(),
        std::sync::Arc::new(crate::store::SystemClock),
    )
    .expect("the key parses");
    assert_eq!(
        published,
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            codec.public_key()
        )
    );
    assert_eq!(codec.server_id(), config.server_domain);
    assert!(
        body.get("federation_url").is_none(),
        "a deployment without FEDERATION_URL publishes no federation endpoint"
    );
}

#[tokio::test]
async fn federation_url_is_published_only_when_federation_is_switched_on() {
    // Opt-in by the switch (#509), and the record is the only way a peer learns it. A URL
    // on its own is a deployment that never decided, so it publishes nothing and mounts a
    // federation surface that answers `404`.
    let root = tempfile::tempdir().expect("a scratch directory");
    for (enabled, published) in [(false, None), (true, Some("https://capsule.example/v1"))] {
        let mut pairs = vec![("FEDERATION_URL", "https://capsule.example/v1")];
        if enabled {
            pairs.push(("FEDERATION_ENABLED", "true"));
        }
        let config = memory_config_with(root.path(), &pairs);
        let assembled = assemble(&config).await.expect("it assembles");
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
        let body: serde_json::Value = client
            .get("/.well-known/capsule/server-info")
            .header("accept", "application/json")
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json();
        assert_eq!(
            body.get("federation_url")
                .and_then(serde_json::Value::as_str),
            published,
            "FEDERATION_ENABLED={enabled}"
        );

        let report = client
            .post("/v1/federation/reports")
            .header("accept", "application/json")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .json(&serde_json::json!({}))
            .send()
            .await;
        assert_eq!(
            report.status() == kynos::http::StatusCode::NOT_FOUND,
            !enabled,
            "FEDERATION_ENABLED={enabled}: report intake answered {}",
            report.status()
        );
    }
}

#[tokio::test]
async fn the_published_protocol_window_is_the_configured_one() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let config = memory_config(root.path());
    let assembled = assemble(&config).await.expect("it assembles");
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
    let body: serde_json::Value = client
        .get("/.well-known/capsule/server-info")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK)
        .json();
    assert_eq!(body["protocol_version"]["min"], config.protocol_min);
    assert_eq!(body["protocol_version"]["max"], config.protocol_max);
    assert_eq!(body["server_id"], config.server_domain);
    assert_eq!(body["api_base_url"], config.api_base_url);
}

/// The window the handshake enforces and advertises is the configured one (issue #404).
///
/// Before this the upload policy was `UploadPolicy::default()` regardless of `PROTOCOL_MIN`
/// and `PROTOCOL_MAX`, so a deployment that narrowed its window published one range on
/// `/.well-known/capsule/server-info` and enforced another on `POST /v1/upload`.
#[tokio::test]
async fn the_enforced_and_advertised_window_is_the_configured_one() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let config = memory_config(root.path());
    let assembled = assemble(&config).await.expect("it assembles");
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));

    // Every response advertises the window, an exempt read included.
    let response = client
        .get("/v1/version")
        .header("accept", "application/json")
        .send()
        .await;
    response.assert_status(kynos::http::StatusCode::OK);
    assert_eq!(
        response.header("x-capsule-protocol-min"),
        Some(config.protocol_min.as_str())
    );
    assert_eq!(
        response.header("x-capsule-protocol-max"),
        Some(config.protocol_max.as_str())
    );
    assert_eq!(
        response.header("x-capsule-min-client-build"),
        Some(config.min_client_build.as_str())
    );

    // And the gate holds a write to the same window: a version one day below the
    // configured minimum is refused before authentication is even looked at. (A read would
    // be admitted at any date — threat-model/validation.md — which is why this is a `DELETE`.)
    let below = format!(
        "{}",
        config
            .protocol_min
            .parse::<jiff::civil::Date>()
            .expect("the configured minimum is a date")
            .yesterday()
            .expect("there is a day before it")
    );
    let refused = client
        .delete("/v1/upload/anything")
        .header("x-capsule-protocol", &below)
        .send()
        .await;
    refused.assert_status(kynos::http::StatusCode::UPGRADE_REQUIRED);
    assert_eq!(
        refused.header("x-capsule-protocol-min"),
        Some(config.protocol_min.as_str())
    );
}

#[tokio::test]
async fn an_account_can_be_registered_and_signed_in_to() {
    // The whole point of the amended deliverable boundary: `mise run serve-memory` is a
    // server a client developer can point at, not a surface they can only read.
    let root = tempfile::tempdir().expect("a scratch directory");
    let assembled = assemble(&memory_config(root.path()))
        .await
        .expect("it assembles");
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));

    let registered: serde_json::Value = client
        .post("/v1/auth/register")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "email": "somebody@example.test",
            "password": "correct horse battery staple",
        }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK)
        .json();
    assert!(registered["access_token"].is_string(), "{registered}");

    let signed_in: serde_json::Value = client
        .post("/v1/auth/login")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "email": "somebody@example.test",
            "password": "correct horse battery staple",
        }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK)
        .json();
    assert!(signed_in["access_token"].is_string(), "{signed_in}");
}

#[tokio::test]
async fn the_assembled_maintenance_workers_see_the_links_the_application_issues() {
    // #653, Decision 5: the share store lives in `Stores`, so the collector and the scrub
    // read the very store `issue_share` writes. Two stores would mean a collector that sees
    // no links and sweeps every record, which only an assembly-level case can catch.
    use base64::Engine as _;

    let root = tempfile::tempdir().expect("a scratch directory");
    let assembled = assemble(&memory_config(root.path()))
        .await
        .expect("it assembles");
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));

    let registered: serde_json::Value = client
        .post("/v1/auth/register")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "email": "somebody@example.test",
            "password": "correct horse battery staple",
        }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK)
        .json();
    let token = registered["access_token"]
        .as_str()
        .expect("an access token");

    let record = b"a share's sealed metadata record";
    let metadata = capsule_core::crypto::hash::hash_bytes(record).to_hex();
    client
        .post("/v1/shares")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("authorization", &format!("Bearer {token}"))
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "opaque_id": "0123456789abcdef0123456789abcdef",
            "metadata_hash": metadata,
            "serves": [metadata],
            "metadata_blob": base64::engine::general_purpose::STANDARD.encode(record),
        }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::CREATED);

    let address = crate::blob::ContentAddress::parse(&metadata).expect("an address");
    let report = crate::gc::collect(&assembled.maintenance.collection, crate::gc::Mode::Apply)
        .await
        .expect("a collection pass");
    assert!(
        !report.marked.contains(&address),
        "the collector must read the store the link was issued into: {report:?}"
    );
    let scrubbed = crate::scrub::scrub(
        &assembled.maintenance.scrub,
        crate::scrub::Depth::Structural,
    )
    .await
    .expect("a scrub pass");
    assert_eq!(
        scrubbed.count("orphan"),
        0,
        "and so must the scrub: {:?}",
        scrubbed.findings
    );
}

#[tokio::test]
async fn a_locked_account_recovers_through_the_login_route_once_the_window_passes() {
    // Asserted through the **route** rather than against the adapter, because that is where
    // the property actually has to hold: `login` asks the directory first and answers `423`
    // before it verifies anything, so a lockout that did not decay would be an account no
    // request could ever open again — there is no unlock operation on any surface.
    //
    // A one-attempt threshold and a one-second window, crossed by moving an injected clock
    // rather than by sleeping (#784). Both numbers are settings rather than constants so this
    // is expressible: driving the default ten-failure ceiling through the route would cost
    // ten Argon2id verifications. A real wait raced the window on a loaded machine, so time
    // is the one input taken off the composition root under test; everything else is the
    // assembly `serve` runs. The clock starts at the real present because `jsonwebtoken`
    // checks `exp` against the system clock, so a token minted at the epoch would be stale.
    let root = tempfile::tempdir().expect("a scratch directory");
    let config = memory_config_with(
        root.path(),
        &[
            ("LOCKOUT_MAX_ATTEMPTS", "1"),
            ("LOCKOUT_WINDOW_SECONDS", "1"),
        ],
    );
    let clock = ManualClock::new(jiff::Timestamp::now());
    let assembled = assemble_on(&config, std::sync::Arc::new(clock.clone()))
        .await
        .expect("it assembles");
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));

    register(&client, "correct horse battery staple").await;
    assert_eq!(
        login(&client, "wrong").await,
        kynos::http::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&client, "correct horse battery staple").await,
        kynos::http::StatusCode::LOCKED,
        "the ceiling engages, and a correct password is told so rather than refused"
    );

    clock.advance(jiff::SignedDuration::from_millis(999));
    assert_eq!(
        login(&client, "correct horse battery staple").await,
        kynos::http::StatusCode::LOCKED,
        "inside the window the lockout holds, so the recovery below is the clock's doing"
    );

    clock.advance(jiff::SignedDuration::from_millis(1_001));
    assert_eq!(
        login(&client, "correct horse battery staple").await,
        kynos::http::StatusCode::OK,
        "the window passed, so the account is the owner's again"
    );
}

#[tokio::test]
async fn a_durable_maintenance_command_without_a_database_url_refuses_by_name() {
    // An operator running `capsule-server scrub` without `--memory` is asking for the
    // durable stores, and the first one those workers read is the index. Naming
    // `VALKEY_URL` first would send them to configure the second thing before the first.
    // `Config::load` names both at once (#520); this is the boot path's backstop, reached by
    // a `Config` that lost both after loading.
    let root = tempfile::tempdir().expect("a scratch directory");
    let environment: BTreeMap<String, String> = [
        ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
        (
            "DATABASE_URL".to_owned(),
            "postgres://127.0.0.1:1/capsule".to_owned(),
        ),
        ("VALKEY_URL".to_owned(), "redis://127.0.0.1:1".to_owned()),
    ]
    .into_iter()
    .collect();
    let mut config = Config::load(&environment, &Overrides::default(), Demands::Maintenance)
        .expect("maintenance demands nothing else");
    config.database_url = None;
    config.valkey_url = None;
    let error = super::assemble_maintenance(&config)
        .await
        .expect_err("it refuses");
    assert!(
        matches!(
            error,
            BootError::Missing {
                key: "DATABASE_URL"
            }
        ),
        "{error:?}"
    );
    let message = format!("{error}");
    assert!(!message.contains("VALKEY_URL"), "{message}");
}

#[tokio::test]
async fn a_wrong_password_is_refused_rather_than_granted() {
    // The property that makes the adapter real rather than permissive: the credential
    // double `tests/support/mod.rs` warns about would accept this.
    let root = tempfile::tempdir().expect("a scratch directory");
    let assembled = assemble(&memory_config(root.path()))
        .await
        .expect("it assembles");
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
    client
        .post("/v1/auth/register")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "email": "somebody@example.test",
            "password": "correct horse battery staple",
        }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK);
    client
        .post("/v1/auth/login")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "email": "somebody@example.test",
            "password": "the wrong password entirely",
        }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::UNAUTHORIZED);
}

/// Sign in through the credential route at `path` and return the refresh token it issued.
async fn signed_in_refresh_token(
    client: &kynos::test::TestClient<App>,
    path: &str,
    password: &str,
) -> String {
    let body: serde_json::Value = client
        .post(path)
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({ "email": "somebody@example.test", "password": password }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK)
        .json();
    body["refresh_token"]
        .as_str()
        .expect("a sign-in issues a refresh token")
        .to_owned()
}

/// Exchange `token` at the refresh route: the rotated refresh token, or the refusing status.
async fn refresh(
    client: &kynos::test::TestClient<App>,
    token: &str,
) -> Result<String, kynos::http::StatusCode> {
    let response = client
        .post("/v1/auth/refresh")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .json(&serde_json::json!({ "refresh_token": token }))
        .send()
        .await;
    let status = response.status();
    if status != kynos::http::StatusCode::OK {
        return Err(status);
    }
    let body: serde_json::Value = response.json();
    Ok(body["refresh_token"]
        .as_str()
        .expect("a refresh answers with a rotated refresh token")
        .to_owned())
}

#[tokio::test]
async fn the_configured_session_window_and_hard_cap_reach_the_session_store() {
    // Through the refresh route on the assembly `serve --memory` runs, so what is asserted is
    // that `SESSION_TTL_DAYS` and `SESSION_HARD_CAP_DAYS` reach the store boot builds, not that
    // the store honours a lifetime it was handed (the store's own tests do that). A one-day
    // window under a two-day cap, crossed on an injected clock that starts at the real present
    // for the reason the lockout case above records.
    let root = tempfile::tempdir().expect("a scratch directory");
    let config = memory_config_with(
        root.path(),
        &[("SESSION_TTL_DAYS", "1"), ("SESSION_HARD_CAP_DAYS", "2")],
    );
    let clock = ManualClock::new(jiff::Timestamp::now());
    let assembled = assemble_on(&config, std::sync::Arc::new(clock.clone()))
        .await
        .expect("it assembles");
    let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
    let password = "correct horse battery staple";

    let mut token = signed_in_refresh_token(&client, "/v1/auth/register", password).await;
    // Each refresh inside the one-day window slides it on.
    for _ in 0..2 {
        clock.advance(jiff::SignedDuration::from_hours(23));
        token = refresh(&client, &token)
            .await
            .expect("inside the sliding window the session refreshes");
    }
    // 49 hours after sign-in and three after the last refresh: the window is open and the
    // two-day cap is not, so the refusal is the cap's.
    clock.advance(jiff::SignedDuration::from_hours(3));
    assert_eq!(
        refresh(&client, &token).await,
        Err(kynos::http::StatusCode::UNAUTHORIZED),
        "the configured hard cap ends the session however recently it refreshed"
    );

    // A fresh session left idle past the one-day window ends well inside the default 180.
    let idle = signed_in_refresh_token(&client, "/v1/auth/login", password).await;
    clock.advance(jiff::SignedDuration::from_hours(25));
    assert_eq!(
        refresh(&client, &idle).await,
        Err(kynos::http::StatusCode::UNAUTHORIZED),
        "the configured sliding window ends an idle session"
    );
}
