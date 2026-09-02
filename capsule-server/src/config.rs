//! [`Config`] — everything an operator gets to decide, read once at startup.
//!
//! # Environment only, and why there is no file
//!
//! `--config PATH` is accepted on the command line and **refused**: a configuration-file crate
//! would be a new dependency in a domain `design/dependencies.md` has no row for, and the
//! server this replaces was environment-only plus `dotenvy`
//! (`legacy-review/server-salvo/environment/`), so an operator loses nothing familiar. The flag
//! exists rather than being absent so the refusal is a sentence rather than clap's "unexpected
//! argument", and so the precedence table below already names the slot a file layer would sit
//! in.
//!
//! Precedence, highest first: **command line → process environment → built-in default.**
//!
//! # Every fault, once
//!
//! [`Config::load`] reports **all** of them ([`ConfigError`] holds a list) instead of failing on
//! the first. An operator bringing a deployment up otherwise restarts the process once per
//! variable, learning one missing key at a time from a server that already knew about four.
//!
//! # What is required depends on the subcommand
//!
//! `gc`, `purge` and `scrub` need a blob root and **no key material** — demanding a signing key
//! to sweep a directory would be a reason to keep a production key on a maintenance host. So
//! the requirement set is a parameter ([`Demands`]) rather than a property of the type.
//!
//! # Secrets
//!
//! [`SecretBytes`] redacts itself in `Debug`, the way
//! [`SessionTokens`](crate::auth::SessionTokens) does by hand: `Config` is logged at startup,
//! and a `Debug` that printed the token-signing key is how one reaches a log file.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as BASE64, STANDARD_NO_PAD as BASE64_NO_PAD};
use jiff::SignedDuration;

use crate::sync::CURSOR_KEY_LEN;

/// The bind address a deployment gets without saying anything.
const DEFAULT_LISTEN: &str = "0.0.0.0:3000";

/// The port half of [`DEFAULT_LISTEN`], for composing `SERVER_HOST` with no `SERVER_PORT`.
const DEFAULT_PORT: u16 = 3000;

/// The domain a deployment gets without saying anything.
const DEFAULT_DOMAIN: &str = "localhost";

/// The drain deadline, matching Kynos's own default — under the usual 30-second orchestrator
/// termination window, which is the whole reason that number is what it is.
const DEFAULT_SHUTDOWN_TIMEOUT: u64 = 25;

/// The accepted-connection ceiling, matching Kynos's own default.
const DEFAULT_MAX_CONNECTIONS: usize = 10_000;

/// How long an account stays locked after enough failed credential presentations.
///
/// Fifteen minutes. `design/authentication.md` names no figure — it says only that a locked
/// account is locked at password change too — so this is a decision recorded here rather than a
/// value read from somewhere: long enough that an online guessing run is throttled to
/// uselessness, short enough that a person who mistyped their password four times gets back into
/// their own account without an operator.
///
/// It has to decay at all, because there is **no unlock endpoint and no operator command that
/// clears it**: every route that could reset the state (`login`, `reauthenticate`,
/// `password`) refuses on `Locked` before it verifies anything, so a permanent lockout is
/// a permanently lost account. Seconds rather than minutes as the unit so a test can pick a
/// window it can actually wait out.
const DEFAULT_LOCKOUT_WINDOW_SECONDS: u64 = 15 * 60;

/// How many consecutive failures inside that window lock an account.
///
/// The companion number to the window — a lockout is not one policy but two, and a deployment
/// that wants a tighter one needs to move both. Ten is
/// [`MAX_FAILED_ATTEMPTS`](crate::auth::accounts_memory::MAX_FAILED_ATTEMPTS), which is where the
/// reasoning for the figure lives.
const DEFAULT_LOCKOUT_ATTEMPTS: u32 = crate::auth::accounts_memory::MAX_FAILED_ATTEMPTS;

/// The seed [`HybridSigningKey`](capsule_core::crypto::keys::HybridSigningKey) is built from.
const ATTESTATION_SEED_LEN: usize = 64;

/// HKDF `info` for the sync-cursor MAC key derived from the token-signing key.
const CURSOR_KEY_INFO: &[u8] = b"capsule/sync-cursor-mac/v1";

/// HKDF `info` for the attestation seed derived from the token-signing key.
const ATTESTATION_SEED_INFO: &[u8] = b"capsule/attestation-seed/v1";

/// Where a setting is read from.
///
/// A trait rather than [`std::env::var`] directly so the precedence table is a unit test rather
/// than a claim: a test builds the environment it wants and asserts what came out, with no
/// process-global state two concurrent tests would fight over.
pub trait Environment: fmt::Debug {
    /// The value of `key`, or `None` when it is unset **or set to the empty string**.
    ///
    /// Empty is absent on purpose. `FOO=` in a compose file or a `.env` is how an operator
    /// writes "I did not set this", and a server that read it as a zero-length signing key
    /// would fail somewhere much less obvious than here.
    fn var(&self, key: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Debug, Clone, Copy)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|value| !value.is_empty())
    }
}

impl Environment for BTreeMap<String, String> {
    fn var(&self, key: &str) -> Option<String> {
        self.get(key).filter(|value| !value.is_empty()).cloned()
    }
}

/// Bytes that must not be printed.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Hold `bytes` as a secret.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes, for the one caller that has to use them.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(<redacted>)")
    }
}

/// Which family of adapters a process runs on.
///
/// Two arms, and the second one is not implemented yet — see
/// [`assemble`](crate::boot::assemble). It is an enum rather than a trait because the
/// `Arc<dyn Port>` fields in [`Modules`](crate::app::Modules) already **are** the abstraction;
/// a second one over the top would abstract the composition root from itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backends {
    /// Every deterministic in-crate adapter, over a real filesystem blob store.
    ///
    /// An explicit operator act (`--memory`, or `CAPSULE_PROFILE=memory`) and **never** a
    /// fallback: a deployment that forgets `VALKEY_URL` must fail closed rather than come up
    /// holding state it will lose on the next restart.
    Memory,
    /// Postgres and Valkey, selected by `DATABASE_URL` and `VALKEY_URL`.
    Durable,
}

/// How the log stream is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per event — what a log shipper wants.
    Json,
    /// Multi-line, coloured, human-first — what a developer wants.
    Pretty,
}

/// What a subcommand needs before it can do anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Demands {
    /// `serve`: a blob root, a token-signing key, and a chosen backend family.
    Serve,
    /// `gc` / `purge` / `scrub`: a blob root, and deliberately no key material.
    Maintenance,
    /// `gen-openapi`: nothing at all. The document is a property of the router's types.
    Nothing,
}

/// One thing wrong with the configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigFault {
    /// A required setting is absent.
    #[error("{key} is required and is not set")]
    Missing {
        /// The environment variable or flag an operator has to set.
        key: &'static str,
    },
    /// A setting is present and cannot be used.
    ///
    /// `detail` never quotes the value: `JWT_ED25519_DER` is a private key, and a startup error
    /// is the most-copied line in any incident channel.
    #[error("{key} is not usable: {detail}")]
    Invalid {
        /// The setting.
        key: &'static str,
        /// What is wrong with it — never the value itself.
        detail: String,
    },
    /// A setting is understood, accepted at the boundary, and not implemented.
    #[error("{key} is not supported yet: {detail}")]
    Unsupported {
        /// The setting.
        key: &'static str,
        /// What to do instead.
        detail: String,
    },
}

/// Everything wrong with the configuration, in one message.
///
/// `Display` is hand-written rather than `thiserror`-generated because the message *is* a list;
/// a derived one-line format would put five faults on one line, which is the shape an operator
/// reads worst. The variants themselves are `thiserror` as the repository requires.
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError {
    faults: Vec<ConfigFault>,
}

impl ConfigError {
    /// Every fault found, in the order the fields are read.
    pub fn faults(&self) -> &[ConfigFault] {
        &self.faults
    }

    /// Whether `key` is among the faults, for a test that asserts one was reported.
    pub fn names(&self, key: &str) -> bool {
        self.faults.iter().any(|fault| match fault {
            ConfigFault::Missing { key: named }
            | ConfigFault::Invalid { key: named, .. }
            | ConfigFault::Unsupported { key: named, .. } => *named == key,
        })
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the server configuration is not usable ({} problem{})",
            self.faults.len(),
            if self.faults.len() == 1 { "" } else { "s" }
        )?;
        for fault in &self.faults {
            write!(f, "\n  - {fault}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

/// The command line's say, applied over the environment.
///
/// Every field is an `Option` (or a `bool` that is only ever set) so "the operator did not pass
/// this flag" and "the operator passed this flag with the default value" are different states —
/// which is what makes the precedence table implementable at all.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// `--config PATH`. Refused; see the module docs.
    pub config_file: Option<PathBuf>,
    /// `--listen HOST:PORT`.
    pub listen: Option<SocketAddr>,
    /// `--blob-root PATH`.
    pub blob_root: Option<PathBuf>,
    /// `--memory`.
    pub memory: bool,
    /// `--grace-window-hours N`.
    pub grace_window_hours: Option<u64>,
}

/// Everything an operator gets to decide.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the process accepts connections.
    pub listen: SocketAddr,
    /// This deployment's canonical origin — the `server_id` every published record carries.
    pub server_domain: String,
    /// The absolute base URL clients reach the versioned API at.
    pub api_base_url: String,
    /// The filesystem tree ciphertext blobs are written to. There is no object store.
    pub blob_root: Option<PathBuf>,
    /// The Postgres URL, once an adapter reads it (#402).
    pub database_url: Option<String>,
    /// The Valkey URL, once an adapter reads it (#403).
    pub valkey_url: Option<String>,
    /// The PKCS#8 Ed25519 private key access and refresh tokens are signed with.
    pub signing_key_der: Option<SecretBytes>,
    /// The HMAC key sync cursors are authenticated under.
    pub sync_cursor_mac_key: Option<[u8; CURSOR_KEY_LEN]>,
    /// The seed the attestation signing key is built from.
    pub attestation_key_seed: Option<[u8; ATTESTATION_SEED_LEN]>,
    /// The oldest `protocol_version` accepted for writes.
    pub protocol_min: String,
    /// The newest `protocol_version` this server speaks.
    pub protocol_max: String,
    /// How long a blob sits at zero references before the collector may sweep it.
    pub grace_window: SignedDuration,
    /// How long an account stays locked after too many failed credential presentations.
    pub lockout_window: SignedDuration,
    /// How many consecutive failures inside that window lock it.
    pub lockout_attempts: u32,
    /// How long a shutdown may take to drain.
    pub shutdown_timeout: std::time::Duration,
    /// The accepted-connection ceiling.
    pub max_connections: NonZeroUsize,
    /// How the log stream is rendered.
    pub log_format: LogFormat,
    /// Which family of adapters to run on.
    pub backends: Backends,
}

impl Config {
    /// Read the configuration for a subcommand that demands `demands`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] carrying **every** fault found, never only the first.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the settings table, in the order the table is written"
    )]
    pub fn load(
        env: &dyn Environment,
        overrides: &Overrides,
        demands: Demands,
    ) -> Result<Self, ConfigError> {
        let mut faults = Vec::new();

        if let Some(path) = &overrides.config_file {
            faults.push(ConfigFault::Unsupported {
                key: "--config",
                detail: format!(
                    "config files are not supported yet; every setting is read from the \
                     environment, so drop `--config {}` and export the variables instead",
                    path.display()
                ),
            });
        }

        // ── Listener ────────────────────────────────────────────────────────────────────
        let port = parse_number::<u16>(env, "SERVER_PORT", &mut faults).unwrap_or(DEFAULT_PORT);
        let listen = overrides.listen.or_else(|| {
            let host = env.var("SERVER_HOST")?;
            match host.parse::<IpAddr>() {
                Ok(address) => Some(SocketAddr::new(address, port)),
                Err(error) => {
                    faults.push(ConfigFault::Invalid {
                        key: "SERVER_HOST",
                        // The value is an address, not a secret, and a typo is the whole point.
                        detail: format!("`{host}` is not an IP address to bind ({error})"),
                    });
                    None
                }
            }
        });
        let listen = listen.unwrap_or_else(|| {
            let default: SocketAddr = DEFAULT_LISTEN
                .parse()
                .expect("the built-in default listener parses");
            SocketAddr::new(default.ip(), port)
        });

        // ── Identity ────────────────────────────────────────────────────────────────────
        let server_domain = env
            .var("SERVER_DOMAIN")
            .unwrap_or_else(|| DEFAULT_DOMAIN.to_owned());
        // `/v1` included: `ServerInfo` derives the published auth endpoints by appending to this,
        // so a base URL without the version prefix publishes `http://host/auth/login`, which no
        // route serves. The default is what a developer reaches on their own machine.
        let api_base_url = env
            .var("API_BASE_URL")
            .unwrap_or_else(|| format!("http://{server_domain}:{}/v1", listen.port()));

        // ── Storage ─────────────────────────────────────────────────────────────────────
        // `UPLOAD_DIR` is the name the retired deployment used, accepted so an operator's
        // existing environment keeps working, and warned about so it does not become the name.
        let blob_root = overrides
            .blob_root
            .clone()
            .or_else(|| env.var("BLOB_ROOT").map(PathBuf::from))
            .or_else(|| {
                let legacy = env.var("UPLOAD_DIR").map(PathBuf::from)?;
                tracing::warn!(
                    "UPLOAD_DIR is the retired name for BLOB_ROOT and is still honoured; \
                     rename it"
                );
                Some(legacy)
            });
        let database_url = env.var("DATABASE_URL");
        let valkey_url = env.var("VALKEY_URL");

        // ── Backend family ──────────────────────────────────────────────────────────────
        let backends = if overrides.memory
            || env
                .var("CAPSULE_PROFILE")
                .is_some_and(|profile| profile.eq_ignore_ascii_case("memory"))
        {
            Backends::Memory
        } else {
            Backends::Durable
        };

        // ── Key material ────────────────────────────────────────────────────────────────
        let signing_key_der =
            decode_base64(env, "JWT_ED25519_DER", &mut faults).map(SecretBytes::new);
        let sync_cursor_mac_key =
            decode_fixed::<CURSOR_KEY_LEN>(env, "SYNC_CURSOR_MAC_KEY", &mut faults);
        let attestation_key_seed = decode_seed(env, &mut faults);

        // The sync-cursor MAC key is HKDF-derived from the token-signing key when unset. That is
        // sound: a cursor MAC and a session token are the same trust domain — both are
        // operational secrets this server holds to authenticate its own output — so deriving one
        // from the other adds no capability to anybody who holds either.
        //
        // **The attestation seed is not, outside the development profile**, and that is a
        // correction rather than a preference. `attestation/mod.rs` requires the attestation key
        // to be distinct from the operational key precisely so that holding the operational key
        // does not let anything manufacture custody evidence. Deriving the seed from
        // `JWT_ED25519_DER` collapses exactly that distinction: anyone with the token-signing key
        // recomputes the attestation key and signs receipts. So a real deployment must set
        // `ATTESTATION_KEY_SEED` (see the `Demands::Serve` arm below), and only
        // `Backends::Memory` — an explicit `--memory`, a development act, where the whole
        // application state is discarded on exit — keeps the derivation, so `serve --memory`
        // needs one variable rather than two.
        let sync_cursor_mac_key = sync_cursor_mac_key.or_else(|| {
            derive::<CURSOR_KEY_LEN>(signing_key_der.as_ref()?.expose(), CURSOR_KEY_INFO)
        });
        let attestation_key_seed = attestation_key_seed.or_else(|| match backends {
            Backends::Memory => derive::<ATTESTATION_SEED_LEN>(
                signing_key_der.as_ref()?.expose(),
                ATTESTATION_SEED_INFO,
            ),
            Backends::Durable => None,
        });

        // ── Protocol window ─────────────────────────────────────────────────────────────
        let protocol_max = env
            .var("PROTOCOL_MAX")
            .unwrap_or_else(|| capsule_core::crypto::PROTOCOL_VERSION.to_owned());
        let protocol_min = env
            .var("PROTOCOL_MIN")
            .unwrap_or_else(|| capsule_core::crypto::PROTOCOL_VERSION.to_owned());
        if protocol_min > protocol_max {
            faults.push(ConfigFault::Invalid {
                key: "PROTOCOL_MIN",
                detail: format!("`{protocol_min}` is newer than PROTOCOL_MAX `{protocol_max}`"),
            });
        }

        // ── Operational knobs ───────────────────────────────────────────────────────────
        let grace_window = overrides
            .grace_window_hours
            .or_else(|| parse_number::<u64>(env, "GC_GRACE_WINDOW_HOURS", &mut faults))
            .map_or(crate::gc::DEFAULT_GRACE_WINDOW, |hours| {
                SignedDuration::from_hours(i64::try_from(hours).unwrap_or(i64::MAX))
            });
        let lockout_window = SignedDuration::from_secs(
            parse_number::<i64>(env, "LOCKOUT_WINDOW_SECONDS", &mut faults).map_or_else(
                || {
                    i64::try_from(DEFAULT_LOCKOUT_WINDOW_SECONDS)
                        .expect("the built-in lockout window fits")
                },
                |seconds| seconds.max(0),
            ),
        );
        let lockout_attempts = parse_number::<u32>(env, "LOCKOUT_MAX_ATTEMPTS", &mut faults)
            .and_then(|attempts| {
                if attempts == 0 {
                    // Zero would lock every account on its first wrong keystroke and never
                    // unlock it before the window passed, which is a denial of service dressed
                    // as a policy. Refused rather than clamped to one: an operator who typed it
                    // meant something, and guessing what is worse than saying it is not allowed.
                    faults.push(ConfigFault::Invalid {
                        key: "LOCKOUT_MAX_ATTEMPTS",
                        detail: "zero would lock every account on its first failure".to_owned(),
                    });
                    None
                } else {
                    Some(attempts)
                }
            })
            .unwrap_or(DEFAULT_LOCKOUT_ATTEMPTS);
        let shutdown_timeout = std::time::Duration::from_secs(
            parse_number::<u64>(env, "SHUTDOWN_TIMEOUT_SECONDS", &mut faults)
                .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT),
        );
        let max_connections = parse_number::<usize>(env, "MAX_CONNECTIONS", &mut faults)
            .and_then(|limit| {
                NonZeroUsize::new(limit).or_else(|| {
                    faults.push(ConfigFault::Invalid {
                        key: "MAX_CONNECTIONS",
                        detail: "zero would accept nothing at all".to_owned(),
                    });
                    None
                })
            })
            .unwrap_or_else(|| {
                NonZeroUsize::new(DEFAULT_MAX_CONNECTIONS)
                    .expect("the built-in connection ceiling is non-zero")
            });
        let log_format = match env.var("LOG_FORMAT") {
            None => {
                // JSON in release because the reader is a log shipper; pretty in debug because
                // the reader is a person with the source open.
                if cfg!(debug_assertions) {
                    LogFormat::Pretty
                } else {
                    LogFormat::Json
                }
            }
            Some(format) if format.eq_ignore_ascii_case("json") => LogFormat::Json,
            Some(format) if format.eq_ignore_ascii_case("pretty") => LogFormat::Pretty,
            Some(format) => {
                faults.push(ConfigFault::Invalid {
                    key: "LOG_FORMAT",
                    detail: format!("`{format}` is neither `json` nor `pretty`"),
                });
                LogFormat::Json
            }
        };

        // ── What the subcommand demands ─────────────────────────────────────────────────
        //
        // Last, and after every parse, so one message carries both "this is malformed" and
        // "that is missing" rather than a restart between them.
        match demands {
            Demands::Nothing => {}
            Demands::Maintenance => {
                require(blob_root.is_some(), "BLOB_ROOT", &mut faults);
            }
            Demands::Serve => {
                require(blob_root.is_some(), "BLOB_ROOT", &mut faults);
                require(signing_key_der.is_some(), "JWT_ED25519_DER", &mut faults);
                // The refusal `store/mod.rs` has always documented and nothing has ever
                // enforced: Valkey is required, and the in-memory adapters are a development
                // profile an operator opts into rather than something to fall back on.
                if backends == Backends::Durable {
                    if valkey_url.is_none() {
                        faults.push(ConfigFault::Missing { key: "VALKEY_URL" });
                    }
                    // Required rather than derived; see the key-material section above for what
                    // deriving it from the token-signing key would give away. Demanded only on
                    // the durable path because `--memory` derives it, so a development server
                    // still comes up on one variable.
                    if attestation_key_seed.is_none() {
                        faults.push(ConfigFault::Missing {
                            key: "ATTESTATION_KEY_SEED",
                        });
                    }
                }
            }
        }

        if faults.is_empty() {
            Ok(Self {
                listen,
                server_domain,
                api_base_url,
                blob_root,
                database_url,
                valkey_url,
                signing_key_der,
                sync_cursor_mac_key,
                attestation_key_seed,
                protocol_min,
                protocol_max,
                grace_window,
                lockout_window,
                lockout_attempts,
                shutdown_timeout,
                max_connections,
                log_format,
                backends,
            })
        } else {
            Err(ConfigError { faults })
        }
    }
}

/// Record a missing required setting.
fn require(present: bool, key: &'static str, faults: &mut Vec<ConfigFault>) {
    if !present {
        faults.push(ConfigFault::Missing { key });
    }
}

/// Parse `key` as `T`, recording a fault rather than failing the whole read.
fn parse_number<T>(
    env: &dyn Environment,
    key: &'static str,
    faults: &mut Vec<ConfigFault>,
) -> Option<T>
where
    T: std::str::FromStr,
    T::Err: fmt::Display,
{
    let raw = env.var(key)?;
    match raw.trim().parse::<T>() {
        Ok(value) => Some(value),
        Err(error) => {
            faults.push(ConfigFault::Invalid {
                key,
                detail: format!("`{raw}` is not a number this setting accepts ({error})"),
            });
            None
        }
    }
}

/// Decode `key` from base64, accepting padded and unpadded input.
///
/// Two alphabets rather than one because the documented way to produce `JWT_ED25519_DER` is
/// `openssl genpkey … | base64 -w 0`, and a shell pipeline that strips the padding is common
/// enough that refusing it would be a support question rather than a security property.
fn decode_base64(
    env: &dyn Environment,
    key: &'static str,
    faults: &mut Vec<ConfigFault>,
) -> Option<Vec<u8>> {
    let raw = env.var(key)?;
    let trimmed = raw.trim();
    if let Ok(bytes) = BASE64.decode(trimmed) {
        return Some(bytes);
    }
    match BASE64_NO_PAD.decode(trimmed) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            faults.push(ConfigFault::Invalid {
                key,
                // The error names a position, never the bytes: this value is a private key.
                detail: format!("it is not base64 ({error})"),
            });
            None
        }
    }
}

/// Decode `key` from base64 and require exactly `N` bytes.
fn decode_fixed<const N: usize>(
    env: &dyn Environment,
    key: &'static str,
    faults: &mut Vec<ConfigFault>,
) -> Option<[u8; N]> {
    let bytes = decode_base64(env, key, faults)?;
    let found = bytes.len();
    <[u8; N]>::try_from(bytes.as_slice()).ok().or_else(|| {
        faults.push(ConfigFault::Invalid {
            key,
            detail: format!("it decodes to {found} bytes and must be exactly {N}"),
        });
        None
    })
}

/// Decode `ATTESTATION_KEY_SEED`, accepting 32 bytes and expanding them to 64.
///
/// Thirty-two is what every general-purpose "generate a seed" instruction produces, and the
/// hybrid signing key needs sixty-four; expanding rather than refusing means an operator's
/// `openssl rand -base64 32` works, and the expansion is domain-separated so the two halves are
/// not the same 32 bytes twice.
fn decode_seed(
    env: &dyn Environment,
    faults: &mut Vec<ConfigFault>,
) -> Option<[u8; ATTESTATION_SEED_LEN]> {
    let bytes = decode_base64(env, "ATTESTATION_KEY_SEED", faults)?;
    match bytes.len() {
        ATTESTATION_SEED_LEN => <[u8; ATTESTATION_SEED_LEN]>::try_from(bytes.as_slice()).ok(),
        32 => derive::<ATTESTATION_SEED_LEN>(&bytes, ATTESTATION_SEED_INFO),
        found => {
            faults.push(ConfigFault::Invalid {
                key: "ATTESTATION_KEY_SEED",
                detail: format!(
                    "it decodes to {found} bytes and must be 32 or {ATTESTATION_SEED_LEN}"
                ),
            });
            None
        }
    }
}

/// HKDF-SHA256 `secret` into `N` bytes under `info`.
///
/// `ring` rather than a second HKDF implementation: it is already this crate's HMAC for the sync
/// cursor, so the derived key and the key it authenticates come from one primitive.
fn derive<const N: usize>(secret: &[u8], info: &[u8]) -> Option<[u8; N]> {
    /// The output length, as `ring`'s key-type trait wants it.
    #[derive(Debug, Clone, Copy)]
    struct Len(usize);

    impl ring::hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }

    // An empty salt is HKDF's documented default and the right one here: the derivation is
    // domain-separated by `info`, and a salt would have to be configured — one more variable an
    // operator can get wrong for no gain, because the input is already a private key.
    let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, &[]).extract(secret);
    let mut out = [0u8; N];
    prk.expand(&[info], Len(N)).ok()?.fill(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use jiff::SignedDuration;

    use super::{Backends, Config, Demands, LogFormat, Overrides};

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
        assert_eq!(config.protocol_min, config.protocol_max);
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

        // The durable path names both of its own, alongside everything else.
        let error = Config::load(&environment, &Overrides::default(), Demands::Serve)
            .expect_err("it refuses");
        assert!(error.names("VALKEY_URL"), "{error}");
        assert!(error.names("ATTESTATION_KEY_SEED"), "{error}");
    }

    #[test]
    fn serving_without_valkey_and_without_the_memory_profile_is_refused_by_name() {
        // `store/mod.rs` has documented this refusal since `S-C29` and nothing enforced it.
        let error = Config::load(&serveable(), &Overrides::default(), Demands::Serve)
            .expect_err("it refuses");
        assert!(error.names("VALKEY_URL"), "{error}");
    }

    #[test]
    fn the_memory_profile_is_also_reachable_from_the_environment() {
        let mut environment = serveable();
        environment.insert("CAPSULE_PROFILE".to_owned(), "Memory".to_owned());
        let config =
            Config::load(&environment, &Overrides::default(), Demands::Serve).expect("it loads");
        assert_eq!(config.backends, Backends::Memory);
    }

    #[test]
    fn maintenance_needs_a_blob_root_and_no_key_material() {
        // A maintenance host that had to hold the production token-signing key to sweep a
        // directory would be a reason to put the key on a maintenance host.
        let config = Config::load(
            &env(&[("BLOB_ROOT", "/blobs")]),
            &Overrides::default(),
            Demands::Maintenance,
        )
        .expect("it loads");
        assert!(config.signing_key_der.is_none());

        let error = Config::load(
            &env(&[("JWT_ED25519_DER", EXAMPLE_DER)]),
            &Overrides::default(),
            Demands::Maintenance,
        )
        .expect_err("it refuses");
        assert!(error.names("BLOB_ROOT"), "{error}");
    }

    #[test]
    fn describing_the_router_needs_nothing() {
        let config = Config::load(&BTreeMap::new(), &Overrides::default(), Demands::Nothing)
            .expect("it loads");
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
            ("VALKEY_URL", "redis://127.0.0.1:6379"),
        ]);
        let error = Config::load(&environment, &Overrides::default(), Demands::Serve)
            .expect_err("it refuses");
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
    fn a_secret_is_not_printed_by_debug() {
        let config = Config::load(&serveable(), &memory(), Demands::Serve).expect("it loads");
        let rendered = format!("{config:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("MC4CAQAwBQYDK2Vw"), "{rendered}");
    }
}
