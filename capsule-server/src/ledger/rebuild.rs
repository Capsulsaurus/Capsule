//! `capsule-server rebuild`: reconstruct the asset index from the owner ledgers and the blobs.
//!
//! [Server Filesystem § Rebuild] is the contract. Per owner:
//!
//! 1. Verify the ledger chain ([`LedgerKeys::verify`]). A break stops that owner's replay at
//!    the last good entry and is reported; other owners continue.
//! 2. Replay the verified custody entries the index did not refuse, in ledger order: read each
//!    named blob's presence, decode each provenance blob through the one manifest parser
//!    ([`super::manifest`]), and assemble the row the entries imply.
//! 3. Compare each row with the index. A row the index lacks is restored with the `sync_seq`
//!    the ledger proved ([`AssetIndex::restore`]); a row it holds is compared and never
//!    overwritten. Quota attribution is recomputed for every restored blob; reference counts are
//!    a query over rows, so restoring the rows is recomputing them.
//! 4. Report blobs nothing names, entries naming a missing blob, and provenance blobs that fail
//!    structural validation (moved to `quarantine/` with their reason, never dropped).
//!
//! When the ledger cannot prove an owner's sequence — the chain breaks, or a custody entry was
//! never resolved because the process stopped between it and the index write — the rows it
//! cannot number are given numbers above every number the owner has used, and the owner moves to
//! a new sync epoch ([`AssetIndex::bump_sync_epoch`]), so a client's cursor from before the
//! rebuild is refused rather than resumed against a numbering it never saw.
//!
//! # Dry run by default
//!
//! [`Mode::DryRun`] reads everything and writes nothing: no row, no quota charge, no epoch, no
//! quarantine move. The report is the same shape either way, so an operator reads what a rebuild
//! would do before doing it.
//!
//! # Idempotent
//!
//! Every write is conditional on the index not holding the row already, and the quota charge is
//! a no-op for an attributed address, so a second `--apply` over the same ledger restores
//! nothing. The epoch is the one exception worth naming: it bumps again whenever the ledger
//! still cannot prove the sequence, which is harmless (a cursor from the previous epoch is
//! refused, and a client resyncs once more) and is reported.
//!
//! # What a rebuild trusts
//!
//! The ledger's own signatures when the host holds the attestation key's public half, its links
//! otherwise (the report says which). It does not re-verify a manifest's device signature: the
//! ledger records the device-directory versions it would need (`LedgerEvent::Directory`), and
//! no producer appends them yet. Lifecycle actions taken through `POST /albums/{id}/ops` are not
//! ledgered either, so a rebuild into an empty schema restores what was uploaded and not what
//! was deleted since; both are named in the design doc as the ledger's remaining producers.
//!
//! [Server Filesystem § Rebuild]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/filesystem/server.md#rebuild

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::provenance::{Action, ManifestCore};
use capsule_i18n::error_codes;
use jiff::Timestamp;

use super::manifest::{self, ManifestFault};
use super::replay::{self, Custody, LedgerKeys, Resolution};
use super::{ChainFault, LedgerError, OwnerLedger};
use crate::blob::{BlobError, BlobStore, ContentAddress, QuarantineReason};
use crate::gc::Mode;
use crate::index::{AssetIndex, AssetRow, AssetState, BlobRef, RestoreOutcome};
use crate::quota::{QuotaLimits, QuotaStore};
use crate::store::{AlbumId, AssetId, BlobRole, Clock, OwnerId, StoreError, UserId};

/// Why a rebuild could not finish.
///
/// Distinct from what it *found*: a broken chain or a missing blob is a report line, and the
/// pass continues. These are the failures that leave the pass unable to compare at all.
#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    /// The ledger could not be read.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    /// The blob store could not be read or written.
    #[error(transparent)]
    Blobs(#[from] BlobError),
    /// The index or the quota store could not answer.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The rebuild's collaborators.
#[derive(Debug, Clone)]
pub struct RebuildContext {
    index: Arc<dyn AssetIndex>,
    blobs: Arc<dyn BlobStore>,
    ledger: Arc<dyn OwnerLedger>,
    quotas: Arc<dyn QuotaStore>,
    keys: LedgerKeys,
    clock: Arc<dyn Clock>,
}

impl RebuildContext {
    /// Assembles the rebuild over the stores it reads and writes.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        ledger: Arc<dyn OwnerLedger>,
        quotas: Arc<dyn QuotaStore>,
        keys: LedgerKeys,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            index,
            blobs,
            ledger,
            quotas,
            keys,
            clock,
        }
    }
}

/// A row the index holds that disagrees with the one the ledger implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDifference {
    /// The asset.
    pub asset: AssetId,
    /// The facts that disagree, by name.
    pub facts: Vec<&'static str>,
}

/// What a rebuild found, and did, for one owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerReport {
    /// The owner.
    pub owner: OwnerId,
    /// How many ledger entries, from the first, verified.
    pub verified: usize,
    /// Why the chain stopped verifying before its end, if it did.
    pub chain_fault: Option<ChainFault>,
    /// Rows the index lacked: restored under `--apply`, and what would be in a dry run.
    pub restored: Vec<AssetId>,
    /// Rows the index holds that agree with the ledger.
    pub agreed: Vec<AssetId>,
    /// Rows the index holds that disagree with the ledger. Never overwritten.
    pub differs: Vec<RowDifference>,
    /// Rows whose `sync_seq` the ledger cannot prove, numbered above the owner's head.
    pub unprovable: Vec<AssetId>,
    /// Bundles the ledger records without a decodable manifest and metadata blob: nothing a
    /// client could see, so nothing to restore.
    pub incomplete: Vec<AssetId>,
    /// Entries naming a blob the store does not hold.
    pub dangling: Vec<(AssetId, ContentAddress)>,
    /// Provenance blobs that are not a usable manifest, and why. Under `--apply` a malformed one
    /// is moved to `quarantine/` with its reason unless a row still references it; a record a
    /// newer client wrote ([`ManifestFault::NewerVersion`]) is never moved.
    pub unusable_manifests: Vec<(AssetId, ContentAddress, ManifestFault)>,
    /// Provenance blobs whose manifest names another asset than the entry does.
    pub foreign_manifests: Vec<(AssetId, ContentAddress)>,
    /// Whether the owner must move to a new sync epoch: the sequence is not provable.
    pub epoch_bump_needed: bool,
    /// The epoch the owner moved to, under `--apply`.
    pub epoch: Option<u64>,
}

impl OwnerReport {
    /// Whether anything here needs an operator: a fault, as opposed to a row to restore.
    pub fn has_faults(&self) -> bool {
        self.chain_fault.is_some()
            || !self.differs.is_empty()
            || !self.dangling.is_empty()
            || !self.unusable_manifests.is_empty()
            || !self.foreign_manifests.is_empty()
    }
}

/// What one rebuild pass found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    /// Whether anything was written.
    pub applied: bool,
    /// Whether the ledgers were verified by links only (no attestation key on this host).
    pub links_only: bool,
    /// One report per owner with a ledger, in owner order.
    pub owners: Vec<OwnerReport>,
    /// Blobs no verified ledger entry names and no index row references: left for GC.
    pub orphans: Vec<ContentAddress>,
}

impl RebuildReport {
    /// Whether any owner's report needs an operator.
    pub fn has_faults(&self) -> bool {
        self.owners.iter().any(OwnerReport::has_faults)
    }
}

/// Run one rebuild pass.
///
/// # Errors
///
/// [`RebuildError`] when a store cannot be read or written. What the pass found is never an
/// error; see [`RebuildReport`].
#[tracing::instrument(skip(context), fields(mode = ?mode))]
pub async fn rebuild(context: &RebuildContext, mode: Mode) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport {
        applied: mode == Mode::Apply,
        links_only: !context.keys.checks_signatures(),
        ..RebuildReport::default()
    };
    let mut named: BTreeSet<ContentAddress> = BTreeSet::new();

    for owner in context.ledger.owners().await? {
        let owner_report = rebuild_owner(context, mode, &owner, &mut named).await?;
        tracing::info!(
            %owner,
            verified = owner_report.verified,
            restored = owner_report.restored.len(),
            agreed = owner_report.agreed.len(),
            differs = owner_report.differs.len(),
            unprovable = owner_report.unprovable.len(),
            dangling = owner_report.dangling.len(),
            unusable_manifests = owner_report.unusable_manifests.len(),
            epoch = ?owner_report.epoch,
            "rebuilt an owner's rows from its ledger"
        );
        report.owners.push(owner_report);
    }

    report.orphans = orphans(context, &named).await?;
    tracing::info!(
        applied = report.applied,
        owners = report.owners.len(),
        orphans = report.orphans.len(),
        faults = report.has_faults(),
        "an index rebuild finished"
    );
    Ok(report)
}

/// One asset's facts, accumulated over its custody entries in ledger order.
#[derive(Debug, Default)]
struct Accumulated {
    blobs: BTreeMap<(BlobRole, String), BlobRef>,
    /// The account each address's storage is attributed to, from its latest custody entry.
    uploaders: BTreeMap<ContentAddress, UserId>,
    superseded: Vec<ContentAddress>,
    head: Option<(ContentAddress, Option<Hash32>, ManifestCore)>,
    proven: Vec<u64>,
    unresolved: bool,
    created_at: Option<Timestamp>,
    updated_at: Option<Timestamp>,
}

/// Steps 1–3 for one owner.
async fn rebuild_owner(
    context: &RebuildContext,
    mode: Mode,
    owner: &OwnerId,
    named: &mut BTreeSet<ContentAddress>,
) -> Result<OwnerReport, RebuildError> {
    let scan = context.ledger.scan(owner).await?;
    let chain = context.keys.verify(owner, &scan);
    let mut report = OwnerReport {
        owner: owner.clone(),
        verified: chain.verified,
        epoch_bump_needed: chain.fault.is_some(),
        chain_fault: chain.fault,
        restored: Vec::new(),
        agreed: Vec::new(),
        differs: Vec::new(),
        unprovable: Vec::new(),
        incomplete: Vec::new(),
        dangling: Vec::new(),
        unusable_manifests: Vec::new(),
        foreign_manifests: Vec::new(),
        epoch: None,
    };

    // Ledger order, and assets in the order they first appear, so unprovable rows are numbered
    // in the order the server took custody of them.
    let mut order: Vec<AssetId> = Vec::new();
    let mut assets: BTreeMap<AssetId, Accumulated> = BTreeMap::new();
    for record in replay::custody(&scan.entries[..chain.verified]) {
        if record.resolution == Resolution::Refused {
            continue;
        }
        let asset = AssetId::new(record.asset_id.as_str());
        if !assets.contains_key(&asset) {
            order.push(asset.clone());
        }
        let facts = assets.entry(asset.clone()).or_default();
        accumulate(context, mode, &asset, &record, facts, &mut report, named).await?;
    }

    // The owner's numbering as it stands: what the ledger proved and what the index has used.
    let mut ceiling = context.index.head_seq(owner).await?;
    for facts in assets.values() {
        ceiling = ceiling.max(facts.proven.iter().copied().max().unwrap_or(0));
    }

    for asset in order {
        let Some(facts) = assets.remove(&asset) else {
            continue;
        };
        let Some(mut row) = assemble(owner, &asset, &facts) else {
            report.incomplete.push(asset);
            continue;
        };
        if facts.unresolved || row.sync_seq.is_none() {
            // The ledger cannot prove where this write sits in the owner's sequence. Numbered
            // above everything the owner has used, so it is new to every reader, and the owner
            // moves epoch so no reader resumes against the old numbering.
            if row.sync_seq.is_none() {
                ceiling += 1;
                row.sync_seq = Some(ceiling);
                row.first_seq = Some(ceiling);
            }
            report.unprovable.push(asset.clone());
            report.epoch_bump_needed = true;
        }
        compare_or_restore(context, mode, row, &facts, &mut report).await?;
    }

    if mode == Mode::Apply && report.epoch_bump_needed {
        report.epoch = Some(context.index.bump_sync_epoch(owner).await?);
    }
    Ok(report)
}

/// Fold one custody entry into its asset's facts.
async fn accumulate(
    context: &RebuildContext,
    mode: Mode,
    asset: &AssetId,
    record: &Custody,
    facts: &mut Accumulated,
    report: &mut OwnerReport,
    named: &mut BTreeSet<ContentAddress>,
) -> Result<(), RebuildError> {
    match record.resolution {
        Resolution::Indexed {
            sync_seq: Some(seq),
        } => facts.proven.push(seq),
        Resolution::Indexed { sync_seq: None } | Resolution::Refused => {}
        Resolution::Unresolved => facts.unresolved = true,
    }
    let at = record.received_at.parse::<Timestamp>().ok();
    facts.created_at = facts.created_at.or(at);
    facts.updated_at = at.or(facts.updated_at);

    let Ok(address) = ContentAddress::parse(&record.content_hash.to_hex()) else {
        return Ok(());
    };
    named.insert(address.clone());
    facts
        .uploaders
        .insert(address.clone(), UserId::new(record.upload_user_id.as_str()));
    let Some(role) = BlobRole::from_token(&record.blob_role) else {
        tracing::warn!(%asset, role = %record.blob_role, "a ledger entry names a role this build does not know");
        return Ok(());
    };
    if context.blobs.stat(&address).await?.is_none() {
        report.dangling.push((asset.clone(), address));
        return Ok(());
    }

    if role == BlobRole::Provenance {
        match manifest::read(context.blobs.as_ref(), &address).await? {
            None => {
                report.dangling.push((asset.clone(), address));
                return Ok(());
            }
            Some(Err(fault)) => {
                quarantine(context, mode, asset, &address, fault, report).await?;
                return Ok(());
            }
            Some(Ok(parsed)) => {
                if let Err(fault) = manifest::check_structure(&parsed) {
                    quarantine(context, mode, asset, &address, fault, report).await?;
                    return Ok(());
                }
                if parsed.core.file_id.to_string() != asset.as_str() {
                    report.foreign_manifests.push((asset.clone(), address));
                    return Ok(());
                }
                if let Some((previous, _, _)) = &facts.head
                    && previous != &address
                    && !facts.superseded.contains(previous)
                {
                    facts.superseded.push(previous.clone());
                }
                facts.head = Some((address.clone(), record.manifest_sha256, parsed.core));
            }
        }
    }

    // A singular role holds one address: a later custody entry for it is a replace, which
    // re-points it. A plural role accumulates.
    if crate::index::is_singular(role) {
        facts.blobs.retain(|(held, _), _| *held != role);
    }
    facts.blobs.insert(
        (role, address.as_str().to_owned()),
        BlobRef {
            role,
            address,
            size: record.size,
        },
    );
    Ok(())
}

/// Record a provenance blob that is not a usable manifest, and under `--apply` move it to
/// `quarantine/` — unless a row still references it, which is the scrub's to report rather than
/// the rebuild's to break, or a newer client wrote it, which is not damage at all.
async fn quarantine(
    context: &RebuildContext,
    mode: Mode,
    asset: &AssetId,
    address: &ContentAddress,
    fault: ManifestFault,
    report: &mut OwnerReport,
) -> Result<(), RebuildError> {
    report
        .unusable_manifests
        .push((asset.clone(), address.clone(), fault));
    if mode == Mode::DryRun || matches!(fault, ManifestFault::NewerVersion { .. }) {
        return Ok(());
    }
    if context.index.reference_count(address).await? > 0 {
        tracing::warn!(%asset, %address, %fault, "a malformed manifest is still referenced; left in place");
        return Ok(());
    }
    let moved = context
        .blobs
        .quarantine(
            address,
            QuarantineReason {
                code: error_codes::UPLOAD_ENVELOPE_REJECTED.to_owned(),
                detail: format!("index rebuild: {fault}"),
                at: context.clock.now(),
            },
        )
        .await?;
    tracing::warn!(%asset, %address, %fault, moved, "quarantined a manifest the rebuild could not use");
    Ok(())
}

/// The row an asset's accumulated facts imply, or `None` when its index tier never landed.
fn assemble(owner: &OwnerId, asset: &AssetId, facts: &Accumulated) -> Option<AssetRow> {
    let (_, chain_head, core) = facts.head.as_ref()?;
    let blobs: Vec<BlobRef> = facts.blobs.values().cloned().collect();
    let mut row = AssetRow {
        asset_id: asset.clone(),
        owner_id: owner.clone(),
        album_id: AlbumId::new(core.album_id.to_string()),
        protocol_version: core.protocol_version.clone(),
        crypto_suite_id: core.crypto_suite_id,
        state: AssetState::Visible,
        blobs,
        first_seq: facts.proven.iter().copied().min(),
        sync_seq: facts.proven.iter().copied().max(),
        chain_head: *chain_head,
        amk_version: u64::from(core.amk_version.0),
        superseded: facts.superseded.clone(),
        hold: None,
        retention_until: core
            .retention_until
            .as_deref()
            .and_then(|at| at.parse::<Timestamp>().ok()),
        created_at: facts.created_at.unwrap_or(Timestamp::UNIX_EPOCH),
        updated_at: facts.updated_at.unwrap_or(Timestamp::UNIX_EPOCH),
    };
    if !row.is_publishable() {
        return None;
    }
    if core.action == Action::Delete {
        row.state = AssetState::Tombstoned;
    }
    Some(row)
}

/// Step 3 for one row: compare it with the index, or restore it.
async fn compare_or_restore(
    context: &RebuildContext,
    mode: Mode,
    row: AssetRow,
    facts: &Accumulated,
    report: &mut OwnerReport,
) -> Result<(), RebuildError> {
    let asset = row.asset_id.clone();
    let existing = match context.index.read(&asset).await? {
        Some(existing) => Some(existing),
        None if mode == Mode::DryRun => None,
        None => match context.index.restore(row.clone()).await? {
            RestoreOutcome::Restored => {
                charge(context, &row, facts).await?;
                None
            }
            // A finalization landed between the read and the restore.
            RestoreOutcome::Present(existing) => Some(*existing),
        },
    };
    match existing {
        None => report.restored.push(asset),
        Some(existing) => {
            let facts = differences(&existing, &row);
            if facts.is_empty() {
                report.agreed.push(asset);
            } else {
                report.differs.push(RowDifference { asset, facts });
            }
        }
    }
    Ok(())
}

/// Re-attribute a restored row's blobs to the accounts the ledger names.
///
/// Quota is charged by content address, once: an address already attributed is a no-op, so a
/// second rebuild, or a quota ledger restored from the same backup, is not charged twice.
async fn charge(
    context: &RebuildContext,
    row: &AssetRow,
    facts: &Accumulated,
) -> Result<(), RebuildError> {
    let now = context.clock.now();
    for blob in &row.blobs {
        let Some(user) = facts.uploaders.get(&blob.address) else {
            continue;
        };
        context
            .quotas
            .charge(
                user,
                &blob.address,
                blob.size,
                now,
                QuotaLimits::unlimited(),
            )
            .await?;
    }
    Ok(())
}

/// The facts on which `held` and `rebuilt` disagree, by name.
///
/// Only facts a rebuild can reconstruct: a serving hold, a retention floor set by an unledgered
/// delete, and the superseded chain of unledgered lifecycle writes are the backup's alone, and
/// comparing them would report every such row as a disagreement.
pub fn differences(held: &AssetRow, rebuilt: &AssetRow) -> Vec<&'static str> {
    let mut facts = Vec::new();
    if held.owner_id != rebuilt.owner_id {
        facts.push("owner_id");
    }
    if held.album_id != rebuilt.album_id {
        facts.push("album_id");
    }
    if held.protocol_version != rebuilt.protocol_version {
        facts.push("protocol_version");
    }
    if held.crypto_suite_id != rebuilt.crypto_suite_id {
        facts.push("crypto_suite_id");
    }
    if held.sync_seq.is_some() != rebuilt.sync_seq.is_some() {
        facts.push("sync_seq");
    }
    let addresses = |row: &AssetRow| -> BTreeSet<(BlobRole, String)> {
        row.blobs
            .iter()
            .map(|blob| (blob.role, blob.address.as_str().to_owned()))
            .collect()
    };
    if !addresses(rebuilt).is_subset(&addresses(held)) {
        facts.push("blobs");
    }
    facts
}

/// Blobs no verified ledger entry names and no row references.
async fn orphans(
    context: &RebuildContext,
    named: &BTreeSet<ContentAddress>,
) -> Result<Vec<ContentAddress>, RebuildError> {
    let mut found = Vec::new();
    let mut after = None;
    loop {
        let page = context.blobs.enumerate(after.as_ref(), 256).await?;
        for entry in &page.entries {
            if !named.contains(&entry.address)
                && context.index.reference_count(&entry.address).await? == 0
            {
                found.push(entry.address.clone());
            }
        }
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(found),
        }
    }
}

#[cfg(test)]
mod tests;
