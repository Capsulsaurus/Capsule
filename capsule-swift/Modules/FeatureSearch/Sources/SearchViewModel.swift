import AssetKit
import CapsuleFoundation
import Foundation
import Observation

/// Drives the search screen: loads the unified timeline once, filters it in
/// memory as the facets change, and offers category suggestions plus recent
/// searches for the full-screen search panel.
@MainActor
@Observable
public final class SearchViewModel {
    /// The active facets; mutating it re-filters the results.
    public var filter = SearchFilter() {
        didSet { applyFilter() }
    }

    /// The live search-field text — filters the visible suggestions.
    public var query = ""

    public private(set) var results: [Asset] = []
    public private(set) var recentSearches: [String] = []
    public private(set) var isLoading = true

    private var allAssets: [Asset] = []
    private let provider: any AssetProvider
    private static let recentsKey = "search.recents"

    public init(provider: any AssetProvider) {
        self.provider = provider
        recentSearches = (UserDefaults.standard.array(forKey: Self.recentsKey) as? [String]) ?? []
    }

    /// Load the timeline to search over. Call once, on appear.
    public func load() async {
        if let snapshot = try? await provider.loadTimeline() {
            allAssets = (0 ..< snapshot.count).map { snapshot.asset(at: $0) }
        }
        isLoading = false
        applyFilter()
    }

    /// Every category a search can pivot on.
    public var allSuggestions: [SearchSuggestion] {
        [
            .media(.photo), .media(.video), .media(.livePhoto),
            .date(.today), .date(.thisWeek), .date(.thisMonth), .date(.thisYear),
        ]
    }

    /// Suggestions matching the current query (all of them when it's empty).
    public var visibleSuggestions: [SearchSuggestion] {
        guard !query.isEmpty else { return allSuggestions }
        return allSuggestions.filter { $0.title.localizedCaseInsensitiveContains(query) }
    }

    /// Apply a suggestion's facet and remember it as a recent search.
    public func apply(_ suggestion: SearchSuggestion) {
        switch suggestion {
        case let .media(type): filter.mediaType = type
        case let .date(range): filter.dateRange = range
        }
        addRecent(suggestion.title)
    }

    /// Apply a recent search term by matching it back to a suggestion.
    public func applyRecent(_ term: String) {
        guard let suggestion = allSuggestions.first(where: { $0.title == term }) else { return }
        apply(suggestion)
    }

    public func clearMediaType() { filter.mediaType = nil }
    public func clearDateRange() { filter.dateRange = .anytime }

    public func clearRecents() {
        recentSearches = []
        UserDefaults.standard.removeObject(forKey: Self.recentsKey)
    }

    private func addRecent(_ term: String) {
        recentSearches.removeAll { $0 == term }
        recentSearches.insert(term, at: 0)
        recentSearches = Array(recentSearches.prefix(8))
        UserDefaults.standard.set(recentSearches, forKey: Self.recentsKey)
    }

    private func applyFilter() {
        results = allAssets.filter { filter.matches($0) }
    }
}

/// A search category — a media type or a capture-date window — surfaced as a
/// suggestion chip and search completion.
public enum SearchSuggestion: Identifiable, Hashable, Sendable {
    case media(MediaType)
    case date(DateRangeOption)

    public var id: String {
        switch self {
        case let .media(type): "media-\(type)"
        case let .date(range): "date-\(range.rawValue)"
        }
    }

    public var title: String {
        switch self {
        case let .media(type):
            switch type {
            case .photo: "Photos"
            case .video: "Videos"
            case .livePhoto: "Live Photos"
            }
        case let .date(range): range.title
        }
    }

    public var systemImage: String {
        switch self {
        case let .media(type):
            switch type {
            case .photo: "photo"
            case .video: "video"
            case .livePhoto: "livephoto"
            }
        case .date: "calendar"
        }
    }
}
