//! [`BreadcrumbIndex`]: the lightweight per-asset breadcrumb of each pulled album
//! (design/federation.md, Federated Breadcrumb Index).
//!
//! # What a breadcrumb is
//!
//! Search has two tiers: the full local index over local and cached content, and, for an
//! accessible remote album, one [`Breadcrumb`] per asset — its chain head's hash, its
//! timestamp, its author, its size and its album. Viewing the album promotes its assets through
//! the normal indexing pipeline; nothing is pre-indexed wholesale.
//!
//! # Read from what a pull committed, and nothing else
//!
//! A breadcrumb is derived from the asset's **latest** entry that
//! [`PulledAlbums::commit`](super::pull::PulledAlbums::commit) wrote, so it is only ever built
//! from input that passed re-validation, and it is never stored on its own: the index holds no
//! row the pulled album does not, grows only as the album's own entries do, and cannot drift
//! from them. An asset whose latest entry is a delete or a purged tombstone has no breadcrumb.
//! Every field comes from the head record, which the pull already parsed and checked:
//!
//! | Field | From |
//! | --- | --- |
//! | `hash` | the entry's chain head, the record's hash |
//! | `timestamp` | the record's self-asserted write time: what a member searches by, never load-bearing |
//! | `author` | the record's `created_by_user`, the account that wrote the head |
//! | `size` | the record's `plaintext_size` |
//! | album | the job's album, on the job's peer |
//!
//! # Read a page at a time
//!
//! [`BreadcrumbIndex::breadcrumbs`] answers at most [`MAX_BREADCRUMB_PAGE`] breadcrumbs, by
//! asset id in byte order, after a given asset id, so a reader walks an album of any size in
//! bounded steps.
//!
//! # What lives where
//!
//! - This file — the port, [`Breadcrumb`] and [`breadcrumb`], the one derivation both adapters
//!   run.
//! - The adapters are the pulled-album stores themselves:
//!   [`InMemoryPulledAlbums`](super::pull::InMemoryPulledAlbums) and
//!   [`PostgresPulledAlbums`](super::pull::pulled::postgres::PostgresPulledAlbums), which read
//!   the entries they hold.
//! - `conformance` (test builds) — the suite both adapters pass.

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;

use super::pull::JobKey;
use super::pull::pulled::{AcceptedEntry, Change};
use crate::ledger::manifest;
use crate::store::{StoreError, StoreFuture};

#[cfg(test)]
pub(crate) mod conformance;

/// The most breadcrumbs one read answers.
pub const MAX_BREADCRUMB_PAGE: usize = 1_000;

/// One pulled asset, as remote search sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breadcrumb {
    /// The album, on its home peer.
    pub job: JobKey,
    /// The asset.
    pub asset_id: String,
    /// Its chain head: the hash of its latest accepted provenance record.
    pub hash: Hash32,
    /// When the head record says it was written. Self-asserted and audit-only.
    pub timestamp: Timestamp,
    /// The account that wrote the head record.
    pub author: uuid::Uuid,
    /// The asset's plaintext size in bytes.
    pub size: u64,
}

/// Whether `entry`, as an asset's latest, leaves the asset a breadcrumb.
///
/// The predicate the durable adapter's query restates: not a delete, not purged, and carrying
/// the record and its head.
pub fn leaves_a_breadcrumb(entry: &AcceptedEntry) -> bool {
    entry.change != Change::Deleted
        && !entry.purged
        && entry.manifest.is_some()
        && entry.head.is_some()
}

/// The breadcrumb `entry`, an asset's latest entry in `job`, leaves; `None` when it leaves none.
///
/// # Errors
///
/// [`StoreError::Corrupt`] when a held record no longer parses, or its timestamp does not: the
/// pull parsed both before committing it, so a stored one that does not is a damaged row.
pub fn breadcrumb(job: &JobKey, entry: &AcceptedEntry) -> Result<Option<Breadcrumb>, StoreError> {
    let (Some(bytes), Some(head)) = (&entry.manifest, entry.head) else {
        return Ok(None);
    };
    if !leaves_a_breadcrumb(entry) {
        return Ok(None);
    }
    let corrupt = |detail: String| {
        tracing::error!(%job, asset = %entry.asset_id, %detail, "a held pulled record does not read back");
        StoreError::Corrupt {
            store: "pulled albums",
            record: "Breadcrumb",
            detail,
        }
    };
    let record = manifest::parse_valid(bytes)
        .map_err(|fault| corrupt(format!("the held record does not parse: {fault}")))?;
    let core = &record.core;
    let timestamp = core
        .timestamp
        .parse::<Timestamp>()
        .map_err(|_| corrupt("the held record's timestamp is not RFC 3339".to_owned()))?;
    Ok(Some(Breadcrumb {
        job: job.clone(),
        asset_id: entry.asset_id.clone(),
        hash: head,
        timestamp,
        author: core.created_by_user,
        size: core.plaintext_size,
    }))
}

/// Where the breadcrumbs of pulled albums are read.
pub trait BreadcrumbIndex: std::fmt::Debug + Send + Sync {
    /// The breadcrumbs of `job`'s assets whose id is after `after`, by asset id in byte order,
    /// at most `limit` of them and never more than [`MAX_BREADCRUMB_PAGE`].
    fn breadcrumbs<'a>(
        &'a self,
        job: &'a JobKey,
        after: Option<&'a str>,
        limit: usize,
    ) -> StoreFuture<'a, Vec<Breadcrumb>>;
}

#[cfg(test)]
mod tests;
