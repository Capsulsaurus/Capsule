import AssetKit
import Foundation
import Observation

/// Drives the search screen: loads the unified timeline once, then filters it
/// in memory as the facets change.
@MainActor
@Observable
public final class SearchViewModel {
    /// The active facets; mutating it re-filters the results.
    public var filter = SearchFilter() {
        didSet { applyFilter() }
    }

    public private(set) var results: [Asset] = []
    public private(set) var isLoading = true

    private var allAssets: [Asset] = []
    private let provider: any AssetProvider

    public init(provider: any AssetProvider) {
        self.provider = provider
    }

    /// Load the timeline to search over. Call once, on appear.
    public func load() async {
        if let snapshot = try? await provider.loadTimeline() {
            allAssets = (0 ..< snapshot.count).map { snapshot.asset(at: $0) }
        }
        isLoading = false
        applyFilter()
    }

    private func applyFilter() {
        results = allAssets.filter { filter.matches($0) }
    }
}
