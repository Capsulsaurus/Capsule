//! Lifecycle ops from an app: one signed `delete` or `trash-restore` record, as the opaque
//! canonical CBOR the workspace hands out, sent to its album's ops surface
//! (`POST /v1/albums/{album_id}/ops`) through [`AlbumClient::apply_op`].
//!
//! # What crosses the boundary
//!
//! The record bytes come from foreign code, so they are judged before anything is sent
//! ([`decode_op_record`]): bounded in size, decoded as a [`ProvenanceRecord`], and refused
//! unless they are that record's own canonical encoding, so the bytes the server hashes as the
//! chain head are exactly the bytes the device signed. Only the actions that move no bytes and
//! bind no metadata blob are accepted: a `delete` or a `trash-restore`.
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

use std::sync::Arc;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::provenance::{Action, ManifestCore, ProvenanceRecord};

use super::workspace::{parse_uuid, trash_error};
use super::{FfiError, FfiSession, FfiWorkspace};
use crate::albums::{AlbumClient, AlbumTransport, AppliedOp, LifecycleOpError};

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

/// The server-visible projection of a lifecycle manifest, as the CLI's lifecycle ops send it.
fn op_envelope(core: &ManifestCore) -> crate::rest::types::ManifestEnvelope {
    let wire = |value: serde_json::Result<serde_json::Value>| {
        value
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    crate::rest::types::ManifestEnvelope {
        crypto_suite_id: i64::from(core.crypto_suite_id),
        protocol_version: core.protocol_version.clone(),
        album_id: Some(core.album_id.to_string()),
        file_id: core.file_id.to_string(),
        amk_version: i64::from(core.amk_version.0),
        ciphertext_hash: core.ciphertext_hash.to_hex(),
        plaintext_size: i64::try_from(core.plaintext_size).unwrap_or(i64::MAX),
        chunk_size: i64::from(core.chunk_size),
        key_mode: wire(serde_json::to_value(core.key_mode)),
        metadata_blob_hash: core.metadata_blob_hash.map(|h| h.to_hex()),
        original_blob_hash: None,
        derivative_role: None,
        derivative_format: None,
        created_by_user: core.created_by_user.to_string(),
        created_by_device: core.created_by_device.to_string(),
        client_version: core.client_version.clone(),
        timestamp: core.timestamp.clone(),
        action: wire(serde_json::to_value(core.action)),
        prior_provenance_hash: core.prior_provenance_hash.map(|h| h.to_hex()),
        retention_until: core.retention_until.clone(),
    }
}

/// The hash of the head the feed lists for an asset: `None` for an entry that carries no
/// record, which names no position in any chain.
fn listed_head(server_head: &[u8]) -> Option<Hash32> {
    (!server_head.is_empty()).then(|| hash_bytes(server_head))
}

/// The records of `records` (a chain, oldest first) the server lacks when it holds `head`:
/// those after it. Where `head` already is the last record, the last record alone, which the
/// server answers as a replay; where `head` is unknown or not in the chain, the last record
/// alone too, which the server judges (a stale chain is refused, never applied).
///
/// The same rule as the CLI's held delivery, so a purge sent from either client sends the same
/// stretch.
pub(super) fn stretch_after(
    records: &[ProvenanceRecord],
    head: Option<Hash32>,
) -> &[ProvenanceRecord] {
    let last = records.len().saturating_sub(1);
    let start = head
        .and_then(|head| records.iter().position(|r| r.record_hash() == head))
        .map_or(last, |at| (at + 1).min(last));
    records.get(start..).unwrap_or_default()
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
    /// [`FfiError::InvalidArgument`] for bytes [`decode_op_record`] refuses or an
    /// `api_base_url` that is not an http(s) URL, before anything is sent;
    /// [`FfiError::Auth`] for a refused session.
    pub async fn apply_lifecycle_op(
        &self,
        api_base_url: String,
        record: Vec<u8>,
        server_head: Option<Vec<u8>>,
    ) -> Result<FfiLifecycleOpOutcome, FfiError> {
        let decoded = decode_op_record(&record)?;
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
    /// while the chain is read, never across the network.
    ///
    /// `server_head` is as [`apply_lifecycle_op`](Self::apply_lifecycle_op) takes it: `None`
    /// sends nothing and answers [`FfiLifecycleOpOutcome::NotIndexed`]. The answer is the last
    /// record's `Applied` — the purge reached the server — or the first record that was not
    /// applied, whose `Failed` stops the stretch.
    ///
    /// # Errors
    ///
    /// [`FfiError::Trash`] with [`FfiTrashRefusal::NotFound`](super::FfiTrashRefusal::NotFound)
    /// for an asset this workspace has not purged; [`FfiError::InvalidArgument`] for a malformed
    /// `asset_id` or `api_base_url`; [`FfiError::Auth`] for a refused session.
    pub async fn deliver_purge(
        &self,
        api_base_url: String,
        workspace: Arc<FfiWorkspace>,
        asset_id: String,
        server_head: Option<Vec<u8>>,
    ) -> Result<FfiLifecycleOpOutcome, FfiError> {
        let asset = parse_uuid("asset_id", &asset_id)?;
        let client = self.album_client(&api_base_url)?;
        let head = server_head.as_deref().map(listed_head);
        let stretch: Vec<(ProvenanceRecord, Vec<u8>)> = workspace.with(|ws| {
            let chain = ws.purged_chain(&asset).ok_or_else(|| {
                trash_error(capsule_library::lifecycle::LifecycleError::NotFound(
                    format!("purged asset {asset}"),
                ))
            })?;
            stretch_after(chain, head.flatten())
                .iter()
                .map(|record| {
                    let bytes = capsule_core::cbor::to_canonical_vec(record).map_err(|e| {
                        FfiError::Workspace {
                            detail: format!("encoding the kept chain of {asset}: {e}"),
                        }
                    })?;
                    Ok((record.clone(), bytes))
                })
                .collect::<Result<_, FfiError>>()
        })?;
        if head.is_none() {
            tracing::info!(%asset, held = stretch.len(), "ffi: the feed does not list the purged asset; its delete is held");
            return Ok(FfiLifecycleOpOutcome::NotIndexed);
        }
        tracing::info!(%asset, records = stretch.len(), "ffi: delivering a purge to the album's ops surface");
        let mut outcome = FfiLifecycleOpOutcome::NotIndexed;
        for (record, bytes) in &stretch {
            outcome = send(&client, record, bytes).await?;
            if !matches!(outcome, FfiLifecycleOpOutcome::Applied { .. }) {
                tracing::warn!(%asset, ?outcome, "ffi: the purge's delivery stopped; the kept chain still holds it");
                return Ok(outcome);
            }
        }
        tracing::info!(%asset, "ffi: the purge reached the server");
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
