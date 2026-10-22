//! Client re-announce: restoring an index row from a signed manifest a client still holds (#781).
//!
//! # Why it exists
//!
//! [Server Filesystem § Sync Epoch and Client Re-Announce] is the contract. An index rebuild that
//! cannot replay part of an owner's ledger (a chain break, a lost tail) leaves rows missing and
//! moves the owner's sync epoch. Every client then resyncs from zero, and what it holds that the
//! feed no longer lists it **re-announces**: it sends the signed manifest, and nothing else. The
//! bytes the manifest names are usually still in the blob store, since a rebuild loses rows, not
//! blobs; when they are, the row comes back without a byte being uploaded again.
//!
//! # What a manifest has to pass
//!
//! The same write authority an upload's envelope passes, decided against durable state now:
//!
//! - it decodes through the server's one manifest parser ([`manifest::parse_valid`]);
//! - `created_by_user` is the caller, so a re-announced manifest can only restore what the
//!   caller's own devices signed;
//! - the caller may write to the manifest's album, and the manifest carries the album's pin
//!   (invariant 6);
//! - `created_by_device` is in the caller's published directory, added no later than the
//!   manifest's timestamp (invariant 7).
//!
//! The clock-drift check (invariant 8) is deliberately not run: a manifest being re-announced is
//! old by definition. Signatures are not checked, as they are not on an upload: the server is
//! key-free, and every client runs `verify_asset` on what the feed serves.
//!
//! # What is restored
//!
//! The row the chain head implies: the head as its provenance blob, the metadata blob of the
//! most recent manifest that names one, and the original every manifest names. A head that names
//! no metadata blob (a derivative or a trash-restore) is walked back through the store, one
//! `prior_provenance_hash` at a time, to the manifest that does; the manifests walked past are
//! the row's superseded chain. Every blob must be in the store, or the answer names what is
//! missing and nothing is written: the client re-uploads through the transfer protocol. A
//! deletion is not re-announced: a tombstone is not something a client loses, and restoring one
//! would need the retention facts a purge may already have acted on.
//!
//! Derivatives are not restored: an asset manifest does not name them. The next push re-attaches
//! them from the feed's truth.
//!
//! # Ledgered like a finalization
//!
//! A restored row that no ledger entry names would be lost again by the next rebuild. So the
//! restore appends what a finalization appends, in the same order: custody entries for the
//! provenance, metadata and original blobs, and one lifecycle entry per manifest after the one
//! that named the metadata, all **before** the index is touched; then the entry resolving them
//! with the sequence number the restore minted. A rebuild replays them into the same row.
//!
//! [Server Filesystem § Sync Epoch and Client Re-Announce]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/filesystem/server.md#sync-epoch-and-client-re-announce

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::provenance::{Action, AssetManifest};
use capsule_i18n::error_codes;
use jiff::Timestamp;
use uuid::Uuid;

use crate::attestation::AttestationContext;
use crate::blob::ContentAddress;
use crate::index::{AssetRow, AssetState, BlobRef, OpAction, RestoreOutcome};
use crate::ledger::{LedgerEvent, manifest};
use crate::quota::{QuotaLimits, QuotaStore};
use crate::store::{AlbumId, AssetId, BlobRole, OwnerId, UserId};
use crate::upload::{AlbumWriteAccess, UploadContext};

/// The most manifests one request may re-announce.
///
/// A manifest is a few kilobytes and the route's body cap is a mebibyte, so the cap would refuse
/// a batch well before this in practice; the count bounds the server work one request buys.
pub const MAX_REANNOUNCE_BATCH: usize = 64;

/// The furthest a head is walked back to find the manifest naming its metadata blob.
///
/// Derivatives and trash moves are the only manifests that name none, and an asset accumulates a
/// handful. A chain longer than this is answered as malformed rather than read without bound.
pub const MAX_CHAIN_WALK: usize = 16;

/// Why a re-announced manifest was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not a manifest this server can use: undecodable, structurally invalid, from a newer
    /// client, or a chain that does not walk back to a metadata-bearing manifest.
    Malformed,
    /// The caller may not restore it: another account signed it, the caller cannot write to its
    /// album, its pin is not the album's, its device is not in the caller's directory, or the
    /// asset id is filed under another album or owner.
    ///
    /// One answer for all of them, as the upload gate's `403` is: telling a caller which applied
    /// would make the route an oracle for albums and asset ids it cannot otherwise see.
    Denied,
    /// The chain head is a deletion, which re-announce does not restore.
    Deleted,
}

impl Refusal {
    /// The stable catalog code the client localizes.
    pub fn code(self) -> &'static str {
        match self {
            Self::Malformed => error_codes::SYNC_REANNOUNCE_MALFORMED,
            Self::Denied => error_codes::SYNC_REANNOUNCE_DENIED,
            Self::Deleted => error_codes::SYNC_REANNOUNCE_DELETED,
        }
    }
}

/// What re-announcing one manifest did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The index lacked the row and now holds it, at `sync_seq`.
    Restored {
        /// The asset.
        asset: AssetId,
        /// The position the restored row was published at, when the read after the restore
        /// could see it.
        sync_seq: Option<u64>,
    },
    /// The index already holds the row. Nothing was written.
    AlreadyIndexed {
        /// The asset.
        asset: AssetId,
    },
    /// The store lacks blobs the row needs, named here. Nothing was written: the client
    /// re-uploads through the transfer protocol.
    MissingBlobs {
        /// The asset.
        asset: AssetId,
        /// What the store does not hold.
        missing: Vec<ContentAddress>,
    },
    /// Refused. Nothing was written.
    Refused {
        /// The asset, when the manifest decoded far enough to name one.
        asset: Option<AssetId>,
        /// Why.
        refusal: Refusal,
    },
}

/// A collaborator could not answer, so the manifest was neither restored nor refused.
#[derive(Debug, thiserror::Error)]
#[error("re-announce could not be decided: {0}")]
pub struct Unavailable(pub String);

/// The collaborators a re-announce reads and writes.
#[derive(Clone, Copy)]
pub struct Reannounce<'a> {
    /// The index, the blob store, the write authority and the clock.
    pub upload: &'a UploadContext,
    /// The owner ledger and the key that signs its entries.
    pub attestation: &'a AttestationContext,
    /// Quota attribution for the restored blobs.
    pub quotas: &'a dyn QuotaStore,
}

/// One manifest of the walked chain, newest first.
struct Walked {
    manifest: AssetManifest,
    /// The SHA-256 of the record's bytes, which is its content address and the next record's
    /// `prior_provenance_hash`.
    hash: Hash32,
    address: ContentAddress,
}

/// A refusal, logged with the reason the wire does not carry.
fn refuse(asset: Option<&AssetId>, refusal: Refusal, reason: &'static str) -> Answer {
    tracing::info!(asset = ?asset.map(AssetId::as_str), ?refusal, reason, "a re-announced manifest was refused");
    Answer::Refused {
        asset: asset.cloned(),
        refusal,
    }
}

/// The content address of a SHA-256 digest: equal today, and computed the way finalization
/// computes it rather than assumed.
fn address_of(hash: Hash32) -> Result<ContentAddress, Unavailable> {
    ContentAddress::parse(&hash.to_hex())
        .map_err(|error| Unavailable(format!("a digest is not an address: {error}")))
}

/// Re-announce `bytes`, the exact provenance record a client holds as an asset's chain head, on
/// behalf of `caller`.
///
/// # Errors
///
/// [`Unavailable`] when the index, the store, the authority or the ledger could not answer.
/// Everything the manifest itself decides is an [`Answer`].
#[tracing::instrument(skip(context, bytes), fields(caller = %caller, size = bytes.len()))]
pub async fn reannounce(
    context: Reannounce<'_>,
    caller: &UserId,
    bytes: &[u8],
) -> Result<Answer, Unavailable> {
    let head = match manifest::parse_valid(bytes) {
        Ok(head) => head,
        Err(fault) => {
            tracing::info!(
                fault = fault.as_str(),
                "a re-announced record is not a usable manifest"
            );
            return Ok(Answer::Refused {
                asset: None,
                refusal: Refusal::Malformed,
            });
        }
    };
    let asset = AssetId::new(head.core.file_id.to_string());
    let album = AlbumId::new(head.core.album_id.to_string());

    if Uuid::parse_str(caller.as_str()).ok() != Some(head.core.created_by_user) {
        return Ok(refuse(
            Some(&asset),
            Refusal::Denied,
            "another account signed it",
        ));
    }
    if head.core.action == Action::Delete {
        return Ok(refuse(
            Some(&asset),
            Refusal::Deleted,
            "the head is a deletion",
        ));
    }
    let Some(owner) = authorize(context.upload, caller, &album, &head, &asset).await? else {
        return Ok(refuse(Some(&asset), Refusal::Denied, "no write authority"));
    };

    if let Some(row) = context
        .upload
        .index()
        .read(&asset)
        .await
        .map_err(|error| Unavailable(format!("read the asset row: {error}")))?
    {
        return Ok(held(&row, &owner, &album, asset));
    }

    let head_hash = hash_bytes(bytes);
    let chain = match walk(context.upload, head, head_hash).await? {
        Ok(chain) => chain,
        Err(Walk::Missing(address)) => {
            tracing::info!(%asset, %address, "a manifest the head chains onto is not in the store");
            return Ok(Answer::MissingBlobs {
                asset,
                missing: vec![address],
            });
        }
        Err(Walk::Malformed(reason)) => {
            return Ok(refuse(Some(&asset), Refusal::Malformed, reason));
        }
    };

    let restore = match assemble(context.upload, &owner, &album, &asset, &chain).await? {
        Ok(restore) => restore,
        Err(missing) => {
            tracing::info!(%asset, missing = missing.len(), "a re-announced manifest names blobs the store lacks");
            return Ok(Answer::MissingBlobs { asset, missing });
        }
    };

    commit(context, caller, &owner, &album, &asset, &chain, restore).await
}

/// Invariants 6 and 7 for the caller, and the namespace the row files under.
///
/// `None` is a refusal.
async fn authorize(
    upload: &UploadContext,
    caller: &UserId,
    album: &AlbumId,
    head: &AssetManifest,
    asset: &AssetId,
) -> Result<Option<OwnerId>, Unavailable> {
    let access = upload
        .authority()
        .album_write_access(caller, album)
        .await
        .map_err(|error| Unavailable(error.to_string()))?;
    let AlbumWriteAccess::Writable {
        owner_id,
        protocol_pin,
        ..
    } = access
    else {
        tracing::debug!(%asset, %album, "the caller may not write to the manifest's album");
        return Ok(None);
    };
    if head.core.protocol_version != protocol_pin {
        tracing::debug!(%asset, %album, "the manifest's pin is not the album's");
        return Ok(None);
    }
    let added_at = upload
        .authority()
        .device_added_at(caller, head.core.created_by_device)
        .await
        .map_err(|error| Unavailable(error.to_string()))?;
    let signed_at = head.core.timestamp.parse::<Timestamp>().ok();
    match (added_at, signed_at) {
        (Some(added_at), Some(signed_at)) if added_at <= signed_at => Ok(Some(owner_id)),
        _ => {
            tracing::debug!(%asset, "the signing device is not in the caller's directory at the manifest's time");
            Ok(None)
        }
    }
}

/// The answer for an asset the index already holds.
fn held(row: &AssetRow, owner: &OwnerId, album: &AlbumId, asset: AssetId) -> Answer {
    if &row.owner_id == owner && &row.album_id == album {
        tracing::debug!(%asset, "a re-announced asset is already indexed");
        Answer::AlreadyIndexed { asset }
    } else {
        // Somebody else's row under the same id, or another album's: the same answer a guessed
        // id gets, so the route says nothing about a row the caller cannot see.
        refuse(
            Some(&asset),
            Refusal::Denied,
            "the asset id is filed elsewhere",
        )
    }
}

/// Why a chain could not be walked.
enum Walk {
    /// A manifest the chain needs is not in the store.
    Missing(ContentAddress),
    /// The chain is not one a client could have written.
    Malformed(&'static str),
}

/// A walked chain: newest first, ending at the newest manifest that names a metadata blob.
struct Chain {
    walked: Vec<Walked>,
    /// The metadata blob the last manifest of `walked` names.
    metadata: Hash32,
}

impl Chain {
    /// The chain head: the manifest the client sent.
    fn head(&self) -> &Walked {
        &self.walked[0]
    }

    /// The newest manifest naming a metadata blob.
    fn base(&self) -> &Walked {
        &self.walked[self.walked.len() - 1]
    }
}

/// Walk back from the head to the newest manifest naming a metadata blob.
async fn walk(
    upload: &UploadContext,
    head: AssetManifest,
    head_hash: Hash32,
) -> Result<Result<Chain, Walk>, Unavailable> {
    let file_id = head.core.file_id;
    let mut walked = vec![Walked {
        address: address_of(head_hash)?,
        hash: head_hash,
        manifest: head,
    }];
    loop {
        let current = &walked[walked.len() - 1].manifest;
        if let Some(metadata) = current.core.metadata_blob_hash {
            return Ok(Ok(Chain { walked, metadata }));
        }
        if walked.len() > MAX_CHAIN_WALK {
            return Ok(Err(Walk::Malformed("the chain is longer than the walk")));
        }
        // `structural_ok` holds every manifest here to "a predecessor iff not a create", and a
        // create names its metadata, so a manifest without either is not one the parser passed.
        let Some(prior) = current.core.prior_provenance_hash else {
            return Ok(Err(Walk::Malformed(
                "a manifest names no metadata and no predecessor",
            )));
        };
        let address = address_of(prior)?;
        let parsed = manifest::read(upload.blobs(), &address)
            .await
            .map_err(|error| Unavailable(format!("read a prior manifest: {error}")))?;
        let previous = match parsed {
            None => return Ok(Err(Walk::Missing(address))),
            Some(Err(fault)) => {
                tracing::debug!(%address, fault = fault.as_str(), "a prior manifest is not usable");
                return Ok(Err(Walk::Malformed("a prior manifest is not usable")));
            }
            Some(Ok(previous)) => previous,
        };
        if manifest::check_structure(&previous).is_err() || previous.core.file_id != file_id {
            return Ok(Err(Walk::Malformed(
                "a prior manifest belongs to another asset",
            )));
        }
        walked.push(Walked {
            manifest: previous,
            hash: prior,
            address,
        });
    }
}

/// The row a walked chain implies, and the blobs it names, or what the store lacks.
struct Restore {
    row: AssetRow,
    /// The metadata-bearing manifest's provenance blob, and the metadata and original blobs,
    /// with the sizes the store reports: what the custody entries name.
    custody: Vec<BlobRef>,
}

/// Assemble the row, after checking the store holds every blob it names.
async fn assemble(
    upload: &UploadContext,
    owner: &OwnerId,
    album: &AlbumId,
    asset: &AssetId,
    chain: &Chain,
) -> Result<Result<Restore, Vec<ContentAddress>>, Unavailable> {
    let (head, base) = (chain.head(), chain.base());
    let wanted = [
        (BlobRole::Provenance, head.address.clone()),
        (BlobRole::Provenance, base.address.clone()),
        (BlobRole::Metadata, address_of(chain.metadata)?),
        (
            BlobRole::Original,
            address_of(head.manifest.core.ciphertext_hash)?,
        ),
    ];
    let mut missing = Vec::new();
    let mut sizes = std::collections::BTreeMap::new();
    for (_, address) in &wanted {
        if sizes.contains_key(address) || missing.contains(address) {
            continue;
        }
        match upload
            .blobs()
            .stat(address)
            .await
            .map_err(|error| Unavailable(format!("stat a named blob: {error}")))?
        {
            Some(stat) => {
                sizes.insert(address.clone(), stat.size);
            }
            None => missing.push(address.clone()),
        }
    }
    if !missing.is_empty() {
        return Ok(Err(missing));
    }
    let blob = |role: BlobRole, address: &ContentAddress| BlobRef {
        role,
        address: address.clone(),
        size: sizes.get(address).copied().unwrap_or_default(),
    };

    let [head_provenance, base_provenance, metadata, original] = &wanted;
    let mut blobs = vec![
        blob(head_provenance.0, &head_provenance.1),
        blob(metadata.0, &metadata.1),
        blob(original.0, &original.1),
    ];
    blobs.sort_by(|a, b| (a.role, a.address.as_str()).cmp(&(b.role, b.address.as_str())));
    let now = upload.clock().now();
    let core = &head.manifest.core;
    let row = AssetRow {
        asset_id: asset.clone(),
        owner_id: owner.clone(),
        album_id: album.clone(),
        protocol_version: core.protocol_version.clone(),
        crypto_suite_id: core.crypto_suite_id,
        state: AssetState::Visible,
        blobs,
        first_seq: None,
        sync_seq: None,
        chain_head: Some(head.hash),
        amk_version: u64::from(core.amk_version.0),
        // Oldest first, and never the head itself, which is the row's provenance blob.
        superseded: chain.walked[1..]
            .iter()
            .rev()
            .map(|walked| walked.address.clone())
            .collect(),
        hold: None,
        retention_until: None,
        created_at: now,
        updated_at: now,
    };
    Ok(Ok(Restore {
        row,
        custody: vec![
            blob(base_provenance.0, &base_provenance.1),
            blob(metadata.0, &metadata.1),
            blob(original.0, &original.1),
        ],
    }))
}

/// The lifecycle token a manifest after the metadata-bearing one is ledgered under, as the ops
/// route ledgers it.
fn op_token(action: Action) -> Option<&'static str> {
    match action {
        Action::DerivativeAdd | Action::DerivativeReplace => Some(OpAction::Derivative.as_str()),
        Action::Delete => Some(OpAction::Delete.as_str()),
        Action::TrashRestore => Some(OpAction::TrashRestore.as_str()),
        // These name a metadata blob, so the walk stopped at them.
        Action::Create | Action::Replace | Action::MetadataUpdate => None,
    }
}

/// Ledger, restore, resolve, attribute.
async fn commit(
    context: Reannounce<'_>,
    caller: &UserId,
    owner: &OwnerId,
    album: &AlbumId,
    asset: &AssetId,
    chain: &Chain,
    restore: Restore,
) -> Result<Answer, Unavailable> {
    let ledger = context.attestation.ledger();
    let signer = context.attestation.signer();
    let clock = context.upload.clock();
    // The id pairing the custody entries with what resolves them: a fresh UUIDv7 in the upload
    // id space no session ever holds, as an adoption's is.
    let reannounce_id = Uuid::now_v7().to_string();
    let received_at = clock.now().to_string();
    let base = chain.base();

    // Before the index is touched: a row never exists without the entries a rebuild needs.
    for blob in &restore.custody {
        let content_hash = Hash32::from_hex(blob.address.as_str())
            .map_err(|_| Unavailable("a content address is not a digest".to_owned()))?;
        let event = LedgerEvent::Custody {
            upload_id: reannounce_id.clone(),
            asset_id: asset.as_str().to_owned(),
            blob_role: blob.role.as_str().to_owned(),
            content_hash,
            size: blob.size,
            manifest_sha256: (blob.role == BlobRole::Provenance).then_some(base.hash),
            upload_user_id: caller.as_str().to_owned(),
            uploader_device: Some(base.manifest.core.created_by_device.to_string()),
            received_at: received_at.clone(),
        };
        ledger
            .append(owner, event, clock.now(), signer)
            .await
            .map_err(|error| Unavailable(format!("ledger custody: {error}")))?;
    }
    for event in lifecycle_events(chain, None) {
        ledger
            .append(owner, event, clock.now(), signer)
            .await
            .map_err(|error| Unavailable(format!("ledger a lifecycle op: {error}")))?;
    }

    let outcome = context
        .upload
        .index()
        .restore(restore.row.clone())
        .await
        .map_err(|error| Unavailable(format!("restore the row: {error}")))?;
    if let RestoreOutcome::Present(existing) = outcome {
        // A finalization or another device's re-announce landed between the read and the
        // restore. The custody entries are resolved as refused, so a rebuild does not replay
        // them over the row that won.
        append_best_effort(
            context,
            owner,
            LedgerEvent::Refused {
                upload_id: reannounce_id,
                asset_id: asset.as_str().to_owned(),
                reason: "the asset was indexed while it was being re-announced".to_owned(),
            },
        )
        .await;
        return Ok(held(&existing, owner, album, asset.clone()));
    }

    let sync_seq = match context.upload.index().read(asset).await {
        Ok(row) => row.and_then(|row| row.sync_seq),
        Err(error) => {
            tracing::error!(%error, %asset, "the restored row could not be read back for its position");
            None
        }
    };
    append_best_effort(
        context,
        owner,
        LedgerEvent::Indexed {
            upload_id: reannounce_id,
            asset_id: asset.as_str().to_owned(),
            sync_seq,
            receipt: None,
        },
    )
    .await;
    for event in lifecycle_events(chain, sync_seq) {
        append_best_effort(context, owner, event).await;
    }
    attribute(context, caller, &restore.row).await;
    tracing::info!(%asset, %owner, ?sync_seq, walked = chain.walked.len(), "restored an index row from a re-announced manifest");
    Ok(Answer::Restored {
        asset: asset.clone(),
        sync_seq,
    })
}

/// One lifecycle entry per manifest after the metadata-bearing one, oldest first, carrying
/// `sync_seq` once the restore has minted one.
///
/// A manifest's hash is the next one's `prior_provenance_hash`, which is how the walk read it;
/// the head's is the hash of the bytes the client sent.
fn lifecycle_events(chain: &Chain, sync_seq: Option<u64>) -> Vec<LedgerEvent> {
    chain.walked[..chain.walked.len() - 1]
        .iter()
        .rev()
        .filter_map(|walked| {
            op_token(walked.manifest.core.action).map(|action| LedgerEvent::Lifecycle {
                asset_id: walked.manifest.core.file_id.to_string(),
                action: action.to_owned(),
                manifest_hash: Some(walked.hash),
                sync_seq,
            })
        })
        .collect()
}

/// Append an entry that resolves what the restore already committed. Never fails the restore:
/// a lost entry is the crash case a rebuild already handles.
async fn append_best_effort(context: Reannounce<'_>, owner: &OwnerId, event: LedgerEvent) {
    let kind = event.kind();
    if let Err(error) = context
        .attestation
        .ledger()
        .append(
            owner,
            event,
            context.upload.clock().now(),
            context.attestation.signer(),
        )
        .await
    {
        tracing::error!(%error, %owner, kind, "the owner ledger could not record a re-announce; a rebuild will not be able to prove this owner's sequence");
    }
}

/// Attribute the restored blobs to the caller, once each.
///
/// Unlimited, as a rebuild charges: nothing new is stored, and an address already attributed (a
/// quota ledger restored from the same backup) is a no-op.
async fn attribute(context: Reannounce<'_>, caller: &UserId, row: &AssetRow) {
    let now = context.upload.clock().now();
    for blob in &row.blobs {
        if let Err(error) = context
            .quotas
            .charge(
                caller,
                &blob.address,
                blob.size,
                now,
                QuotaLimits::unlimited(),
            )
            .await
        {
            tracing::error!(%error, address = %blob.address, "a restored blob could not be attributed to its uploader");
        }
    }
}

#[cfg(test)]
mod tests;
