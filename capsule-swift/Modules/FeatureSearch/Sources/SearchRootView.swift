import AssetKit
import CapsuleFoundation
import CapsuleUI
import FeatureViewer
import ImagePipeline
import SwiftUI

/// The Search screen — a full-screen search panel over the unified library.
///
/// A Liquid Glass search field with category suggestions and recent searches;
/// tapping a suggestion pivots the library on a facet (media type or capture
/// window). Active facets show as clearable chips above the results grid.
public struct SearchRootView: View {
    @State private var model: SearchViewModel
    @State private var viewerSelection: SearchViewerSelection?
    private let assetProvider: any AssetProvider
    private let albumProvider: any AlbumProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader

    public init(
        assetProvider: any AssetProvider,
        albumProvider: any AlbumProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader
    ) {
        _model = State(wrappedValue: SearchViewModel(provider: assetProvider))
        self.assetProvider = assetProvider
        self.albumProvider = albumProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        NavigationStack {
            content
                .navigationTitle("Search")
                .navigationBarTitleDisplayMode(.inline)
                .searchable(
                    text: $model.query,
                    placement: .navigationBarDrawer(displayMode: .always),
                    prompt: "Photos, Videos, Moments"
                )
                .searchSuggestions { suggestionList }
        }
        .task { await model.load() }
        .fullScreenCover(item: $viewerSelection) { selection in
            AssetViewerView(
                assets: selection.assets,
                startIndex: selection.startIndex,
                provider: assetProvider,
                mediaLoader: mediaLoader,
                albumProvider: albumProvider
            )
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoading {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if model.filter.isActive {
            resultsView
        } else {
            browseView
        }
    }

    // MARK: Browse (idle)

    private var browseView: some View {
        List {
            if !model.recentSearches.isEmpty {
                Section("Recent") {
                    ForEach(model.recentSearches, id: \.self) { term in
                        Button { model.applyRecent(term) } label: {
                            Label(term, systemImage: "clock.arrow.circlepath")
                        }
                    }
                    Button("Clear", role: .destructive) { model.clearRecents() }
                        .font(.footnote)
                }
            }
            Section("Categories") {
                ForEach(model.allSuggestions) { suggestion in
                    Button { model.apply(suggestion) } label: {
                        Label(suggestion.title, systemImage: suggestion.systemImage)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var suggestionList: some View {
        ForEach(model.visibleSuggestions) { suggestion in
            Button { model.apply(suggestion) } label: {
                Label(suggestion.title, systemImage: suggestion.systemImage)
            }
        }
    }

    // MARK: Results (facet active)

    private var resultsView: some View {
        VStack(spacing: 0) {
            activeChips
            Divider()
            if model.results.isEmpty {
                ContentUnavailableView(
                    "No Matches",
                    systemImage: "magnifyingglass",
                    description: Text("No photos match these filters.")
                )
            } else {
                PhotoGridView(
                    sections: [PhotoGridSection(id: "results", title: "", assets: model.results)],
                    columnCount: 5,
                    thumbnails: thumbnails,
                    showsSectionHeaders: false,
                    onSelect: openViewer
                )
                .ignoresSafeArea(edges: .bottom)
            }
        }
    }

    private var activeChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: CapsuleTheme.Spacing.small) {
                if let mediaType = model.filter.mediaType {
                    facetChip(SearchSuggestion.media(mediaType).title) { model.clearMediaType() }
                }
                if model.filter.dateRange != .anytime {
                    facetChip(model.filter.dateRange.title) { model.clearDateRange() }
                }
            }
            .padding(.horizontal)
            .padding(.vertical, CapsuleTheme.Spacing.small)
        }
    }

    private func facetChip(_ title: String, onClear: @escaping () -> Void) -> some View {
        HStack(spacing: CapsuleTheme.Spacing.xSmall) {
            Text(title).font(.subheadline.weight(.medium))
            Button(action: onClear) {
                Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
            }
            .accessibilityLabel("Clear \(title)")
        }
        .padding(.horizontal, CapsuleTheme.Spacing.medium)
        .padding(.vertical, CapsuleTheme.Spacing.xSmall)
        .capsuleGlass(in: Capsule())
    }

    private func openViewer(_ asset: Asset) {
        guard let index = model.results.firstIndex(of: asset) else { return }
        viewerSelection = SearchViewerSelection(assets: model.results, startIndex: index)
    }
}

/// The asset list and entry index handed to a presented viewer.
private struct SearchViewerSelection: Identifiable {
    let id = UUID()
    let assets: [Asset]
    let startIndex: Int
}
