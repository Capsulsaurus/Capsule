import AssetKit
import CapsuleFoundation
import CapsuleUI
import Foundation
import Observation

/// Drives the timeline screen: authorization, the initial load, live updates,
/// and grid density.
///
/// Day/month sectioning runs off the main actor (``buildSections(from:)``) so a
/// large library never blocks a frame; only the resulting `[PhotoGridSection]`
/// is published back on the main actor.
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

    /// Permitted grid densities, coarse to fine.
    public static let columnOptions = [3, 5, 7]

    public private(set) var state: LoadState = .loading
    public private(set) var sections: [PhotoGridSection] = []

    /// The grid column count; persisted across launches.
    public var columnCount: Int {
        didSet {
            guard columnCount != oldValue else { return }
            UserDefaults.standard.set(columnCount, forKey: Self.columnCountKey)
        }
    }

    private let provider: any AssetProvider
    // `nonisolated(unsafe)` so `deinit` can cancel it: the property is only
    // mutated on the main actor during the model's life, and `deinit` cannot
    // race with that (a live method call would be retaining `self`).
    private nonisolated(unsafe) var changeObservation: Task<Void, Never>?

    public init(provider: any AssetProvider) {
        self.provider = provider
        let stored = UserDefaults.standard.object(forKey: Self.columnCountKey) as? Int
        columnCount = Self.columnOptions.contains(stored ?? 0) ? (stored ?? 5) : 5
    }

    deinit {
        changeObservation?.cancel()
    }

    /// Request access and load the timeline. Safe to call once, on appear.
    public func load() async {
        let status = await provider.requestAuthorization()
        guard status.isUsable else {
            state = .needsAuthorization
            return
        }
        await reload()
        observeLibraryChanges()
    }

    private func reload() async {
        do {
            let snapshot = try await provider.loadTimeline()
            sections = await Self.buildSections(from: snapshot)
            state = .ready
        } catch {
            CapsuleLog.interface.error("timeline load failed: \(String(describing: error), privacy: .public)")
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
                let rebuilt = await Self.buildSections(from: change.snapshot)
                self?.sections = rebuilt
            }
        }
    }

    /// Materialise and section a snapshot off the main actor.
    private nonisolated static func buildSections(
        from snapshot: any AssetSnapshot
    ) async -> [PhotoGridSection] {
        let assets = (0 ..< snapshot.count).map { snapshot.asset(at: $0) }
        return TimelineSectioning.sections(from: assets)
    }

    private static let columnCountKey = "timeline.columnCount"
}
