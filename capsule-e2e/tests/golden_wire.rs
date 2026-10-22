//! Golden wire fixtures per protocol version ([Versioning — Validation]): what a build put on
//! the wire stays readable by every build after it.
//!
//! Each directory under `tests/fixtures/golden/` is one protocol version's recording (see
//! [`capsule_e2e::fixtures::golden`] for the layout). Against every directory, this build must:
//!
//! - **server** — answer every replayable request with the status it was recorded with, on a
//!   fresh account, or refuse the first write with `426` once the version has left its window
//!   (invariant 3: a mismatch refuses a write, it never misreads one);
//! - **client** — decode every recorded response into the generated client's type for it, and
//!   still decode it with a field added that this build has never heard of (invariant 9: a
//!   newer server's additions are optional and a client ignores what it does not know);
//! - **records** — decode, verify and re-emit every signed record byte for byte (invariant 2),
//!   verify every custody receipt under the attestation key the same session published, and
//!   refuse — not misread — the same provenance record stamped one version past this build's.
//!
//! The current protocol version must have a directory, so a protocol bump cannot land without
//! recording what it speaks. Recording is the ignored [`record_golden_fixtures`] test:
//!
//! ```text
//! CAPSULE_GOLDEN_RECORD=1 cargo test -p capsule-e2e --test golden_wire -- --ignored
//! ```
//!
//! It refuses to touch a directory that exists. `CAPSULE_GOLDEN_RECORD=overwrite` re-records
//! one, which is only ever right before any release has spoken that version.
//!
//! [Versioning — Validation]: https://docs/design/versioning/#validation

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::cbor;
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::{DeviceDirectory, HybridVerifyingKey};
use capsule_core::crypto::provenance::{
    PROVENANCE_RECORD_VERSION, ProvenanceRecord, RecordDecodeError,
};
use capsule_core::crypto::receipts::CustodyReceipt;
use capsule_e2e::fixtures::golden::{
    self, Exchange, RECORDS_FILE, Records, TRANSCRIPT_FILE, Transcript,
};
use capsule_e2e::push::push_asset;
use capsule_e2e::{Device, PASSWORD, PROTOCOL_VERSION, Server};
use capsule_sdk::auth::Session;
use capsule_sdk::rest::types as wire;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::{Value, json};

/// The account the replayable half of the script registers. A fresh server per replay, so the
/// fixed address never collides.
const EMAIL: &str = "golden@e2e.capsule.test";

/// The album the script provisions: a fixed, canonical UUIDv7 so the request replays verbatim.
const ALBUM_ID: &str = "0190f5c0-0000-7000-8000-000000000001";

/// A protocol date before every window this server has advertised.
const STALE_PROTOCOL: &str = "1999-01-01";

/// The gate's refusal code (`capsule_i18n::error_codes`).
const VERSION_UNSUPPORTED: &str = "error.protocol.version_unsupported";

/// The key a later client must ignore when a newer server adds it.
const UNKNOWN_FIELD: &str = "x_capsule_field_from_a_newer_server";

// ── The script ───────────────────────────────────────────────────────────────────────────────

/// One scripted request, before it is sent.
struct Step {
    name: &'static str,
    method: &'static str,
    path: String,
    protocol: Option<&'static str>,
    authenticated: bool,
    request: Option<Value>,
    schema: &'static str,
}

impl Step {
    fn new(name: &'static str, method: &'static str, path: &str, schema: &'static str) -> Self {
        Self {
            name,
            method,
            path: path.to_owned(),
            protocol: None,
            authenticated: false,
            request: None,
            schema,
        }
    }

    fn authenticated(mut self) -> Self {
        self.authenticated = true;
        self
    }

    fn body(mut self, body: Value) -> Self {
        self.request = Some(body);
        self
    }

    fn protocol(mut self, protocol: &'static str) -> Self {
        self.protocol = Some(protocol);
        self
    }
}

/// The replayable half: every request a fresh account can make with nothing but the bytes
/// recorded here, in the order it makes them.
fn replayable_script() -> Vec<Step> {
    let credentials = json!({ "email": EMAIL, "password": PASSWORD });
    vec![
        Step::new("get_version", "GET", "/v1/version", "VersionResponse"),
        Step::new(
            "server_info",
            "GET",
            "/.well-known/capsule/server-info",
            "ServerInfoResponse",
        ),
        Step::new(
            "attestation_keys",
            "GET",
            "/.well-known/capsule/attestation-keys",
            "AttestationKeysResponse",
        ),
        Step::new(
            "register_user",
            "POST",
            "/v1/auth/register",
            "TokenResponse",
        )
        .body(credentials.clone()),
        Step::new(
            "register_user_again",
            "POST",
            "/v1/auth/register",
            "CodedProblem",
        )
        .body(credentials.clone()),
        Step::new("login_user", "POST", "/v1/auth/login", "TokenResponse").body(credentials),
        Step::new("get_profile", "GET", "/v1/auth/profile", "ProfileResponse").authenticated(),
        Step::new(
            "update_profile",
            "PATCH",
            "/v1/auth/profile",
            "ProfileResponse",
        )
        .authenticated()
        .body(json!({ "display_name": "Golden" })),
        Step::new(
            "provision_album",
            "POST",
            "/v1/albums",
            "ProvisionAlbumResponse",
        )
        .authenticated()
        .body(json!({ "album_id": ALBUM_ID })),
        Step::new("get_quota", "GET", "/v1/quota", "QuotaResponse").authenticated(),
        Step::new("sync_feed", "GET", "/v1/sync", "SyncPageResponse").authenticated(),
        Step::new(
            "list_upload_sessions",
            "GET",
            "/v1/upload/sessions",
            "SessionsResponse",
        )
        .authenticated(),
        Step::new("list_devices", "GET", "/v1/auth/devices", "DevicesResponse").authenticated(),
        Step::new(
            "create_upload_outside_the_window",
            "POST",
            "/v1/upload",
            "ProtocolRangeProblem",
        )
        .authenticated()
        .protocol(STALE_PROTOCOL)
        .body(json!({})),
    ]
}

// ── Raw HTTP ─────────────────────────────────────────────────────────────────────────────────

/// A raw HTTP client: what a fixture is sent and recorded through. Nothing typed stands
/// between the fixture's bytes and the socket.
struct Wire {
    http: reqwest::Client,
    base: String,
    protocol: String,
    token: Option<String>,
}

/// What came back.
struct Answer {
    status: u16,
    body: Value,
    protocol_min: Option<String>,
    protocol_max: Option<String>,
}

impl Wire {
    fn new(server: &Server, protocol: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: server.base_url().to_owned(),
            protocol: protocol.to_owned(),
            token: None,
        }
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        protocol: Option<&str>,
        authenticated: bool,
        body: Option<&Value>,
    ) -> Answer {
        let method = reqwest::Method::from_bytes(method.as_bytes()).expect("an HTTP method");
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("x-capsule-protocol", protocol.unwrap_or(&self.protocol));
        if authenticated {
            let token = self.token.as_deref().expect("a token from an earlier step");
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(serde_json::to_vec(body).expect("a JSON body serializes"));
        }
        answer(request.send().await.expect("the server answers")).await
    }

    /// Adopt the access token a `TokenResponse` carried.
    fn adopt_token(&mut self, body: &Value) {
        if let Some(token) = body.get("access_token").and_then(Value::as_str) {
            self.token = Some(token.to_owned());
        }
    }
}

async fn answer(response: reqwest::Response) -> Answer {
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let protocol_min = header("x-capsule-protocol-min");
    let protocol_max = header("x-capsule-protocol-max");
    let status = response.status().as_u16();
    let bytes = response.bytes().await.expect("the body reads");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "a JSON body ({e}): {}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(512)])
            )
        })
    };
    Answer {
        status,
        body,
        protocol_min,
        protocol_max,
    }
}

/// A read through an SDK session's own client and bearer token, at this build's protocol.
async fn session_read(session: &Session, server: &Server, path: &str) -> reqwest::Response {
    let url = format!("{}{path}", server.base_url());
    session
        .execute(|http| {
            http.get(&url)
                .header("x-capsule-protocol", PROTOCOL_VERSION)
        })
        .await
        .expect("the server answers")
}

// ── The client half: decode a recorded response ──────────────────────────────────────────────

fn decode_as<T: DeserializeOwned>(body: &Value) -> Result<(), String> {
    serde_json::from_value::<T>(body.clone())
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Decode `body` into the generated client's type for the OpenAPI component `schema`.
///
/// A component a fixture names and this build no longer generates is a failure, not a skip:
/// evolution is additive (invariant 9), so a schema a released server answered with never
/// leaves the contract.
fn decode(schema: &str, body: &Value) -> Result<(), String> {
    match schema {
        "VersionResponse" => decode_as::<wire::VersionResponse>(body),
        "ServerInfoResponse" => decode_as::<wire::ServerInfoResponse>(body),
        "AttestationKeysResponse" => decode_as::<wire::AttestationKeysResponse>(body),
        "TokenResponse" => decode_as::<wire::TokenResponse>(body),
        "CodedProblem" => decode_as::<wire::CodedProblem>(body),
        "ProtocolRangeProblem" => decode_as::<wire::ProtocolRangeProblem>(body),
        "ProfileResponse" => decode_as::<wire::ProfileResponse>(body),
        "ProvisionAlbumResponse" => decode_as::<wire::ProvisionAlbumResponse>(body),
        "QuotaResponse" => decode_as::<wire::QuotaResponse>(body),
        "SyncPageResponse" => decode_as::<wire::SyncPageResponse>(body),
        "SessionsResponse" => decode_as::<wire::SessionsResponse>(body),
        "DevicesResponse" => decode_as::<wire::DevicesResponse>(body),
        "AssetReceiptsResponse" => decode_as::<wire::AssetReceiptsResponse>(body),
        other => Err(format!(
            "no generated client type is mapped for component {other}"
        )),
    }
}

// ── The suites ───────────────────────────────────────────────────────────────────────────────

/// The protocol version this build speaks has been recorded, so a protocol bump cannot land
/// without the fixtures every later build will be held to.
#[test]
fn the_current_protocol_version_has_golden_fixtures() {
    let versions: Vec<String> = golden::versions().into_iter().map(|(v, _)| v).collect();
    assert!(
        versions.iter().any(|v| v == PROTOCOL_VERSION),
        "no golden fixtures for protocol {PROTOCOL_VERSION} (have {versions:?}); record them \
         with CAPSULE_GOLDEN_RECORD=1 cargo test -p capsule-e2e --test golden_wire -- --ignored"
    );
    for (version, dir) in golden::versions() {
        let transcript = golden::load_transcript(&dir);
        assert_eq!(
            transcript.protocol_version, version,
            "a transcript sits in its own version's directory"
        );
    }
}

/// Client: every recorded response decodes into the generated type, and still does with a
/// field this build has never heard of added at the top level.
#[test]
fn golden_responses_decode_into_the_generated_client_types() {
    for (version, dir) in golden::versions() {
        for exchange in golden::load_transcript(&dir).exchanges {
            let at = format!("{version}/{}", exchange.name);
            decode(&exchange.schema, &exchange.response)
                .unwrap_or_else(|e| panic!("{at}: the recorded response decodes: {e}"));

            let mut extended = exchange.response.clone();
            let object = extended
                .as_object_mut()
                .unwrap_or_else(|| panic!("{at}: every recorded response is a JSON object"));
            object.insert(UNKNOWN_FIELD.to_owned(), json!({ "nested": [1, "two"] }));
            decode(&exchange.schema, &extended).unwrap_or_else(|e| {
                panic!("{at}: a response with an unknown field still decodes: {e}")
            });
        }
    }
}

/// How a replay ended.
#[derive(Debug, PartialEq, Eq)]
enum Replayed {
    /// Every replayable request drew its recorded status and component.
    Completed,
    /// The version is outside the server's window, and the named write — the first the
    /// transcript makes — was refused with `426`.
    RefusedAt(String),
}

/// Replay `transcript`'s replayable requests, in order, on a fresh account on `server`.
///
/// Each draws the status it was recorded with and a body the generated client decodes as the
/// same component — unless the server's window (read off the first response) excludes the
/// transcript's version, in which case the first write must be refused with `426` and the
/// version-unsupported code, and nothing after it is attempted.
async fn replay(server: &Server, transcript: &Transcript) -> Replayed {
    let version = transcript.protocol_version.as_str();
    let mut wire = Wire::new(server, version);
    let mut in_window = None;
    for exchange in transcript.exchanges.iter().filter(|e| e.replay) {
        let at = format!("{version}/{}", exchange.name);
        let got = wire
            .send(
                &exchange.method,
                &exchange.path,
                exchange.protocol.as_deref(),
                exchange.authenticated,
                exchange.request.as_ref(),
            )
            .await;
        let inside = *in_window.get_or_insert_with(|| {
            let min = got
                .protocol_min
                .clone()
                .expect("the window rides every response");
            let max = got
                .protocol_max
                .clone()
                .expect("the window rides every response");
            min.as_str() <= version && version <= max.as_str()
        });
        if !inside && exchange.request.is_some() {
            assert_eq!(got.status, 426, "{at}: a write from outside the window");
            assert_eq!(got.body["code"], VERSION_UNSUPPORTED, "{at}");
            return Replayed::RefusedAt(exchange.name.clone());
        }
        assert_eq!(got.status, exchange.status, "{at}: {}", got.body);
        decode(&exchange.schema, &got.body)
            .unwrap_or_else(|e| panic!("{at}: this server's answer decodes: {e}"));
        wire.adopt_token(&got.body);
    }
    Replayed::Completed
}

/// Server: every recorded version replays against this build's server. The version this build
/// speaks is inside its own default window, so its replay must complete; an older one may
/// instead have its first write refused, which is the only other outcome invariant 3 allows.
#[tokio::test]
async fn golden_requests_replay_against_the_current_server() {
    for (version, dir) in golden::versions() {
        let server = Server::boot().await;
        let outcome = replay(&server, &golden::load_transcript(&dir)).await;
        if version == PROTOCOL_VERSION {
            assert_eq!(outcome, Replayed::Completed, "{version}");
        }
    }
}

/// Server, the other branch: a server whose window has moved past every recorded version
/// refuses each one's first write with `426`, rather than reading it.
#[tokio::test]
async fn golden_requests_from_outside_the_window_are_refused_at_the_first_write() {
    let server = Server::boot_with_window("2100-01-01", "2100-12-31").await;
    for (version, dir) in golden::versions() {
        let transcript = golden::load_transcript(&dir);
        let first_write = transcript
            .exchanges
            .iter()
            .find(|e| e.replay && e.request.is_some())
            .expect("a transcript writes");
        assert_eq!(
            replay(&server, &transcript).await,
            Replayed::RefusedAt(first_write.name.clone()),
            "{version}"
        );
    }
}

/// Records: the device directory and the provenance record decode, verify and re-emit byte
/// for byte, and the record's manifest is signed by a device the directory lists.
#[test]
fn golden_records_decode_verify_and_round_trip_byte_identically() {
    for (version, dir) in golden::versions() {
        let records = golden::load_records(&dir);

        let directory_bytes = from_hex(&records.device_directory);
        let directory: DeviceDirectory = cbor::from_slice(&directory_bytes)
            .unwrap_or_else(|e| panic!("{version}: the directory decodes: {e}"));
        let user_ik = HybridVerifyingKey::from_bytes(&from_hex(&records.user_ik_public))
            .unwrap_or_else(|e| panic!("{version}: the identity key decodes: {e}"));
        assert!(
            directory.verify(&user_ik),
            "{version}: the directory verifies"
        );
        assert_eq!(
            cbor::to_canonical_vec(&directory).expect("re-encodes"),
            directory_bytes,
            "{version}: the directory re-emits byte for byte"
        );

        let record = check_record(&version, &directory, &records.provenance_record);
        assert!(
            record.manifest.core.unknown.is_empty(),
            "{version}: this version's own writer emits no key it does not define"
        );

        // Invariant 2: a newer writer's keys survive the read, the re-emit and the signature,
        // and dropping them fails verification.
        let at = format!("{version} (unknown fields)");
        let future = check_record(
            &at,
            &directory,
            &records.provenance_record_with_unknown_fields,
        );
        assert!(
            !future.manifest.core.unknown.is_empty(),
            "{at}: the record carries keys this build does not define"
        );
        let mut stripped = future.manifest.clone();
        stripped.core.unknown.clear();
        let signer = directory
            .device(&stripped.core.created_by_device)
            .expect("checked above");
        assert!(
            !signer
                .dsk_public
                .verify(&stripped.signing_bytes(), &stripped.device_sig),
            "{at}: stripping the unknown keys fails verification"
        );
    }
}

/// Decode the hex-encoded provenance record `hex`, and assert it re-emits byte for byte, its
/// chain hash is the digest of those bytes, and its manifest's device signature verifies under
/// the device `directory` lists for it.
fn check_record(at: &str, directory: &DeviceDirectory, hex: &str) -> ProvenanceRecord {
    let bytes = from_hex(hex);
    let record = ProvenanceRecord::decode(&bytes)
        .unwrap_or_else(|e| panic!("{at}: the provenance record decodes: {e}"));
    assert_eq!(
        cbor::to_canonical_vec(&record).expect("re-encodes"),
        bytes,
        "{at}: the record re-emits byte for byte"
    );
    assert_eq!(
        record.record_hash(),
        hash_bytes(&bytes),
        "{at}: the chain hash is the digest of the bytes served"
    );
    let signer = directory
        .device(&record.manifest.core.created_by_device)
        .unwrap_or_else(|| panic!("{at}: the directory lists the signing device"));
    assert!(
        signer.dsk_public.verify(
            &record.manifest.signing_bytes(),
            &record.manifest.device_sig
        ),
        "{at}: the manifest's device signature verifies"
    );
    record
}

/// The bytes a fixture's hex spells; a fixture that does not parse is a broken fixture.
fn from_hex(hex: &str) -> Vec<u8> {
    hex::decode(hex).expect("a golden record is hex")
}

/// The length of the canonical CBOR map header at the start of `bytes`, if it is one that
/// declares exactly `entries` entries (RFC 8949 §3: major type 5, shortest-form argument).
fn map_header_len(bytes: &[u8], entries: usize) -> Option<usize> {
    let first = *bytes.first()?;
    if first >> 5 != 5 {
        return None;
    }
    let (len, count) = match first & 0x1F {
        info @ 0..=23 => (1, u64::from(info)),
        24 => (2, u64::from(*bytes.get(1)?)),
        25 => (
            3,
            u64::from(u16::from_be_bytes(bytes.get(1..3)?.try_into().ok()?)),
        ),
        26 => (
            5,
            u64::from(u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?)),
        ),
        27 => (9, u64::from_be_bytes(bytes.get(1..9)?.try_into().ok()?)),
        _ => return None,
    };
    (count == entries as u64).then_some(len)
}

/// Records, the older-reader direction (invariant 3): the golden record leads with its
/// version on the wire, and stamped one version past what this build reads it is refused as
/// such, never decoded as a record this build knows.
#[test]
fn a_golden_record_from_a_newer_writer_is_refused_not_misread() {
    /// The text key `version` as canonical CBOR: major type 3, length 7.
    const VERSION_KEY: &[u8] = b"\x67version";
    let newer = u64::from(PROVENANCE_RECORD_VERSION) + 1;
    for (version, dir) in golden::versions() {
        let bytes = from_hex(&golden::load_records(&dir).provenance_record);
        let record = ProvenanceRecord::decode(&bytes).expect("the golden record decodes");

        // The record is a map; the first key after its header is `version`, however many
        // entries a later version adds. Only the keys are counted, so the values are skipped.
        #[expect(
            clippy::zero_sized_map_values,
            reason = "counts keys; values are skipped"
        )]
        let entries = cbor::from_slice::<BTreeMap<String, IgnoredAny>>(&bytes)
            .expect("a record is a text-keyed map")
            .len();
        let header = map_header_len(&bytes, entries)
            .unwrap_or_else(|| panic!("{version}: a canonical {entries}-entry map header"));
        assert!(
            bytes[header..].starts_with(VERSION_KEY),
            "{version}: the record leads with its version (invariant 2)"
        );
        let at = header + VERSION_KEY.len();
        let old = cbor::to_canonical_vec(&u64::from(record.version)).expect("encodes");
        assert!(
            bytes[at..].starts_with(&old),
            "{version}: the version value"
        );
        let mut stamped = bytes[..at].to_vec();
        stamped.extend(cbor::to_canonical_vec(&newer).expect("encodes"));
        stamped.extend_from_slice(&bytes[at + old.len()..]);

        match ProvenanceRecord::decode(&stamped) {
            Err(RecordDecodeError::UnsupportedVersion { found, max_known }) => {
                assert_eq!(found, newer, "{version}");
                assert_eq!(max_known, PROVENANCE_RECORD_VERSION, "{version}");
            }
            other => panic!("{version}: expected UnsupportedVersion, got {other:?}"),
        }
        assert!(
            cbor::from_slice::<ProvenanceRecord>(&stamped).is_err(),
            "{version}: no other decode path reads it as a known version"
        );
    }
}

/// Records: every custody receipt the session was served decodes, re-emits byte for byte,
/// matches the JSON projection served beside it, and verifies under the attestation key the
/// same server published.
#[test]
fn golden_receipts_verify_under_the_published_attestation_keys() {
    for (version, dir) in golden::versions() {
        let transcript = golden::load_transcript(&dir);
        let keys: wire::AttestationKeysResponse = serde_json::from_value(
            transcript
                .exchange("attestation_keys")
                .expect("the transcript fetched the attestation keys")
                .response
                .clone(),
        )
        .expect("the attestation keys decode");
        let receipts: wire::AssetReceiptsResponse = serde_json::from_value(
            transcript
                .exchange("get_asset_receipts")
                .expect("the transcript fetched the pushed asset's receipts")
                .response
                .clone(),
        )
        .expect("the receipts decode");
        assert!(
            !receipts.receipts.is_empty(),
            "{version}: a pushed asset has receipts"
        );

        for served in &receipts.receipts {
            let at = format!("{version}/receipt {}", served.receipt_seq);
            let bytes = BASE64
                .decode(&served.receipt_cbor)
                .unwrap_or_else(|e| panic!("{at}: base64: {e}"));
            let receipt = CustodyReceipt::from_canonical_cbor(&bytes)
                .unwrap_or_else(|e| panic!("{at}: the receipt decodes: {e}"));
            assert_eq!(receipt.to_canonical_cbor(), bytes, "{at}: byte for byte");
            assert_eq!(
                receipt.core.ciphertext_hash.to_hex(),
                served.ciphertext_hash,
                "{at}"
            );
            assert_eq!(receipt.core.blob_role, served.blob_role, "{at}");
            assert_eq!(
                receipt.core.server_key_id.to_hex(),
                served.server_key_id,
                "{at}"
            );

            let key = keys
                .keys
                .iter()
                .find(|k| k.key_id == served.server_key_id)
                .unwrap_or_else(|| panic!("{at}: the signing key is published"));
            let key = HybridVerifyingKey::from_bytes(
                &BASE64.decode(&key.public).expect("a base64 public key"),
            )
            .expect("a hybrid public key");
            assert!(receipt.verify_under(&key), "{at}: the receipt verifies");
        }
    }
}

// ── The recorder ─────────────────────────────────────────────────────────────────────────────

/// Record this build's protocol version: the replayable script over raw HTTP, then an asset
/// imported and pushed through the real library and SDK, whose feed entry, receipts, device
/// directory and provenance record are captured as served.
#[tokio::test]
#[ignore = "records fixtures: CAPSULE_GOLDEN_RECORD=1 cargo test -p capsule-e2e --test golden_wire -- --ignored"]
async fn record_golden_fixtures() {
    let mode = std::env::var("CAPSULE_GOLDEN_RECORD").unwrap_or_default();
    let dir = golden::root().join(PROTOCOL_VERSION);
    match mode.as_str() {
        "1" => assert!(
            !dir.exists(),
            "{} exists; fixtures a release may have spoken are never re-recorded \
             (CAPSULE_GOLDEN_RECORD=overwrite if no release has)",
            dir.display()
        ),
        "overwrite" => {}
        _ => panic!("set CAPSULE_GOLDEN_RECORD=1 to record, or =overwrite to re-record"),
    }
    std::fs::create_dir_all(&dir).expect("the version directory creates");

    let server = Server::boot().await;
    let mut exchanges = Vec::new();

    let mut raw = Wire::new(&server, PROTOCOL_VERSION);
    for step in replayable_script() {
        let got = raw
            .send(
                step.method,
                &step.path,
                step.protocol,
                step.authenticated,
                step.request.as_ref(),
            )
            .await;
        decode(step.schema, &got.body)
            .unwrap_or_else(|e| panic!("{}: the answer is a {}: {e}", step.name, step.schema));
        raw.adopt_token(&got.body);
        exchanges.push(exchange(&step, got.status, got.body, true));
    }

    // The half a replay cannot reproduce: an asset through the real library and SDK.
    let mut device = Device::register(&server, "golden").await;
    let asset = device.import_jpeg("golden.jpg");
    push_asset(&device, &server, &asset).await;

    let reads = [
        Step::new(
            "sync_feed_after_push",
            "GET",
            "/v1/sync",
            "SyncPageResponse",
        )
        .authenticated(),
        Step::new(
            "get_asset_receipts",
            "GET",
            &format!("/v1/assets/{asset}/receipts"),
            "AssetReceiptsResponse",
        )
        .authenticated(),
    ];
    for step in reads {
        let got = answer(session_read(&device.session, &server, &step.path).await).await;
        decode(step.schema, &got.body)
            .unwrap_or_else(|e| panic!("{}: the answer is a {}: {e}", step.name, step.schema));
        exchanges.push(exchange(&step, got.status, got.body, false));
    }

    let served = session_read(
        &device.session,
        &server,
        &format!("/v1/auth/devices/directory/{}", device.user_id),
    )
    .await;
    assert_eq!(served.status().as_u16(), 200, "the directory is served");
    let directory = served.bytes().await.expect("the directory reads").to_vec();
    let head = device.head_record(&asset);
    let record = cbor::to_canonical_vec(&head).expect("encodes");
    let future = cbor::to_canonical_vec(&with_unknown_fields(&device, &head)).expect("encodes");

    write(
        &dir,
        &Transcript {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            exchanges,
        },
        &Records {
            device_directory: hex::encode(&directory),
            user_ik_public: hex::encode(device.workspace.user_ik_public().to_bytes()),
            provenance_record: hex::encode(&record),
            provenance_record_with_unknown_fields: hex::encode(&future),
        },
    );
}

/// `head` as a newer writer would emit it: its manifest core gains keys no version has
/// defined — a nested map holding a float and an array, and an array of a full-width integer
/// and text — and is
/// re-signed over them by the harness's proposer device, which the published directory lists.
///
/// The library keeps its own device key private, so the proposer signs both slots; the golden
/// suite checks the device signature, the one the directory binds.
fn with_unknown_fields(device: &Device, head: &ProvenanceRecord) -> ProvenanceRecord {
    /// A value of whatever CBOR type `value` serializes to, as the manifest's unknown map holds
    /// it.
    fn cbor_value<T: serde::Serialize, V: DeserializeOwned>(value: &T) -> V {
        cbor::from_slice(&cbor::to_canonical_vec(value).expect("encodes")).expect("decodes")
    }

    let mut core = head.manifest.core.clone();
    core.created_by_device = device.proposer_id;
    let nested: BTreeMap<&str, (f64, Vec<i64>)> = BTreeMap::from([("weights", (0.5, vec![-3]))]);
    core.unknown
        .insert("x_future_field".to_owned(), cbor_value(&nested));
    core.unknown
        .insert("z".to_owned(), cbor_value(&(u64::MAX, "newer")));
    let manifest = core
        .sign(&device.proposer, &device.proposer)
        .expect("the core signs");
    ProvenanceRecord::new(head.asset_id, manifest, head.prior_provenance_hash)
}

fn exchange(step: &Step, status: u16, mut response: Value, replay: bool) -> Exchange {
    golden::redact_tokens(&mut response);
    Exchange {
        name: step.name.to_owned(),
        method: step.method.to_owned(),
        path: step.path.clone(),
        protocol: step.protocol.map(str::to_owned),
        authenticated: step.authenticated,
        replay,
        request: step.request.clone(),
        status,
        schema: step.schema.to_owned(),
        response,
    }
}

fn write(dir: &Path, transcript: &Transcript, records: &Records) {
    golden::write_json(&dir.join(TRANSCRIPT_FILE), transcript);
    golden::write_json(&dir.join(RECORDS_FILE), records);
}
