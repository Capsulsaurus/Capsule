import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - MockLibraryStore

/// The library graph: the derived assets, the user's edits over them, the
/// container albums they live in, and the smart-album definitions computed over
/// them.
///
/// One actor because it is one consistency boundary. Moving an asset changes an
/// album's count; trashing one changes the timeline, the trash view, and two day
/// counts; deleting an album has to refuse when it is the designated default.
/// Splitting those across actors would mean a screen could observe an album
/// whose count disagreed with the assets it listed — the exact class of bug a
/// mock is supposed to make impossible rather than reproduce.
///
/// Writes are real. Every mutation lands in the overlay, is visible to every
/// later read, and emits on the streams that care.
public actor MockLibraryStore {
    public nonisolated let library: MockLibrary
    public nonisolated let configuration: MockConfiguration

    private var overlay = MockOverlay()
    private var containers: [AlbumID: ContainerAlbum]
    private var containerOrder: [AlbumID]
    private var defaultAlbumID: AlbumID
    private var smartAlbums: [SmartAlbumID: SmartAlbumDefinition]
    private var smartAlbumOrder: [SmartAlbumID]
    private var derivedAlbumCounts: [AlbumID: Int]?
    private var scopeOverrides: [ImportScope: AlbumID] = [:]

    nonisolated let libraryChanges = ChangeBroadcaster<LibraryChange>()
    nonisolated let albumChanges = ChangeBroadcaster<Void>()
    nonisolated let smartAlbumChanges = ChangeBroadcaster<Void>()

    public init(configuration: MockConfiguration) {
        self.configuration = configuration
        library = MockLibrary(profile: configuration.profile)
        defaultAlbumID = MockIdentifiers.albumID(seed: configuration.seed, ordinal: 0)
        let seededAlbums = MockAlbumSeed.containers(configuration: configuration, library: library)
        containers = Dictionary(uniqueKeysWithValues: seededAlbums.map { ($0.id, $0) })
        containerOrder = seededAlbums.map(\.id)
        let seededSmartAlbums = MockSmartAlbumSeed.definitions(configuration: configuration)
        smartAlbums = Dictionary(uniqueKeysWithValues: seededSmartAlbums.map { ($0.smartAlbumID, $0) })
        smartAlbumOrder = seededSmartAlbums.map(\.smartAlbumID)
    }

    /// The clock every deadline in this store is measured against.
    var now: CapsuleTimestamp { configuration.clock.now }

    /// A snapshot of the derived library plus the current edits, for evaluating
    /// one query. Taken by value so a long scan does not hold the actor.
    var engine: MockQueryEngine {
        MockQueryEngine(library: library, overlay: overlay, now: now)
    }

    /// The device this replica writes as — the author of every stamped edit and
    /// the tiebreaker every LWW register orders on.
    var authoringDevice: DeviceID {
        MockTagIdentity.authoringDevice(seed: configuration.seed)
    }

    // MARK: Mutation plumbing

    /// Edit one asset and announce it.
    ///
    /// Every write goes through here so no mutation can forget to notify. The
    /// notification names the affected day rather than describing the change:
    /// a diff computed here would have to assume what window the reader is
    /// showing and would be wrong for every other reader.
    func mutate(_ identifier: AssetID, _ body: (inout MockAssetPatch) -> Void) async {
        overlay.edit(identifier, body)
        derivedAlbumCounts = nil
        await libraryChanges.send(.assetsChanged(dayKeys: dayKeys(for: [identifier])))
    }

    /// Edit several assets and announce them once.
    func mutate(_ identifiers: [AssetID], _ body: (inout MockAssetPatch) -> Void) async {
        for identifier in identifiers {
            overlay.edit(identifier, body)
        }
        derivedAlbumCounts = nil
        await libraryChanges.send(.assetsChanged(dayKeys: dayKeys(for: identifiers)))
    }

    /// Announce a change whose extent cannot be narrowed to a set of days.
    func announceReload() async {
        derivedAlbumCounts = nil
        await libraryChanges.send(.reload)
        await libraryChanges.send(.dayCountsChanged)
    }

    /// The sections an edit touched, so a grid can invalidate two rather than
    /// the whole timeline.
    private func dayKeys(for identifiers: [AssetID]) -> Set<DayKey> {
        var keys = Set<DayKey>()
        for identifier in identifiers {
            guard let ref = MockAssetRef.decode(identifier), library.contains(ref) else { continue }
            keys.insert(library.dayKey(forDay: library.captureInstant(for: ref).dayIndex))
        }
        return keys
    }

    /// Record the outcome of a representation fetch or release.
    ///
    /// Lives here rather than in the transfer store because the ladder is part
    /// of the asset, and two actors owning one field is how a grid ends up
    /// drawing a tier the device no longer holds.
    func applyFetchOutcome(
        _ identifier: AssetID,
        representations: LocalRepresentations,
        state: AssetSyncState
    ) async {
        await mutate(identifier) { patch in
            patch.representations = representations
            patch.syncState = state
        }
    }

    /// Read the current edits, for the ports that need to inspect them.
    var currentOverlay: MockOverlay { overlay }

    /// Issue an OR-set add id. Monotonic per store, never reset.
    func issueAddID() -> AddID {
        overlay.nextAddID(device: authoringDevice)
    }

    // MARK: Albums

    var albumList: [ContainerAlbum] {
        containerOrder.compactMap { identifier in
            guard var album = containers[identifier] else { return nil }
            album.count = albumCount(identifier)
            album.isDefault = identifier == defaultAlbumID
            return album
        }
    }

    var designatedDefaultAlbumID: AlbumID { defaultAlbumID }

    func container(_ identifier: AlbumID) -> ContainerAlbum? {
        albumList.first { $0.id == identifier }
    }

    func setDefaultAlbum(_ identifier: AlbumID) {
        defaultAlbumID = identifier
    }

    func insertContainer(_ album: ContainerAlbum) {
        containers[album.id] = album
        containerOrder.append(album.id)
    }

    func updateContainer(_ identifier: AlbumID, _ body: (inout ContainerAlbum) -> Void) {
        guard var album = containers[identifier] else { return }
        body(&album)
        containers[identifier] = album
    }

    func removeContainer(_ identifier: AlbumID) {
        containers[identifier] = nil
        containerOrder.removeAll { $0 == identifier }
    }

    /// How many assets an album holds.
    ///
    /// Counted once over the derived album assignment and then cached until the
    /// next write. The scan is one hash per asset — no capture instant, no
    /// allocation — which is why it stays viable at 250 000.
    func albumCount(_ identifier: AlbumID) -> Int {
        if let cached = derivedAlbumCounts { return cached[identifier] ?? 0 }
        var counts: [AlbumID: Int] = [:]
        let engine = self.engine
        for index in 0 ..< library.assetCount where !engine.isSuppressed(liveIndex: index) {
            let ordinal = library.albumOrdinal(derivationIndex: index)
            let derived = MockIdentifiers.albumID(seed: configuration.seed, ordinal: ordinal)
            let moved = overlay.patch(for: library.identifier(at: index))?.albumID
            counts[moved ?? derived, default: 0] += 1
        }
        derivedAlbumCounts = counts
        return counts[identifier] ?? 0
    }

    // MARK: Smart albums

    var smartAlbumList: [SmartAlbumDefinition] {
        smartAlbumOrder.compactMap { smartAlbums[$0] }
    }

    func smartAlbum(_ identifier: SmartAlbumID) -> SmartAlbumDefinition? {
        smartAlbums[identifier]
    }

    func putSmartAlbum(_ definition: SmartAlbumDefinition) {
        if smartAlbums[definition.smartAlbumID] == nil {
            smartAlbumOrder.append(definition.smartAlbumID)
        }
        smartAlbums[definition.smartAlbumID] = definition
    }

    /// Delete a definition.
    ///
    /// A tombstone in the register rather than a row removal in the real system;
    /// here the ordering entry is kept so a later re-`save` of the same id lands
    /// back in the same position, which is the observable half of that.
    func removeSmartAlbum(_ identifier: SmartAlbumID) {
        smartAlbums[identifier] = nil
    }

    // MARK: Scope overrides

    var recordedScopeOverrides: [ImportScope: AlbumID] { scopeOverrides }

    func setScopeOverride(_ albumID: AlbumID?, for scope: ImportScope) {
        scopeOverrides[scope] = albumID
    }
}
