use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use jiff::SignedDuration;
use kynos::http::forwarded::ProxyHeader;

use super::{BASE64, Backends, Config, ConfigFault, Demands, LogFormat, Overrides, redacted_url};

/// A PKCS#8 v1 Ed25519 key, base64, from the retired deployment's own `.env.example`.
///
/// A committed *example* key rather than a generated one, because these tests assert
/// **derivation is deterministic**, and a fresh key per run would make that unassertable.
/// It signs nothing: no deployment ever used it, and any that did published the fact.
const EXAMPLE_DER: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// The environment a `serve --memory` needs and nothing more.
fn serveable() -> BTreeMap<String, String> {
    env(&[
        ("BLOB_ROOT", "/var/lib/capsule/blobs"),
        ("JWT_ED25519_DER", EXAMPLE_DER),
    ])
}

fn memory() -> Overrides {
    Overrides {
        memory: true,
        ..Overrides::default()
    }
}

#[test]
fn the_defaults_are_a_complete_configuration() {
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.listen.to_string(), "0.0.0.0:3000");
    assert_eq!(config.server_domain, "localhost");
    assert_eq!(config.api_base_url, "http://localhost:3000/v1");
    assert_eq!(config.backends, Backends::Memory);
    // The policy's year window, not the single day core speaks: a build one day behind
    // still writes, and the day core speaks sits strictly inside it.
    assert_eq!(
        config.protocol_min,
        crate::upload::policy::DEFAULT_PROTOCOL_MIN
    );
    assert_eq!(
        config.protocol_max,
        crate::upload::policy::DEFAULT_PROTOCOL_MAX
    );
    let spoken = capsule_core::crypto::primitives::PROTOCOL_VERSION;
    assert!(config.protocol_min.as_str() < spoken && spoken < config.protocol_max.as_str());
    assert_eq!(
        config.min_client_build,
        crate::upload::policy::DEFAULT_MIN_CLIENT_BUILD
    );
}

#[test]
fn the_connection_bounds_are_explicit_and_configurable() {
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.header_read_timeout, Duration::from_secs(30));
    assert_eq!(
        config.chunk_pace,
        crate::upload::policy::ChunkPace::new(
            crate::upload::policy::DEFAULT_CHUNK_STALL_TIMEOUT,
            crate::upload::policy::DEFAULT_CHUNK_MIN_RATE,
        )
    );

    let mut environment = serveable();
    environment.insert("HEADER_READ_TIMEOUT_SECONDS".to_owned(), "7".to_owned());
    environment.insert("CHUNK_STALL_TIMEOUT_SECONDS".to_owned(), "45".to_owned());
    environment.insert(
        "CHUNK_MIN_RATE_BYTES_PER_SECOND".to_owned(),
        "512".to_owned(),
    );
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.header_read_timeout, Duration::from_secs(7));
    assert_eq!(
        config.chunk_pace,
        crate::upload::policy::ChunkPace::new(Duration::from_secs(45), 512)
    );
}

/// The stall bound must sit below the idle timeout a typical reverse proxy applies (sixty
/// seconds for nginx and AWS ALB), or a stalled chunk is cut by the proxy with a bare `504`
/// before this server can answer its `408`.
#[test]
fn the_default_stall_bound_sits_below_a_typical_proxy_idle_timeout() {
    assert!(crate::upload::policy::DEFAULT_CHUNK_STALL_TIMEOUT <= Duration::from_secs(30));
}

/// A zero rate floor is a setting, not an outage: it leaves the stall bound alone.
#[test]
fn a_zero_minimum_rate_turns_the_floor_off() {
    let mut environment = serveable();
    environment.insert("CHUNK_MIN_RATE_BYTES_PER_SECOND".to_owned(), "0".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.chunk_pace.min_bytes_per_second, 0);
    assert_eq!(
        config.chunk_pace.stall,
        crate::upload::policy::DEFAULT_CHUNK_STALL_TIMEOUT
    );
}

/// The total-duration setting the stall bound replaced is no longer read, and does not stop a
/// server that still carries it from booting.
#[test]
fn the_retired_chunk_read_timeout_is_ignored() {
    let mut environment = serveable();
    environment.insert("CHUNK_READ_TIMEOUT_SECONDS".to_owned(), "0".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(
        config.chunk_pace.stall,
        crate::upload::policy::DEFAULT_CHUNK_STALL_TIMEOUT
    );
}

#[test]
fn a_connection_bound_of_zero_or_not_a_number_is_refused() {
    for key in ["HEADER_READ_TIMEOUT_SECONDS", "CHUNK_STALL_TIMEOUT_SECONDS"] {
        for value in ["0", "-1", "thirty"] {
            let mut environment = serveable();
            environment.insert(key.to_owned(), value.to_owned());
            let error =
                Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
            assert!(error.names(key), "{key}={value}: {error}");
        }
    }
    for value in ["-1", "fast"] {
        let mut environment = serveable();
        environment.insert(
            "CHUNK_MIN_RATE_BYTES_PER_SECOND".to_owned(),
            value.to_owned(),
        );
        let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
        assert!(
            error.names("CHUNK_MIN_RATE_BYTES_PER_SECOND"),
            "{value}: {error}"
        );
    }
}

#[test]
fn a_protocol_bound_that_is_not_a_strict_date_is_refused() {
    for (key, value) in [
        ("PROTOCOL_MIN", "2026-6-1"),
        ("PROTOCOL_MAX", "2026-02-30"),
        ("PROTOCOL_MAX", "yesterday"),
        ("PROTOCOL_MIN", "2026-05-31T00:00:00Z"),
    ] {
        let mut environment = serveable();
        environment.insert(key.to_owned(), value.to_owned());
        let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
        assert!(error.names(key), "{key}={value}: {error}");
    }
}

#[test]
fn a_window_of_one_day_is_a_legitimate_operator_choice() {
    let mut environment = serveable();
    environment.insert("PROTOCOL_MIN".to_owned(), "2026-05-31".to_owned());
    environment.insert("PROTOCOL_MAX".to_owned(), "2026-05-31".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.protocol_min, config.protocol_max);
}

#[test]
fn the_client_build_cutoff_is_semver_or_refused() {
    let mut environment = serveable();
    environment.insert("MIN_CLIENT_BUILD".to_owned(), "1.4.0".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.min_client_build, "1.4.0");

    for bad in ["1.4", "v1.4.0", "1.4.0-beta", "one.two.three", ""] {
        let mut environment = serveable();
        environment.insert("MIN_CLIENT_BUILD".to_owned(), bad.to_owned());
        match Config::load(&environment, &memory(), Demands::Serve) {
            // Empty is unset, which is the default and loads.
            Ok(config) if bad.is_empty() => assert_eq!(config.min_client_build, "0.0.0"),
            Ok(_) => panic!("MIN_CLIENT_BUILD={bad} loaded"),
            Err(error) => assert!(error.names("MIN_CLIENT_BUILD"), "{bad}: {error}"),
        }
    }
}

#[test]
fn a_flag_beats_the_environment_which_beats_the_default() {
    // The whole precedence table in one case: the environment moves the port off the
    // built-in default, and the flag moves it off the environment.
    let mut environment = serveable();
    environment.insert("SERVER_PORT".to_owned(), "5000".to_owned());

    let from_env = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(from_env.listen.to_string(), "0.0.0.0:5000");

    let overridden = Overrides {
        listen: Some("127.0.0.1:6000".parse().expect("a literal address parses")),
        ..memory()
    };
    let from_flag = Config::load(&environment, &overridden, Demands::Serve).expect("it loads");
    assert_eq!(from_flag.listen.to_string(), "127.0.0.1:6000");
}

#[test]
fn server_host_and_server_port_compose_into_one_listener() {
    let environment = env(&[
        ("BLOB_ROOT", "/blobs"),
        ("JWT_ED25519_DER", EXAMPLE_DER),
        ("SERVER_HOST", "127.0.0.1"),
        ("SERVER_PORT", "8080"),
    ]);
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.listen.to_string(), "127.0.0.1:8080");
}

#[test]
fn no_proxy_is_trusted_unless_one_is_named() {
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert!(config.trusted_proxies.trusts_nobody());
}

#[test]
fn trusted_proxies_are_read_from_the_environment() {
    let mut environment = serveable();
    environment.insert("TRUSTED_PROXY_HOPS".to_owned(), "1".to_owned());
    environment.insert(
        "TRUSTED_PROXIES".to_owned(),
        "10.0.0.0/8, fd00::/8".to_owned(),
    );
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.trusted_proxies.hops, 1);
    assert_eq!(
        config.trusted_proxies.networks,
        vec![
            ("10.0.0.0".parse().expect("v4"), 8),
            ("fd00::".parse().expect("v6"), 8),
        ]
    );
}

#[test]
fn the_trusted_proxy_header_defaults_to_x_forwarded_for() {
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.trusted_proxies.header, ProxyHeader::XForwarded);
}

#[test]
fn the_trusted_proxy_header_is_read_from_the_environment() {
    for (value, header) in [
        ("forwarded", ProxyHeader::Forwarded),
        (" Forwarded ", ProxyHeader::Forwarded),
        ("x-forwarded-for", ProxyHeader::XForwarded),
        ("X-Forwarded-For", ProxyHeader::XForwarded),
    ] {
        let mut environment = serveable();
        environment.insert("TRUSTED_PROXY_HEADER".to_owned(), value.to_owned());
        let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
        assert_eq!(config.trusted_proxies.header, header, "{value}");
    }
}

#[test]
fn a_malformed_trusted_proxy_setting_is_refused() {
    for (key, value) in [
        ("TRUSTED_PROXIES", "10.0.0.0/40"),
        ("TRUSTED_PROXIES", "proxy.internal"),
        ("TRUSTED_PROXY_HOPS", "-1"),
        ("TRUSTED_PROXY_HEADER", "x-real-ip"),
        ("TRUSTED_PROXY_HEADER", "forwarded, x-forwarded-for"),
    ] {
        let mut environment = serveable();
        environment.insert(key.to_owned(), value.to_owned());
        let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
        assert!(error.names(key), "{key}={value}: {error}");
    }
}

#[test]
fn every_fault_is_reported_in_one_pass() {
    // The property the aggregate exists for: an operator bringing a deployment up learns
    // about all four at once rather than restarting four times.
    let environment = env(&[
        ("SERVER_PORT", "not-a-port"),
        ("LOG_FORMAT", "yaml"),
        ("MAX_CONNECTIONS", "0"),
    ]);
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("SERVER_PORT"), "{error}");
    assert!(error.names("LOG_FORMAT"), "{error}");
    assert!(error.names("MAX_CONNECTIONS"), "{error}");
    assert!(error.names("BLOB_ROOT"), "{error}");
    assert!(error.names("JWT_ED25519_DER"), "{error}");
    // Not `ATTESTATION_KEY_SEED`: `--memory` derives it, so naming it here would send a
    // developer looking for a variable the development profile does not want.
    assert!(!error.names("ATTESTATION_KEY_SEED"), "{error}");

    // The durable path names its own, alongside everything else.
    let error =
        Config::load(&environment, &Overrides::default(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("DATABASE_URL"), "{error}");
    assert!(error.names("VALKEY_URL"), "{error}");
    assert!(error.names("ATTESTATION_KEY_SEED"), "{error}");
}

#[test]
fn serving_without_valkey_and_without_the_memory_profile_is_refused_by_name() {
    // `store/mod.rs` documents this refusal, and boot enforces it.
    let error =
        Config::load(&serveable(), &Overrides::default(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("VALKEY_URL"), "{error}");
}

#[test]
fn a_durable_store_is_demanded_whole_and_in_the_order_it_is_opened() {
    // Every subcommand that opens the durable stores is refused here, by the aggregating
    // front door and with its exit code, naming both URLs at once: the database first,
    // because it is the first store opened.
    let maintenance = env(&[("BLOB_ROOT", "/blobs")]);
    for (environment, demands) in [
        (serveable(), Demands::Serve),
        (maintenance.clone(), Demands::Maintenance),
    ] {
        let error =
            Config::load(&environment, &Overrides::default(), demands).expect_err("it refuses");
        let missing: Vec<_> = error
            .faults()
            .iter()
            .filter_map(|fault| match fault {
                ConfigFault::Missing { key } if key.ends_with("_URL") => Some(*key),
                _ => None,
            })
            .collect();
        assert_eq!(
            missing,
            ["DATABASE_URL", "VALKEY_URL"],
            "{demands:?}: {error}"
        );

        let mut with_database = environment.clone();
        with_database.insert(
            "DATABASE_URL".to_owned(),
            "postgres://db/capsule".to_owned(),
        );
        let error =
            Config::load(&with_database, &Overrides::default(), demands).expect_err("it refuses");
        assert!(!error.names("DATABASE_URL"), "{demands:?}: {error}");
        assert!(error.names("VALKEY_URL"), "{demands:?}: {error}");
    }

    // `--memory` reads neither.
    Config::load(&maintenance, &memory(), Demands::Maintenance).expect("it loads");
    Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
}

#[test]
fn the_memory_profile_is_not_reachable_from_the_environment() {
    // Only `--memory` selects it (#521): the retired variable is ignored, so a serve that
    // names it and not the flag is the durable one, refused for the Valkey it lacks.
    let mut environment = serveable();
    environment.insert("CAPSULE_PROFILE".to_owned(), "memory".to_owned());
    let error =
        Config::load(&environment, &Overrides::default(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("VALKEY_URL"), "{error}");

    let config =
        Config::load(&environment, &Overrides::default(), Demands::Nothing).expect("it loads");
    assert_eq!(config.backends, Backends::Durable);
}

#[test]
fn maintenance_needs_a_blob_root_and_no_key_material() {
    // A maintenance host that had to hold the production token-signing key to sweep a
    // directory would be a reason to put the key on a maintenance host.
    let config = Config::load(
        &env(&[
            ("BLOB_ROOT", "/blobs"),
            ("DATABASE_URL", "postgres://127.0.0.1:5432/capsule"),
            ("VALKEY_URL", "redis://127.0.0.1:6379"),
        ]),
        &Overrides::default(),
        Demands::Maintenance,
    )
    .expect("it loads");
    assert!(config.signing_key_der.is_none());

    let error = Config::load(
        &env(&[("JWT_ED25519_DER", EXAMPLE_DER)]),
        &memory(),
        Demands::Maintenance,
    )
    .expect_err("it refuses");
    assert!(error.names("BLOB_ROOT"), "{error}");
}

#[test]
fn describing_the_router_needs_nothing() {
    let config =
        Config::load(&BTreeMap::new(), &Overrides::default(), Demands::Nothing).expect("it loads");
    assert!(config.blob_root.is_none());
}

#[test]
fn a_durable_serve_must_be_given_an_attestation_seed() {
    // The attestation key must be distinct from the operational key — `attestation/mod.rs`
    // requires it so that holding the token signer does not let anything manufacture custody
    // evidence. Deriving the seed from `JWT_ED25519_DER` collapses exactly that, so a real
    // deployment is made to say what its attestation identity is.
    let environment = env(&[
        ("BLOB_ROOT", "/blobs"),
        ("JWT_ED25519_DER", EXAMPLE_DER),
        ("DATABASE_URL", "postgres://127.0.0.1:5432/capsule"),
        ("VALKEY_URL", "redis://127.0.0.1:6379"),
    ]);
    let error =
        Config::load(&environment, &Overrides::default(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("ATTESTATION_KEY_SEED"), "{error}");

    let mut with_seed = environment.clone();
    with_seed.insert(
        "ATTESTATION_KEY_SEED".to_owned(),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [9_u8; 64]),
    );
    let config = Config::load(&with_seed, &Overrides::default(), Demands::Serve)
        .expect("it loads with a seed of its own");
    assert_eq!(config.attestation_key_seed, Some([9; 64]));
}

#[test]
fn the_cursor_key_and_the_attestation_seed_are_derived_from_the_signing_key() {
    // The **development** profile only. A durable deployment is made to set the seed; see
    // the case above.
    let first = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    let second = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");

    let cursor = first.sync_cursor_mac_key.expect("it is derived");
    let seed = first.attestation_key_seed.expect("it is derived");
    assert_eq!(
        Some(cursor),
        second.sync_cursor_mac_key,
        "derivation is stable"
    );
    assert_eq!(
        Some(seed),
        second.attestation_key_seed,
        "derivation is stable"
    );
    // Domain separation: the two derivations of one key must not be the same bytes.
    assert_ne!(
        &seed[..32],
        &cursor[..],
        "the two infos separate the outputs"
    );
}

#[test]
fn the_lockout_threshold_is_configurable_and_may_not_be_zero() {
    // A lockout is two numbers, not one, and a deployment that wants a tighter policy has to
    // be able to move both.
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.lockout_attempts, 10);

    let mut environment = serveable();
    environment.insert("LOCKOUT_MAX_ATTEMPTS".to_owned(), "3".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.lockout_attempts, 3);

    environment.insert("LOCKOUT_MAX_ATTEMPTS".to_owned(), "0".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("LOCKOUT_MAX_ATTEMPTS"), "{error}");
}

#[test]
fn the_lockout_window_defaults_to_fifteen_minutes_and_is_configurable() {
    // It has to decay at all: no route resets a lockout — every one of them refuses on
    // `Locked` before it verifies anything — so a permanent lockout is a lost account.
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.lockout_window, SignedDuration::from_mins(15));

    let mut environment = serveable();
    environment.insert("LOCKOUT_WINDOW_SECONDS".to_owned(), "1".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.lockout_window, SignedDuration::from_secs(1));
}

#[test]
fn the_session_lifetime_defaults_to_the_design_and_is_configurable() {
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.session_ttl, SignedDuration::from_hours(180 * 24));
    assert_eq!(
        config.session_hard_cap,
        SignedDuration::from_hours(365 * 24)
    );

    let mut environment = serveable();
    environment.insert("SESSION_TTL_DAYS".to_owned(), "30".to_owned());
    environment.insert("SESSION_HARD_CAP_DAYS".to_owned(), " 90 ".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.session_ttl, SignedDuration::from_hours(30 * 24));
    assert_eq!(config.session_hard_cap, SignedDuration::from_hours(90 * 24));

    // A cap equal to the window is coherent: the window simply never outlives it.
    environment.insert("SESSION_HARD_CAP_DAYS".to_owned(), "30".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.session_hard_cap, config.session_ttl);
}

#[test]
fn a_session_lifetime_of_zero_negative_or_not_a_number_is_refused() {
    for key in ["SESSION_TTL_DAYS", "SESSION_HARD_CAP_DAYS"] {
        for value in ["0", "-1", "a year", "1.5", "106751991167301"] {
            let mut environment = serveable();
            environment.insert(key.to_owned(), value.to_owned());
            let error =
                Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
            assert!(error.names(key), "{key}={value}: {error}");
        }
    }
}

#[test]
fn a_hard_cap_shorter_than_the_sliding_window_is_refused() {
    let mut environment = serveable();
    environment.insert("SESSION_HARD_CAP_DAYS".to_owned(), "179".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("SESSION_HARD_CAP_DAYS"), "{error}");
    assert!(!error.names("SESSION_TTL_DAYS"), "{error}");

    // A malformed window is reported once, not again as a cap shorter than its default.
    let mut environment = serveable();
    environment.insert("SESSION_TTL_DAYS".to_owned(), "0".to_owned());
    environment.insert("SESSION_HARD_CAP_DAYS".to_owned(), "30".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("SESSION_TTL_DAYS"), "{error}");
    assert!(!error.names("SESSION_HARD_CAP_DAYS"), "{error}");
}

#[test]
fn an_explicit_cursor_key_wins_over_the_derived_one() {
    let mut environment = serveable();
    let explicit = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        [0x5C_u8; super::CURSOR_KEY_LEN],
    );
    environment.insert("SYNC_CURSOR_MAC_KEY".to_owned(), explicit);
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(
        config.sync_cursor_mac_key,
        Some([0x5C; super::CURSOR_KEY_LEN])
    );
}

#[test]
fn a_cursor_key_of_the_wrong_length_is_refused_by_length_and_not_by_value() {
    let mut environment = serveable();
    environment.insert(
        "SYNC_CURSOR_MAC_KEY".to_owned(),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7_u8; 16]),
    );
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("SYNC_CURSOR_MAC_KEY"), "{error}");
    assert!(format!("{error}").contains("16 bytes"), "{error}");
}

#[test]
fn a_thirty_two_byte_attestation_seed_is_expanded_rather_than_refused() {
    let mut environment = serveable();
    environment.insert(
        "ATTESTATION_KEY_SEED".to_owned(),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [3_u8; 32]),
    );
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    let seed = config.attestation_key_seed.expect("it is expanded");
    // Expanded, not repeated: the two halves of the hybrid key must not share a seed.
    assert_ne!(&seed[..32], &seed[32..]);
}

#[test]
fn a_signing_key_that_is_not_base64_is_refused_without_quoting_it() {
    let mut environment = serveable();
    environment.insert(
        "JWT_ED25519_DER".to_owned(),
        "not base64 at all!!".to_owned(),
    );
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    let message = format!("{error}");
    assert!(error.names("JWT_ED25519_DER"), "{message}");
    assert!(
        !message.contains("not base64 at all"),
        "a startup error must not echo the key material: {message}"
    );
}

#[test]
fn an_unpadded_signing_key_loads() {
    // `openssl genpkey … | base64` piped through anything that strips `=` is common enough
    // that refusing it would be a support question rather than a security property.
    let mut environment = serveable();
    environment.insert(
        "JWT_ED25519_DER".to_owned(),
        EXAMPLE_DER.trim_end_matches('=').to_owned(),
    );
    assert!(Config::load(&environment, &memory(), Demands::Serve).is_ok());
}

#[test]
fn an_empty_variable_is_an_absent_one() {
    // `FOO=` in a compose file means "I did not set this", and reading it as a zero-length
    // signing key would fail somewhere much less obvious than here.
    let mut environment = serveable();
    environment.insert("JWT_ED25519_DER".to_owned(), String::new());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("JWT_ED25519_DER"), "{error}");
}

#[test]
fn the_retired_upload_dir_name_is_still_honoured() {
    let environment = env(&[
        ("UPLOAD_DIR", "/legacy/uploads"),
        ("JWT_ED25519_DER", EXAMPLE_DER),
    ]);
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(
        config.blob_root.as_deref(),
        Some(std::path::Path::new("/legacy/uploads"))
    );
}

#[test]
fn a_config_file_is_refused_with_what_to_do_instead() {
    let overrides = Overrides {
        config_file: Some("/etc/capsule/server.toml".into()),
        ..memory()
    };
    let error = Config::load(&serveable(), &overrides, Demands::Serve).expect_err("it refuses");
    assert!(error.names("--config"), "{error}");
    assert!(format!("{error}").contains("environment"), "{error}");
}

#[test]
fn an_inverted_protocol_window_is_refused() {
    let mut environment = serveable();
    environment.insert("PROTOCOL_MIN".to_owned(), "2099-01-01".to_owned());
    environment.insert("PROTOCOL_MAX".to_owned(), "2025-01-01".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("PROTOCOL_MIN"), "{error}");
}

#[test]
fn the_log_format_follows_the_build_profile_when_unset() {
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    let expected = if cfg!(debug_assertions) {
        LogFormat::Pretty
    } else {
        LogFormat::Json
    };
    assert_eq!(config.log_format, expected);
}

#[test]
fn the_grace_window_is_hours_and_the_flag_beats_the_environment() {
    let mut environment = serveable();
    environment.insert("GC_GRACE_WINDOW_HOURS".to_owned(), "72".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.grace_window, jiff::SignedDuration::from_hours(72));

    let overrides = Overrides {
        grace_window_hours: Some(1),
        ..memory()
    };
    let config = Config::load(&environment, &overrides, Demands::Serve).expect("it loads");
    assert_eq!(config.grace_window, jiff::SignedDuration::from_hours(1));
}

#[test]
fn oidc_is_off_unless_an_issuer_is_named() {
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert!(config.oidc.is_none());
}

#[test]
fn federation_is_off_unless_switched_on_and_a_url_alone_does_not_turn_it_on() {
    // #509: the deferred surface is opt-in. A deployment carrying a `FEDERATION_URL` from
    // before the switch existed is one that never decided, so it does not federate.
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert!(!config.federation_enabled);
    assert_eq!(config.federation_endpoint(), None);

    let mut environment = serveable();
    environment.insert(
        "FEDERATION_URL".to_owned(),
        "https://capsule.example/v1".to_owned(),
    );
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.federation_endpoint(), None);

    for on in ["true", "1", "YES", " on "] {
        environment.insert("FEDERATION_ENABLED".to_owned(), on.to_owned());
        let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
        assert_eq!(
            config.federation_endpoint(),
            Some("https://capsule.example/v1"),
            "FEDERATION_ENABLED={on}"
        );
    }
    environment.insert("FEDERATION_ENABLED".to_owned(), "off".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(config.federation_endpoint(), None);
}

#[test]
fn switching_federation_on_without_a_url_or_with_a_typo_is_refused_by_name() {
    let mut environment = serveable();
    environment.insert("FEDERATION_ENABLED".to_owned(), "true".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("FEDERATION_URL"), "{error}");
    // Only `serve` federates, so maintenance is not sent looking for a URL it never reads.
    environment.insert("BLOB_ROOT".to_owned(), "/blobs".to_owned());
    Config::load(&environment, &memory(), Demands::Maintenance)
        .expect("maintenance loads without one");

    let mut environment = serveable();
    environment.insert("FEDERATION_ENABLED".to_owned(), "enabled".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("FEDERATION_ENABLED"), "{error}");
}

/// The environment a `serve --memory` with a whole OIDC relying party needs.
fn serveable_with_oidc() -> BTreeMap<String, String> {
    let mut environment = serveable();
    environment.insert(
        "OIDC_ISSUER".to_owned(),
        "https://idp.example.test/realm".to_owned(),
    );
    environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
    environment
}

#[test]
fn password_auth_is_on_unless_switched_off() {
    let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
    assert!(!config.password_auth_disabled);

    let mut environment = serveable_with_oidc();
    for on in ["true", "1", "YES", " on "] {
        environment.insert("PASSWORD_AUTH_DISABLED".to_owned(), on.to_owned());
        let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
        assert!(config.password_auth_disabled, "PASSWORD_AUTH_DISABLED={on}");
    }
    environment.insert("PASSWORD_AUTH_DISABLED".to_owned(), "off".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert!(!config.password_auth_disabled);
}

#[test]
fn disabling_passwords_without_an_identity_provider_is_refused_by_name() {
    let mut environment = serveable();
    environment.insert("PASSWORD_AUTH_DISABLED".to_owned(), "true".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("OIDC_ISSUER"), "{error}");
    // Only `serve` admits anybody, so maintenance is not sent looking for a provider.
    Config::load(&environment, &memory(), Demands::Maintenance)
        .expect("maintenance loads without one");

    // A half-configured relying party is told what it lacks, not that its issuer is missing.
    environment.insert(
        "OIDC_ISSUER".to_owned(),
        "https://idp.example.test/realm".to_owned(),
    );
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("OIDC_CLIENT_ID"), "{error}");
    assert!(!error.names("OIDC_ISSUER"), "{error}");

    let mut environment = serveable_with_oidc();
    environment.insert("PASSWORD_AUTH_DISABLED".to_owned(), "sometimes".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("PASSWORD_AUTH_DISABLED"), "{error}");
}

#[test]
fn an_oidc_relying_party_is_read_whole() {
    let mut environment = serveable();
    environment.insert(
        "OIDC_ISSUER".to_owned(),
        "https://idp.example.test/realm".to_owned(),
    );
    environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
    environment.insert("OIDC_CLIENT_SECRET".to_owned(), "hunter2".to_owned());
    environment.insert(
        "OIDC_REDIRECT_URL".to_owned(),
        "https://app.example.test/oidc/callback".to_owned(),
    );
    environment.insert(
        "OIDC_ALLOW_LOOPBACK_REDIRECT".to_owned(),
        "false".to_owned(),
    );
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    let oidc = config.oidc.clone().expect("configured");
    assert_eq!(oidc.issuer, "https://idp.example.test/realm");
    assert_eq!(oidc.client_id, "capsule");
    assert!(oidc.client_secret.is_some());
    assert!(
        oidc.redirects
            .admits("https://app.example.test/oidc/callback")
    );
    assert!(!oidc.redirects.admits("http://127.0.0.1:4242/cb"));
    assert!(
        !format!("{config:?}").contains("hunter2"),
        "the client secret is redacted"
    );
}

#[test]
fn loopback_redirects_are_opt_in_and_a_secret_is_optional() {
    // RFC 8252 §8.5: a native client cannot keep a secret; PKCE is what makes it sound. And
    // the loopback arm is the one knob that widens where the server redirects to, so it is
    // off until a deployment with such a client turns it on.
    let mut environment = serveable();
    environment.insert(
        "OIDC_ISSUER".to_owned(),
        "https://idp.example.test".to_owned(),
    );
    environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    let oidc = config.oidc.expect("configured");
    assert!(oidc.client_secret.is_none());
    assert!(!oidc.redirects.admits("http://127.0.0.1:4242/cb"));

    environment.insert("OIDC_ALLOW_LOOPBACK_REDIRECT".to_owned(), "true".to_owned());
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert!(
        config
            .oidc
            .expect("configured")
            .redirects
            .admits("http://127.0.0.1:4242/cb")
    );
}

#[test]
fn a_ca_bundle_is_a_path_read_later_not_here() {
    let mut environment = serveable();
    environment.insert(
        "OIDC_ISSUER".to_owned(),
        "https://idp.example.test".to_owned(),
    );
    environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
    environment.insert(
        "OIDC_CA_BUNDLE".to_owned(),
        "/etc/capsule/idp-ca.pem".to_owned(),
    );
    // The file does not exist and loading does not care: reading it is boot's job.
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    assert_eq!(
        config.oidc.expect("configured").ca_bundle.as_deref(),
        Some(std::path::Path::new("/etc/capsule/idp-ca.pem"))
    );
}

#[test]
fn a_redirect_url_is_https_or_loopback_http() {
    for (redirect, ok) in [
        ("https://app.example.test/cb", true),
        ("http://127.0.0.1:4242/cb", true),
        ("http://app.example.test/cb", false),
        ("http://localhost:4242/cb", false),
    ] {
        let mut environment = serveable();
        environment.insert(
            "OIDC_ISSUER".to_owned(),
            "https://idp.example.test".to_owned(),
        );
        environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
        environment.insert("OIDC_REDIRECT_URL".to_owned(), redirect.to_owned());
        let result = Config::load(&environment, &memory(), Demands::Serve);
        assert_eq!(result.is_ok(), ok, "{redirect}: {result:?}");
        if let Err(error) = result {
            assert!(error.names("OIDC_REDIRECT_URL"), "{error}");
        }
    }
}

#[test]
fn half_a_relying_party_is_refused_both_ways_round() {
    let mut environment = serveable();
    environment.insert(
        "OIDC_ISSUER".to_owned(),
        "https://idp.example.test".to_owned(),
    );
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("OIDC_CLIENT_ID"), "{error}");

    let mut environment = serveable();
    environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("OIDC_ISSUER"), "{error}");
}

#[test]
fn an_issuer_is_https_or_loopback_http_with_no_query() {
    for (issuer, ok) in [
        ("https://idp.example.test", true),
        ("http://127.0.0.1:5556/dex", true),
        ("http://idp.example.test", false),
        ("https://idp.example.test/?x=1", false),
        ("https://idp.example.test/#frag", false),
        ("idp.example.test", false),
        ("ftp://idp.example.test", false),
    ] {
        let mut environment = serveable();
        environment.insert("OIDC_ISSUER".to_owned(), issuer.to_owned());
        environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
        let result = Config::load(&environment, &memory(), Demands::Serve);
        assert_eq!(result.is_ok(), ok, "{issuer}: {result:?}");
        if let Err(error) = result {
            assert!(error.names("OIDC_ISSUER"), "{error}");
        }
    }
}

#[test]
fn a_malformed_redirect_url_or_loopback_flag_is_refused_by_name() {
    let mut environment = serveable();
    environment.insert(
        "OIDC_ISSUER".to_owned(),
        "https://idp.example.test".to_owned(),
    );
    environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
    environment.insert("OIDC_REDIRECT_URL".to_owned(), "not a url".to_owned());
    environment.insert(
        "OIDC_ALLOW_LOOPBACK_REDIRECT".to_owned(),
        "maybe".to_owned(),
    );
    let error = Config::load(&environment, &memory(), Demands::Serve).expect_err("it refuses");
    assert!(error.names("OIDC_REDIRECT_URL"), "{error}");
    assert!(error.names("OIDC_ALLOW_LOOPBACK_REDIRECT"), "{error}");
}

#[test]
fn a_secret_is_not_printed_by_debug() {
    let mut environment = serveable();
    environment.insert(
        "DATABASE_URL".to_owned(),
        "postgres://capsule:pg-hunter2@db.internal:5432/capsule?password=pg-query-secret"
            .to_owned(),
    );
    environment.insert(
        "VALKEY_URL".to_owned(),
        "rediss://default:vk-hunter2@cache.internal:6380/0".to_owned(),
    );
    let attestation_seed = [0x5a_u8; 32];
    environment.insert(
        "ATTESTATION_KEY_SEED".to_owned(),
        BASE64.encode(attestation_seed),
    );
    let config = Config::load(&environment, &memory(), Demands::Serve).expect("it loads");
    let mac_key = config.sync_cursor_mac_key.expect("derived when unset");
    let seed = config.attestation_key_seed.expect("configured");
    for rendered in [format!("{config:?}"), format!("{config:#?}")] {
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("MC4CAQAwBQYDK2Vw"), "{rendered}");
        for secret in [
            "pg-hunter2",
            "pg-query-secret",
            "vk-hunter2",
            "capsule:",
            "default:",
        ] {
            assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
        }
        // The fixed-size keys print as byte lists under a derived `Debug`.
        for key in [&mac_key[..], &seed[..]] {
            assert!(!rendered.contains(&format!("{key:?}")), "{rendered}");
        }
        // What an operator needs to see which service was reached survives.
        assert!(rendered.contains("db.internal:5432/capsule"), "{rendered}");
        assert!(rendered.contains("cache.internal:6380/0"), "{rendered}");
    }
}

#[test]
fn a_url_keeps_its_service_and_loses_every_credential() {
    for (url, expected) in [
        (
            "postgres://user:pa@ss@db:5432/capsule?sslmode=require&password=x",
            "postgres://<redacted>@db:5432/capsule?<redacted>",
        ),
        (
            "redis://:secret@cache:6379",
            "redis://<redacted>@cache:6379",
        ),
        ("redis://cache:6379/0", "redis://cache:6379/0"),
        (
            "redis+unix:///run/valkey.sock?pass=x",
            "redis+unix:///run/valkey.sock?<redacted>",
        ),
        (
            "postgres://u:p@db/c#frag",
            "postgres://<redacted>@db/c#<redacted>",
        ),
        ("user:secret@db:5432", "<redacted>@db:5432"),
        ("//user:secret@db:5432", "<redacted>@db:5432"),
        // A malformed password still never reaches the log, whatever it holds.
        ("postgres://u:p?ss@db/x", "postgres://<redacted>@db/x"),
        ("postgres://u:p/ss@db/x", "postgres://<redacted>@db/x"),
        ("postgres://u:p#ss@db/x", "postgres://<redacted>@db/x"),
        (
            "postgres://[::1]:5432/capsule",
            "postgres://[::1]:5432/capsule",
        ),
    ] {
        assert_eq!(redacted_url(url), expected, "{url}");
    }
}

#[test]
fn a_keyword_value_connection_string_keeps_its_service_and_loses_every_credential() {
    for (conninfo, expected) in [
        (
            "host=db.internal port=5432 dbname=capsule user=capsule password=pg-hunter2",
            "host=db.internal port=5432 dbname=capsule user=<redacted> password=<redacted>",
        ),
        // Spaces around `=`, a quoted password holding spaces and an escaped quote.
        (
            "host = db  password = 'a b\\' c' sslmode=require",
            "host=db password=<redacted> sslmode=<redacted>",
        ),
        // An unquoted value with an escaped space is still one value.
        ("password=a\\ b host=db", "password=<redacted> host=db"),
        (
            "hostaddr=10.0.0.1 dbname='my db'",
            "hostaddr=10.0.0.1 dbname='my db'",
        ),
        // What does not parse is redacted whole rather than guessed at.
        ("host=db password='never closed", "<redacted>"),
        ("host=db secret", "<redacted>"),
    ] {
        assert_eq!(redacted_url(conninfo), expected, "{conninfo}");
    }
}
