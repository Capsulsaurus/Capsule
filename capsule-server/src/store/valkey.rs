//! The Valkey adapters — the required backend behind every state port (slice `S-C29`'s owed
//! half, #403).
//!
//! # Shape
//!
//! One struct per port, mirroring [`super::memory`], sharing one [`Valkey`] handle: a single
//! `redis::aio::ConnectionManager`, which is a multiplexed, self-reconnecting connection. There
//! is no pool. A server talking to one Valkey needs exactly one connection that survives a
//! restart of the other side, and that is what the manager is.
//!
//! # Every multi-key mutation is one Lua script
//!
//! A multiplexed connection cannot hold `WATCH`: the optimistic transaction needs an exclusive
//! connection across the whole read-then-`MULTI` loop, which is the read-then-write window the
//! ports exist to close (`claim_finalize`, the counter's `hit`, `consume`, `redeem`). So every
//! operation that touches more than one key, or decides and writes, is a script — `EVALSHA`
//! with an automatic `SCRIPT LOAD` on `NOSCRIPT`, which is what `redis::Script` does — and the
//! server runs it atomically. The scripts are the whole of the CAS story here; each is a `const`
//! beside the method that invokes it.
//!
//! # Expiry: the injected clock is the contract, `PEXPIRE` is the collector
//!
//! Every record hash carries an adapter-internal `expires_at` written from the injected
//! [`Clock`] at the moment the record is opened, and every script that reads a record checks
//! it before answering: a record past it is reported absent. Valkey's own `PEXPIRE` is set on
//! the same key with the same lifetime and is what actually removes it.
//!
//! The check is **non-destructive**, deliberately. A replica whose clock runs ahead would
//! otherwise delete, for every other replica, state that is still live by the store's own
//! lifetime. For the direct-read scripts (`READ_RECORD`, `READ_UPLOAD`, `CHUNK_AT`,
//! `IS_LIVE`, `LOOKUP_CHANNEL` and the mutations guarded by `live`) a false "not live" verdict
//! costs that replica one early miss and nobody else anything. The index-listing scripts
//! (`SESSIONS_FOR_USER`, `UPLOADS_FOR_UPLOADER`, `PENDING_FOR_ADDRESS`, `IN_FLIGHT_FOR_ALBUM`,
//! `LEAST_RECENTLY_PROGRESSED`) are the residual: they `SREM`/`ZREM` a member whose record they
//! judge dead, and the index is **shared**, so a fast clock on one replica hides a still-live
//! record from every replica's listings until a state change re-indexes it or the record
//! expires for real. The record itself is untouched — a direct read on a well-clocked replica
//! still finds it — and the exposure is bounded by the clock skew, which is why this is the
//! accepted cost of not requiring synchronised clocks (the #403 decision record, decision 12)
//! rather than a reason to let a reader delete. So one fact, one collector, and a read gate
//! that only ever answers. The port's `Clock` seam is what makes expiry *testable*:
//! the [`conformance`](super::conformance) suite advances a manual clock to one nanosecond
//! either side of a boundary and expects a different answer on each side, which no harness can
//! arrange against a real clock by sleeping. Gating on the injected clock lets the same suite
//! drive this adapter exactly as it drives the double, with no sleeps and no margins, and in
//! production the gate and the collector read the same wall clock.
//!
//! # Indexes are derived, and heal on read
//!
//! The per-user session set, the per-uploader set, the per-album set, the pending-address set
//! and the global progress sorted-set are all derived from the record hashes. Every listing
//! resolves each member through its record inside the script and drops — `SREM`/`ZREM` — any
//! member whose record is gone or no longer matches. That is what makes
//! `an_expired_session_leaves_no_listing_entry_behind` hold without a second lifetime on the
//! index: the two independent TTLs the Salvo adapter kept were exactly what leaked. A heal is
//! logged at `warn`, because in production it is the signal that TTL and index have drifted.
//!
//! # Keys
//!
//! `capsule:` throughout, keeping the retired server's namespace:
//!
//! | key | type | lifetime |
//! |---|---|---|
//! | `capsule:session:{sid}` | hash | session TTL |
//! | `capsule:user_sessions:{uid}` | set | refreshed to the session TTL on every open |
//! | `capsule:upload:session:{id}` | hash | lifetime cap |
//! | `capsule:upload:chunks:{id}` | hash (`offset` → chunk) | the record's remaining TTL |
//! | `capsule:upload:uploader:{uid}` | set | refreshed on open |
//! | `capsule:upload:album:{album}` | set | refreshed on open |
//! | `capsule:upload:pending:{owner}:{hash}` | set | refreshed on open |
//! | `capsule:upload:progress` | zset (score = `last_progress_at` µs) | none — heals only when the pressure sweep (`least_recently_progressed`) runs |
//! | `capsule:challenge:{token}` | hash | challenge TTL |
//! | `capsule:enroll:code:{spelling}` | hash, written under both spellings | code TTL |
//! | `capsule:enroll:channel:{id}` | hash | channel TTL |
//! | `capsule:enroll:mbox:{id}:{a\|b}` | list | the channel's remaining TTL |
//! | `capsule:cohorts:{uid}` | hash (`cohort_hash` → `first last`) | **none** |
//!
//! Scripts derive a few of these keys from a record they have just read (`SREM` the previous
//! user's index on close, for instance) rather than taking them as `KEYS`. That is legal on a
//! standalone server, which is the only topology this adapter supports; Redis Cluster is out of
//! scope by the #403 decision record.
//!
//! # Encoding
//!
//! One hash field per record field; timestamps as RFC 3339 (`jiff` round-trips them
//! nanosecond-exact); enums by their existing `as_str()`; `expires_at` and sorted-set scores as
//! integer microseconds, because a Lua number is a double and microseconds stay exact in one
//! until the year 2255 while nanoseconds do not. That is a floor: a record expires at the
//! microsecond its nanosecond deadline falls in, and two progress instants inside one
//! microsecond order by upload id (the exact `<` against the record's own timestamp is applied
//! in Rust where a horizon is compared). `PEXPIRE` takes whole milliseconds, rounded **up**, so
//! the collector never removes a record before its logical lifetime has passed. A field that
//! will not parse is [`StoreError::Corrupt`], which is the variant that exists for exactly this.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{
    ErrorKind, FromRedisValue, RedisError, Script, ScriptInvocation, ServerErrorKind, Value,
};
use uuid::Uuid;

use super::auth::{AuthStateStore, CohortRecord, CohortStore, DEFAULT_SESSION_TTL, SessionRecord};
use super::ceremony::{
    CHALLENGE_TTL, ChallengeStore, ChannelStore, Direction, DrainOutcome, ENROLLMENT_CODE_TTL,
    EnrollmentStore, PendingEnrollment, RELAY_CHANNEL_TTL, RelayChannel, RelayOutcome,
    RelayPayload, RevokeAllChallenge,
};
use super::ids::{
    AlbumId, AssetId, ChallengeToken, ChannelId, EnrollmentCode, OwnerId, SessionId, UploadId,
    UserId,
};
use super::upload::{
    AcceptedChunk, BlobRole, FinalizeClaim, LIFETIME_CAP, UploadSessionRecord, UploadSessionStatus,
    UploadSessionStore,
};
use super::{Clock, StoreError, StoreFuture, deadline};

// ===========================================================================================
// The connection
// ===========================================================================================

/// How long one command may take before the adapter reports the store unavailable.
///
/// A hung Valkey must surface as [`StoreError::Unavailable`] — which every consumer treats as a
/// refusal — rather than as a request that never answers.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long one connection attempt may take.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times a lost connection is retried, with exponential backoff, before a command
/// fails. Also the number of attempts the initial connection gets, so a server started a
/// moment before its Valkey (compose ordering) does not refuse for that alone.
const RECONNECT_ATTEMPTS: usize = 3;

/// One Valkey server, reached over one multiplexed connection.
///
/// `Clone` is a refcount bump: every adapter holds a clone of the same manager.
#[derive(Clone)]
pub struct Valkey {
    manager: ConnectionManager,
}

impl fmt::Debug for Valkey {
    /// Never the URL: a `VALKEY_URL` carries the password.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Valkey(<connected>)")
    }
}

impl Valkey {
    /// Connect to `url` (`redis://` or `rediss://`, TLS in rustls) and prove it answers `PING`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`] if the URL is not one this adapter can open or the server
    /// does not answer within the retry budget. The detail never quotes the URL.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        const STORE: &str = "valkey";
        let client = redis::Client::open(url).map_err(|_| StoreError::Unavailable {
            store: STORE,
            detail: "VALKEY_URL is not a redis:// or rediss:// URL this adapter can open"
                .to_owned(),
        })?;
        let config = ConnectionManagerConfig::new()
            .set_number_of_retries(RECONNECT_ATTEMPTS)
            .set_min_delay(Duration::from_millis(200))
            .set_max_delay(Duration::from_secs(2))
            .set_connection_timeout(Some(CONNECTION_TIMEOUT))
            .set_response_timeout(Some(RESPONSE_TIMEOUT));
        // Nothing was sent yet, whatever the driver says went wrong, so this is the one place
        // every failure is `Unavailable`.
        let manager = ConnectionManager::new_with_config(client, config)
            .await
            .map_err(|error| StoreError::Unavailable {
                store: STORE,
                detail: error.to_string(),
            })?;
        let valkey = Self { manager };
        let pong: String = valkey.command(STORE, redis::cmd("PING")).await?;
        if pong != "PONG" {
            return Err(StoreError::Rejected {
                store: STORE,
                detail: format!("PING was answered with {pong:?}"),
            });
        }
        tracing::info!("connected to Valkey");
        Ok(valkey)
    }

    /// Run one plain command.
    pub(crate) async fn command<T: FromRedisValue>(
        &self,
        store: &'static str,
        cmd: redis::Cmd,
    ) -> Result<T, StoreError> {
        let mut connection = self.manager.clone();
        cmd.query_async(&mut connection)
            .await
            .map_err(|error| classify(store, "command", error))
    }

    /// Run one script, `EVALSHA` first and `SCRIPT LOAD` on a `NOSCRIPT`.
    async fn eval<T: FromRedisValue>(
        &self,
        store: &'static str,
        script: &Lua,
        keys: &[String],
        args: &[String],
    ) -> Result<T, StoreError> {
        let mut invocation = script.script().prepare_invoke();
        for key in keys {
            invocation.key(key.as_str());
        }
        for arg in args {
            invocation.arg(arg.as_str());
        }
        self.invoke(store, &invocation).await
    }

    /// Run one prepared script invocation, `EVALSHA` first and `SCRIPT LOAD` on a `NOSCRIPT`.
    pub(crate) async fn invoke<T: FromRedisValue>(
        &self,
        store: &'static str,
        invocation: &ScriptInvocation<'_>,
    ) -> Result<T, StoreError> {
        let mut connection = self.manager.clone();
        invocation
            .invoke_async(&mut connection)
            .await
            .map_err(|error| classify(store, "script", error))
    }
}

/// Sort a driver error into the port's three failure modes.
///
/// The line that matters is between *the operation certainly did not happen* and *the server
/// was reached and refused, or may have run it*: a caller that gets [`StoreError::Unavailable`]
/// may retry, one that gets [`StoreError::Rejected`] must not assume anything about state. So
/// only a failure the driver can place **before** the command was sent — a connection it could
/// not open, a server that answered `LOADING`/`TRYAGAIN`/`MASTERDOWN`/`CLUSTERDOWN` without
/// executing — is `Unavailable`. A response timeout or a connection dropped mid-flight means
/// the script may already have burned the challenge or won the claim, and that is `Rejected`.
///
/// A reply of the wrong shape is [`StoreError::Corrupt`]. Its detail carries the driver's
/// *kind* and never its text: redis-rs quotes the offending value in a type error, and for the
/// ceremony stores that value is the record, which carries the bearer secret the typed ids
/// redact from every other log line.
fn classify(store: &'static str, what: &'static str, error: RedisError) -> StoreError {
    // `NOSCRIPT` belongs here too: the server declined to run a script it does not hold, and
    // `redis::Script` normally answers it with a `SCRIPT LOAD` and a retry. Reaching this point
    // means that retry failed as well, and nothing was executed either time.
    let never_sent = error.is_connection_refusal()
        || matches!(
            error.kind(),
            ErrorKind::Server(
                ServerErrorKind::BusyLoading
                    | ServerErrorKind::TryAgain
                    | ServerErrorKind::MasterDown
                    | ServerErrorKind::ClusterDown
                    | ServerErrorKind::NoScript
            ) | ErrorKind::ClusterConnectionNotFound
        );
    if never_sent {
        tracing::warn!(store, what, %error, "the Valkey store is unavailable");
        return StoreError::Unavailable {
            store,
            detail: error.to_string(),
        };
    }
    if matches!(
        error.kind(),
        ErrorKind::Parse | ErrorKind::UnexpectedReturnType
    ) {
        let kind = format!("{:?}", error.kind());
        tracing::error!(
            store,
            what,
            kind,
            "the Valkey store answered a shape it should not"
        );
        return StoreError::Corrupt {
            store,
            record: what,
            detail: format!("the reply could not be decoded ({kind})"),
        };
    }
    if error.is_io_error() || error.is_timeout() || error.is_connection_dropped() {
        tracing::warn!(store, what, %error, "the Valkey connection failed mid-operation");
    } else {
        tracing::error!(store, what, %error, "the Valkey store rejected an operation");
    }
    StoreError::Rejected {
        store,
        detail: error.to_string(),
    }
}

/// Log a listing's self-heal. A member whose record is simply gone is the routine consequence of
/// an index outliving its expired members and is `debug`; one whose record is present but names
/// a different owner is drift between record and index, which is the `warn` the module doc
/// promises.
fn healed(store: &'static str, scope: &str, gone: u64, mismatched: u64) {
    if gone > 0 {
        tracing::debug!(store, scope, gone, "expired index entries were reclaimed");
    }
    if mismatched > 0 {
        tracing::warn!(
            store,
            scope,
            mismatched,
            "stale index entries were reclaimed"
        );
    }
}

// ===========================================================================================
// Encoding
// ===========================================================================================

/// Microseconds since the epoch, the unit every Lua comparison and sorted-set score uses.
pub(crate) fn micros(at: Timestamp) -> i64 {
    at.as_microsecond()
}

/// A microsecond count back to a [`Timestamp`], for a value a script computed.
pub(crate) fn from_micros(
    store: &'static str,
    record: &'static str,
    value: i64,
) -> Result<Timestamp, StoreError> {
    Timestamp::from_microsecond(value).map_err(|error| StoreError::Corrupt {
        store,
        record,
        detail: format!("microsecond timestamp {value} is out of range: {error}"),
    })
}

/// The flat `field value field value …` list `HSET` takes and `HGETALL` returns.
#[derive(Debug, Default)]
struct Encoder(Vec<String>);

impl Encoder {
    fn field(&mut self, name: &str, value: impl fmt::Display) -> &mut Self {
        self.0.push(name.to_owned());
        self.0.push(value.to_string());
        self
    }

    fn optional(&mut self, name: &str, value: Option<impl fmt::Display>) -> &mut Self {
        if let Some(value) = value {
            self.field(name, value);
        }
        self
    }

    fn finish(self) -> Vec<String> {
        self.0
    }
}

/// One record's hash, as read back, with typed accessors that fail as [`StoreError::Corrupt`].
struct Fields {
    store: &'static str,
    record: &'static str,
    map: BTreeMap<String, String>,
}

impl Fields {
    fn from_flat(
        store: &'static str,
        record: &'static str,
        flat: Vec<String>,
    ) -> Result<Self, StoreError> {
        if !flat.len().is_multiple_of(2) {
            return Err(StoreError::Corrupt {
                store,
                record,
                detail: format!("a hash came back with {} entries", flat.len()),
            });
        }
        let mut map = BTreeMap::new();
        let mut items = flat.into_iter();
        while let (Some(name), Some(value)) = (items.next(), items.next()) {
            map.insert(name, value);
        }
        Ok(Self { store, record, map })
    }

    fn corrupt(&self, detail: String) -> StoreError {
        tracing::error!(
            store = self.store,
            record = self.record,
            %detail,
            "a stored record could not be decoded"
        );
        StoreError::Corrupt {
            store: self.store,
            record: self.record,
            detail,
        }
    }

    fn required(&self, name: &str) -> Result<&str, StoreError> {
        self.map
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| self.corrupt(format!("field `{name}` is missing")))
    }

    fn optional(&self, name: &str) -> Option<String> {
        self.map.get(name).cloned()
    }

    fn timestamp(&self, name: &str) -> Result<Timestamp, StoreError> {
        let text = self.required(name)?;
        text.parse()
            .map_err(|error| self.corrupt(format!("field `{name}` is not a timestamp: {error}")))
    }

    fn number<T: FromStr>(&self, name: &str) -> Result<T, StoreError>
    where
        T::Err: fmt::Display,
    {
        let text = self.required(name)?;
        text.parse()
            .map_err(|error| self.corrupt(format!("field `{name}` is not a number: {error}")))
    }
}

fn encode_session(record: &SessionRecord, expires_at: Timestamp) -> Vec<String> {
    let mut encoder = Encoder::default();
    encoder
        .field("session_id", &record.session_id)
        .field("user_id", &record.user_id)
        .field("created_at", record.created_at)
        .field("authenticated_at", record.authenticated_at)
        .field("last_active_at", record.last_active_at)
        .optional("user_agent", record.user_agent.as_deref())
        .optional("ip_address", record.ip_address.as_deref())
        .optional("cohort_hash", record.cohort_hash.as_deref())
        .optional("device_id", record.device_id)
        .field("expires_at", micros(expires_at));
    encoder.finish()
}

fn decode_session(flat: Vec<String>) -> Result<SessionRecord, StoreError> {
    let fields = Fields::from_flat(AUTH, "SessionRecord", flat)?;
    let device_id = match fields.optional("device_id") {
        Some(text) => Some(
            Uuid::parse_str(&text)
                .map_err(|error| fields.corrupt(format!("device_id is not a uuid: {error}")))?,
        ),
        None => None,
    };
    Ok(SessionRecord {
        session_id: SessionId::new(fields.required("session_id")?),
        user_id: UserId::new(fields.required("user_id")?),
        created_at: fields.timestamp("created_at")?,
        authenticated_at: fields.timestamp("authenticated_at")?,
        last_active_at: fields.timestamp("last_active_at")?,
        user_agent: fields.optional("user_agent"),
        ip_address: fields.optional("ip_address"),
        cohort_hash: fields.optional("cohort_hash"),
        device_id,
    })
}

fn parse_role(text: &str) -> Option<BlobRole> {
    [
        BlobRole::Original,
        BlobRole::Derivative,
        BlobRole::Metadata,
        BlobRole::Provenance,
        BlobRole::Backup,
    ]
    .into_iter()
    .find(|role| role.as_str() == text)
}

fn parse_status(text: &str) -> Option<UploadSessionStatus> {
    [
        UploadSessionStatus::Pending,
        UploadSessionStatus::Uploading,
        UploadSessionStatus::WaitingForProcessing,
        UploadSessionStatus::Completed,
        UploadSessionStatus::FailedProcessing,
    ]
    .into_iter()
    .find(|status| status.as_str() == text)
}

fn encode_upload(record: &UploadSessionRecord, expires_at: Timestamp) -> Vec<String> {
    let mut encoder = Encoder::default();
    encoder
        .field("upload_id", &record.upload_id)
        .field("asset_id", &record.asset_id)
        .field("owner_id", &record.owner_id)
        .field("upload_user_id", &record.upload_user_id)
        .optional("album_id", record.album_id.as_ref())
        .optional("content_type", record.content_type.as_deref())
        .field("expected_hash", &record.expected_hash)
        .field("crypto_suite_id", record.crypto_suite_id)
        .field("protocol_version", &record.protocol_version)
        .field("blob_role", record.blob_role.as_str())
        .optional("intent_id", record.intent_id.as_deref())
        .field("manifest_envelope", &record.manifest_envelope)
        .field("received_bytes", record.received_bytes)
        .field("total_size", record.total_size)
        .field("status", record.status.as_str())
        .field("created_at", record.created_at)
        .field("last_progress_at", record.last_progress_at)
        .field("progress_score", micros(record.last_progress_at))
        .field("expires_at", micros(expires_at));
    encoder.finish()
}

fn decode_upload(flat: Vec<String>) -> Result<UploadSessionRecord, StoreError> {
    let fields = Fields::from_flat(UPLOADS, "UploadSessionRecord", flat)?;
    let blob_role = parse_role(fields.required("blob_role")?)
        .ok_or_else(|| fields.corrupt("blob_role is not a known role".to_owned()))?;
    let status = parse_status(fields.required("status")?)
        .ok_or_else(|| fields.corrupt("status is not a known status".to_owned()))?;
    Ok(UploadSessionRecord {
        upload_id: UploadId::new(fields.required("upload_id")?),
        asset_id: AssetId::new(fields.required("asset_id")?),
        owner_id: OwnerId::new(fields.required("owner_id")?),
        upload_user_id: UserId::new(fields.required("upload_user_id")?),
        album_id: fields.optional("album_id").map(AlbumId::new),
        content_type: fields.optional("content_type"),
        expected_hash: fields.required("expected_hash")?.to_owned(),
        crypto_suite_id: fields.number("crypto_suite_id")?,
        protocol_version: fields.required("protocol_version")?.to_owned(),
        blob_role,
        intent_id: fields.optional("intent_id"),
        manifest_envelope: fields.required("manifest_envelope")?.to_owned(),
        received_bytes: fields.number("received_bytes")?,
        total_size: fields.number("total_size")?,
        status,
        created_at: fields.timestamp("created_at")?,
        last_progress_at: fields.timestamp("last_progress_at")?,
    })
}

/// `chunk_hash \t next_offset \t accepted_at`: the hash is hex and a timestamp has no tab.
fn encode_chunk(chunk: &AcceptedChunk) -> String {
    format!(
        "{}\t{}\t{}",
        chunk.chunk_hash, chunk.next_offset, chunk.accepted_at
    )
}

fn decode_chunk(offset: u64, packed: &str) -> Result<AcceptedChunk, StoreError> {
    let corrupt = |detail: String| StoreError::Corrupt {
        store: UPLOADS,
        record: "AcceptedChunk",
        detail,
    };
    let mut parts = packed.split('\t');
    let chunk_hash = parts
        .next()
        .ok_or_else(|| corrupt("no chunk hash".to_owned()))?
        .to_owned();
    let next_offset = parts
        .next()
        .ok_or_else(|| corrupt("no next offset".to_owned()))?
        .parse()
        .map_err(|error| corrupt(format!("next offset is not a number: {error}")))?;
    let accepted_at = parts
        .next()
        .ok_or_else(|| corrupt("no accepted_at".to_owned()))?
        .parse()
        .map_err(|error| corrupt(format!("accepted_at is not a timestamp: {error}")))?;
    Ok(AcceptedChunk {
        offset,
        chunk_hash,
        next_offset,
        accepted_at,
    })
}

// ===========================================================================================
// Keys
// ===========================================================================================

/// Port names, for the log line and the error.
const AUTH: &str = "auth";
const UPLOADS: &str = "uploads";
const CHALLENGES: &str = "challenges";
const ENROLLMENTS: &str = "enrollments";
const CHANNELS: &str = "channels";
const COHORTS: &str = "cohorts";

/// The global eviction view.
const PROGRESS_KEY: &str = "capsule:upload:progress";

fn session_key(session: &SessionId) -> String {
    format!("capsule:session:{session}")
}

fn user_sessions_key(user: &UserId) -> String {
    format!("capsule:user_sessions:{user}")
}

fn upload_key(upload: &UploadId) -> String {
    format!("capsule:upload:session:{upload}")
}

fn chunks_key(upload: &UploadId) -> String {
    format!("capsule:upload:chunks:{upload}")
}

fn uploader_key(user: &UserId) -> String {
    format!("capsule:upload:uploader:{user}")
}

fn album_key(album: &AlbumId) -> String {
    format!("capsule:upload:album:{album}")
}

/// The one key with two variable parts, which is why the port's ids must contain no `:` — a
/// precondition the scripts that rebuild this key from a record (`SET_STATUS`, `DISCARD_UPLOAD`,
/// `OPEN_UPLOAD`) cannot check for themselves. Owner ids are UUIDs and the hash is hex, so the
/// assertion is a guard against a future id space, not a live case.
fn pending_key(owner: &OwnerId, expected_hash: &str) -> String {
    debug_assert!(
        !owner.as_str().contains(':') && !expected_hash.contains(':'),
        "an owner id or a content hash must contain no `:`; it is a key segment"
    );
    format!("capsule:upload:pending:{owner}:{expected_hash}")
}

fn challenge_key(token: &ChallengeToken) -> String {
    format!("capsule:challenge:{}", token.as_str())
}

fn enrollment_key(code: &EnrollmentCode) -> String {
    format!("capsule:enroll:code:{}", code.as_str())
}

fn channel_key(channel: &ChannelId) -> String {
    format!("capsule:enroll:channel:{channel}")
}

fn mailbox_key(channel: &ChannelId, direction: Direction) -> String {
    format!("capsule:enroll:mbox:{channel}:{}", direction.as_str())
}

fn cohorts_key(user: &UserId) -> String {
    format!("capsule:cohorts:{user}")
}

// ===========================================================================================
// Scripts
// ===========================================================================================

/// One Lua script: its source, and the `redis::Script` (SHA-1 for `EVALSHA`) built on first use.
///
/// The source is kept beside the script so a unit test can assert that every key a script
/// derives from a record it read spells the same prefix the Rust key functions write.
pub(crate) struct Lua {
    source: &'static str,
    script: OnceLock<Script>,
}

impl Lua {
    pub(crate) const fn new(source: &'static str) -> Self {
        Self {
            source,
            script: OnceLock::new(),
        }
    }

    pub(crate) fn script(&self) -> &Script {
        self.script.get_or_init(|| Script::new(self.source))
    }
}

/// Declare a script with the shared helpers prepended.
///
/// `live(key, now, ...)` answers whether the record hash at `key` exists and has not passed its
/// `expires_at` (microseconds) as of `now`. It never deletes: the injected clock decides what a
/// reader is told, `PEXPIRE` decides what is removed (see the module doc). The trailing
/// arguments name the record's dependent keys and are accepted so a call site reads as a
/// statement of what expires together; the collector's TTLs are what act on them.
/// `expired(flat, now)` asks the same of a record already read back with `HGETALL`.
/// `extend(key, ttl)` raises a derived key's lifetime to `ttl` and never lowers it — an index
/// set is shared by every member, so one member's remaining life must not shorten another's.
macro_rules! script {
    ($name:ident, $body:literal) => {
        static $name: Lua = Lua::new(concat!(
            "local function live(key, now, ...)\n",
            "  local expires = redis.call('HGET', key, 'expires_at')\n",
            "  if not expires then return false end\n",
            "  return tonumber(expires) > tonumber(now)\n",
            "end\n",
            "local function expired(flat, now)\n",
            "  for i = 1, #flat, 2 do\n",
            "    if flat[i] == 'expires_at' then return tonumber(flat[i + 1]) <= tonumber(now) end\n",
            "  end\n",
            "  return true\n",
            "end\n",
            "local function extend(key, ttl)\n",
            "  if ttl > 0 and redis.call('PTTL', key) < ttl then redis.call('PEXPIRE', key, ttl) end\n",
            "end\n",
            $body
        ));
    };
}

// ---- sessions -----------------------------------------------------------------------------

// KEYS: record, user index. ARGV: ttl_ms, session_id, user_id, fields…
script!(
    OPEN_SESSION,
    "local previous = redis.call('HGET', KEYS[1], 'user_id')
redis.call('DEL', KEYS[1])
redis.call('HSET', KEYS[1], unpack(ARGV, 4))
redis.call('PEXPIRE', KEYS[1], ARGV[1])
redis.call('SADD', KEYS[2], ARGV[2])
redis.call('PEXPIRE', KEYS[2], ARGV[1])
if previous and previous ~= ARGV[3] then
  redis.call('SREM', 'capsule:user_sessions:' .. previous, ARGV[2])
end
return 1"
);

// KEYS: record. ARGV: now.
script!(
    READ_RECORD,
    "if not live(KEYS[1], ARGV[1]) then return nil end
return redis.call('HGETALL', KEYS[1])"
);

// KEYS: record. ARGV: now, field, value. `HSET` never touches a hash's TTL, which is what makes
// the absolute-lifetime contract hold.
script!(
    SET_SESSION_FIELD,
    "if not live(KEYS[1], ARGV[1]) then return nil end
redis.call('HSET', KEYS[1], ARGV[2], ARGV[3])
return redis.call('HGETALL', KEYS[1])"
);

// KEYS: record. ARGV: now, session_id. Both halves go together or neither does.
script!(
    CLOSE_SESSION,
    "local record = redis.call('HGETALL', KEYS[1])
if #record == 0 then return nil end
local user
for i = 1, #record, 2 do if record[i] == 'user_id' then user = record[i + 1] end end
redis.call('DEL', KEYS[1])
if user then redis.call('SREM', 'capsule:user_sessions:' .. user, ARGV[2]) end
if expired(record, ARGV[1]) then return nil end
return record"
);

// KEYS: user index. ARGV: now, user_id, close ('1' removes what it lists). Returns
// {gone, mismatched, {record…}}.
script!(
    SESSIONS_FOR_USER,
    "local gone = 0
local mismatched = 0
local found = {}
for _, sid in ipairs(redis.call('SMEMBERS', KEYS[1])) do
  local key = 'capsule:session:' .. sid
  if not live(key, ARGV[1]) then
    redis.call('SREM', KEYS[1], sid)
    gone = gone + 1
  elseif redis.call('HGET', key, 'user_id') ~= ARGV[2] then
    redis.call('SREM', KEYS[1], sid)
    mismatched = mismatched + 1
  else
    found[#found + 1] = redis.call('HGETALL', key)
    if ARGV[3] == '1' then redis.call('DEL', key) end
  end
end
if ARGV[3] == '1' then redis.call('DEL', KEYS[1]) end
return {gone, mismatched, found}"
);

// ---- uploads ------------------------------------------------------------------------------

// KEYS: record, chunks, uploader index, pending set, progress zset, [album set].
// ARGV: ttl_ms, upload_id, in_flight, evictable, score, fields… Re-opening an id under a
// different uploader, owner, hash or album unindexes the previous record first, as
// `OPEN_SESSION` does for a previous user.
script!(
    OPEN_UPLOAD,
    "local previous = redis.call('HMGET', KEYS[1], 'upload_user_id', 'owner_id', 'expected_hash', 'album_id')
if previous[1] then
  local uploader = 'capsule:upload:uploader:' .. previous[1]
  if uploader ~= KEYS[3] then redis.call('SREM', uploader, ARGV[2]) end
  local pending = 'capsule:upload:pending:' .. previous[2] .. ':' .. previous[3]
  if pending ~= KEYS[4] then redis.call('SREM', pending, ARGV[2]) end
  if previous[4] and ('capsule:upload:album:' .. previous[4]) ~= KEYS[6] then
    redis.call('SREM', 'capsule:upload:album:' .. previous[4], ARGV[2])
  end
end
redis.call('DEL', KEYS[1], KEYS[2])
redis.call('HSET', KEYS[1], unpack(ARGV, 6))
redis.call('PEXPIRE', KEYS[1], ARGV[1])
redis.call('SADD', KEYS[3], ARGV[2])
redis.call('PEXPIRE', KEYS[3], ARGV[1])
if ARGV[3] == '1' then
  redis.call('SADD', KEYS[4], ARGV[2])
  redis.call('PEXPIRE', KEYS[4], ARGV[1])
  if KEYS[6] then
    redis.call('SADD', KEYS[6], ARGV[2])
    redis.call('PEXPIRE', KEYS[6], ARGV[1])
  end
else
  redis.call('SREM', KEYS[4], ARGV[2])
  if KEYS[6] then redis.call('SREM', KEYS[6], ARGV[2]) end
end
if ARGV[4] == '1' then
  redis.call('ZADD', KEYS[5], ARGV[5], ARGV[2])
else
  redis.call('ZREM', KEYS[5], ARGV[2])
end
return 1"
);

// KEYS: record, chunks. ARGV: now.
script!(
    READ_UPLOAD,
    "if not live(KEYS[1], ARGV[1], KEYS[2]) then return nil end
return redis.call('HGETALL', KEYS[1])"
);

// KEYS: uploader index. ARGV: now, uploader. Returns {gone, mismatched, {record…}}.
script!(
    UPLOADS_FOR_UPLOADER,
    "local gone = 0
local mismatched = 0
local found = {}
for _, id in ipairs(redis.call('SMEMBERS', KEYS[1])) do
  local key = 'capsule:upload:session:' .. id
  if not live(key, ARGV[1], 'capsule:upload:chunks:' .. id) then
    redis.call('SREM', KEYS[1], id)
    gone = gone + 1
  elseif redis.call('HGET', key, 'upload_user_id') ~= ARGV[2] then
    redis.call('SREM', KEYS[1], id)
    mismatched = mismatched + 1
  else
    found[#found + 1] = redis.call('HGETALL', key)
  end
end
return {gone, mismatched, found}"
);

// KEYS: record, chunks, progress zset. ARGV: now, offset, packed chunk, next_offset,
// accepted_at, score, upload_id. The counter, the clock and the replay entry move together.
script!(
    RECORD_PROGRESS,
    "if not live(KEYS[1], ARGV[1], KEYS[2]) then return nil end
local status = redis.call('HGET', KEYS[1], 'status')
if status == 'completed' or status == 'failed_processing' then return nil end
redis.call('HSET', KEYS[2], ARGV[2], ARGV[3])
local ttl = redis.call('PTTL', KEYS[1])
if ttl > 0 then redis.call('PEXPIRE', KEYS[2], ttl) end
redis.call('HSET', KEYS[1], 'received_bytes', ARGV[4], 'last_progress_at', ARGV[5], 'progress_score', ARGV[6])
if status == 'pending' then
  redis.call('HSET', KEYS[1], 'status', 'uploading')
  status = 'uploading'
end
if status == 'uploading' then redis.call('ZADD', KEYS[3], ARGV[6], ARGV[7]) end
return redis.call('HGETALL', KEYS[1])"
);

// KEYS: record, chunks. ARGV: now, offset.
script!(
    CHUNK_AT,
    "if not live(KEYS[1], ARGV[1], KEYS[2]) then return nil end
return redis.call('HGET', KEYS[2], ARGV[2])"
);

// KEYS: record, chunks. ARGV: now, on_disk. The one write that does not touch the clock.
script!(
    RECONCILE_BYTES,
    "if not live(KEYS[1], ARGV[1], KEYS[2]) then return nil end
redis.call('HSET', KEYS[1], 'received_bytes', ARGV[2])
return redis.call('HGETALL', KEYS[1])"
);

// KEYS: record, chunks, progress zset. ARGV: now, status, upload_id, in_flight, evictable.
script!(
    SET_STATUS,
    "if not live(KEYS[1], ARGV[1], KEYS[2]) then return nil end
redis.call('HSET', KEYS[1], 'status', ARGV[2])
local owner = redis.call('HGET', KEYS[1], 'owner_id')
local hash = redis.call('HGET', KEYS[1], 'expected_hash')
local album = redis.call('HGET', KEYS[1], 'album_id')
local ttl = redis.call('PTTL', KEYS[1])
local pending = 'capsule:upload:pending:' .. owner .. ':' .. hash
if ARGV[4] == '1' then
  redis.call('SADD', pending, ARGV[3])
  extend(pending, ttl)
  if album then
    redis.call('SADD', 'capsule:upload:album:' .. album, ARGV[3])
    extend('capsule:upload:album:' .. album, ttl)
  end
else
  redis.call('SREM', pending, ARGV[3])
  if album then redis.call('SREM', 'capsule:upload:album:' .. album, ARGV[3]) end
end
if ARGV[5] == '1' then
  redis.call('ZADD', KEYS[3], redis.call('HGET', KEYS[1], 'progress_score'), ARGV[3])
else
  redis.call('ZREM', KEYS[3], ARGV[3])
end
return redis.call('HGETALL', KEYS[1])"
);

// KEYS: record, chunks, progress zset. ARGV: now, upload_id. Returns 0 (not found), 1 (already
// claimed) or the record (won) — the compare and the set are one script, so two racing
// finalizers cannot both see `pending`.
script!(
    CLAIM_FINALIZE,
    "if not live(KEYS[1], ARGV[1], KEYS[2]) then return 0 end
local status = redis.call('HGET', KEYS[1], 'status')
if status ~= 'pending' and status ~= 'uploading' then return 1 end
redis.call('HSET', KEYS[1], 'status', 'waiting_for_processing')
redis.call('ZREM', KEYS[3], ARGV[2])
return redis.call('HGETALL', KEYS[1])"
);

// KEYS: record, chunks, progress zset. ARGV: now, upload_id. Record, replay entries and every
// view entry, together.
script!(
    DISCARD_UPLOAD,
    "local record = redis.call('HGETALL', KEYS[1])
redis.call('DEL', KEYS[1], KEYS[2])
redis.call('ZREM', KEYS[3], ARGV[2])
if #record == 0 then return nil end
local f = {}
for i = 1, #record, 2 do f[record[i]] = record[i + 1] end
if f.upload_user_id then redis.call('SREM', 'capsule:upload:uploader:' .. f.upload_user_id, ARGV[2]) end
if f.owner_id and f.expected_hash then
  redis.call('SREM', 'capsule:upload:pending:' .. f.owner_id .. ':' .. f.expected_hash, ARGV[2])
end
if f.album_id then redis.call('SREM', 'capsule:upload:album:' .. f.album_id, ARGV[2]) end
if expired(record, ARGV[1]) then return nil end
return record"
);

// KEYS: pending set. ARGV: now, owner, expected_hash. Returns {gone, mismatched, {upload_id…}}.
// A terminal session counts as gone: its promise ended, and the set is derived.
script!(
    PENDING_FOR_ADDRESS,
    "local gone = 0
local mismatched = 0
local found = {}
for _, id in ipairs(redis.call('SMEMBERS', KEYS[1])) do
  local key = 'capsule:upload:session:' .. id
  if not live(key, ARGV[1], 'capsule:upload:chunks:' .. id) then
    redis.call('SREM', KEYS[1], id)
    gone = gone + 1
  else
    local r = redis.call('HMGET', key, 'owner_id', 'expected_hash', 'status')
    if r[1] ~= ARGV[2] or r[2] ~= ARGV[3] then
      redis.call('SREM', KEYS[1], id)
      mismatched = mismatched + 1
    elseif r[3] == 'completed' or r[3] == 'failed_processing' then
      redis.call('SREM', KEYS[1], id)
      gone = gone + 1
    else
      found[#found + 1] = id
    end
  end
end
return {gone, mismatched, found}"
);

// KEYS: album set. ARGV: now, album_id. Returns {gone, mismatched, count}.
script!(
    IN_FLIGHT_FOR_ALBUM,
    "local gone = 0
local mismatched = 0
local count = 0
for _, id in ipairs(redis.call('SMEMBERS', KEYS[1])) do
  local key = 'capsule:upload:session:' .. id
  if not live(key, ARGV[1], 'capsule:upload:chunks:' .. id) then
    redis.call('SREM', KEYS[1], id)
    gone = gone + 1
  else
    local r = redis.call('HMGET', key, 'album_id', 'status')
    if r[1] ~= ARGV[2] then
      redis.call('SREM', KEYS[1], id)
      mismatched = mismatched + 1
    elseif r[2] == 'completed' or r[2] == 'failed_processing' then
      redis.call('SREM', KEYS[1], id)
      gone = gone + 1
    else
      count = count + 1
    end
  end
end
return {gone, mismatched, count}"
);

// KEYS: progress zset. ARGV: now, inclusive score bound, limit. Returns
// {gone, {{upload_id, last_progress_at}…}}, least recently progressed first; a member whose
// record is gone or no longer evictable is dropped from the view here.
script!(
    LEAST_RECENTLY_PROGRESSED,
    "local gone = 0
local found = {}
local offset = 0
local limit = tonumber(ARGV[3])
while #found < limit do
  local page = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[2], 'LIMIT', offset, 32)
  if #page == 0 then break end
  local kept = 0
  for _, id in ipairs(page) do
    local key = 'capsule:upload:session:' .. id
    local keep = false
    local r
    if live(key, ARGV[1], 'capsule:upload:chunks:' .. id) then
      r = redis.call('HMGET', key, 'status', 'last_progress_at')
      keep = r[1] == 'pending' or r[1] == 'uploading'
    end
    if keep then
      kept = kept + 1
      found[#found + 1] = {id, r[2]}
    else
      redis.call('ZREM', KEYS[1], id)
      gone = gone + 1
    end
  end
  offset = offset + kept
  if #page < 32 then break end
end
return {gone, found}"
);

// ---- ceremonies ---------------------------------------------------------------------------

// KEYS: every key the record is written under. ARGV: ttl_ms, fields… Written identically
// under each key, as one fact.
script!(
    PUT_RECORD,
    "for _, key in ipairs(KEYS) do
  redis.call('DEL', key)
  redis.call('HSET', key, unpack(ARGV, 2))
  redis.call('PEXPIRE', key, ARGV[1])
end
return #KEYS"
);

// KEYS: record. ARGV: now. The read *is* the removal: burned on every attempt, live or not.
script!(
    CONSUME_RECORD,
    "local record = redis.call('HGETALL', KEYS[1])
redis.call('DEL', KEYS[1])
if #record == 0 or expired(record, ARGV[1]) then return nil end
return record"
);

// KEYS: record. ARGV: now.
script!(
    IS_LIVE,
    "if live(KEYS[1], ARGV[1]) then return 1 end
return 0"
);

// KEYS: the presented spelling. ARGV: now. Whichever spelling was presented, both die.
script!(
    REDEEM_ENROLLMENT,
    "local record = redis.call('HGETALL', KEYS[1])
if #record == 0 then return nil end
local f = {}
for i = 1, #record, 2 do f[record[i]] = record[i + 1] end
redis.call('DEL', KEYS[1])
if f.code then redis.call('DEL', 'capsule:enroll:code:' .. f.code) end
if f.text_fallback then redis.call('DEL', 'capsule:enroll:code:' .. f.text_fallback) end
if expired(record, ARGV[1]) then return nil end
return record"
);

// KEYS: channel, mailbox a, mailbox b. ARGV: ttl_ms, fields… Re-opening an id must not
// resurrect an earlier ceremony's undelivered payloads.
script!(
    OPEN_CHANNEL,
    "redis.call('DEL', KEYS[1], KEYS[2], KEYS[3])
redis.call('HSET', KEYS[1], unpack(ARGV, 2))
redis.call('PEXPIRE', KEYS[1], ARGV[1])
return 1"
);

// KEYS: channel, mailbox a, mailbox b. ARGV: now.
script!(
    LOOKUP_CHANNEL,
    "if not live(KEYS[1], ARGV[1], KEYS[2], KEYS[3]) then return nil end
return redis.call('HGETALL', KEYS[1])"
);

// KEYS: channel, this mailbox, the other mailbox. ARGV: now, payload. Liveness and the append
// are one script, so the channel cannot expire between them.
script!(
    ENQUEUE_PAYLOAD,
    "if not live(KEYS[1], ARGV[1], KEYS[2], KEYS[3]) then return nil end
local depth = redis.call('RPUSH', KEYS[2], ARGV[2])
local ttl = redis.call('PTTL', KEYS[1])
if ttl > 0 then redis.call('PEXPIRE', KEYS[2], ttl) end
return depth"
);

// KEYS: channel, this mailbox, the other mailbox. ARGV: now.
script!(
    DRAIN_MAILBOX,
    "if not live(KEYS[1], ARGV[1], KEYS[2], KEYS[3]) then return nil end
local items = redis.call('LRANGE', KEYS[2], 0, -1)
redis.call('DEL', KEYS[2])
return items"
);

// KEYS: channel, mailbox a, mailbox b. ARGV: now.
script!(
    CLOSE_CHANNEL,
    "local was_live = live(KEYS[1], ARGV[1], KEYS[2], KEYS[3])
redis.call('DEL', KEYS[1], KEYS[2], KEYS[3])
if was_live then return 1 end
return 0"
);

// KEYS: cohort hash. ARGV: cohort_hash, at. Returns {first_seen, last_seen, was_known}.
script!(
    OBSERVE_COHORT,
    "local held = redis.call('HGET', KEYS[1], ARGV[1])
local first = ARGV[2]
if held then first = string.match(held, '^(%S+)') end
redis.call('HSET', KEYS[1], ARGV[1], first .. ' ' .. ARGV[2])
if held then return {first, ARGV[2], 1} end
return {first, ARGV[2], 0}"
);

// ===========================================================================================
// Auth state
// ===========================================================================================

/// Valkey [`AuthStateStore`].
#[derive(Debug)]
pub struct ValkeyAuthState {
    valkey: Valkey,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl ValkeyAuthState {
    /// A store on `valkey` and `clock` with the given session lifetime.
    pub fn new(valkey: Valkey, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self { valkey, clock, ttl }
    }

    /// A store with [`DEFAULT_SESSION_TTL`].
    pub fn with_default_ttl(valkey: Valkey, clock: Arc<dyn Clock>) -> Self {
        Self::new(valkey, clock, DEFAULT_SESSION_TTL)
    }

    fn now(&self) -> String {
        micros(self.clock.now()).to_string()
    }

    async fn set_field(
        &self,
        session: &SessionId,
        field: &str,
        value: Timestamp,
    ) -> Result<Option<SessionRecord>, StoreError> {
        let flat: Option<Vec<String>> = self
            .valkey
            .eval(
                AUTH,
                &SET_SESSION_FIELD,
                &[session_key(session)],
                &[self.now(), field.to_owned(), value.to_string()],
            )
            .await?;
        flat.map(decode_session).transpose()
    }

    async fn list(&self, user: &UserId, close: bool) -> Result<Vec<SessionRecord>, StoreError> {
        let (gone, mismatched, records): (u64, u64, Vec<Vec<String>>) = self
            .valkey
            .eval(
                AUTH,
                &SESSIONS_FOR_USER,
                &[user_sessions_key(user)],
                &[
                    self.now(),
                    user.to_string(),
                    if close { "1" } else { "0" }.to_owned(),
                ],
            )
            .await?;
        healed(AUTH, user.as_str(), gone, mismatched);
        let mut found = records
            .into_iter()
            .map(decode_session)
            .collect::<Result<Vec<_>, _>>()?;
        found.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        Ok(found)
    }
}

impl AuthStateStore for ValkeyAuthState {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn open_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            let session_id = record.session_id.clone();
            let user_id = record.user_id.clone();
            let mut args = vec![
                ttl_millis(self.ttl),
                session_id.to_string(),
                user_id.to_string(),
            ];
            args.extend(encode_session(&record, expires_at));
            let _: i64 = self
                .valkey
                .eval(
                    AUTH,
                    &OPEN_SESSION,
                    &[session_key(&session_id), user_sessions_key(&user_id)],
                    &args,
                )
                .await?;
            tracing::debug!(%session_id, %user_id, "opened session");
            Ok(())
        })
    }

    fn read_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let flat: Option<Vec<String>> = self
                .valkey
                .eval(AUTH, &READ_RECORD, &[session_key(session)], &[self.now()])
                .await?;
            tracing::trace!(%session, hit = flat.is_some(), "read session");
            flat.map(decode_session).transpose()
        })
    }

    fn touch_session<'a>(
        &'a self,
        session: &'a SessionId,
        last_active_at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let touched = self
                .set_field(session, "last_active_at", last_active_at)
                .await?;
            tracing::trace!(%session, hit = touched.is_some(), "touched session");
            Ok(touched)
        })
    }

    fn mark_authenticated<'a>(
        &'a self,
        session: &'a SessionId,
        at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let marked = self.set_field(session, "authenticated_at", at).await?;
            if marked.is_some() {
                tracing::info!(%session, "a session re-authenticated");
            } else {
                tracing::trace!(%session, "re-authentication found no live session");
            }
            Ok(marked)
        })
    }

    fn close_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let flat: Option<Vec<String>> = self
                .valkey
                .eval(
                    AUTH,
                    &CLOSE_SESSION,
                    &[session_key(session)],
                    &[self.now(), session.to_string()],
                )
                .await?;
            let removed = flat.map(decode_session).transpose()?;
            if let Some(record) = &removed {
                tracing::info!(%session, user_id = %record.user_id, "closed session");
            } else {
                tracing::debug!(%session, "close found no live session");
            }
            Ok(removed)
        })
    }

    fn sessions_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        Box::pin(async move {
            let found = self.list(user, false).await?;
            tracing::trace!(%user, count = found.len(), "listed sessions for user");
            Ok(found)
        })
    }

    fn close_all_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        Box::pin(async move {
            let removed = self.list(user, true).await?;
            tracing::info!(%user, revoked = removed.len(), "revoked every session for user");
            Ok(removed)
        })
    }
}

/// A lifetime as the millisecond string `PEXPIRE` takes, never below one.
fn ttl_millis(ttl: SignedDuration) -> String {
    // Rounded up: the collector must never remove a record before its logical lifetime — the
    // microsecond `expires_at` the read gate compares — has passed.
    let nanos = ttl.as_nanos().max(1);
    let millis = (nanos + 999_999) / 1_000_000;
    millis.max(1).to_string()
}

// ===========================================================================================
// Upload sessions
// ===========================================================================================

/// Valkey [`UploadSessionStore`].
#[derive(Debug)]
pub struct ValkeyUploadSessions {
    valkey: Valkey,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl ValkeyUploadSessions {
    /// A store on `valkey` and `clock` with the given lifetime cap.
    pub fn new(valkey: Valkey, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self { valkey, clock, ttl }
    }

    /// A store with the [`LIFETIME_CAP`].
    pub fn with_default_ttl(valkey: Valkey, clock: Arc<dyn Clock>) -> Self {
        Self::new(valkey, clock, LIFETIME_CAP)
    }

    fn now(&self) -> String {
        micros(self.clock.now()).to_string()
    }

    /// The three keys most record scripts take: the record, its replay hash, the eviction view.
    fn record_keys(upload: &UploadId) -> [String; 3] {
        [
            upload_key(upload),
            chunks_key(upload),
            PROGRESS_KEY.to_owned(),
        ]
    }

    async fn record_script(
        &self,
        script: &Lua,
        upload: &UploadId,
        args: &[String],
    ) -> Result<Option<UploadSessionRecord>, StoreError> {
        let flat: Option<Vec<String>> = self
            .valkey
            .eval(UPLOADS, script, &Self::record_keys(upload), args)
            .await?;
        flat.map(decode_upload).transpose()
    }
}

fn flag(value: bool) -> String {
    if value { "1" } else { "0" }.to_owned()
}

impl UploadSessionStore for ValkeyUploadSessions {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn open(&self, record: UploadSessionRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            let upload_id = record.upload_id.clone();
            let uploader = record.upload_user_id.clone();
            let mut keys = vec![
                upload_key(&upload_id),
                chunks_key(&upload_id),
                uploader_key(&uploader),
                pending_key(&record.owner_id, &record.expected_hash),
                PROGRESS_KEY.to_owned(),
            ];
            if let Some(album) = &record.album_id {
                keys.push(album_key(album));
            }
            let mut args = vec![
                ttl_millis(self.ttl),
                upload_id.to_string(),
                flag(record.status.is_active()),
                flag(record.status.is_evictable()),
                micros(record.last_progress_at).to_string(),
            ];
            args.extend(encode_upload(&record, expires_at));
            let _: i64 = self
                .valkey
                .eval(UPLOADS, &OPEN_UPLOAD, &keys, &args)
                .await?;
            tracing::debug!(%upload_id, %uploader, "opened upload session");
            Ok(())
        })
    }

    fn read<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let flat: Option<Vec<String>> = self
                .valkey
                .eval(
                    UPLOADS,
                    &READ_UPLOAD,
                    &[upload_key(upload), chunks_key(upload)],
                    &[self.now()],
                )
                .await?;
            tracing::trace!(%upload, hit = flat.is_some(), "read upload session");
            flat.map(decode_upload).transpose()
        })
    }

    fn sessions_for_uploader<'a>(
        &'a self,
        uploader: &'a UserId,
    ) -> StoreFuture<'a, Vec<UploadSessionRecord>> {
        Box::pin(async move {
            let (gone, mismatched, records): (u64, u64, Vec<Vec<String>>) = self
                .valkey
                .eval(
                    UPLOADS,
                    &UPLOADS_FOR_UPLOADER,
                    &[uploader_key(uploader)],
                    &[self.now(), uploader.to_string()],
                )
                .await?;
            healed(UPLOADS, uploader.as_str(), gone, mismatched);
            let mut found = records
                .into_iter()
                .map(decode_upload)
                .collect::<Result<Vec<_>, _>>()?;
            found.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.upload_id.cmp(&b.upload_id))
            });
            tracing::trace!(%uploader, count = found.len(), "listed upload sessions");
            Ok(found)
        })
    }

    fn record_progress<'a>(
        &'a self,
        upload: &'a UploadId,
        chunk: AcceptedChunk,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let updated = self
                .record_script(
                    &RECORD_PROGRESS,
                    upload,
                    &[
                        self.now(),
                        chunk.offset.to_string(),
                        encode_chunk(&chunk),
                        chunk.next_offset.to_string(),
                        chunk.accepted_at.to_string(),
                        micros(chunk.accepted_at).to_string(),
                        upload.to_string(),
                    ],
                )
                .await?;
            if let Some(record) = &updated {
                tracing::debug!(
                    %upload,
                    received_bytes = record.received_bytes,
                    "accepted chunk"
                );
            } else {
                tracing::debug!(%upload, "progress found no active session");
            }
            Ok(updated)
        })
    }

    fn chunk_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
    ) -> StoreFuture<'a, Option<AcceptedChunk>> {
        Box::pin(async move {
            let packed: Option<String> = self
                .valkey
                .eval(
                    UPLOADS,
                    &CHUNK_AT,
                    &[upload_key(upload), chunks_key(upload)],
                    &[self.now(), offset.to_string()],
                )
                .await?;
            tracing::trace!(%upload, offset, hit = packed.is_some(), "chunk replay lookup");
            packed
                .map(|packed| decode_chunk(offset, &packed))
                .transpose()
        })
    }

    fn reconcile_received_bytes<'a>(
        &'a self,
        upload: &'a UploadId,
        on_disk: u64,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let reconciled = self
                .record_script(&RECONCILE_BYTES, upload, &[self.now(), on_disk.to_string()])
                .await?;
            if reconciled.is_some() {
                tracing::info!(%upload, now = on_disk, "reconciled received bytes to disk");
            }
            Ok(reconciled)
        })
    }

    fn set_status<'a>(
        &'a self,
        upload: &'a UploadId,
        status: UploadSessionStatus,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let updated = self
                .record_script(
                    &SET_STATUS,
                    upload,
                    &[
                        self.now(),
                        status.as_str().to_owned(),
                        upload.to_string(),
                        flag(status.is_active()),
                        flag(status.is_evictable()),
                    ],
                )
                .await?;
            tracing::debug!(
                %upload,
                status = status.as_str(),
                hit = updated.is_some(),
                "set upload status"
            );
            Ok(updated)
        })
    }

    fn claim_finalize<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, FinalizeClaim> {
        Box::pin(async move {
            let outcome: Value = self
                .valkey
                .eval(
                    UPLOADS,
                    &CLAIM_FINALIZE,
                    &Self::record_keys(upload),
                    &[self.now(), upload.to_string()],
                )
                .await?;
            match outcome {
                Value::Int(0) => {
                    tracing::debug!(%upload, "finalize claim found no session");
                    Ok(FinalizeClaim::NotFound)
                }
                Value::Int(1) => {
                    tracing::debug!(%upload, "finalize already claimed");
                    Ok(FinalizeClaim::AlreadyClaimed)
                }
                record => {
                    let flat = Vec::<String>::from_redis_value(record).map_err(|_| {
                        tracing::error!(%upload, "the claim script answered a shape it should not");
                        StoreError::Corrupt {
                            store: UPLOADS,
                            record: "UploadSessionRecord",
                            detail: "the claim script answered neither a verdict nor a record"
                                .to_owned(),
                        }
                    })?;
                    let claimed = decode_upload(flat)?;
                    tracing::info!(%upload, "claimed finalization");
                    Ok(FinalizeClaim::Won(Box::new(claimed)))
                }
            }
        })
    }

    fn discard<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let removed = self
                .record_script(&DISCARD_UPLOAD, upload, &[self.now(), upload.to_string()])
                .await?;
            tracing::info!(%upload, hit = removed.is_some(), "discarded upload session");
            Ok(removed)
        })
    }

    fn pending_for_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        expected_hash: &'a str,
    ) -> StoreFuture<'a, Option<UploadId>> {
        Box::pin(async move {
            let (gone, mismatched, mut ids): (u64, u64, Vec<String>) = self
                .valkey
                .eval(
                    UPLOADS,
                    &PENDING_FOR_ADDRESS,
                    &[pending_key(owner, expected_hash)],
                    &[self.now(), owner.to_string(), expected_hash.to_owned()],
                )
                .await?;
            healed(UPLOADS, owner.as_str(), gone, mismatched);
            // Smallest id first, so two sessions declaring the same bytes give a deterministic
            // answer — the same rule the in-memory double applies.
            ids.sort();
            Ok(ids.into_iter().next().map(UploadId::new))
        })
    }

    fn in_flight_for_album<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, u64> {
        Box::pin(async move {
            let (gone, mismatched, count): (u64, u64, u64) = self
                .valkey
                .eval(
                    UPLOADS,
                    &IN_FLIGHT_FOR_ALBUM,
                    &[album_key(album)],
                    &[self.now(), album.to_string()],
                )
                .await?;
            healed(UPLOADS, album.as_str(), gone, mismatched);
            Ok(count)
        })
    }

    fn least_recently_progressed(
        &self,
        not_progressed_since: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<UploadId>> {
        Box::pin(async move {
            if limit == 0 {
                return Ok(Vec::new());
            }
            // The score is microseconds, the record nanoseconds: fetch up to the truncated
            // horizon inclusive — one page beyond `limit`, so the members sharing the horizon's
            // microsecond do not cost a candidate — and apply the exact `<` against each
            // record's own timestamp.
            let (gone, candidates): (u64, Vec<(String, String)>) = self
                .valkey
                .eval(
                    UPLOADS,
                    &LEAST_RECENTLY_PROGRESSED,
                    &[PROGRESS_KEY.to_owned()],
                    &[
                        self.now(),
                        micros(not_progressed_since).to_string(),
                        limit.saturating_add(32).to_string(),
                    ],
                )
                .await?;
            healed(UPLOADS, PROGRESS_KEY, gone, 0);
            let mut picked = Vec::with_capacity(candidates.len());
            for (id, last_progress_at) in candidates {
                let at: Timestamp = last_progress_at.parse().map_err(|error| {
                    tracing::error!(%id, "an eviction candidate's last_progress_at will not parse");
                    StoreError::Corrupt {
                        store: UPLOADS,
                        record: "UploadSessionRecord",
                        detail: format!("last_progress_at is not a timestamp: {error}"),
                    }
                })?;
                if at < not_progressed_since {
                    picked.push((at, UploadId::new(id)));
                }
            }
            picked.sort();
            let picked: Vec<UploadId> = picked.into_iter().take(limit).map(|(_, id)| id).collect();
            tracing::debug!(count = picked.len(), "listed eviction candidates");
            Ok(picked)
        })
    }
}

// ===========================================================================================
// Device cohorts
// ===========================================================================================

/// Valkey [`CohortStore`]: one hash per account, no expiry.
///
/// The interim home of the durable cohort map until the Postgres adapter (#402) carries it.
/// The key has no TTL, so the data is migratable by one `HGETALL` per account.
#[derive(Debug)]
pub struct ValkeyCohorts {
    valkey: Valkey,
}

impl ValkeyCohorts {
    /// A store on `valkey`.
    pub fn new(valkey: Valkey) -> Self {
        Self { valkey }
    }
}

fn decode_cohort(user: &UserId, hash: &str, packed: &str) -> Result<CohortRecord, StoreError> {
    let corrupt = |detail: String| StoreError::Corrupt {
        store: COHORTS,
        record: "CohortRecord",
        detail,
    };
    let (first, last) = packed
        .split_once(' ')
        .ok_or_else(|| corrupt(format!("cohort `{hash}` holds `{packed}`")))?;
    Ok(CohortRecord {
        user_id: user.clone(),
        cohort_hash: hash.to_owned(),
        first_seen: first
            .parse()
            .map_err(|error| corrupt(format!("first_seen is not a timestamp: {error}")))?,
        last_seen: last
            .parse()
            .map_err(|error| corrupt(format!("last_seen is not a timestamp: {error}")))?,
    })
}

impl CohortStore for ValkeyCohorts {
    fn observe<'a>(
        &'a self,
        user: &'a UserId,
        cohort_hash: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, CohortRecord> {
        Box::pin(async move {
            let (first, last, known): (String, String, u8) = self
                .valkey
                .eval(
                    COHORTS,
                    &OBSERVE_COHORT,
                    &[cohorts_key(user)],
                    &[cohort_hash.to_owned(), at.to_string()],
                )
                .await?;
            if known == 0 {
                tracing::info!(%user, "an account was seen under a new device cohort");
            }
            decode_cohort(user, cohort_hash, &format!("{first} {last}"))
        })
    }

    fn cohorts_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<CohortRecord>> {
        Box::pin(async move {
            let mut cmd = redis::cmd("HGETALL");
            cmd.arg(cohorts_key(user));
            let held: BTreeMap<String, String> = self.valkey.command(COHORTS, cmd).await?;
            let mut found = held
                .iter()
                .map(|(hash, packed)| decode_cohort(user, hash, packed))
                .collect::<Result<Vec<_>, _>>()?;
            found.sort_by(|a, b| {
                a.first_seen
                    .cmp(&b.first_seen)
                    .then_with(|| a.cohort_hash.cmp(&b.cohort_hash))
            });
            Ok(found)
        })
    }
}

// ===========================================================================================
// Ceremonies
// ===========================================================================================

/// Valkey [`ChallengeStore`].
#[derive(Debug)]
pub struct ValkeyChallenges {
    valkey: Valkey,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl ValkeyChallenges {
    /// A store on `valkey` and `clock` with the given challenge lifetime.
    pub fn new(valkey: Valkey, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self { valkey, clock, ttl }
    }

    /// A store with the [`CHALLENGE_TTL`].
    pub fn with_default_ttl(valkey: Valkey, clock: Arc<dyn Clock>) -> Self {
        Self::new(valkey, clock, CHALLENGE_TTL)
    }
}

fn decode_challenge(flat: Vec<String>) -> Result<RevokeAllChallenge, StoreError> {
    let fields = Fields::from_flat(CHALLENGES, "RevokeAllChallenge", flat)?;
    Ok(RevokeAllChallenge {
        user_id: UserId::new(fields.required("user_id")?),
        issued_at: fields.timestamp("issued_at")?,
    })
}

impl ChallengeStore for ValkeyChallenges {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn issue<'a>(
        &'a self,
        token: &'a ChallengeToken,
        record: RevokeAllChallenge,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            let mut encoder = Encoder::default();
            encoder
                .field("user_id", &record.user_id)
                .field("issued_at", record.issued_at)
                .field("expires_at", micros(expires_at));
            let mut args = vec![ttl_millis(self.ttl)];
            args.extend(encoder.finish());
            let _: i64 = self
                .valkey
                .eval(CHALLENGES, &PUT_RECORD, &[challenge_key(token)], &args)
                .await?;
            tracing::info!(user_id = %record.user_id, "issued revoke-all challenge");
            Ok(())
        })
    }

    fn consume<'a>(
        &'a self,
        token: &'a ChallengeToken,
    ) -> StoreFuture<'a, Option<RevokeAllChallenge>> {
        Box::pin(async move {
            let flat: Option<Vec<String>> = self
                .valkey
                .eval(
                    CHALLENGES,
                    &CONSUME_RECORD,
                    &[challenge_key(token)],
                    &[micros(self.clock.now()).to_string()],
                )
                .await?;
            tracing::debug!(hit = flat.is_some(), "consumed revoke-all challenge");
            flat.map(decode_challenge).transpose()
        })
    }
}

/// Valkey [`EnrollmentStore`].
///
/// The record is written under both spellings by one script and burned from both by one script,
/// so one spelling can never outlive the other.
#[derive(Debug)]
pub struct ValkeyEnrollments {
    valkey: Valkey,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl ValkeyEnrollments {
    /// A store on `valkey` and `clock` with the given code lifetime.
    pub fn new(valkey: Valkey, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self { valkey, clock, ttl }
    }

    /// A store with the [`ENROLLMENT_CODE_TTL`].
    pub fn with_default_ttl(valkey: Valkey, clock: Arc<dyn Clock>) -> Self {
        Self::new(valkey, clock, ENROLLMENT_CODE_TTL)
    }
}

fn decode_enrollment(flat: Vec<String>) -> Result<PendingEnrollment, StoreError> {
    let fields = Fields::from_flat(ENROLLMENTS, "PendingEnrollment", flat)?;
    Ok(PendingEnrollment {
        user_id: UserId::new(fields.required("user_id")?),
        code: EnrollmentCode::new(fields.required("code")?),
        text_fallback: EnrollmentCode::new(fields.required("text_fallback")?),
        issued_at: fields.timestamp("issued_at")?,
    })
}

impl EnrollmentStore for ValkeyEnrollments {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn issue(&self, record: PendingEnrollment) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            let mut encoder = Encoder::default();
            encoder
                .field("user_id", &record.user_id)
                .field("code", record.code.as_str())
                .field("text_fallback", record.text_fallback.as_str())
                .field("issued_at", record.issued_at)
                .field("expires_at", micros(expires_at));
            let mut args = vec![ttl_millis(self.ttl)];
            args.extend(encoder.finish());
            let _: i64 = self
                .valkey
                .eval(
                    ENROLLMENTS,
                    &PUT_RECORD,
                    &[
                        enrollment_key(&record.code),
                        enrollment_key(&record.text_fallback),
                    ],
                    &args,
                )
                .await?;
            tracing::info!(user_id = %record.user_id, "issued enrollment code");
            Ok(())
        })
    }

    fn is_taken<'a>(&'a self, code: &'a EnrollmentCode) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let live: u8 = self
                .valkey
                .eval(
                    ENROLLMENTS,
                    &IS_LIVE,
                    &[enrollment_key(code)],
                    &[micros(self.clock.now()).to_string()],
                )
                .await?;
            tracing::trace!(taken = live == 1, "checked enrollment code collision");
            Ok(live == 1)
        })
    }

    fn redeem<'a>(
        &'a self,
        code: &'a EnrollmentCode,
    ) -> StoreFuture<'a, Option<PendingEnrollment>> {
        Box::pin(async move {
            let flat: Option<Vec<String>> = self
                .valkey
                .eval(
                    ENROLLMENTS,
                    &REDEEM_ENROLLMENT,
                    &[enrollment_key(code)],
                    &[micros(self.clock.now()).to_string()],
                )
                .await?;
            let redeemed = flat.map(decode_enrollment).transpose()?;
            if let Some(record) = &redeemed {
                tracing::info!(user_id = %record.user_id, "redeemed enrollment code");
            } else {
                tracing::debug!("enrollment code unknown, expired or already redeemed");
            }
            Ok(redeemed)
        })
    }
}

/// Valkey [`ChannelStore`].
///
/// The mailboxes are lists whose TTL is copied from the channel's remaining lifetime on every
/// append, and which every script deletes alongside an expired or closed channel — they carry no
/// lifetime of their own to get out of step with.
#[derive(Debug)]
pub struct ValkeyChannels {
    valkey: Valkey,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl ValkeyChannels {
    /// A store on `valkey` and `clock` with the given channel lifetime.
    pub fn new(valkey: Valkey, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self { valkey, clock, ttl }
    }

    /// A store with the [`RELAY_CHANNEL_TTL`].
    pub fn with_default_ttl(valkey: Valkey, clock: Arc<dyn Clock>) -> Self {
        Self::new(valkey, clock, RELAY_CHANNEL_TTL)
    }

    fn now(&self) -> String {
        micros(self.clock.now()).to_string()
    }

    /// The channel and both mailboxes, `direction`'s first.
    fn keys(channel: &ChannelId, direction: Direction) -> [String; 3] {
        let other = match direction {
            Direction::ToInitiator => Direction::ToEnrollee,
            Direction::ToEnrollee => Direction::ToInitiator,
        };
        [
            channel_key(channel),
            mailbox_key(channel, direction),
            mailbox_key(channel, other),
        ]
    }
}

fn decode_channel(flat: Vec<String>) -> Result<RelayChannel, StoreError> {
    let fields = Fields::from_flat(CHANNELS, "RelayChannel", flat)?;
    Ok(RelayChannel {
        initiator_user_id: UserId::new(fields.required("initiator_user_id")?),
        opened_at: fields.timestamp("opened_at")?,
    })
}

impl ChannelStore for ValkeyChannels {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn open<'a>(&'a self, channel: &'a ChannelId, record: RelayChannel) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            let mut encoder = Encoder::default();
            encoder
                .field("initiator_user_id", &record.initiator_user_id)
                .field("opened_at", record.opened_at)
                .field("expires_at", micros(expires_at));
            let mut args = vec![ttl_millis(self.ttl)];
            args.extend(encoder.finish());
            let _: i64 = self
                .valkey
                .eval(
                    CHANNELS,
                    &OPEN_CHANNEL,
                    &Self::keys(channel, Direction::ToInitiator),
                    &args,
                )
                .await?;
            tracing::info!(
                %channel,
                initiator = %record.initiator_user_id,
                "opened enrollment relay channel"
            );
            Ok(())
        })
    }

    fn lookup<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, Option<RelayChannel>> {
        Box::pin(async move {
            let flat: Option<Vec<String>> = self
                .valkey
                .eval(
                    CHANNELS,
                    &LOOKUP_CHANNEL,
                    &Self::keys(channel, Direction::ToInitiator),
                    &[self.now()],
                )
                .await?;
            tracing::trace!(%channel, hit = flat.is_some(), "looked up relay channel");
            flat.map(decode_channel).transpose()
        })
    }

    fn enqueue<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
        payload: RelayPayload,
    ) -> StoreFuture<'a, RelayOutcome> {
        Box::pin(async move {
            let payload_len = payload.len();
            let depth: Option<u64> = self
                .valkey
                .eval(
                    CHANNELS,
                    &ENQUEUE_PAYLOAD,
                    &Self::keys(channel, direction),
                    &[self.now(), payload.as_str().to_owned()],
                )
                .await?;
            let Some(depth) = depth else {
                tracing::debug!(%channel, "relay send found no live channel");
                return Ok(RelayOutcome::NoChannel);
            };
            tracing::debug!(
                %channel,
                direction = direction.as_str(),
                payload_len,
                depth,
                "relayed opaque enrollment payload"
            );
            Ok(RelayOutcome::Enqueued {
                depth: usize::try_from(depth).unwrap_or(usize::MAX),
            })
        })
    }

    fn drain<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
    ) -> StoreFuture<'a, DrainOutcome> {
        Box::pin(async move {
            let items: Option<Vec<String>> = self
                .valkey
                .eval(
                    CHANNELS,
                    &DRAIN_MAILBOX,
                    &Self::keys(channel, direction),
                    &[self.now()],
                )
                .await?;
            let Some(items) = items else {
                tracing::debug!(%channel, "relay drain found no live channel");
                return Ok(DrainOutcome::NoChannel);
            };
            tracing::debug!(
                %channel,
                direction = direction.as_str(),
                drained = items.len(),
                "drained enrollment relay mailbox"
            );
            Ok(DrainOutcome::Drained(
                items.into_iter().map(RelayPayload::new).collect(),
            ))
        })
    }

    fn close<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let was_live: u8 = self
                .valkey
                .eval(
                    CHANNELS,
                    &CLOSE_CHANNEL,
                    &Self::keys(channel, Direction::ToInitiator),
                    &[self.now()],
                )
                .await?;
            tracing::info!(%channel, was_live = was_live == 1, "closed enrollment relay channel");
            Ok(was_live == 1)
        })
    }
}

// ===========================================================================================
// The bundle
// ===========================================================================================

/// Every Valkey store on one connection and one clock — what the `Durable` boot arm builds and
/// what the container-backed conformance harness wraps.
#[derive(Debug, Clone)]
pub struct ValkeyStores {
    valkey: Valkey,
    auth: Arc<ValkeyAuthState>,
    uploads: Arc<ValkeyUploadSessions>,
    challenges: Arc<ValkeyChallenges>,
    enrollments: Arc<ValkeyEnrollments>,
    channels: Arc<ValkeyChannels>,
    cohorts: Arc<ValkeyCohorts>,
}

impl ValkeyStores {
    /// Connect to `url` and build every store with its production lifetime.
    ///
    /// # Errors
    ///
    /// As [`Valkey::connect`].
    pub async fn connect(url: &str, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        let valkey = Valkey::connect(url).await?;
        Ok(Self::with_ttl(
            valkey,
            clock,
            DEFAULT_SESSION_TTL,
            LIFETIME_CAP,
            CHALLENGE_TTL,
            ENROLLMENT_CODE_TTL,
            RELAY_CHANNEL_TTL,
        ))
    }

    /// Connect to `url` and build every store on one lifetime, for the conformance suite.
    ///
    /// # Errors
    ///
    /// As [`Valkey::connect`].
    pub async fn connect_with_uniform_ttl(
        url: &str,
        clock: Arc<dyn Clock>,
        ttl: SignedDuration,
    ) -> Result<Self, StoreError> {
        let valkey = Valkey::connect(url).await?;
        Ok(Self::with_ttl(valkey, clock, ttl, ttl, ttl, ttl, ttl))
    }

    fn with_ttl(
        valkey: Valkey,
        clock: Arc<dyn Clock>,
        session: SignedDuration,
        upload: SignedDuration,
        challenge: SignedDuration,
        enrollment: SignedDuration,
        channel: SignedDuration,
    ) -> Self {
        Self {
            auth: Arc::new(ValkeyAuthState::new(
                valkey.clone(),
                Arc::clone(&clock),
                session,
            )),
            uploads: Arc::new(ValkeyUploadSessions::new(
                valkey.clone(),
                Arc::clone(&clock),
                upload,
            )),
            challenges: Arc::new(ValkeyChallenges::new(
                valkey.clone(),
                Arc::clone(&clock),
                challenge,
            )),
            enrollments: Arc::new(ValkeyEnrollments::new(
                valkey.clone(),
                Arc::clone(&clock),
                enrollment,
            )),
            channels: Arc::new(ValkeyChannels::new(valkey.clone(), clock, channel)),
            cohorts: Arc::new(ValkeyCohorts::new(valkey.clone())),
            valkey,
        }
    }

    /// The connection every store here shares, for the counter adapter.
    pub fn valkey(&self) -> &Valkey {
        &self.valkey
    }

    /// The session store.
    pub fn auth(&self) -> Arc<ValkeyAuthState> {
        Arc::clone(&self.auth)
    }

    /// The upload-session store.
    pub fn uploads(&self) -> Arc<ValkeyUploadSessions> {
        Arc::clone(&self.uploads)
    }

    /// The revoke-all challenge store.
    pub fn challenges(&self) -> Arc<ValkeyChallenges> {
        Arc::clone(&self.challenges)
    }

    /// The enrollment-code store.
    pub fn enrollments(&self) -> Arc<ValkeyEnrollments> {
        Arc::clone(&self.enrollments)
    }

    /// The relay-channel store.
    pub fn channels(&self) -> Arc<ValkeyChannels> {
        Arc::clone(&self.channels)
    }

    /// The device-cohort map.
    pub fn cohorts(&self) -> Arc<ValkeyCohorts> {
        Arc::clone(&self.cohorts)
    }
}

#[cfg(test)]
mod tests {
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
            pending_key(&OwnerId::new("o"), "h"),
            "capsule:upload:pending:o:h"
        );
        assert!(
            SET_STATUS
                .source
                .contains("'capsule:upload:pending:' .. owner .. ':' .. hash"),
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
            last_active_at: Timestamp::UNIX_EPOCH,
            user_agent: None,
            ip_address: Some("203.0.113.7".to_owned()),
            cohort_hash: None,
            device_id: Some(Uuid::from_u128(7)),
        };
        let flat = encode_session(&record, Timestamp::UNIX_EPOCH);
        assert!(flat.contains(&"expires_at".to_owned()));
        assert!(
            !flat.contains(&"user_agent".to_owned()),
            "absent stays absent"
        );
        assert_eq!(decode_session(flat).expect("decodes"), record);
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
            status: UploadSessionStatus::WaitingForProcessing,
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
            pending_key(&OwnerId::new("o"), "h"),
            "capsule:upload:pending:o:h"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "must contain no `:`")]
    fn a_key_segment_with_a_colon_is_refused_in_debug() {
        let _ = pending_key(&OwnerId::new("not:an:id"), "h");
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
}
