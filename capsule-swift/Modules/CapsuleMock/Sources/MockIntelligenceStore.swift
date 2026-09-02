import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockIntelligenceStore

/// The on-device machine-learning surfaces: model slots, people, places, and
/// search.
///
/// Grouped into one actor because they share one invariant — **AI output
/// containment**. Model output lives in its own namespace, scoped to a
/// `(model_id, model_version)` slot, and is never compared across versions. So a
/// slot's status is a first-class read here, "stale" is a normal reportable
/// state rather than an error, and removing a model removes everything derived
/// from it. Splitting these across four actors would let one of them forget.
public actor MockIntelligenceStore {
    private nonisolated let store: MockLibraryStore
    nonisolated let storeConfiguration: MockConfiguration
    private var slots: [ModelSlot: AIModelStatus]
    private var slotOrder: [ModelSlot]
    private var isEnabled: Bool
    private var clusterNames: [PersonID: Lww<String>] = [:]
    private var hiddenClusters: Set<PersonID> = []
    private var mergedInto: [PersonID: PersonID] = [:]
    /// Assets pulled out of a cluster the grouping got wrong, by the cluster
    /// they were pulled out of.
    private var splitAway: [PersonID: Set<AssetID>] = [:]
    /// Clusters that exist only because a user split them out, with their
    /// members. Membership is arithmetic for derived clusters, so a split has to
    /// be recorded rather than computed.
    private var splitClusters: [PersonID: [AssetID]] = [:]
    private var recentSearchTerms: [String] = []

    nonisolated let modelChanges = ChangeBroadcaster<[AIModelStatus]>()
    nonisolated let peopleChanges = ChangeBroadcaster<Void>()

    public init(store: MockLibraryStore, configuration: MockConfiguration) {
        self.store = store
        storeConfiguration = configuration
        isEnabled = true
        let seeded = Self.seedSlots(configuration: configuration)
        slotOrder = seeded.map(\.slot)
        slots = Dictionary(uniqueKeysWithValues: seeded.map { ($0.slot, $0) })
    }

    /// The four slots, in the order a settings screen lists them.
    ///
    /// ``AIModelStatus/Availability/notDownloaded`` is a **normal steady
    /// state** — weights are never in the repository — so one slot starts that
    /// way on purpose. Every AI surface has to render usefully without it, and a
    /// mock where everything is ready never proves that.
    private static func seedSlots(configuration: MockConfiguration) -> [AIModelStatus] {
        let pending = max(0, configuration.profile.assetCount / 40)
        return [
            AIModelStatus(
                slot: MockTables.sceneTaggingSlot,
                purpose: .sceneTagging,
                availability: .ready,
                pendingAssetCount: pending
            ),
            AIModelStatus(
                slot: MockTables.staleTaggingSlot,
                purpose: .sceneTagging,
                availability: .supersededBy(MockTables.sceneTaggingSlot)
            ),
            AIModelStatus(
                slot: MockTables.faceEmbeddingSlot,
                purpose: .faceEmbedding,
                availability: .ready,
                pendingAssetCount: pending / 3
            ),
            AIModelStatus(
                slot: MockTables.imageEmbeddingSlot,
                purpose: .imageEmbedding,
                availability: .notDownloaded
            ),
        ]
    }

    var seed: UInt64 { storeConfiguration.seed }
    var libraryStore: MockLibraryStore { store }

    // MARK: Slot state

    var statusList: [AIModelStatus] {
        slotOrder.compactMap { slots[$0] }
    }

    func setStatus(_ status: AIModelStatus) {
        slots[status.slot] = status
    }

    func removeSlot(_ slot: ModelSlot) {
        slots[slot] = AIModelStatus(
            slot: slot,
            purpose: slots[slot]?.purpose ?? .sceneTagging,
            availability: .notDownloaded
        )
    }

    var processingEnabled: Bool { isEnabled }

    func updateProcessingEnabled(_ enabled: Bool) {
        isEnabled = enabled
    }

    // MARK: People state

    func name(for identifier: PersonID) -> Lww<String>? {
        clusterNames[identifier]
    }

    func updateName(_ register: Lww<String>, for identifier: PersonID) {
        clusterNames[identifier] = register
    }

    func isHidden(_ identifier: PersonID) -> Bool {
        hiddenClusters.contains(identifier)
    }

    func updateHidden(_ hidden: Bool, for identifier: PersonID) {
        if hidden { hiddenClusters.insert(identifier) } else { hiddenClusters.remove(identifier) }
    }

    func mergeTarget(of identifier: PersonID) -> PersonID? {
        mergedInto[identifier]
    }

    func recordMerge(_ identifier: PersonID, into target: PersonID) {
        mergedInto[identifier] = target
    }

    var mergedAway: Set<PersonID> {
        Set(mergedInto.keys)
    }

    func splitAwayAssets(from identifier: PersonID) -> Set<AssetID> {
        splitAway[identifier] ?? []
    }

    func splitClusterMembers(_ identifier: PersonID) -> [AssetID]? {
        splitClusters[identifier]
    }

    var splitClusterIDs: [PersonID] {
        splitClusters.keys.sorted { $0.rawValue < $1.rawValue }
    }

    func recordSplit(_ assetIDs: [AssetID], from source: PersonID, into created: PersonID) {
        splitAway[source, default: []].formUnion(assetIDs)
        splitClusters[created] = assetIDs
    }

    // MARK: Search state

    var recentSearchHistory: [String] { recentSearchTerms }

    func recordSearch(_ text: String) {
        guard !text.isEmpty else { return }
        recentSearchTerms.removeAll { $0 == text }
        recentSearchTerms.insert(text, at: 0)
        recentSearchTerms = Array(recentSearchTerms.prefix(12))
    }

    func clearSearches() {
        recentSearchTerms.removeAll()
    }
}
