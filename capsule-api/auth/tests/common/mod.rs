use auth::config::AuthConfig;
use auth::service::PasskeyService;
use auth::session::SessionManager;
use auth::state::AppState;
use migration::Migrator;
use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use std::sync::Once;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use testcontainers_modules::postgres::Postgres;

static TRACING: Once = Once::new();

pub struct TestContext {
    pub _postgres: Option<ContainerAsync<Postgres>>,
    pub _valkey: Option<ContainerAsync<GenericImage>>,
    pub app_state: AppState,
    pub db: DatabaseConnection,
}

pub async fn setup() -> TestContext {
    TRACING.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter("info,sqlx=error,sea_orm=error")
            .with_test_writer()
            .init();
    });

    let (postgres_container, connection_string) =
        if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
            (None, url)
        } else {
            let container = Postgres::default()
                .with_tag("17")
                .start()
                .await
                .expect("Failed to start Postgres");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("Failed to get Postgres port");
            (
                Some(container),
                format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port),
            )
        };

    let db = Database::connect(&connection_string)
        .await
        .expect("Failed to connect to database");
    Migrator::refresh(&db)
        .await
        .expect("Failed to run migrations");

    // Start Valkey container or use external
    let (valkey_container, valkey_url) = if let Ok(url) = std::env::var("TEST_VALKEY_URL") {
        (None, url)
    } else {
        let container = GenericImage::new("valkey/valkey", "8.0.1")
            .with_exposed_port(testcontainers::core::ContainerPort::Tcp(6379))
            .with_wait_for(testcontainers::core::WaitFor::message_on_stdout(
                "Ready to accept connections",
            ))
            .start()
            .await
            .expect("Failed to start Valkey");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Valkey port");
        (Some(container), format!("redis://127.0.0.1:{}", port))
    };

    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let priv_bytes = engine
        .decode("MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF")
        .expect("Failed to decode priv key");
    let pub_bytes = engine
        .decode("MCowBQYDK2VwAyEA66iVaMz1x2ogToGm5Hw34aITBLLqz0iEonbwjK57pWU=")
        .expect("Failed to decode pub key");

    let enc_key = jsonwebtoken::EncodingKey::from_ed_der(&priv_bytes);
    let dec_key = jsonwebtoken::DecodingKey::from_ed_der(&pub_bytes);

    let config = AuthConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        domain: "localhost".to_string(),
        jwt_eddsa_encoding_key: enc_key,
        jwt_eddsa_decoding_key: dec_key,
        jwt_refresh_token_duration_seconds: 3600,
        jwt_access_token_duration_seconds: 300,
        valkey_url: valkey_url.clone(),
        totp_issuer: "Capsule-Test".to_string(),
        allowed_origins: vec!["*".to_string()],
    };

    let session_manager = SessionManager::new(valkey_url, std::time::Duration::from_secs(3600))
        .await
        .expect("Failed to create session manager");

    let rp_origin = webauthn_rs::prelude::Url::parse("https://localhost").expect("valid URL");
    let webauthn = std::sync::Arc::new(
        webauthn_rs::prelude::WebauthnBuilder::new("localhost", &rp_origin)
            .expect("valid builder")
            .build()
            .expect("valid webauthn"),
    );
    let passkey_service = PasskeyService::new(db.clone(), webauthn);
    let app_state = AppState::new(db.clone(), config, session_manager, passkey_service);

    TestContext {
        _postgres: postgres_container,
        _valkey: valkey_container,
        app_state,
        db,
    }
}

/// Build a salvo Service from a TestContext for integration testing.
pub fn build_service(ctx: &TestContext) -> salvo::Service {
    let router = auth::get_router_with_state(ctx.app_state.clone());
    salvo::Service::new(router)
}
