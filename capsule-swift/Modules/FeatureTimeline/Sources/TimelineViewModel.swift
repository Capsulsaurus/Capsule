import AssetKit
import CapsuleFoundation
import CapsuleUI
import Foundation
import Observation

/// Drives the timeline screen: authorization, the initial load, live updates,
/// grid density, the Years / Months / All aggregation level, and the hidden
/// overlay.
///
/// Sectioning runs off the main actor (``buildSections(for:from:)``) so a large
/// library never blocks a frame. The materialised timeline is cached
/// (``allAssets``) so switching aggregation level re-sections in memory; hidden
/// assets are filtered out of ``visibleAssets`` before sectioning.
@MainActor
@Observable
public final class TimelineViewModel {
    /// The screen's coarse display state.
    public enum LoadState: Sendable, Equatable {
        case loading
        case ready
        case needsAuthorization
        case failed(String)
    }

    /// The Apple-Photos aggregation levels, coarse to fine.
    public enum TimelineLevel: Sendable, Equatable, CaseIterable {
        case years
        case months
        case all
    }

    /// Permitted grid densities, coarse to fine.
    public static let columnOptions = [3, 5, 7]

    public private(set) var state: LoadState = .loading
    public private(set) var sections: [PhotoGridSection] = []

    /// The current aggregation level (Years / Months / All Photos).
    public private(set) var level: TimelineLevel = .all
    /// A section to scroll into view after a level change (drill-down focus).
    public private(set) var focusSectionID: String?

    /// The grid column count; persisted across launches.
    public var columnCount: Int {
        didSet {
            guard columnCount != oldValue else { return }
            UserDefaults.standard.set(columnCount, forKey: Self.columnCountKey)
        }
    }

    /// How the grid should lay out the current level.
    public var gridStyle: PhotoGridStyle {
        switch level {
        case .all: .uniform(columns: columnCount)
        case .months, .years: .cards
        }
    }

    private let provider: any AssetProvider
    private let hiddenStore: HiddenStore?
    private var allAssets: [Asset] = []
    private var hiddenIDs: Set<AssetID> = []
    // `nonisolated(unsafe)` so `deinit` can cancel them: the properties are only
    // mutated on the main actor during the model's life, and `deinit` cannot
    // race with that (a live method call would be retaining `self`).
    private nonisolated(unsafe) var changeObservation: Task<Void, Never>?
    private nonisolated(unsafe) var hiddenObservation: Task<Void, Never>?

    /// The timeline minus hidden assets.
    private var visibleAssets: [Asset] {
        hiddenIDs.isEmpty ? allAssets : allAssets.filter { !hiddenIDs.contains($0.id) }
    }

    public init(provider: any AssetProvider, hiddenStore: HiddenStore? = nil) {
        self.provider = provider
        self.hiddenStore = hiddenStore
        let stored = UserDefaults.standard.object(forKey: Self.columnCountKey) as? Int
        columnCount = Self.columnOptions.contains(stored ?? 0) ? (stored ?? 5) : 5
    }

    deinit {
        changeObservation?.cancel()
        hiddenObservation?.cancel()
    }

    /// Request access and load the timeline. Safe to call once, on appear.
    public func load() async {
        let status = await provider.requestAuthorization()
        Diagnostics.shared.record(.photoPermission(granted: status.isUsable))
        guard status.isUsable else {
            state = .needsAuthorization
            return
        }
        hiddenIDs = await hiddenStore?.hiddenIDs() ?? []
        await reload()
        observeLibraryChanges()
        observeHiddenChanges()
    }

    /// Switch aggregation level, re-sectioning the cached timeline in memory.
    public func setLevel(_ newLevel: TimelineLevel) {
        guard newLevel != level else { return }
        level = newLevel
        focusSectionID = nil
        Task { sections = await Self.buildSections(for: newLevel, from: visibleAssets) }
    }

    /// Step the aggregation level for a pinch — `true` zooms in (finer level).
    public func zoom(in zoomingIn: Bool) {
        let order = TimelineLevel.allCases // years, months, all
        guard let index = order.firstIndex(of: level) else { return }
        let next = zoomingIn ? min(index + 1, order.count - 1) : max(index - 1, 0)
        setLevel(order[next])
    }

    /// Drill from a tapped representative card one level finer, scrolling the
    /// finer level to the tapped period.
    public func drillDown(into section: PhotoGridSection) {
        let finer: TimelineLevel
        switch level {
        case .years: finer = .months
        case .months: finer = .all
        case .all: return
        }
        level = finer
        Task {
            let built = await Self.buildSections(for: finer, from: visibleAssets)
            sections = built
            focusSectionID = built.first { $0.id.hasPrefix(section.id) }?.id
        }
    }

    private func reload() async {
        do {
            let snapshot = try await provider.loadTimeline()
            allAssets = Self.materialize(snapshot)
            sections = await Self.buildSections(for: level, from: visibleAssets)
            state = .ready
        } catch {
            CapsuleLog.interface.error("timeline load failed: \(String(describing: error), privacy: .public)")
            Diagnostics.shared.recordError(operation: .timelineLoad)
            state = .failed("Couldn't load your photo library.")
        }
    }

    /// Observe the provider's change stream and re-section on every update.
    private func observeLibraryChanges() {
        changeObservation?.cancel()
        let provider = provider
        changeObservation = Task { [weak self] in
            for await change in provider.changes() {
                guard !Task.isCancelled else { return }
                let assets = Self.materialize(change.snapshot)
                guard let self else { return }
                allAssets = assets
                let level = level
                let visible = visibleAssets
                sections = await Self.buildSections(for: level, from: visible)
            }
        }
    }

    /// Observe the hidden overlay and re-section when it changes.
    private func observeHiddenChanges() {
        guard let hiddenStore else { return }
        hiddenObservation?.cancel()
        hiddenObservation = Task { [weak self] in
            for await _ in hiddenStore.changes() {
                guard !Task.isCancelled, let self else { return }
                hiddenIDs = await hiddenStore.hiddenIDs()
                let level = level
                let visible = visibleAssets
                sections = await Self.buildSections(for: level, from: visible)
            }
        }
    }

    /// Materialise a snapshot into a plain asset array (cheap; index access).
    private nonisolated static func materialize(_ snapshot: any AssetSnapshot) -> [Asset] {
        (0 ..< snapshot.count).map { snapshot.asset(at: $0) }
    }

    /// Section assets for a level, off the main actor.
    private nonisolated static func buildSections(
        for level: TimelineLevel,
        from assets: [Asset]
    ) async -> [PhotoGridSection] {
        switch level {
        case .all: return TimelineSectioning.sections(from: assets)
        case .months: return TimelineSectioning.monthSections(from: assets)
        case .years: return TimelineSectioning.yearSections(from: assets)
        }
    }

    private static let columnCountKey = "timeline.columnCount"
}
