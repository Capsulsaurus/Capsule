import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - QuarantineGroup

/// One of the eight surfaces, with the items held on it.
public struct QuarantineGroup: Sendable, Equatable, Identifiable {
    public var surface: QuarantineSurface
    public var items: [QuarantineItem]

    public var id: String { surface.rawValue }

    public init(surface: QuarantineSurface, items: [QuarantineItem]) {
        self.surface = surface
        self.items = items
    }
}

// MARK: - QuarantineInboxModel

/// Drives ``QuarantineInboxView``.
///
/// Grouped by the **eight** surfaces of the threat model's own table, in that
/// table's order. The union exists so the UI surface and the operator audit
/// share one inventory of "things that need a human to look at"; a ninth group
/// here would be a category no owner doc defends.
///
/// The empty state is the **good** state: nothing held means nothing was
/// silently dropped and nothing was silently applied.
@MainActor
@Observable
public final class QuarantineInboxModel {
    /// A page big enough that triage rarely pages, small enough not to
    /// materialise an audit log.
    public static let pageSize = 100

    public private(set) var phase: ScreenPhase = .loading
    /// Non-empty groups, in the threat model's table order.
    public private(set) var groups: [QuarantineGroup] = []
    /// Capture dates and LQIP colours for the asset-scoped rows.
    public private(set) var assets: [AssetID: LibraryAsset] = [:]
    public private(set) var totalCount = 0
    public private(set) var connection: ConnectionClass = .unmetered

    private let quarantine: any QuarantinePort
    private let library: any LibraryPort
    private let sync: any SyncPort
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(quarantine: any QuarantinePort, library: any LibraryPort, sync: any SyncPort) {
        self.quarantine = quarantine
        self.library = library
        self.sync = sync
    }

    deinit {
        observation?.cancel()
    }

    // MARK: Derived

    /// How many of the eight surfaces this build monitors. Stated on screen so
    /// an empty inbox reads as "eight checks, nothing held" rather than
    /// "nothing here, possibly because nothing is watching".
    public var monitoredSurfaceCount: Int { QuarantineSurface.knownCases.count }

    public func asset(for item: QuarantineItem) -> LibraryAsset? {
        guard let assetID = item.assetID else { return nil }
        return assets[.managed(uuid: assetID)]
    }

    // MARK: Loading

    public func load() async {
        await reload()
        observeChanges()
    }

    public func reload() async {
        do {
            connection = await (try? sync.status().connectionClass) ?? connection
            let page = try await quarantine.items(offset: 0, limit: Self.pageSize)
            totalCount = try await quarantine.itemCount()
            await apply(page.items)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    private func apply(_ items: [QuarantineItem]) async {
        // Canonical table order, not arrival order: the eight rows are a list a
        // reader learns, and re-ordering them per device would defeat that.
        groups = QuarantineSurface.knownCases.compactMap { surface in
            let onSurface = items
                .filter { $0.surface == surface }
                .sorted { $0.detectedAt > $1.detectedAt }
            return onSurface.isEmpty ? nil : QuarantineGroup(surface: surface, items: onSurface)
        }
        let unknownItems = items.filter { !QuarantineSurface.knownCases.contains($0.surface) }
        if !unknownItems.isEmpty {
            // A surface from a newer build is surfaced verbatim rather than
            // dropped — dropping it would be the exact failure this inbox exists
            // to prevent.
            groups.append(QuarantineGroup(surface: unknownItems[0].surface, items: unknownItems))
        }
        assets = await resolveAssets(for: items)
        phase = groups.isEmpty ? .empty : .ready
    }

    private func resolveAssets(for items: [QuarantineItem]) async -> [AssetID: LibraryAsset] {
        let ids = Set(items.compactMap(\.assetID).map { AssetID.managed(uuid: $0) })
        guard !ids.isEmpty, let resolved = try? await library.assets(for: Array(ids)) else { return [:] }
        return Dictionary(resolved.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
    }

    private func observeChanges() {
        observation?.cancel()
        let stream = quarantine.changes()
        observation = Task { [weak self] in
            for await _ in stream {
                guard !Task.isCancelled else { return }
                await self?.reload()
            }
        }
    }
}
