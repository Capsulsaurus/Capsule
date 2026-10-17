import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - AIPort

/// On-device machine learning.
///
/// Everything here runs locally. The design constraint that shapes the protocol
/// is **AI output containment**: model output lives in its own namespace, is
/// scoped to a `(model_id, model_version)` slot, and is never compared across
/// versions. So a slot's status is a first-class read, and "stale" is a normal
/// reportable state rather than an error.
public protocol AIPort: Sendable {
    /// Every model slot and its availability.
    ///
    /// ``AIModelStatus/Availability/notDownloaded`` is a normal steady state —
    /// weights are never in the repository — so every AI surface must render
    /// usefully without them.
    ///
    /// Maps to `ai.model_status`.
    func modelStatuses() async throws -> [AIModelStatus]

    /// Fetch a model's weights, streaming progress.
    ///
    /// Maps to `ai.download_model`.
    func downloadModel(slot: ModelSlot) -> AsyncStream<AIModelStatus>

    /// Delete a model's weights and everything derived from that slot — the
    /// only honest way to undo it, since output from a slot with no model is
    /// unverifiable.
    ///
    /// Maps to `ai.remove_model`.
    func removeModel(slot: ModelSlot) async throws

    /// Whether on-device processing is enabled.
    ///
    /// Maps to `settings.get_ai_enabled`.
    func isProcessingEnabled() async -> Bool

    /// Enable or disable on-device processing.
    ///
    /// Maps to `settings.set_ai_enabled`.
    func setProcessingEnabled(_ enabled: Bool) async throws

    /// Re-run a slot over assets whose output is stale after a model change.
    ///
    /// Maps to `ai.regenerate_slot`.
    func regenerate(slot: ModelSlot) -> AsyncStream<AIModelStatus>

    /// A stream of model-status updates.
    func changes() -> AsyncStream<[AIModelStatus]>
}

// MARK: - PeoplePort

/// Face clusters.
public protocol PeoplePort: Sendable {
    /// Every cluster, most populous first.
    ///
    /// Stale clusters — those whose slot's canonical model changed — are
    /// included with ``PersonCluster/isStale`` set rather than omitted, because
    /// silently hiding a named person is worse than showing them as pending
    /// regeneration.
    ///
    /// Maps to `people.list_clusters`.
    func clusters(offset: Int, limit: Int) async throws -> Page<PersonCluster>

    /// One cluster.
    ///
    /// Maps to `people.get_cluster`.
    func cluster(_ id: PersonID) async throws -> PersonCluster?

    /// The assets in a cluster.
    ///
    /// Maps to `people.cluster_assets`.
    func assets(in id: PersonID, offset: Int, limit: Int) async throws -> Page<LibraryAsset>

    /// Name a cluster. An LWW write, so naming from two devices converges.
    ///
    /// Maps to `people.set_name`.
    func setName(_ name: String?, for id: PersonID) async throws

    /// Merge clusters that are the same person.
    ///
    /// Only valid **within one model slot** — merging across slots would be the
    /// cross-model comparison the containment rule forbids.
    ///
    /// Maps to `people.merge_clusters`.
    func merge(_ ids: [PersonID], into target: PersonID) async throws

    /// Split assets out of a cluster the grouping got wrong.
    ///
    /// Maps to `people.split_cluster`.
    func split(assetIDs: [AssetID], from id: PersonID) async throws -> PersonID

    /// Hide a cluster from the People surface. A view-layer choice; it removes
    /// nothing.
    ///
    /// Maps to `people.set_hidden`.
    func setHidden(_ hidden: Bool, for id: PersonID) async throws

    /// A stream that fires when clustering output changes.
    func changes() -> AsyncStream<Void>
}

// MARK: - PlacesPort

/// The map surface.
public protocol PlacesPort: Sendable {
    /// Clusters inside a region, at a zoom-appropriate granularity.
    ///
    /// Coordinates come back **in their stored datum** and are never converted
    /// at rest. A GCJ-02 cluster rendered on a WGS-84 map must be marked
    /// approximate — the inverse is lossy, and an unmarked pin is a wrong pin.
    ///
    /// Maps to `places.clusters_in_region`.
    func clusters(in region: MapRegion, granularity: Int) async throws -> [PlaceCluster]

    /// The assets in a cluster.
    ///
    /// Maps to `places.cluster_assets`.
    func assets(in clusterID: String, offset: Int, limit: Int) async throws -> Page<LibraryAsset>

    /// The bounding region containing every located asset, for an initial
    /// camera position. `nil` when nothing is located.
    ///
    /// Maps to `places.bounding_region`.
    func boundingRegion() async throws -> MapRegion?
}

// MARK: - SearchPort

/// Library search.
public protocol SearchPort: Sendable {
    /// Run a search.
    ///
    /// A term over a model-scoped facet whose slot changed evaluates as
    /// **stale-excluded** rather than being compared across model versions — so
    /// results can legitimately shrink after a model upgrade, and the UI should
    /// say so instead of implying assets vanished.
    ///
    /// Maps to `search.query`.
    func search(
        _ text: String,
        scope: SearchScope,
        offset: Int,
        limit: Int
    ) async throws -> Page<SearchResult>

    /// Completion candidates for a partial query — tags, names, places.
    ///
    /// Maps to `search.suggest`.
    func suggestions(for partial: String, limit: Int) async throws -> [String]

    /// This device's recent searches. Local-only; never synced, because a search
    /// history is a far more sensitive record than the photos it searched.
    ///
    /// Maps to `search.recent`.
    func recentSearches() async throws -> [String]

    /// Clear the local search history.
    ///
    /// Maps to `search.clear_recent`.
    func clearRecentSearches() async throws
}
