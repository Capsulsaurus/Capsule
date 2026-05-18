import AssetKit
import CapsuleFoundation
import CapsuleUI
import FeatureViewer
import ImagePipeline
import SwiftUI

/// The search screen — filter the unified library by media type and capture
/// date, and browse the matches.
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
            VStack(spacing: 0) {
                filterBar
                Divider()
                results
            }
            .navigationTitle("Search")
            .navigationBarTitleDisplayMode(.inline)
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

    private var filterBar: some View {
        VStack(spacing: 10) {
            Picker("Media Type", selection: $model.filter.mediaType) {
                Text("All").tag(MediaType?.none)
                Text("Photos").tag(MediaType?.some(.photo))
                Text("Videos").tag(MediaType?.some(.video))
                Text("Live").tag(MediaType?.some(.livePhoto))
            }
            .pickerStyle(.segmented)

            Picker("When", selection: $model.filter.dateRange) {
                ForEach(DateRangeOption.allCases) { option in
                    Text(option.title).tag(option)
                }
            }
            .pickerStyle(.menu)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding()
    }

    @ViewBuilder
    private var results: some View {
        if model.isLoading {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if model.results.isEmpty {
            ContentUnavailableView(
                "No Matches",
                systemImage: "magnifyingglass",
                description: Text("No photos match the selected filters.")
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
