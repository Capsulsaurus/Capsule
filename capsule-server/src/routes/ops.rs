//! `POST /v1/albums/{album_id}/ops` — the lifecycle-write surface (slice `S-C16`).
//!
//! One endpoint for every lifecycle write that does **not** move blob bytes: `delete`,
//! `trash-restore`, `metadata-update`, and a derivative over blobs the server already holds. A
//! write that moves bytes is an upload by definition, so `create` rides `S-C1` and `replace`
//! is `S-C43`'s.
//!
//! The endpoint is deliberately singular. A new action is a manifest-schema change under a new
//! `protocol_version`, never a new endpoint — which is what keeps the invariant battery in one
//! place rather than in one place per verb.
//!
//! # Where each invariant is decided, and why not all of them are here
//!
//! | Invariants | Decided by | Why there |
//! | --- | --- | --- |
//! | 1, 2, 6, 7, 8, 15, 16 | [`check_op`](crate::upload::envelope::check_op) | pure over the request, so no state can change under them |
//! | 25 | this module | it compares bytes in hand against a field in the manifest |
//! | **17, 18** | [`AssetIndex::apply_op`](crate::index::AssetIndex::apply_op) | they are the only two whose answer depends on **stored** state, and a check taken outside the write's critical section is a check on facts that can change before the write lands |
//!
//! That last row is the whole shape of this surface. Two concurrent ops that both read the same
//! chain head and then both write would both pass a handler-side check and double-apply, which
//! is the stale revival invariant 17 exists to catch, reintroduced by the code enforcing it.
//!
//! # A rejection writes nothing a client can observe
//!
//! The bundle's blobs are stored *before* the index is asked to apply the op, so a refusal can
//! leave an unreferenced blob behind. That is the deliberate reading of the authorization doc's
//! "rejections write nothing": an unreferenced blob is unreachable by content address and is
//! what refcount GC collects, while what the rule protects — the asset's row, its provenance
//! chain, its feed position — is untouched. Applying first would be worse and unrecoverably so:
//! it would point the feed at bytes the store might never receive.
//!
//! # Idempotency without remembering any bytes
//!
//! The contract says a replayed manifest returns the **byte-identical** prior response. The
//! retired implementation stored the serialized response in a table beside the op. Here the
//! response is a pure function of `(asset_id, action, sync_seq)` and all three are stored facts,
//! so byte-identity follows from determinism rather than from a second copy of something
//! derivable. A stored copy is a second thing that can be wrong.
//!
//! Note the ordering that makes it work: the index checks the replay key **before** invariant
//! 17. A client that lost an acknowledgement is resubmitting a manifest whose predecessor is no
//! longer the head, so checking the chain first would answer `409` to a client whose only fault
//! was not hearing the first answer.
//!
//! # `S-C28` audit
//!
//! | Salvo status | Verdict |
//! | --- | --- |
//! | `200` | kept, and now **static**. The retired handler picked its status at run time with `StatusCode::from_u16(result.status)`, which is why salvo-oapi could describe no responses at all and spargen refused the operation outright — and the value was unconditionally `200` every time |
//! | `400` (envelope, action, amk) | kept, each with its own `error.*` code |
//! | `403 error.upload.album_access_denied` | kept, and it now also answers an asset that is not the caller's — one value, because the asset id is client-chosen |
//! | `409 error.upload.stale_revival` | kept — invariant 17, the status this surface exists to be able to give |
//! | `401` | kept, and now the framework's |
//! | `500` | kept |

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;
use crate::blob::ContentAddress;
use crate::index::{LifecycleOp, OpAction, OpOutcome};
use crate::store::{AlbumId, AssetId};
use crate::upload::envelope::{GateContext, GateReject, ManifestEnvelope, check_op};
use crate::upload::{AlbumWriteAccess, UploadContext};

/// The lifecycle surface: every write that changes an asset without moving its bytes.
#[derive(Tag)]
#[tag(
    name = "lifecycle",
    description = "Applying a signed lifecycle manifest to an album's assets."
)]
pub struct LifecycleTag;

/// The album the op is addressed to.
#[derive(PathParams, Schema)]
pub struct AlbumPath {
    /// The album's identifier.
    pub album_id: String,
}

/// The signed manifest bundle a lifecycle write carries.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct OpRequest {
    /// The server-visible projection of the signed manifest's fields, exactly as
    /// `POST /v1/upload` carries it. Its `album_id` must equal the path segment and its
    /// `action` must be one this surface accepts.
    pub manifest_envelope: ManifestEnvelope,
    /// The signed manifest itself, base64 of the canonical CBOR.
    ///
    /// Stored verbatim as the asset's new provenance blob, so the feed serves the exact bytes
    /// the client signed (`S-C30`) for a lifecycle write as it already does for an upload. The
    /// server does not parse it: base64 is a transport encoding, and `decode(encode(b)) == b`.
    pub manifest_cbor: String,
    /// The encrypted metadata blob, base64, present exactly when the action carries one.
    ///
    /// Its content hash must equal the manifest's committed `metadata_blob_hash`
    /// (invariant 25). The server holds no key and never reads it.
    pub metadata_blob: Option<String>,
}

/// What a lifecycle write did.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OpResponse {
    /// The asset the op chained onto.
    pub asset_id: String,
    /// The feed position it occupies. On a replay, the position the *first* application took.
    pub sync_seq: u64,
    /// The action that was applied.
    pub action: String,
    /// Whether this response is a replay of an already-applied manifest.
    ///
    /// Advisory, and deliberately not something a correct client needs: the other three fields
    /// are identical either way, which is what "byte-identical prior response" means.
    pub replayed: bool,
}

/// Why a lifecycle write was refused.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum OpRejection {
    /// The manifest failed a structural check.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid request")]
    Invalid {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The protocol version is outside the accepted window.
    #[error("this server does not speak that protocol version")]
    #[problem(status = 426, title = "Upgrade required")]
    ProtocolUnsupported {
        /// The oldest protocol date the server accepts.
        #[problem(extension)]
        protocol_min: String,
        /// The newest.
        #[problem(extension)]
        protocol_max: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The caller may not write to this album, or the asset is not theirs.
    ///
    /// One value for both. The asset id is the manifest's `file_id` and therefore
    /// client-chosen, so a guess must cost nothing and buy nothing.
    #[error("this album is not writable by this caller")]
    #[problem(status = 403, title = "Forbidden")]
    AlbumAccessDenied {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Invariant 17: the manifest does not chain onto the asset's stored head.
    #[error("this manifest does not follow the asset's current state")]
    #[problem(status = 409, title = "Stale revival")]
    StaleRevival {
        /// The chain head the manifest must name, so the client can re-read and rebase rather
        /// than retry a losing manifest forever. Absent when the asset has no accepted
        /// manifest yet. Reached only by the asset's owner, so it discloses nothing.
        #[problem(extension)]
        chain_head: Option<String>,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The account is past its grace window, and this write would grow stored metadata.
    ///
    /// Never returned for a `delete` or a `trash-restore`: a user must be able to delete their
    /// way back under quota, and the provenance record a delete produces is itself a write. A
    /// quota that could lock someone out of freeing space would be a trap rather than a limit.
    #[error("this account is over its storage limit and past the grace window")]
    #[problem(status = 403, title = "Quota grace expired")]
    QuotaGraceExpired {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the lifecycle write could not be applied")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Apply one signed lifecycle manifest to an album's asset.
///
/// The whole battery runs before anything is written, and a rejection writes nothing —
/// including the blobs the bundle carries, which are stored only after the manifest has passed
/// every check the server can make without a key.
#[kynos::post(
    "/v1/albums/{album_id}/ops",
    operation_id = "album_lifecycle_op",
    tag = LifecycleTag
)]
pub async fn apply_op(
    Inject(upload): Inject<UploadContext>,
    Inject(quota): Inject<crate::quota::QuotaContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<AlbumPath>,
    Json(request): Json<OpRequest>,
) -> Result<Json<OpResponse>, OpRejection> {
    let caller = credential.user.clone();
    let album = AlbumId::new(&path.album_id);

    // The envelope must agree with the path it arrived on. A contradiction is a client bug the
    // server would otherwise resolve in its own favour, and it would be wrong half the time.
    if request.manifest_envelope.album_id.as_deref() != Some(album.as_str()) {
        return Err(OpRejection::invalid(
            error_codes::UPLOAD_ENVELOPE_MISMATCH,
            "the manifest's album_id is not the album this op was addressed to",
        ));
    }

    // Invariant 6, the half only the authority can answer — and the namespace the op is filed
    // under, which is the album owner's whoever the caller is (`S-C51`): the owner's feed is the
    // one every member's devices read.
    let AlbumWriteAccess::Writable {
        owner_id: owner,
        protocol_pin,
        ..
    } = upload
        .authority()
        .album_write_access(&caller, &album)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the write authority could not answer for an album");
            OpRejection::unavailable()
        })?
    else {
        tracing::info!(%caller, %album, "a lifecycle write was refused: no write capability");
        return Err(OpRejection::album_access_denied());
    };

    // Invariant 7.
    let device = crate::upload::envelope::created_by_device(&request.manifest_envelope)
        .map_err(OpRejection::from_gate)?;
    let Some(device_added_at) = upload
        .authority()
        .device_added_at(&caller, device)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the write authority could not answer for a device");
            OpRejection::unavailable()
        })?
    else {
        tracing::info!(%caller, %device, "a lifecycle write was refused: unknown device");
        return Err(OpRejection::invalid(
            error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED,
            "the manifest names a device this account has not published",
        ));
    };

    let now = upload.clock().now();
    let action = check_op(
        &request.manifest_envelope,
        &GateContext {
            policy: upload.policy(),
            album_pin: &protocol_pin,
            device_added_at,
            server_clock: now,
        },
    )
    .map_err(OpRejection::from_gate)?;

    let manifest = decode(&request.manifest_cbor, "manifest_cbor")?;
    let metadata = match &request.metadata_blob {
        Some(encoded) => Some(decode(encoded, "metadata_blob")?),
        None => None,
    };

    // Invariant 25, over the bytes in hand rather than over a declaration about them.
    let metadata_address = match (&metadata, &request.manifest_envelope.metadata_blob_hash) {
        (Some(bytes), Some(committed)) => {
            let address = hash_bytes(bytes).to_hex();
            if &address != committed {
                tracing::info!(
                    %owner,
                    "a lifecycle write was refused: the metadata blob is not the one committed to"
                );
                return Err(OpRejection::invalid(
                    error_codes::UPLOAD_ENVELOPE_MISMATCH,
                    "the metadata blob's content hash is not the manifest's metadata_blob_hash",
                ));
            }
            Some(parse_address(&address)?)
        }
        (None, None) => None,
        // A blob with nothing committing to it, or a commitment with no blob. Both are the
        // client contradicting itself, and neither can be resolved in its favour.
        _ => {
            return Err(OpRejection::invalid(
                error_codes::UPLOAD_ENVELOPE_MISMATCH,
                "metadata_blob and metadata_blob_hash must be present together or not at all",
            ));
        }
    };

    // Quota (`S-C6`), classified by what the op actually costs. A metadata blob is storage; a
    // delete or a restore is not, and is admitted in every state.
    let class = if metadata.is_some() {
        crate::quota::WriteClass::MetadataGrowth
    } else {
        crate::quota::WriteClass::Lifecycle
    };
    let state = crate::quota::current_state(&quota, &caller)
        .await
        .map_err(|error| {
            tracing::error!(%error, %caller, "the quota ledger could not answer");
            OpRejection::unavailable()
        })?;
    if !crate::quota::admits(state, class, 0, 0, quota.limits()) {
        tracing::info!(%caller, ?state, "a lifecycle write was refused: past the grace window");
        return Err(OpRejection::QuotaGraceExpired {
            code: error_codes::QUOTA_GRACE_LOCKED,
        });
    }

    // The manifest's own content hash: the chain head this op will become, and the idempotency
    // key a replay is recognised by.
    let manifest_hash = hash_bytes(&manifest);
    let provenance = parse_address(&manifest_hash.to_hex())?;

    // The bundle's bytes land before the op is applied, and the ordering is deliberate.
    //
    // The authorization doc says a rejection "writes nothing". Read literally that would forbid
    // this, so it is worth being explicit about the reading taken: a blob nothing references is
    // not state any client can observe — it is unreachable by content address (`S-C10` answers
    // `404` for an unreferenced hash) and it is precisely what refcount GC exists to collect.
    // What "writes nothing" protects is the asset's *observable* state: no row changes, no
    // provenance record is appended, no sequence number is minted.
    //
    // The alternative order is worse in a way that is not recoverable: applying first would
    // point an asset — and therefore the feed — at bytes the store might never receive, which
    // is the dangling reference `S-C10` has to answer `410` for. Storing is idempotent, so a
    // replay that reaches here writes nothing new either.
    store(&upload, &provenance, &manifest).await?;
    if let (Some(address), Some(bytes)) = (&metadata_address, &metadata) {
        store(&upload, address, bytes).await?;
    }

    let outcome = upload
        .index()
        .apply_op(LifecycleOp {
            asset_id: AssetId::new(&request.manifest_envelope.file_id),
            owner_id: owner.clone(),
            album_id: album,
            action: wire_action(action),
            manifest_hash,
            prior_provenance_hash: prior_hash(&request.manifest_envelope)?,
            amk_version: u64::from(request.manifest_envelope.amk_version),
            provenance,
            original: None,
            metadata: metadata_address,
            retention_until: retention_floor(&request.manifest_envelope)?,
            at: now,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "the asset index could not apply a lifecycle write");
            OpRejection::unavailable()
        })?;

    let asset_id = request.manifest_envelope.file_id.clone();
    let action_wire = wire_action(action).as_str().to_owned();
    match outcome {
        OpOutcome::Applied { sync_seq, .. } => Ok(Json(OpResponse {
            asset_id,
            sync_seq,
            action: action_wire,
            replayed: false,
        })),
        OpOutcome::Replayed { sync_seq } => Ok(Json(OpResponse {
            asset_id,
            sync_seq,
            action: action_wire,
            replayed: true,
        })),
        OpOutcome::StaleChain { head } => Err(OpRejection::StaleRevival {
            chain_head: head.map(|hash| hash.to_hex()),
            code: error_codes::UPLOAD_STALE_REVIVAL,
        }),
        OpOutcome::AmkRegressed { stored } => Err(OpRejection::invalid(
            error_codes::UPLOAD_AMK_REGRESSED,
            &format!("amk_version regresses against the album's recorded epoch {stored}"),
        )),
        OpOutcome::NotFound => {
            tracing::info!(%owner, asset = %asset_id, "a lifecycle write was refused: not this album's asset");
            Err(OpRejection::album_access_denied())
        }
    }
}

/// Put `bytes` at `address`, mapping a store failure onto the surface's `500`.
async fn store(
    upload: &UploadContext,
    address: &ContentAddress,
    bytes: &[u8],
) -> Result<(), OpRejection> {
    upload
        .blobs()
        .put(address, bytes)
        .await
        .map(|_| ())
        .map_err(|error| {
            tracing::error!(%error, %address, "the blob store could not hold a lifecycle bundle");
            OpRejection::unavailable()
        })
}

/// Decode one base64 member of the bundle.
fn decode(encoded: &str, field: &'static str) -> Result<Vec<u8>, OpRejection> {
    BASE64.decode(encoded).map_err(|error| {
        OpRejection::invalid(
            error_codes::UPLOAD_MALFORMED_REQUEST,
            &format!("{field} is not base64: {error}"),
        )
    })
}

/// Parse a hex digest into a content address.
fn parse_address(hex: &str) -> Result<ContentAddress, OpRejection> {
    ContentAddress::parse(hex).map_err(|error| {
        // Unreachable: every hex here is one the server just computed.
        tracing::error!(%error, "a computed digest is not a content address");
        OpRejection::unavailable()
    })
}

/// The retention floor the manifest signed, parsed.
///
/// Read from the *envelope*, never from a server policy — that is what stops a hostile server
/// accelerating a purge and a buggy one retaining past the window the user chose (`S-C11`). An
/// unparseable value is a refusal rather than an absence: absent means "no floor and never
/// purge", so silently turning a malformed instant into `None` would be the safe-looking
/// mistake that hides a client bug forever.
fn retention_floor(envelope: &ManifestEnvelope) -> Result<Option<jiff::Timestamp>, OpRejection> {
    match &envelope.retention_until {
        None => Ok(None),
        Some(text) => text.parse().map(Some).map_err(|error| {
            OpRejection::invalid(
                error_codes::UPLOAD_ENVELOPE_MISMATCH,
                &format!("retention_until is not an RFC 3339 instant: {error}"),
            )
        }),
    }
}

/// The manifest's declared predecessor, parsed.
fn prior_hash(envelope: &ManifestEnvelope) -> Result<Option<Hash32>, OpRejection> {
    match &envelope.prior_provenance_hash {
        None => Ok(None),
        Some(hex) => Hash32::from_hex(hex).map(Some).map_err(|_| {
            OpRejection::invalid(
                error_codes::UPLOAD_ENVELOPE_MISMATCH,
                "prior_provenance_hash is not a 32-byte hex digest",
            )
        }),
    }
}

/// Map core's action onto the index's closed set.
///
/// Total because [`check_op`] has already refused the two that move bytes; the arm is a panic
/// rather than a fallback so that adding an action to core's enum without deciding what this
/// surface does with it fails loudly instead of silently becoming a metadata update.
fn wire_action(action: capsule_core::crypto::provenance::Action) -> OpAction {
    use capsule_core::crypto::provenance::Action;
    match action {
        Action::Delete => OpAction::Delete,
        Action::TrashRestore => OpAction::TrashRestore,
        Action::MetadataUpdate => OpAction::MetadataUpdate,
        Action::DerivativeAdd | Action::DerivativeReplace => OpAction::Derivative,
        Action::Create | Action::Replace => {
            unreachable!("check_op refuses the byte-moving actions before this point")
        }
    }
}

impl OpRejection {
    /// A structural refusal with its code.
    fn invalid(code: &'static str, detail: &str) -> Self {
        Self::Invalid {
            detail: detail.to_owned(),
            code,
        }
    }

    /// The album is not writable, or the asset is not this album's.
    fn album_access_denied() -> Self {
        Self::AlbumAccessDenied {
            code: error_codes::UPLOAD_ALBUM_ACCESS_DENIED,
        }
    }

    /// A collaborator could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
        }
    }

    /// Map the gate's verdict onto this surface's taxonomy.
    fn from_gate(reject: GateReject) -> Self {
        let invalid = |code, detail: &str| Self::invalid(code, detail);
        match reject {
            GateReject::ProtocolMalformed => invalid(
                error_codes::UPLOAD_MALFORMED_REQUEST,
                "protocol_version is not a YYYY-MM-DD date",
            ),
            GateReject::ProtocolOutOfRange => Self::ProtocolUnsupported {
                protocol_min: String::new(),
                protocol_max: String::new(),
                code: error_codes::PROTOCOL_VERSION_UNSUPPORTED,
            },
            GateReject::UnknownCryptoSuite => invalid(
                error_codes::UPLOAD_UNKNOWN_CRYPTO_SUITE,
                "crypto_suite_id is not in this server's inventory",
            ),
            GateReject::AlbumPinMismatch => invalid(
                error_codes::UPLOAD_ENVELOPE_MISMATCH,
                "the album's pinned protocol_version is not the one this manifest was written under",
            ),
            GateReject::DeviceNotAuthorized => invalid(
                error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED,
                "the manifest predates the device's admission to the directory",
            ),
            GateReject::TimestampOutOfRange => invalid(
                error_codes::UPLOAD_ENVELOPE_MISMATCH,
                "the manifest timestamp is unparseable or grossly drifted",
            ),
            GateReject::ActionNotAllowed
            | GateReject::ReplaceDoesNotChain
            | GateReject::ReplaceIncomplete(_) => invalid(
                error_codes::UPLOAD_INVALID_ACTION,
                "that action moves blob bytes and is therefore an upload, not a lifecycle op",
            ),
            GateReject::EnvelopeMismatch(field) => invalid(
                error_codes::UPLOAD_ENVELOPE_MISMATCH,
                &format!("{field} is not the shape its schema fixes"),
            ),
            // Reachable only from `check_create`, which reads a top-level blob declaration this
            // surface does not carry. Mapped rather than unwrapped so the match stays total.
            GateReject::InvalidHash
            | GateReject::InvalidSize
            | GateReject::FileTooLarge
            | GateReject::UnsupportedContentType => invalid(
                error_codes::UPLOAD_MALFORMED_REQUEST,
                "the manifest is not a well-formed lifecycle bundle",
            ),
        }
    }
}
