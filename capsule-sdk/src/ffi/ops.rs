//! Lifecycle ops from an app: one signed `delete` or `trash-restore` record, as the opaque
//! canonical CBOR the workspace hands out, sent to its album's ops surface
//! (`POST /v1/albums/{album_id}/ops`) through
//! [`AlbumClient::apply_op`](crate::albums::AlbumClient::apply_op).
//!
//! # What crosses the boundary
//!
//! The record bytes come from foreign code, so they are judged before anything is sent
//! ([`decode_op_record`](crate::ffi::ops::decode_op_record)): bounded in size, decoded as a
//! [`ProvenanceRecord`](capsule_core::crypto::provenance::ProvenanceRecord), and refused
//! unless they are that record's own canonical encoding, so the bytes the server hashes as the
//! chain head are exactly the bytes the device signed. Only the actions that move no bytes and
//! bind no metadata blob are accepted: a `delete` or a `trash-restore`.
//!
//! The server cannot verify a record's signatures, so a mis-signed record it accepted would
//! become the asset's chain head and fail every reader's `verify_asset`. The bytes are therefore
//! also judged against the workspace ([`judge_own_record`](crate::ffi::ops::judge_own_record)):
//! the record's device signature verifies under the workspace's device directory, and it is a
//! record of the chain the workspace holds for its asset. A delivery verifies every record of
//! the stretch it sends the same way.
//!
//! # A delete is held until its create is indexed
//!
//! The server never sees a `delete` for an asset it has not indexed (Organization — Deletion
//! State Machine). The evidence is the caller's sync feed: every verb here takes the head the
//! feed lists for the asset (`server_head`, the latest entry's `manifest_cbor`), and `None`, an
//! asset the feed does not list, sends nothing and answers
//! [`FfiLifecycleOpOutcome::NotIndexed`]. Nothing is lost by holding: the record stays on the
//! asset's chain in the workspace (the kept chain, after a purge), and a later delivery reads it
//! from there again, also after a reopen.
//!
//! # A purge's re-issue goes with the stretch it chains onto
//!
//! An early purge appends a shorter-retention `delete` that chains onto the asset's previous
//! head, usually the `delete` that moved it to the trash. When that delete never reached the
//! server, the re-issue alone would be refused as a stale chain, so
//! [`FfiSession::deliver_purge`] sends every record of the kept chain after the head the server
//! holds, in chain order, each once the one before it was applied.
//!
//! # A live or trashed asset's held records go the same way
//!
//! A `delete` or `trash-restore` of an asset the workspace still holds is signed onto its chain
//! and held there until the feed lists the asset. [`FfiSession::deliver_lifecycle_at_hash`]
//! sends the stretch after the head the server holds, by the same rule as a purge and as the
//! CLI ([`plan_delivery`]): held while the asset is not indexed, and never a `trash-restore`
//! that undoes a held `delete` whose window has closed, which answers
//! [`FfiLifecycleOpOutcome::RestoreTooLate`].

use std::sync::Arc;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::provenance::{Action, ProvenanceRecord};
use capsule_core::crypto::verify_asset::verify_device_signature;
use capsule_library::lifecycle::Workspace;
use uuid::Uuid;

use super::workspace::{parse_uuid, trash_error};
use super::{FfiError, FfiSession, FfiWorkspace};
use crate::albums::{
    AlbumClient, AlbumTransport, AppliedOp, HeldDelivery, HoldReason, LifecycleOpError,
    judging_clock, op_envelope, plan_delivery,
};

/// The largest record [`FfiSession::apply_lifecycle_op`] decodes. A lifecycle record is one
/// signed manifest with its chain position, a few kilobytes even under hybrid signatures; this
/// leaves an order of magnitude of room and refuses anything that could only be an attack on
/// the decoder.
pub const MAX_OP_RECORD_BYTES: usize = 64 * 1024;

/// What became of one lifecycle op.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiLifecycleOpOutcome {
    /// The server applied the op, or had applied it already (`replayed`).
    Applied {
        /// The asset the op chained onto (UUID string).
        asset_id: String,
        /// The action the server applied (`delete`, `trash-restore`).
        action: String,
        /// The feed position the op occupies; on a replay, the one the first application took.
        sync_seq: u64,
        /// Whether the server had applied this record already: a resend whose answer was lost.
        replayed: bool,
    },
    /// Nothing was sent: the feed does not list the asset, so its create is not indexed on the
    /// server, and the server never sees a `delete` for an asset it has not indexed. The record
    /// stays on the asset's chain; deliver it once a sync lists the asset.
    NotIndexed,
    /// Nothing was sent: a `trash-restore` in the stretch undoes a `delete` the server has not
    /// seen, and that delete's window closed at `retention_until`. Sent, the server would apply
    /// the delete, refuse the restore and purge an asset the user restored in time. Withheld
    /// for good: the server keeps the asset live at the head it holds, the workspace keeps
    /// every record, and nothing after the pair can be sent either, since the chain is
    /// hash-linked. A later delivery judges it the same way, so it never needs retrying.
    RestoreTooLate {
        /// The withheld `delete`'s signed window (RFC 3339), which had closed.
        retention_until: String,
    },
    /// The server refused the op, or could not be reached. `permanent` says whether sending
    /// the same bytes again can only be refused again.
    Failed {
        /// A stable name for why, as the CLI's `--json` reports it: `retention_elapsed`,
        /// `stale_revival`, `refused`, `forbidden`, `upgrade_required`, `unavailable` or
        /// `malformed`.
        kind: String,
        /// The server's stable `error.*` code, when it gave one.
        code: Option<String>,
        /// Whether a resend can only be refused again: the server judged the op itself (a stale
        /// chain, a closed retention window, a malformed op). Never retry a permanent failure.
        permanent: bool,
        /// On a stale chain, the head the server holds (hex SHA-256).
        chain_head: Option<String>,
        /// On a closed retention window, the signed floor the server judged (RFC 3339).
        retention_until: Option<String>,
        /// English detail (developer/log message).
        detail: String,
    },
}

impl FfiLifecycleOpOutcome {
    fn applied(applied: AppliedOp) -> Self {
        Self::Applied {
            asset_id: applied.asset_id,
            action: applied.action,
            sync_seq: applied.sync_seq,
            replayed: applied.replayed,
        }
    }
}

/// The outcome of a lifecycle op the server did not apply, or the [`FfiError::Auth`] a session
/// it refused is, so the app signs in again rather than retrying.
fn failed(error: LifecycleOpError) -> Result<FfiLifecycleOpOutcome, FfiError> {
    let (chain_head, retention_until) = match &error {
        LifecycleOpError::Refused {
            status: 401, code, ..
        } => {
            return Err(FfiError::Auth {
                code: Some(code.clone()),
                detail: error.to_string(),
            });
        }
        LifecycleOpError::Refused {
            chain_head,
            retention_until,
            ..
        } => (chain_head.clone(), retention_until.clone()),
        _ => (None, None),
    };
    Ok(FfiLifecycleOpOutcome::Failed {
        kind: error.kind().to_owned(),
        code: error.error_code().map(str::to_owned),
        permanent: error.is_permanent(),
        chain_head,
        retention_until,
        detail: error.to_string(),
    })
}

/// Judge foreign `bytes` as a lifecycle op record: at most [`MAX_OP_RECORD_BYTES`], a
/// [`ProvenanceRecord`] whose canonical encoding they are exactly, of a `delete` or a
/// `trash-restore` that binds no metadata blob.
///
/// # Errors
///
/// [`FfiError::InvalidArgument`] naming the first rule the bytes break.
pub(super) fn decode_op_record(bytes: &[u8]) -> Result<ProvenanceRecord, FfiError> {
    let invalid = |detail: String| {
        tracing::warn!(len = bytes.len(), %detail, "ffi: a lifecycle op record was refused");
        FfiError::InvalidArgument { detail }
    };
    if bytes.len() > MAX_OP_RECORD_BYTES {
        return Err(invalid(format!(
            "record: {} bytes is over the {MAX_OP_RECORD_BYTES}-byte bound",
            bytes.len()
        )));
    }
    let record: ProvenanceRecord = capsule_core::cbor::from_slice(bytes)
        .map_err(|e| invalid(format!("record: not a provenance record ({e})")))?;
    let canonical = capsule_core::cbor::to_canonical_vec(&record)
        .map_err(|e| invalid(format!("record: does not re-encode ({e})")))?;
    if canonical != bytes {
        return Err(invalid(
            "record: not in canonical CBOR, so its hash is not the one its chain names".to_owned(),
        ));
    }
    let core = &record.manifest.core;
    if !matches!(core.action, Action::Delete | Action::TrashRestore) {
        return Err(invalid(format!(
            "record: a {:?} is not a lifecycle op this verb sends; only a delete or a trash-restore",
            core.action
        )));
    }
    if core.metadata_blob_hash.is_some() {
        return Err(invalid(
            "record: binds a metadata blob, which a delete or a trash-restore never does"
                .to_owned(),
        ));
    }
    Ok(record)
}

/// Judge `record` against `ws`: its device signature verifies under the workspace's device
/// directory ([`verify_device_signature`]), and it is a record of the chain the workspace holds
/// for its asset, live, trashed or kept by a purge. The server cannot verify signatures, so
/// this is the last place a mis-signed or foreign record is stopped before it would become the
/// asset's chain head on the server.
///
/// # Errors
///
/// The first rule the record breaks, as an English detail.
pub(super) fn judge_own_record(ws: &Workspace, record: &ProvenanceRecord) -> Result<(), String> {
    let core = &record.manifest.core;
    verify_device_signature(&record.manifest, ws.device_directory()).map_err(|reason| {
        format!(
            "record: its device signature does not verify under this workspace's device \
             directory ({reason:?})"
        )
    })?;
    let hash = record.record_hash();
    let own = ws
        .asset(&core.file_id)
        .map(|asset| asset.chain.records())
        .into_iter()
        .chain(ws.purged_chain(&core.file_id))
        .flatten()
        .any(|held| held.record_hash() == hash);
    if !own {
        return Err(format!(
            "record: not a record of this workspace's chain for asset {}",
            core.file_id
        ));
    }
    Ok(())
}

/// The generated request for `record`, whose canonical bytes are `canonical`: the server-visible
/// projection of its manifest, every field verbatim, naming the original's ciphertext and no
/// derivative, and no metadata blob.
fn op_request(record: &ProvenanceRecord, canonical: &[u8]) -> crate::rest::types::OpRequest {
    use base64::Engine as _;
    crate::rest::types::OpRequest {
        manifest_envelope: op_envelope(&record.manifest.core),
        manifest_cbor: base64::engine::general_purpose::STANDARD.encode(canonical),
        metadata_blob: None,
    }
}

/// The hash of the head the feed lists for an asset: `None` for an entry that carries no
/// record, which names no position in any chain.
fn listed_head(server_head: &[u8]) -> Option<Hash32> {
    (!server_head.is_empty()).then(|| hash_bytes(server_head))
}

/// The head hash a caller kept for a listed asset: empty for an entry that carries no record,
/// otherwise the 32-byte record hash.
fn listed_hash(server_head_hash: &[u8]) -> Result<Option<Hash32>, FfiError> {
    if server_head_hash.is_empty() {
        return Ok(None);
    }
    <[u8; 32]>::try_from(server_head_hash)
        .map(|bytes| Some(Hash32::from_bytes(bytes)))
        .map_err(|_| FfiError::InvalidArgument {
            detail: format!(
                "server_head_hash: {} bytes, not an empty head or a 32-byte record hash",
                server_head_hash.len()
            ),
        })
}

#[uniffi::export]
impl FfiWorkspace {
    /// Every asset this workspace has purged (UUID strings), whose kept chain
    /// [`purged_record`](Self::purged_record) reads: the list a relaunch walks to deliver the
    /// purges whose delivery failed or was held, with [`FfiSession::deliver_purge`]. A purge
    /// the server already applied answers as a replay, so walking it again is safe.
    pub fn purged_asset_ids(&self) -> Result<Vec<String>, FfiError> {
        self.with(|ws| Ok(ws.tombstones().iter().map(ToString::to_string).collect()))
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl FfiSession {
    /// Apply one signed lifecycle op `record` — a `delete` or a `trash-restore`, as the opaque
    /// canonical CBOR [`FfiWorkspace::provenance_head`],
    /// [`FfiPurgeOutcome::reissue_record`](super::FfiPurgeOutcome::reissue_record) and
    /// [`FfiWorkspace::purged_record`] carry — to its album's ops surface on `api_base_url`
    /// (`http[s]://host:port`, the root `sync_pull` takes).
    ///
    /// The record must be one `workspace` holds: its device signature verifies under the
    /// workspace's device directory, and it is a record of the workspace's chain for its asset
    /// (live, trashed, or kept by a purge). The server cannot verify signatures, so a record
    /// that is not this workspace's own is refused here rather than becoming the asset's chain
    /// head there.
    ///
    /// `server_head` is the sync feed's evidence for the asset: the `manifest_cbor` of the
    /// latest entry listing it (empty for an entry that carries none), or `None` when the feed
    /// does not list it. With `None` nothing is sent and the answer is
    /// [`FfiLifecycleOpOutcome::NotIndexed`]: the server never sees a lifecycle op for an asset
    /// it has not indexed.
    ///
    /// Idempotent: a record the server already applied answers `Applied` with `replayed` set.
    /// A refusal, or a server that could not be reached, is [`FfiLifecycleOpOutcome::Failed`];
    /// a session the server did not accept is [`FfiError::Auth`].
    ///
    /// # Errors
    ///
    /// [`FfiError::InvalidArgument`] for bytes that are not a lifecycle op record (over the
    /// bound, not canonical, not a `delete` or `trash-restore`, not signed by a device of the
    /// workspace's directory, or not on the workspace's chain for its asset) or an
    /// `api_base_url` that is not an http(s) URL, before anything is sent;
    /// [`FfiError::Auth`] for a refused session.
    pub async fn apply_lifecycle_op(
        &self,
        api_base_url: String,
        workspace: Arc<FfiWorkspace>,
        record: Vec<u8>,
        server_head: Option<Vec<u8>>,
    ) -> Result<FfiLifecycleOpOutcome, FfiError> {
        let decoded = decode_op_record(&record)?;
        workspace.with(|ws| {
            judge_own_record(ws, &decoded).map_err(|detail| {
                tracing::warn!(asset_id = %decoded.manifest.core.file_id, %detail, "ffi: a lifecycle op record was refused");
                FfiError::InvalidArgument { detail }
            })
        })?;
        let client = self.album_client(&api_base_url)?;
        let core = &decoded.manifest.core;
        tracing::info!(
            asset_id = %core.file_id,
            action = ?core.action,
            indexed = server_head.is_some(),
            "ffi: applying a lifecycle op"
        );
        if server_head.is_none() {
            tracing::info!(asset_id = %core.file_id, "ffi: the feed does not list the asset; the op is held");
            return Ok(FfiLifecycleOpOutcome::NotIndexed);
        }
        send(&client, &decoded, &record).await
    }

    /// Deliver a purged asset's `delete` to its album's ops surface: every record of the chain
    /// the purge kept after the head the server holds, in chain order, each once the one
    /// before it was applied. After an early purge that is the shorter-retention re-issue
    /// ([`FfiPurgeOutcome::reissue_record`](super::FfiPurgeOutcome::reissue_record)) together
    /// with any `delete` it chains onto that the server never saw.
    ///
    /// Everything is read from the workspace's kept chain, so a delivery that failed or was
    /// held is retried by calling this again, also after the workspace was reopened
    /// ([`FfiWorkspace::purged_asset_ids`] lists what to walk). The workspace lock is held only
    /// while the chain is read, never across the network. Every record of the stretch is
    /// judged as [`apply_lifecycle_op`](Self::apply_lifecycle_op) judges a foreign one before
    /// any is sent, so a kept chain whose records no longer verify sends nothing.
    ///
    /// `server_head` is as [`apply_lifecycle_op`](Self::apply_lifecycle_op) takes it: `None`
    /// sends nothing and answers [`FfiLifecycleOpOutcome::NotIndexed`]. The answer is the last
    /// record's `Applied` — the purge reached the server — or the first record that was not
    /// applied, whose `Failed` stops the stretch.
    ///
    /// The held rules are a live asset's
    /// ([`deliver_lifecycle_at_hash`](Self::deliver_lifecycle_at_hash)), judged by this
    /// device's clock: a kept chain whose stretch holds a `trash-restore` undoing a `delete` the server never saw, whose window has
    /// closed, is withheld and answers [`FfiLifecycleOpOutcome::RestoreTooLate`].
    ///
    /// # Errors
    ///
    /// [`FfiError::Trash`] with [`FfiTrashRefusal::NotFound`](super::FfiTrashRefusal::NotFound)
    /// for an asset this workspace has not purged; [`FfiError::InvalidArgument`] for a malformed
    /// `asset_id` or `api_base_url`; [`FfiError::Workspace`] for a kept chain whose records do
    /// not verify under the workspace's device directory; [`FfiError::Auth`] for a refused
    /// session.
    pub async fn deliver_purge(
        &self,
        api_base_url: String,
        workspace: Arc<FfiWorkspace>,
        asset_id: String,
        server_head: Option<Vec<u8>>,
    ) -> Result<FfiLifecycleOpOutcome, FfiError> {
        let head = server_head.as_deref().map(listed_head);
        self.deliver_chain(
            &api_base_url,
            &workspace,
            &asset_id,
            head,
            ChainSource::Kept,
            jiff::Timestamp::now(),
        )
        .await
    }

    /// [`deliver_purge`](Self::deliver_purge) with the head the feed lists given by its record
    /// hash rather than its bytes: the SHA-256 of the latest entry's `manifest_cbor`, which is
    /// the head record's hash, so a caller keeps 32 bytes per listed asset instead of the
    /// record. `Some` of an empty vector is an entry that carries no record, and `None` an asset
    /// the feed does not list, exactly as `deliver_purge` reads its `server_head`.
    ///
    /// # Errors
    ///
    /// As [`deliver_purge`](Self::deliver_purge), and [`FfiError::InvalidArgument`] for a
    /// `server_head_hash` that is neither empty nor 32 bytes, before anything is sent.
    pub async fn deliver_purge_at_hash(
        &self,
        api_base_url: String,
        workspace: Arc<FfiWorkspace>,
        asset_id: String,
        server_head_hash: Option<Vec<u8>>,
    ) -> Result<FfiLifecycleOpOutcome, FfiError> {
        let head = server_head_hash.as_deref().map(listed_hash).transpose()?;
        self.deliver_chain(
            &api_base_url,
            &workspace,
            &asset_id,
            head,
            ChainSource::Kept,
            jiff::Timestamp::now(),
        )
        .await
    }

    /// Deliver the held lifecycle records of an asset `workspace` still holds, live or in the
    /// trash: every record of its chain after the head the server holds, in chain order, each
    /// once the one before it was applied. That is a `delete` or `trash-restore` signed while
    /// the server had not indexed the asset, or whose send failed, together with any it chains
    /// onto that the server never saw. The rule is the CLI's and a purge's
    /// ([`plan_delivery`]).
    ///
    /// `server_head_hash` is the feed's evidence for the asset, as
    /// [`deliver_purge_at_hash`](Self::deliver_purge_at_hash) takes it: `None`, an asset the
    /// feed does not list, sends nothing and answers [`FfiLifecycleOpOutcome::NotIndexed`].
    ///
    /// `latest_changed_at` is the latest `changed_at` (RFC 3339) the caller's feed drain saw,
    /// on the server's clock, or `None` when it saw none. It floors this device's clock
    /// ([`judging_clock`]), and a stretch in which a `trash-restore` undoes a `delete` whose
    /// window had closed by that clock is withheld and answers
    /// [`FfiLifecycleOpOutcome::RestoreTooLate`]: sent, the server would apply the delete and
    /// refuse the restore.
    ///
    /// Everything is read from the workspace's chain, so a delivery that failed or was held is
    /// retried by calling this again, also after the workspace was reopened. The workspace lock
    /// is held only while the chain is read, never across the network, and every record of
    /// the stretch is judged as [`apply_lifecycle_op`](Self::apply_lifecycle_op) judges a
    /// foreign one before any is sent. A server level with the chain is sent its head alone,
    /// which it answers as a replay; a caller that compares the listed head with
    /// [`FfiWorkspace::provenance_head`] first sends nothing for it. The answer is the last
    /// record's `Applied`, or the first record that was not applied, whose `Failed` stops the
    /// stretch.
    ///
    /// # Errors
    ///
    /// [`FfiError::Trash`] with [`FfiTrashRefusal::NotFound`](super::FfiTrashRefusal::NotFound)
    /// for an asset this workspace does not hold (a purged one goes through
    /// [`deliver_purge_at_hash`](Self::deliver_purge_at_hash)); [`FfiError::InvalidArgument`]
    /// for a malformed `asset_id` or `api_base_url`, a `server_head_hash` that is neither empty
    /// nor 32 bytes, or a `latest_changed_at` that is not an RFC 3339 timestamp, before
    /// anything is sent; [`FfiError::Workspace`] for a chain whose records do not verify under
    /// the workspace's device directory; [`FfiError::Auth`] for a refused session.
    pub async fn deliver_lifecycle_at_hash(
        &self,
        api_base_url: String,
        workspace: Arc<FfiWorkspace>,
        asset_id: String,
        server_head_hash: Option<Vec<u8>>,
        latest_changed_at: Option<String>,
    ) -> Result<FfiLifecycleOpOutcome, FfiError> {
        let head = server_head_hash.as_deref().map(listed_hash).transpose()?;
        let seen = latest_changed_at
            .as_deref()
            .map(|at| {
                at.parse::<jiff::Timestamp>()
                    .map_err(|e| FfiError::InvalidArgument {
                        detail: format!(
                            "latest_changed_at: {at:?} is not an RFC 3339 timestamp ({e})"
                        ),
                    })
            })
            .transpose()?;
        let judge = judging_clock(jiff::Timestamp::now(), seen);
        self.deliver_chain(
            &api_base_url,
            &workspace,
            &asset_id,
            head,
            ChainSource::Held,
            judge,
        )
        .await
    }
}

/// Which of an asset's chains a delivery reads from the workspace.
#[derive(Debug, Clone, Copy)]
enum ChainSource {
    /// The chain a purge kept ([`Workspace::purged_chain`]).
    Kept,
    /// The chain of an asset the workspace still holds, live or in the trash.
    Held,
}

impl ChainSource {
    /// `asset`'s chain of this kind in `ws`, or the `NotFound` refusal for an asset with none.
    fn read<'a>(self, ws: &'a Workspace, asset: &Uuid) -> Result<&'a [ProvenanceRecord], FfiError> {
        let (chain, what) = match self {
            Self::Kept => (ws.purged_chain(asset), "purged asset"),
            Self::Held => (ws.asset(asset).map(|held| held.chain.records()), "asset"),
        };
        chain.ok_or_else(|| {
            trash_error(capsule_library::lifecycle::LifecycleError::NotFound(
                format!("{what} {asset}"),
            ))
        })
    }

    /// What a log line or detail calls the chain.
    fn name(self) -> &'static str {
        match self {
            Self::Kept => "the kept chain",
            Self::Held => "the chain",
        }
    }
}

impl FfiSession {
    /// The body of every delivery verb, given what the feed lists for the asset (`None` when it
    /// does not list it, otherwise the hash of the head it holds, `None` for an entry that
    /// carries no record), the chain to read, and the clock the held rules judge by.
    async fn deliver_chain(
        &self,
        api_base_url: &str,
        workspace: &FfiWorkspace,
        asset_id: &str,
        head: Option<Option<Hash32>>,
        source: ChainSource,
        judge: jiff::Timestamp,
    ) -> Result<FfiLifecycleOpOutcome, FfiError> {
        let asset = parse_uuid("asset_id", asset_id)?;
        let client = self.album_client(api_base_url)?;
        let chain_name = source.name();
        let planned: Result<Vec<(ProvenanceRecord, Vec<u8>)>, FfiLifecycleOpOutcome> =
            workspace.with(|ws| {
                let chain = source.read(ws, &asset)?;
                let stretch = match plan_delivery(chain, head, judge) {
                    HeldDelivery::Held {
                        reason: HoldReason::NotIndexed,
                        ..
                    } => return Ok(Err(FfiLifecycleOpOutcome::NotIndexed)),
                    HeldDelivery::Held {
                        reason: HoldReason::RestoreTooLate,
                        retention_until,
                    } => {
                        return Ok(Err(FfiLifecycleOpOutcome::RestoreTooLate {
                            retention_until: retention_until.unwrap_or_default(),
                        }));
                    }
                    HeldDelivery::Send(stretch) => stretch,
                };
                stretch
                    .iter()
                    .map(|record| {
                        judge_own_record(ws, record).map_err(|detail| {
                            tracing::error!(%asset, %detail, chain = chain_name, "ffi: a chain record does not verify; nothing is sent");
                            FfiError::Workspace {
                                detail: format!("{chain_name} of {asset}: {detail}"),
                            }
                        })?;
                        let bytes = capsule_core::cbor::to_canonical_vec(record).map_err(|e| {
                            FfiError::Workspace {
                                detail: format!("encoding {chain_name} of {asset}: {e}"),
                            }
                        })?;
                        Ok((record.clone(), bytes))
                    })
                    .collect::<Result<_, FfiError>>()
                    .map(Ok)
            })?;
        let stretch = match planned {
            Ok(stretch) => stretch,
            Err(held) => {
                tracing::info!(%asset, chain = chain_name, outcome = ?held, %judge, "ffi: nothing of the asset's chain is sent; its records are held");
                return Ok(held);
            }
        };
        tracing::info!(%asset, chain = chain_name, records = stretch.len(), "ffi: delivering an asset's records to the album's ops surface");
        let mut outcome = FfiLifecycleOpOutcome::NotIndexed;
        for (record, bytes) in &stretch {
            outcome = send(&client, record, bytes).await?;
            if !matches!(outcome, FfiLifecycleOpOutcome::Applied { .. }) {
                tracing::warn!(%asset, chain = chain_name, ?outcome, "ffi: the delivery stopped; the workspace still holds the records");
                return Ok(outcome);
            }
        }
        tracing::info!(%asset, chain = chain_name, "ffi: the asset's records reached the server");
        Ok(outcome)
    }
}

impl FfiSession {
    /// An album client on `api_base_url`'s album surface, sending through the current path.
    fn album_client(&self, api_base_url: &str) -> Result<AlbumClient, FfiError> {
        let url = reqwest::Url::parse(api_base_url).map_err(|e| FfiError::InvalidArgument {
            detail: format!("api_base_url: {api_base_url:?} is not a URL ({e})"),
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(FfiError::InvalidArgument {
                detail: format!("api_base_url: {api_base_url:?} is not an http(s) URL"),
            });
        }
        let endpoint = format!("{}/v1/albums", api_base_url.trim_end_matches('/'));
        Ok(AlbumClient::new(AlbumTransport::with_session(
            self.path_session(),
            endpoint,
        )))
    }
}

/// Send one judged `record` (canonical bytes `bytes`) and say what became of it.
async fn send(
    client: &AlbumClient,
    record: &ProvenanceRecord,
    bytes: &[u8],
) -> Result<FfiLifecycleOpOutcome, FfiError> {
    match client.apply_op(&op_request(record, bytes)).await {
        Ok(applied) => Ok(FfiLifecycleOpOutcome::applied(applied)),
        Err(error) => failed(error),
    }
}

#[cfg(test)]
mod tests;
