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
//! the requirement set is a parameter ([`Demands`]) rather than a property of the type. The one
//! exception is enforced where the key is used rather than here: `purge --apply` signs each
//! purge into the owner ledger, so it uses `ATTESTATION_KEY_SEED` when set and refuses to apply
//! without it, while its dry run, like `gc` and `scrub`, needs nothing.
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

use crate::auth::oidc::{ClientSecret, RedirectPolicy};
use crate::client_addr::{DEFAULT_TRUSTED_PROXY_HEADER, ProxyTrust};
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

/// How long a client may take to send a request head, in seconds — and so, too, how long a
/// kept-alive connection may sit idle waiting for its next one.
///
/// Thirty, Kynos's own default, now stated here rather than inherited: transfer/upload.md's
/// Hostile Clients section makes the bound part of the contract, and a contract whose number
/// lives in a dependency moves when the dependency does. One setting rather than two because
/// the HTTP/1 driver arms one timer for both: it starts the moment the connection waits for a
/// request head, whether that is the connection's first or the one after a kept-alive response
/// ([`crate::limits::http1`]).
const DEFAULT_HEADER_READ_TIMEOUT: u64 = 30;

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

/// Whether `value` is a `YYYY-MM-DD` calendar date, spelled exactly that way.
///
/// `jiff::civil::Date` parses the strict ISO form and refuses `2026-6-1` and February 30th
/// alike; the round trip back to text refuses a value the parser tolerated but the gate's
/// bytewise comparison would misorder.
fn is_protocol_date(value: &str) -> bool {
    value
        .parse::<jiff::civil::Date>()
        .is_ok_and(|date| date.to_string() == value)
}

/// Whether `value` is `MAJOR.MINOR.PATCH` with three non-negative integers.
fn is_semver(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
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
/// Two arms, both assembled by [`assemble`](crate::boot::assemble). It is an enum rather than a
/// trait because the
/// `Arc<dyn Port>` fields in [`Modules`](crate::app::Modules) already **are** the abstraction;
/// a second one over the top would abstract the composition root from itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backends {
    /// Every deterministic in-crate adapter, over a real filesystem blob store.
    ///
    /// An explicit operator act — the `--memory` flag, and nothing in the environment — and
    /// **never** a fallback: a deployment that forgets `VALKEY_URL` must fail closed rather than
    /// come up holding state it will lose on the next restart. Boot logs a `WARN` every time it
    /// assembles this family.
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

/// The OpenID Connect relying party, when a deployment has one.
///
/// Present only when `OIDC_ISSUER` is set. A half-configured relying party — an issuer with no
/// client id, or the reverse — is a configuration fault rather than a feature that is quietly
/// off, because an operator who set one of the two meant to set both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfig {
    /// The issuer, exactly as the provider's ID tokens will carry it.
    pub issuer: String,
    /// This deployment's `client_id` at the provider.
    pub client_id: String,
    /// The client secret, when the deployment is a confidential client. Absent means PKCE-only.
    pub client_secret: Option<ClientSecret>,
    /// Which redirect URIs a client may name.
    pub redirects: RedirectPolicy,
    /// A PEM bundle of additional trust anchors for reaching the provider, if it sits behind a
    /// private CA. Read at boot, not here: a path is configuration, its contents are not.
    pub ca_bundle: Option<PathBuf>,
}

/// Everything an operator gets to decide.
///
/// `Debug` is written by hand rather than derived: `Config` is logged at startup, and every
/// secret it holds — the key material, and the credentials a connection URL can carry — is
/// formatted redacted. See `redacted_url`.
#[derive(Clone)]
pub struct Config {
    /// Where the process accepts connections.
    pub listen: SocketAddr,
    /// This deployment's canonical origin — the `server_id` every published record carries.
    pub server_domain: String,
    /// The absolute base URL clients reach the versioned API at.
    pub api_base_url: String,
    /// Where federated peers reach this server (`FEDERATION_URL`).
    ///
    /// Read as given, and published only when [`Self::federation_enabled`] is also set: see
    /// [`Self::federation_endpoint`], which is the one question the rest of the server asks.
    pub federation_url: Option<String>,
    /// Whether this deployment federates at all (`FEDERATION_ENABLED`, #509). Off by default.
    ///
    /// Federation is deferred (#476), and its surface does work for unauthenticated callers, so
    /// a deployment has to ask for it. Turning it on demands `FEDERATION_URL` on `serve`; a URL
    /// without the switch is a deployment that does not federate, and says so at startup.
    pub federation_enabled: bool,
    /// The filesystem tree ciphertext blobs are written to. There is no object store.
    pub blob_root: Option<PathBuf>,
    /// The Postgres URL, once an adapter reads it (#402).
    pub database_url: Option<String>,
    /// The Valkey URL the state ports' adapters connect to (`store::valkey`, #403).
    pub valkey_url: Option<String>,
    /// The PKCS#8 Ed25519 private key access and refresh tokens are signed with.
    pub signing_key_der: Option<SecretBytes>,
    /// The HMAC key sync cursors are authenticated under.
    pub sync_cursor_mac_key: Option<[u8; CURSOR_KEY_LEN]>,
    /// The seed the attestation signing key is built from.
    pub attestation_key_seed: Option<[u8; ATTESTATION_SEED_LEN]>,
    /// The oldest `protocol_version` accepted for writes (`YYYY-MM-DD`, validated).
    pub protocol_min: String,
    /// The newest `protocol_version` this server speaks (`YYYY-MM-DD`, validated).
    pub protocol_max: String,
    /// The advisory semver client-build cutoff advertised on every response.
    pub min_client_build: String,
    /// How long a blob sits at zero references before the collector may sweep it.
    pub grace_window: SignedDuration,
    /// How long an account stays locked after too many failed credential presentations.
    pub lockout_window: SignedDuration,
    /// How many consecutive failures inside that window lock it.
    pub lockout_attempts: u32,
    /// The OIDC relying party, if `OIDC_ISSUER` is set.
    pub oidc: Option<OidcConfig>,
    /// Whether password registration and sign-in are refused (`PASSWORD_AUTH_DISABLED`, #1004).
    /// Off by default.
    ///
    /// An OIDC-only deployment: `register`, `login` and `reauthenticate` answer
    /// `error.auth.password_disabled` before any password is hashed or verified, and
    /// `server-info` says so. `serve` refuses it without `OIDC_ISSUER`, because a server with
    /// neither door admits nobody.
    pub password_auth_disabled: bool,
    /// How long a shutdown may take to drain.
    pub shutdown_timeout: std::time::Duration,
    /// The accepted-connection ceiling.
    pub max_connections: NonZeroUsize,
    /// How long a client may take to send a request head, which also bounds how long a
    /// kept-alive connection may idle before its next one (`HEADER_READ_TIMEOUT_SECONDS`).
    pub header_read_timeout: std::time::Duration,
    /// When an upload chunk's body is cut: no bytes for the stall bound
    /// (`CHUNK_STALL_TIMEOUT_SECONDS`), or a sustained rate below the floor
    /// (`CHUNK_MIN_RATE_BYTES_PER_SECOND`). Never for its total duration.
    ///
    /// The body-read half of the per-connection bound, applied by
    /// [`ChunkBody`](crate::upload::body::ChunkBody) and therefore over before finalization
    /// starts: see [`crate::upload::policy::DEFAULT_CHUNK_STALL_TIMEOUT`].
    pub chunk_pace: crate::upload::policy::ChunkPace,
    /// Which reverse-proxy hops may name the client, and through which forwarding field
    /// (`TRUSTED_PROXY_HOPS`, `TRUSTED_PROXIES`, `TRUSTED_PROXY_HEADER`).
    ///
    /// Trusts nobody unless set, so every forwarding header is ignored and the socket peer is the
    /// client. A deployment behind a proxy must set it, or every per-source limiter counts the
    /// proxy — every client at once — against one bucket. See [`crate::client_addr`].
    pub trusted_proxies: ProxyTrust,
    /// How the log stream is rendered.
    pub log_format: LogFormat,
    /// Which family of adapters to run on.
    pub backends: Backends,
}

/// How a held secret is formatted: whether one is set, never what it is.
fn redacted<T>(secret: Option<&T>) -> &'static str {
    if secret.is_some() {
        "Some(<redacted>)"
    } else {
        "None"
    }
}

/// `url` with every place a credential can ride redacted: the userinfo, and the whole query.
///
/// The query goes too because the Postgres and Redis URL grammars both accept a password there
/// (`?password=…`), and telling a credential parameter from a harmless one is a list that would
/// have to be kept current for every driver. What survives — scheme, host, port, path — is what
/// an operator reading the startup dump needs to see which service the process reached.
///
/// Userinfo is everything before the **last** `@` in the whole value, not just in the
/// authority: the value is logged before any driver parses it, and a malformed password holding
/// a raw `/`, `?` or `#` would otherwise end the authority early and leak. An `@` in a path or
/// query over-redacts the host instead, which costs a log reader far less than a credential.
fn redacted_url(url: &str) -> String {
    if is_conninfo(url) {
        return redacted_conninfo(url);
    }
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url.trim_start_matches('/')),
    };
    let (userinfo, rest) = match rest.rfind('@') {
        Some(at) => ("<redacted>@", &rest[at + 1..]),
        None => ("", rest),
    };
    let (rest, fragment) = match rest.split_once('#') {
        Some((rest, _)) => (rest, "#<redacted>"),
        None => (rest, ""),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, _)) => (rest, "?<redacted>"),
        None => (rest, ""),
    };
    match scheme {
        Some(scheme) => format!("{scheme}://{userinfo}{rest}{query}{fragment}"),
        None => format!("{userinfo}{rest}{query}{fragment}"),
    }
}

/// The libpq keywords whose values [`redacted_conninfo`] keeps: where the service is, and which
/// database. Every other keyword's value is redacted, so a credential-bearing keyword nobody
/// listed (`password`, `sslpassword`, a future one) fails closed.
const CONNINFO_KEPT_KEYWORDS: [&str; 4] = ["host", "hostaddr", "port", "dbname"];

/// Whether `value` is a libpq keyword/value connection string (`host=db password=…`) rather
/// than a URL: no `://`, and it opens with a keyword followed by `=`.
fn is_conninfo(value: &str) -> bool {
    if value.contains("://") {
        return false;
    }
    let trimmed = value.trim_start();
    let keyword_end = trimmed
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(trimmed.len());
    keyword_end > 0 && trimmed[keyword_end..].trim_start().starts_with('=')
}

/// `conninfo` with the value of every keyword outside [`CONNINFO_KEPT_KEYWORDS`] redacted.
///
/// Parsed with libpq's grammar: `keyword = value` pairs separated by whitespace, a value either
/// single-quoted (with `\'` and `\\` escapes) or an unquoted run of non-whitespace. A string that
/// does not parse is redacted whole, because a parse that went wrong is exactly the case where a
/// password could be mistaken for something harmless.
fn redacted_conninfo(conninfo: &str) -> String {
    const WHOLLY_REDACTED: &str = "<redacted>";
    let bytes = conninfo.as_bytes();
    let mut index = 0;
    let mut pairs = Vec::new();
    let skip_whitespace = |index: &mut usize| {
        while *index < bytes.len() && bytes[*index].is_ascii_whitespace() {
            *index += 1;
        }
    };
    loop {
        skip_whitespace(&mut index);
        if index == bytes.len() {
            break;
        }
        let keyword_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
        {
            index += 1;
        }
        let keyword = &conninfo[keyword_start..index];
        skip_whitespace(&mut index);
        if keyword.is_empty() || bytes.get(index) != Some(&b'=') {
            return WHOLLY_REDACTED.to_owned();
        }
        index += 1;
        skip_whitespace(&mut index);
        let value_start = index;
        if bytes.get(index) == Some(&b'\'') {
            index += 1;
            loop {
                match bytes.get(index) {
                    None => return WHOLLY_REDACTED.to_owned(),
                    Some(b'\\') => index += 2,
                    Some(b'\'') => {
                        index += 1;
                        break;
                    }
                    Some(_) => index += 1,
                }
            }
        } else {
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += if bytes[index] == b'\\' { 2 } else { 1 };
            }
        }
        let index_end = index.min(bytes.len());
        index = index_end;
        let value = if CONNINFO_KEPT_KEYWORDS.contains(&keyword) {
            &conninfo[value_start..index_end]
        } else {
            "<redacted>"
        };
        pairs.push(format!("{keyword}={value}"));
    }
    pairs.join(" ")
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Destructured, so a field added later is a compile error here until someone decides
        // whether it is a secret.
        let Self {
            listen,
            server_domain,
            api_base_url,
            federation_url,
            federation_enabled,
            blob_root,
            database_url,
            valkey_url,
            signing_key_der,
            sync_cursor_mac_key,
            attestation_key_seed,
            protocol_min,
            protocol_max,
            min_client_build,
            grace_window,
            lockout_window,
            lockout_attempts,
            oidc,
            password_auth_disabled,
            shutdown_timeout,
            max_connections,
            header_read_timeout,
            chunk_pace,
            trusted_proxies,
            log_format,
            backends,
        } = self;
        f.debug_struct("Config")
            .field("listen", listen)
            .field("server_domain", server_domain)
            .field("api_base_url", api_base_url)
            .field("federation_url", federation_url)
            .field("federation_enabled", federation_enabled)
            .field("blob_root", blob_root)
            .field("database_url", &database_url.as_deref().map(redacted_url))
            .field("valkey_url", &valkey_url.as_deref().map(redacted_url))
            .field(
                "signing_key_der",
                &format_args!("{}", redacted(signing_key_der.as_ref())),
            )
            .field(
                "sync_cursor_mac_key",
                &format_args!("{}", redacted(sync_cursor_mac_key.as_ref())),
            )
            .field(
                "attestation_key_seed",
                &format_args!("{}", redacted(attestation_key_seed.as_ref())),
            )
            .field("protocol_min", protocol_min)
            .field("protocol_max", protocol_max)
            .field("min_client_build", min_client_build)
            .field("grace_window", grace_window)
            .field("lockout_window", lockout_window)
            .field("lockout_attempts", lockout_attempts)
            .field("oidc", oidc)
            .field("password_auth_disabled", password_auth_disabled)
            .field("shutdown_timeout", shutdown_timeout)
            .field("max_connections", max_connections)
            .field("header_read_timeout", header_read_timeout)
            .field("chunk_pace", chunk_pace)
            .field("trusted_proxies", trusted_proxies)
            .field("log_format", log_format)
            .field("backends", backends)
            .finish()
    }
}

impl Config {
    /// Where peers reach this server, when — and only when — it federates.
    ///
    /// `FEDERATION_ENABLED` **and** `FEDERATION_URL` (#509). `None` is a deployment that does
    /// not federate: `server-info` publishes no `federation_url`, the federation operations
    /// answer `404`, and no capability is honoured on the read primitives.
    pub fn federation_endpoint(&self) -> Option<&str> {
        self.federation_url
            .as_deref()
            .filter(|_| self.federation_enabled)
    }

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
        // Opt-in, and the value is the URL peers pull from: the federation surface is the
        // versioned API itself (design/federation.md, "no new data protocol"), so a deployment
        // that federates publishes its API base here.
        let federation_url = env.var("FEDERATION_URL");
        // Off unless asked for (#509): federation is deferred (#476), and its surface answers
        // unauthenticated callers, so a deployment that never decided must not carry it.
        let federation_enabled = parse_flag(
            env.var("FEDERATION_ENABLED").as_deref(),
            "FEDERATION_ENABLED",
            &mut faults,
        );

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
        // Only the flag selects the in-memory family. The environment is where a deployment's
        // settings live, so a variable that could select it would let a forgotten line in a
        // deployment's environment bring a server up holding state it loses on restart (#521).
        if env.var("CAPSULE_PROFILE").is_some() {
            tracing::warn!(
                "CAPSULE_PROFILE is no longer read; pass --memory to select the in-memory \
                 adapters"
            );
        }
        let backends = if overrides.memory {
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

        // ── The OIDC relying party ──────────────────────────────────────────────────────
        let oidc = read_oidc(env, &mut faults);
        // Off unless asked for (#1004). Read beside the relying party because it only makes
        // sense with one: `serve` refuses it without `OIDC_ISSUER` below.
        let password_auth_disabled = parse_flag(
            env.var("PASSWORD_AUTH_DISABLED").as_deref(),
            "PASSWORD_AUTH_DISABLED",
            &mut faults,
        );

        // ── Protocol window ─────────────────────────────────────────────────────────────
        //
        // Both ends default to the policy's year window rather than to the single day
        // `capsule-core` speaks: a default that collapsed the window to one date refused every
        // client one build behind on its first write, which nobody chose. Both are parsed as
        // dates, because every reader downstream — the gate's lexicographic comparison, the
        // response header, the discovery record — assumes the `YYYY-MM-DD` grammar, and
        // `2026-6-1` sorts before `2026-12-31` for the wrong reason. `min == max` is a
        // legitimate explicit choice and is not refused.
        let protocol_max = env
            .var("PROTOCOL_MAX")
            .unwrap_or_else(|| crate::upload::policy::DEFAULT_PROTOCOL_MAX.to_owned());
        let protocol_min = env
            .var("PROTOCOL_MIN")
            .unwrap_or_else(|| crate::upload::policy::DEFAULT_PROTOCOL_MIN.to_owned());
        for (key, value) in [
            ("PROTOCOL_MIN", &protocol_min),
            ("PROTOCOL_MAX", &protocol_max),
        ] {
            if !is_protocol_date(value) {
                faults.push(ConfigFault::Invalid {
                    key,
                    detail: "is not a YYYY-MM-DD date".to_owned(),
                });
            }
        }
        if protocol_min > protocol_max {
            faults.push(ConfigFault::Invalid {
                key: "PROTOCOL_MIN",
                detail: format!("`{protocol_min}` is newer than PROTOCOL_MAX `{protocol_max}`"),
            });
        }

        // The advisory client-build cutoff, `X-Capsule-Min-Client-Build` on every response.
        // Validated as three dot-separated integers because it is sent as a header value and
        // compared as semver by clients; `0.0.0` — the default — is "no cutoff announced".
        let min_client_build = env
            .var("MIN_CLIENT_BUILD")
            .unwrap_or_else(|| crate::upload::policy::DEFAULT_MIN_CLIENT_BUILD.to_owned());
        if !is_semver(&min_client_build) {
            faults.push(ConfigFault::Invalid {
                key: "MIN_CLIENT_BUILD",
                detail: "is not a MAJOR.MINOR.PATCH semver build".to_owned(),
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
        // The per-connection bounds (transfer/upload.md, Hostile Clients; #717). Zero is refused
        // for both: a zero header timeout closes every connection before its first request, and
        // a zero stall bound refuses every chunk, so either is an outage spelled as a setting. A
        // zero minimum rate is accepted: it turns the rate floor off and leaves the stall bound.
        let header_read_timeout = parse_timeout(
            env,
            "HEADER_READ_TIMEOUT_SECONDS",
            std::time::Duration::from_secs(DEFAULT_HEADER_READ_TIMEOUT),
            &mut faults,
        );
        // A chunk is cut on stall, never for its total duration (#1041), so the total-duration
        // setting it replaces means nothing any more; warned about rather than refused, as the
        // other retired names are, so an existing environment still boots.
        if env.var("CHUNK_READ_TIMEOUT_SECONDS").is_some() {
            tracing::warn!(
                "CHUNK_READ_TIMEOUT_SECONDS is no longer read: a chunk is cut only when it stalls \
                 (CHUNK_STALL_TIMEOUT_SECONDS) or falls below CHUNK_MIN_RATE_BYTES_PER_SECOND"
            );
        }
        let chunk_pace = crate::upload::policy::ChunkPace::new(
            parse_timeout(
                env,
                "CHUNK_STALL_TIMEOUT_SECONDS",
                crate::upload::policy::DEFAULT_CHUNK_STALL_TIMEOUT,
                &mut faults,
            ),
            parse_number::<u64>(env, "CHUNK_MIN_RATE_BYTES_PER_SECOND", &mut faults)
                .unwrap_or(crate::upload::policy::DEFAULT_CHUNK_MIN_RATE),
        );
        // Which proxy hops may name the client (#704). Unset trusts nobody, the one default a
        // spoofed `X-Forwarded-For` cannot move; a malformed entry is a fault rather than an entry
        // quietly dropped, because a dropped proxy is every client sharing that proxy's bucket.
        let trusted_proxies = ProxyTrust {
            hops: parse_number::<usize>(env, "TRUSTED_PROXY_HOPS", &mut faults).unwrap_or(0),
            networks: env
                .var("TRUSTED_PROXIES")
                .and_then(|text| {
                    ProxyTrust::parse_networks(&text)
                        .map_err(|detail| {
                            faults.push(ConfigFault::Invalid {
                                key: "TRUSTED_PROXIES",
                                detail,
                            });
                        })
                        .ok()
                })
                .unwrap_or_default(),
            // A malformed field is a fault rather than the default: a deployment that named
            // `Forwarded` and silently read `X-Forwarded-For` would believe a field its proxy
            // passes through from the client.
            header: env
                .var("TRUSTED_PROXY_HEADER")
                .and_then(|text| {
                    ProxyTrust::parse_header(&text)
                        .map_err(|detail| {
                            faults.push(ConfigFault::Invalid {
                                key: "TRUSTED_PROXY_HEADER",
                                detail,
                            });
                        })
                        .ok()
                })
                .unwrap_or(DEFAULT_TRUSTED_PROXY_HEADER),
        };
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
        //
        // The durable backends are demanded here for every subcommand that opens a store, both
        // URLs and in the order the stores are opened: `DATABASE_URL`, then `VALKEY_URL`. Left to
        // the boot path, a missing database surfaced as a runtime error (exit 1) one restart
        // after a missing Valkey surfaced as a configuration refusal (exit 2), and an operator
        // missing both was told about one at a time.
        let durable = backends == Backends::Durable;
        let require_durable_backends = |faults: &mut Vec<ConfigFault>| {
            if durable {
                require(database_url.is_some(), "DATABASE_URL", faults);
                // The refusal `store/mod.rs` has always documented: Valkey is required, and
                // the in-memory adapters are a development profile an operator opts into
                // rather than something to fall back on.
                require(valkey_url.is_some(), "VALKEY_URL", faults);
            }
        };
        match demands {
            Demands::Nothing => {}
            Demands::Maintenance => {
                require(blob_root.is_some(), "BLOB_ROOT", &mut faults);
                require_durable_backends(&mut faults);
            }
            Demands::Serve => {
                require(blob_root.is_some(), "BLOB_ROOT", &mut faults);
                require(signing_key_der.is_some(), "JWT_ED25519_DER", &mut faults);
                // An operator who turned federation on meant peers to reach it somewhere, and a
                // federating server with no published endpoint is one no peer can find.
                if federation_enabled {
                    require(federation_url.is_some(), "FEDERATION_URL", &mut faults);
                }
                // A server that refuses passwords and has no identity provider admits nobody,
                // not even the operator who meant to restrict it. Keyed on the issuer itself
                // rather than on `oidc`, so a half-configured relying party — which has already
                // faulted above for what it lacks — is not also told its issuer is missing.
                if password_auth_disabled {
                    require(env.var("OIDC_ISSUER").is_some(), "OIDC_ISSUER", &mut faults);
                }
                require_durable_backends(&mut faults);
                if durable {
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
                federation_url,
                federation_enabled,
                blob_root,
                database_url,
                valkey_url,
                signing_key_der,
                sync_cursor_mac_key,
                attestation_key_seed,
                protocol_min,
                protocol_max,
                min_client_build,
                grace_window,
                lockout_window,
                lockout_attempts,
                oidc,
                password_auth_disabled,
                shutdown_timeout,
                max_connections,
                header_read_timeout,
                chunk_pace,
                trusted_proxies,
                log_format,
                backends,
            })
        } else {
            Err(ConfigError { faults })
        }
    }
}

/// Read the `OIDC_*` settings, or `None` when none of them is set.
///
/// `OIDC_ISSUER` must be an absolute `http(s)` URL with no query or fragment (OpenID Connect
/// Discovery 1.0 §3), and `https` unless it is a loopback address — the development carve-out
/// `auth::oidc::discovery` applies to the provider's endpoints too. `OIDC_REDIRECT_URL` is held
/// to the same scheme rule. `OIDC_ALLOW_LOOPBACK_REDIRECT` defaults to **off**: admitting a
/// redirect to any loopback port (RFC 8252 §7.3) is what a CLI's or desktop client's listener
/// needs, and it is the one knob that widens where the server will send a person back to, so a
/// deployment turns it on when it has such a client (#461's CLI flow will) rather than getting
/// it unasked.
fn read_oidc(env: &dyn Environment, faults: &mut Vec<ConfigFault>) -> Option<OidcConfig> {
    let issuer = env.var("OIDC_ISSUER");
    let client_id = env.var("OIDC_CLIENT_ID");
    let client_secret = env.var("OIDC_CLIENT_SECRET");
    let redirect_url = env.var("OIDC_REDIRECT_URL");
    let allow_loopback = env.var("OIDC_ALLOW_LOOPBACK_REDIRECT");
    let ca_bundle = env.var("OIDC_CA_BUNDLE").map(PathBuf::from);

    if issuer.is_none()
        && client_id.is_none()
        && client_secret.is_none()
        && redirect_url.is_none()
        && allow_loopback.is_none()
        && ca_bundle.is_none()
    {
        return None;
    }

    // Half a relying party is refused, both ways round.
    require(issuer.is_some(), "OIDC_ISSUER", faults);
    require(client_id.is_some(), "OIDC_CLIENT_ID", faults);

    if let Some(issuer) = &issuer {
        match reqwest::Url::parse(issuer) {
            Ok(url) if url.query().is_some() || url.fragment().is_some() => {
                faults.push(ConfigFault::Invalid {
                    key: "OIDC_ISSUER",
                    detail: "an issuer carries no query or fragment".to_owned(),
                });
            }
            Ok(url) if url.scheme() == "https" => {}
            Ok(url)
                if url.scheme() == "http"
                    && crate::auth::oidc::discovery::is_loopback_issuer(issuer) => {}
            Ok(url) => {
                faults.push(ConfigFault::Invalid {
                    key: "OIDC_ISSUER",
                    detail: format!(
                        "`{}://` is not accepted; an issuer is https, or http on a loopback \
                         address for development",
                        url.scheme()
                    ),
                });
            }
            Err(error) => {
                faults.push(ConfigFault::Invalid {
                    key: "OIDC_ISSUER",
                    // The issuer is a public URL, not a secret; quoting it is what a typo needs.
                    detail: format!("`{issuer}` is not an absolute URL ({error})"),
                });
            }
        }
    }

    if let Some(redirect) = &redirect_url {
        match reqwest::Url::parse(redirect) {
            Ok(url) if url.scheme() == "https" => {}
            Ok(url)
                if url.scheme() == "http"
                    && crate::auth::oidc::discovery::is_loopback_issuer(redirect) => {}
            Ok(url) => {
                faults.push(ConfigFault::Invalid {
                    key: "OIDC_REDIRECT_URL",
                    detail: format!(
                        "`{}://` is not accepted; a redirect is https, or http on a loopback \
                         address for development",
                        url.scheme()
                    ),
                });
            }
            Err(error) => {
                faults.push(ConfigFault::Invalid {
                    key: "OIDC_REDIRECT_URL",
                    detail: format!("`{redirect}` is not an absolute URL ({error})"),
                });
            }
        }
    }

    let allow_loopback = parse_flag(
        allow_loopback.as_deref(),
        "OIDC_ALLOW_LOOPBACK_REDIRECT",
        faults,
    );

    Some(OidcConfig {
        issuer: issuer?,
        client_id: client_id?,
        client_secret: client_secret.map(ClientSecret::new),
        redirects: RedirectPolicy::new(redirect_url, allow_loopback),
        ca_bundle,
    })
}

/// Record a missing required setting.
fn require(present: bool, key: &'static str, faults: &mut Vec<ConfigFault>) {
    if !present {
        faults.push(ConfigFault::Missing { key });
    }
}

/// Read an on/off setting, absent meaning off, recording a fault for anything else.
///
/// The spellings an operator reaches for either way are all accepted, case-insensitively,
/// because a switch that silently read `True` or `yes` as off would be worse than one that
/// refused it.
fn parse_flag(raw: Option<&str>, key: &'static str, faults: &mut Vec<ConfigFault>) -> bool {
    match raw.map(str::trim) {
        None => false,
        Some(raw)
            if ["true", "1", "yes", "on"]
                .iter()
                .any(|v| raw.eq_ignore_ascii_case(v)) =>
        {
            true
        }
        Some(raw)
            if ["false", "0", "no", "off"]
                .iter()
                .any(|v| raw.eq_ignore_ascii_case(v)) =>
        {
            false
        }
        Some(raw) => {
            faults.push(ConfigFault::Invalid {
                key,
                detail: format!("`{raw}` is neither `true` nor `false`"),
            });
            false
        }
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

/// Read `key` as a whole number of seconds that must not be zero, or `default` when unset.
fn parse_timeout(
    env: &dyn Environment,
    key: &'static str,
    default: std::time::Duration,
    faults: &mut Vec<ConfigFault>,
) -> std::time::Duration {
    match parse_number::<u64>(env, key, faults) {
        None => default,
        Some(0) => {
            faults.push(ConfigFault::Invalid {
                key,
                detail: "zero would refuse every request it bounds".to_owned(),
            });
            default
        }
        Some(seconds) => std::time::Duration::from_secs(seconds),
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
mod tests;
