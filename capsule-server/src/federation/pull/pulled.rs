//! [`PulledAlbums`]: what a pull writes, and the state the next pull re-validates against.
//!
//! # Only what verified
//!
//! A pull writes through [`PulledAlbums::commit`] and nowhere else, and it commits a page only
//! once every entry of it has passed [`validate`](super::validate). So the port never holds an
//! entry that failed, and it holds the facts invariant 20 is re-applied against: the feed
//! position (`cursor`, the `sync_seq` high-water mark), each asset's chain head and the heads it
//! superseded (17), its AMK epoch (18) and the metadata blob its records carry (25).
//!
//! # A commit is conditional
//!
//! The worker's lease keeps two replicas off one job, but a lease can lapse under a slow pull.
//! A commit therefore names the high-water mark it was validated from and is refused when the
//! album has moved since, so a page validated against stale state is never written over a
//! fresher one.
//!
//! # A commit is bounded by every subscriber's caching budget on the peer
//!
//! What a pull holds is charged to the local accounts it is held for (design/quota.md). A job is
//! one album on one peer, shared by every local subscription to it, live or lapsed, so a
//! metadata blob a job holds is charged **in full to each of its subscribers**, never split
//! between them, and **once per subscriber** however many of that subscriber's albums on the
//! same peer hold it: a subscriber's usage on a peer is the size of the distinct blobs held by
//! the jobs it subscribes to there. [`PeerCachingBudget`] caps that usage per
//! `(receiving_user, source_peer)`. A commit that holds a blob new to some subscriber and leaves
//! that subscriber's usage past the budget is refused whole ([`Committed::OverBudget`]) and
//! writes nothing; a commit that holds nothing new to anyone, or only blobs a subscriber already
//! holds through another album, is never refused, so a lowered budget stalls growth and never
//! removes or blocks what is already held.
//!
//! # What lives where
//!
//! - This file — the port, its records and [`InMemoryPulledAlbums`], the deterministic double
//!   the puller's tests drive.
//! - [`postgres`] — [`PostgresPulledAlbums`](postgres::PostgresPulledAlbums), the durable
//!   adapter: rows beside the subscriptions, the metadata blobs in the blob store, and a hook
//!   ([`PulledBlobCharge`](postgres::PulledBlobCharge)) that charges what a commit newly holds
//!   inside the commit's own transaction; [`PeerBudgetCharge`](postgres::PeerBudgetCharge) is
//!   the one that enforces [`PeerCachingBudget`].
//! - [`conformance`] — the suite both adapters pass.
//!
//! Both adapters are also the [`BreadcrumbIndex`] over the entries they hold
//! ([`crate::federation::breadcrumbs`]): an asset's breadcrumb is read from its latest written
//! entry, so the index stores nothing of its own. Over it, both are [`PulledAlbumReads`], what a
//! subscriber's client reads of an album pulled for it: the breadcrumbs, and the metadata blob
//! each live asset carries.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;

use super::JobKey;
use crate::federation::PeerId;
use crate::federation::breadcrumbs::{
    Breadcrumb, BreadcrumbIndex, MAX_BREADCRUMB_PAGE, breadcrumb, leaves_a_breadcrumb,
};
use crate::federation::subscriptions::SubscriptionStore;
use crate::store::{StoreFuture, UserId};

pub mod conformance;
pub mod postgres;

/// How many superseded heads are kept per asset for the stale-revival check.
///
/// A manifest the album has moved past is refused when it reappears ([`AssetHead::superseded`]).
/// Sixty-four is far more history than a revival attack needs to be caught on the heads a pull
/// actually saw, and bounds what a peer that edits one asset forever can make this server keep.
pub const MAX_SUPERSEDED_HEADS: usize = 64;

/// One asset's chain position, as the last accepted pull left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetHead {
    /// The record hash of the last accepted provenance record: the value its successor names as
    /// `prior_provenance_hash`.
    pub head: Hash32,
    /// Heads this asset has moved past, newest last, at most [`MAX_SUPERSEDED_HEADS`].
    pub superseded: Vec<Hash32>,
    /// Whether the last accepted record is a `delete`.
    pub deleted: bool,
    /// The AMK epoch the last accepted record carries: the floor its successors are held to
    /// (invariant 18, along the asset's own chain).
    pub amk_version: u32,
    /// The metadata blob the asset's live record set carries: the one the last binding record
    /// committed to, which a record binding none (`derivative-*`, `trash-restore`, `delete`)
    /// keeps (invariant 25).
    pub metadata_blob: Option<Hash32>,
}

impl AssetHead {
    /// The asset after `next` is accepted on top of this head: `next`'s position, with this
    /// head's history behind it.
    pub fn advanced(&self, next: Self) -> Self {
        if next.head == self.head {
            return Self {
                superseded: self.superseded.clone(),
                ..next
            };
        }
        let mut superseded = self.superseded.clone();
        superseded.push(self.head);
        let excess = superseded.len().saturating_sub(MAX_SUPERSEDED_HEADS);
        superseded.drain(..excess);
        Self { superseded, ..next }
    }
}

/// One album's pull state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PulledAlbum {
    /// The cursor the next page is asked for after. `None` pulls from the start.
    pub cursor: Option<String>,
    /// The highest `sync_seq` accepted. Every later entry must be above it.
    pub high_water: Option<u64>,
    /// The highest `amk_version` an accepted manifest carried. A record, not a floor: the feed
    /// re-mints positions and replays from zero, so invariant 18 is held per asset
    /// ([`AssetHead::amk_version`]).
    pub amk_version: Option<u32>,
    /// Every asset an accepted entry named, by its id.
    pub assets: BTreeMap<String, AssetHead>,
}

/// What an accepted entry changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The asset appeared.
    Created,
    /// The asset changed.
    Updated,
    /// The asset was deleted.
    Deleted,
}

/// One blob an accepted entry names: its role on the wire, address and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledBlob {
    /// `original`, `metadata`, `derivative` or `provenance`.
    pub role: &'static str,
    /// Its content address.
    pub hash: Hash32,
    /// Its size in bytes.
    pub size: u64,
}

/// One entry that passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedEntry {
    /// The asset.
    pub asset_id: String,
    /// Its position in the album's feed.
    pub sync_seq: u64,
    /// What changed.
    pub change: Change,
    /// When the home server says it changed. Audit only.
    pub changed_at: Timestamp,
    /// The provenance record, verbatim. `None` only on a purged tombstone.
    pub manifest: Option<Vec<u8>>,
    /// The record's hash, the asset's chain head after this entry.
    pub head: Option<Hash32>,
    /// The metadata blob the asset carries after this entry: the one the manifest commits to,
    /// or, under a record that binds none, the one the asset's chain last bound. Its bytes are
    /// in this page, or were kept by an earlier one when the record keeps the held blob.
    pub metadata_blob: Option<Hash32>,
    /// The asset's blobs, by address. Their bytes are fetched when a member views them.
    pub blobs: Vec<PulledBlob>,
    /// The signed derivative manifests, verbatim.
    pub derivative_manifests: Vec<Vec<u8>>,
    /// Whether the original has landed on the home server.
    pub original_held: bool,
    /// Whether the asset has been purged.
    pub purged: bool,
}

/// One validated page, ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPage {
    /// The entries, in feed order.
    pub entries: Vec<AcceptedEntry>,
    /// Every metadata blob the entries commit to, verified against its address.
    pub metadata_blobs: BTreeMap<Hash32, Vec<u8>>,
    /// The album's state once this page is applied, the cursor included.
    pub state: PulledAlbum,
}

/// The default per-`(receiving_user, source_peer)` caching budget, in percent of the
/// receiver's hard quota limit.
pub const DEFAULT_PEER_BUDGET_PERCENT: u8 = 25;

/// How many bytes of pulled data one local account may hold from one source peer.
///
/// Usage is counted as the [module docs](self) say: every metadata blob the account's albums on
/// the peer hold, each distinct blob once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCachingBudget {
    bytes: u64,
}

impl PeerCachingBudget {
    /// A budget of exactly `bytes`.
    pub const fn bytes(bytes: u64) -> Self {
        Self { bytes }
    }

    /// The budget that never refuses: what a store assembled without one runs.
    pub const fn unlimited() -> Self {
        Self::bytes(u64::MAX)
    }

    /// `percent` of `hard_limit`, rounded down, with `percent` past 100 read as 100.
    ///
    /// An unlimited deployment (`hard_limit = u64::MAX`) gets a budget nothing reaches, which
    /// runs the same predicate a limited one does.
    pub fn of_hard_limit(hard_limit: u64, percent: u8) -> Self {
        let percent = u128::from(percent.min(100));
        let bytes = u128::from(hard_limit) * percent / 100;
        Self::bytes(u64::try_from(bytes).unwrap_or(u64::MAX))
    }

    /// The budget in bytes.
    pub const fn limit(self) -> u64 {
        self.bytes
    }

    /// Whether a commit that leaves `subscriber` using `used` bytes on `peer`, `added` of them
    /// new to it, is refused, and if so the refusal.
    ///
    /// Only growth is refused: a commit that adds nothing to the subscriber is admitted however
    /// far past the budget the subscriber already is.
    pub fn refusal(
        self,
        subscriber: &UserId,
        peer: &PeerId,
        used: u64,
        added: u64,
    ) -> Option<OverBudget> {
        (added > 0 && used > self.bytes).then(|| OverBudget {
            subscriber: subscriber.clone(),
            peer: peer.clone(),
            used,
            budget: self.bytes,
        })
    }
}

impl Default for PeerCachingBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Why a commit was refused for a caching budget: the first subscriber it would take past one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverBudget {
    /// The local account whose budget the page would exceed.
    pub subscriber: UserId,
    /// The source peer the budget is on.
    pub peer: PeerId,
    /// The bytes the subscriber would use on the peer had the page been written.
    pub used: u64,
    /// The budget it may not pass.
    pub budget: u64,
}

/// What [`PulledAlbums::commit`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Committed {
    /// The page is written.
    Written,
    /// The album moved since the page was validated; nothing was written.
    Stale,
    /// The page would take a subscriber past its caching budget on the peer; nothing was
    /// written.
    OverBudget(OverBudget),
}

/// Where pulled albums are kept.
pub trait PulledAlbums: std::fmt::Debug + Send + Sync {
    /// `job`'s state; the default (nothing pulled yet) when there is none.
    fn read<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, PulledAlbum>;

    /// Write `page` for `job`, if the album's high-water mark is still `from` and the page takes
    /// none of the job's subscribers past its caching budget on the peer.
    ///
    /// Atomic: the entries, the blobs and the new state land together or not at all. A refusal,
    /// [`Committed::Stale`] or [`Committed::OverBudget`], writes nothing and removes nothing.
    fn commit<'a>(
        &'a self,
        job: &'a JobKey,
        from: Option<u64>,
        page: ValidatedPage,
    ) -> StoreFuture<'a, Committed>;

    /// Forget `job`'s cursor and high-water mark, keeping every asset head it holds.
    ///
    /// What a `409 error.sync.cursor_stale` asks for: the home server's index was rebuilt, so the
    /// feed restarts from zero, and what this server holds is re-validated against the heads it
    /// already accepted rather than discarded.
    fn restart<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, ()>;
}

/// What a member's client reads of an album this server pulled for it: the album's
/// breadcrumbs, and the metadata blob each of its live assets carries.
///
/// Read from the entries a pull committed and the blobs it held, and from nothing else, so a
/// reader only ever sees input that passed re-validation. An asset is readable exactly when it
/// has a breadcrumb ([`leaves_a_breadcrumb`] on its latest entry): a deleted or purged asset
/// has neither. Who may read a job is the route's decision, never the port's.
pub trait PulledAlbumReads: BreadcrumbIndex {
    /// The bytes of the metadata blob `asset_id`'s latest entry in `job` carries; `None` when
    /// that entry leaves no breadcrumb, binds no blob, `job` never named the asset, or `job`
    /// does not hold the blob. A blob `job` holds whose bytes are gone is an error, never
    /// `None`: it is this server's fault, not an absence to answer a client with.
    fn asset_metadata<'a>(
        &'a self,
        job: &'a JobKey,
        asset_id: &'a str,
    ) -> StoreFuture<'a, Option<Vec<u8>>>;
}

/// The deterministic double of [`PulledAlbums`].
///
/// Charges nothing unless assembled [`with_budget`](Self::with_budget). Its subscriptions are
/// read before its lock is taken, so a subscription made during a commit may be missed by it;
/// the durable adapter reads them inside the commit's transaction.
#[derive(Debug, Default)]
pub struct InMemoryPulledAlbums {
    albums: Mutex<BTreeMap<JobKey, Held>>,
    budget: Option<(Arc<dyn SubscriptionStore>, PeerCachingBudget)>,
}

#[derive(Debug, Default, Clone)]
struct Held {
    state: PulledAlbum,
    entries: Vec<AcceptedEntry>,
    metadata_blobs: BTreeMap<Hash32, Vec<u8>>,
}

impl InMemoryPulledAlbums {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// This store, refusing a commit that takes a subscriber `subscriptions` names past
    /// `budget` on the job's peer.
    #[must_use]
    pub fn with_budget(
        mut self,
        subscriptions: Arc<dyn SubscriptionStore>,
        budget: PeerCachingBudget,
    ) -> Self {
        self.budget = Some((subscriptions, budget));
        self
    }

    /// Every entry written for `job`, in the order written.
    pub fn entries(&self, job: &JobKey) -> Vec<AcceptedEntry> {
        self.lock()
            .get(job)
            .map(|held| held.entries.clone())
            .unwrap_or_default()
    }

    /// The metadata blob at `hash` written for `job`.
    pub fn metadata_blob(&self, job: &JobKey, hash: &Hash32) -> Option<Vec<u8>> {
        self.lock()
            .get(job)
            .and_then(|held| held.metadata_blobs.get(hash).cloned())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<JobKey, Held>> {
        self.albums.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every subscriber of `job`, each with the other jobs it subscribes to on `job`'s peer.
    async fn subscribers(
        subscriptions: &dyn SubscriptionStore,
        job: &JobKey,
    ) -> Result<Vec<(UserId, Vec<JobKey>)>, crate::store::StoreError> {
        let mut subscribers = Vec::new();
        for subscription in subscriptions.of_job(job).await? {
            let others = subscriptions
                .of_subscriber(&subscription.subscriber)
                .await?
                .into_iter()
                .map(|held| held.job())
                .filter(|other| other.peer == job.peer && other != job)
                .collect();
            subscribers.push((subscription.subscriber, others));
        }
        Ok(subscribers)
    }
}

/// The first of `subscribers` that holding `page` for `job` takes past `budget`, counted as
/// [`PulledAlbums::commit`] documents.
fn over_budget(
    albums: &BTreeMap<JobKey, Held>,
    job: &JobKey,
    page: &ValidatedPage,
    subscribers: &[(UserId, Vec<JobKey>)],
    budget: PeerCachingBudget,
) -> Option<OverBudget> {
    let held = albums.get(job).map(|held| &held.metadata_blobs);
    let newly: Vec<&Hash32> = page
        .metadata_blobs
        .keys()
        .filter(|hash| held.is_none_or(|held| !held.contains_key(*hash)))
        .collect();
    if newly.is_empty() {
        return None;
    }
    let size = |bytes: &Vec<u8>| u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    for (subscriber, others) in subscribers {
        let mut blobs: BTreeMap<&Hash32, u64> = BTreeMap::new();
        for other in others {
            if let Some(other) = albums.get(other) {
                blobs.extend(
                    other
                        .metadata_blobs
                        .iter()
                        .map(|(hash, bytes)| (hash, size(bytes))),
                );
            }
        }
        let elsewhere: BTreeSet<&Hash32> = blobs.keys().copied().collect();
        if let Some(held) = held {
            blobs.extend(held.iter().map(|(hash, bytes)| (hash, size(bytes))));
        }
        blobs.extend(
            page.metadata_blobs
                .iter()
                .map(|(hash, bytes)| (hash, size(bytes))),
        );
        let added = newly
            .iter()
            .filter(|hash| !elsewhere.contains(**hash))
            .map(|hash| blobs.get(*hash).copied().unwrap_or(0))
            .fold(0_u64, u64::saturating_add);
        let used = blobs.values().copied().fold(0_u64, u64::saturating_add);
        if let Some(refusal) = budget.refusal(subscriber, &job.peer, used, added) {
            return Some(refusal);
        }
    }
    None
}

impl PulledAlbums for InMemoryPulledAlbums {
    fn read<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, PulledAlbum> {
        Box::pin(async move {
            Ok(self
                .lock()
                .get(job)
                .map(|held| held.state.clone())
                .unwrap_or_default())
        })
    }

    fn commit<'a>(
        &'a self,
        job: &'a JobKey,
        from: Option<u64>,
        page: ValidatedPage,
    ) -> StoreFuture<'a, Committed> {
        Box::pin(async move {
            let subscribers = match &self.budget {
                Some((subscriptions, _)) => Self::subscribers(subscriptions.as_ref(), job).await?,
                None => Vec::new(),
            };
            let mut albums = self.lock();
            let current = albums.get(job).and_then(|held| held.state.high_water);
            if current != from {
                tracing::debug!(%job, ?from, ?current, "a pulled page was validated against a stale position");
                return Ok(Committed::Stale);
            }
            if let Some((_, budget)) = &self.budget
                && let Some(refusal) = over_budget(&albums, job, &page, &subscribers, *budget)
            {
                tracing::info!(%job, subscriber = %refusal.subscriber, used = refusal.used, budget = refusal.budget, "a pulled page would pass a subscriber's caching budget; nothing was written");
                return Ok(Committed::OverBudget(refusal));
            }
            let held = albums.entry(job.clone()).or_default();
            held.state = page.state;
            held.entries.extend(page.entries);
            held.metadata_blobs.extend(page.metadata_blobs);
            Ok(Committed::Written)
        })
    }

    fn restart<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            if let Some(held) = self.lock().get_mut(job) {
                held.state.cursor = None;
                held.state.high_water = None;
            }
            Ok(())
        })
    }
}

impl BreadcrumbIndex for InMemoryPulledAlbums {
    fn breadcrumbs<'a>(
        &'a self,
        job: &'a JobKey,
        after: Option<&'a str>,
        limit: usize,
    ) -> StoreFuture<'a, Vec<Breadcrumb>> {
        Box::pin(async move {
            let latest: Vec<AcceptedEntry> = {
                let albums = self.lock();
                let Some(held) = albums.get(job) else {
                    return Ok(Vec::new());
                };
                let mut latest: BTreeMap<&str, &AcceptedEntry> = BTreeMap::new();
                for entry in &held.entries {
                    latest.insert(entry.asset_id.as_str(), entry);
                }
                latest
                    .into_iter()
                    .filter(|(asset, _)| after.is_none_or(|after| *asset > after))
                    .map(|(_, entry)| entry)
                    .filter(|entry| leaves_a_breadcrumb(entry))
                    .take(limit.min(MAX_BREADCRUMB_PAGE))
                    .cloned()
                    .collect()
            };
            latest
                .iter()
                .filter_map(|entry| breadcrumb(job, entry).transpose())
                .collect()
        })
    }
}

impl PulledAlbumReads for InMemoryPulledAlbums {
    fn asset_metadata<'a>(
        &'a self,
        job: &'a JobKey,
        asset_id: &'a str,
    ) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let albums = self.lock();
            let Some(held) = albums.get(job) else {
                return Ok(None);
            };
            Ok(held
                .entries
                .iter()
                .rev()
                .find(|entry| entry.asset_id == asset_id)
                .filter(|entry| leaves_a_breadcrumb(entry))
                .and_then(|entry| entry.metadata_blob)
                .and_then(|hash| held.metadata_blobs.get(&hash).cloned()))
        })
    }
}

/// What the in-memory quota ledger counts of this store: the distinct metadata blobs held by
/// every job `user` subscribes to, live or lapsed, on every peer — the fact the durable ledger's
/// usage statement joins `federation_subscriptions` with `federation_pulled_metadata_blobs` for.
///
/// Only a store assembled [`with_budget`](InMemoryPulledAlbums::with_budget) knows its
/// subscribers; one assembled without holds nothing for anyone. The subscriptions are read
/// before the lock is taken, as [`PulledAlbums::commit`] reads them.
impl crate::quota::PulledHoldings for InMemoryPulledAlbums {
    fn held_by<'a>(
        &'a self,
        user: &'a UserId,
    ) -> StoreFuture<'a, BTreeMap<crate::blob::ContentAddress, u64>> {
        Box::pin(async move {
            let Some((subscriptions, _)) = &self.budget else {
                return Ok(BTreeMap::new());
            };
            let jobs: BTreeSet<JobKey> = subscriptions
                .of_subscriber(user)
                .await?
                .iter()
                .map(crate::federation::Subscription::job)
                .collect();
            let albums = self.lock();
            let mut held = BTreeMap::new();
            for album in jobs.iter().filter_map(|job| albums.get(job)) {
                for (hash, bytes) in &album.metadata_blobs {
                    let address =
                        crate::blob::ContentAddress::parse(&hash.to_hex()).map_err(|error| {
                            crate::store::StoreError::Rejected {
                                store: "pulled albums",
                                detail: format!("{hash} is not a content address: {error}"),
                            }
                        })?;
                    held.insert(address, u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                }
            }
            tracing::trace!(%user, jobs = jobs.len(), blobs = held.len(), "pulled holdings read for the quota ledger");
            Ok(held)
        })
    }
}

impl conformance::Inspect for InMemoryPulledAlbums {
    fn written_entries<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, Vec<AcceptedEntry>> {
        Box::pin(async move { Ok(self.entries(job)) })
    }

    fn held_metadata_blob<'a>(
        &'a self,
        job: &'a JobKey,
        hash: &'a Hash32,
    ) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move { Ok(self.metadata_blob(job, hash)) })
    }
}

#[cfg(test)]
mod tests;
