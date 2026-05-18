import AssetKit
import CapsuleUI
import FeatureViewer
import ImagePipeline
import SwiftUI
import UIKit

/// The photo timeline grid — the app's primary screen.
///
/// Owns a ``TimelineViewModel`` and renders, by load state, a spinner, a
/// permission prompt, an empty state, or the ``PhotoGridView``. Tapping a tile
/// opens the full-screen ``AssetViewerView`` paged across the whole timeline.
public struct TimelineRootView: View {
    @State private var model: TimelineViewModel
    @State private var viewerSelection: ViewerSelection?
    private let assetProvider: any AssetProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader

    public init(
        assetProvider: any AssetProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader
    ) {
        _model = State(wrappedValue: TimelineViewModel(provider: assetProvider))
        self.assetProvider = assetProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        NavigationStack {
            content
                .navigationTitle("Library")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    if model.state == .ready, !model.sections.isEmpty {
                        ToolbarItem(placement: .topBarTrailing) { densityMenu }
                    }
                }
        }
        .task { await model.load() }
        .fullScreenCover(item: $viewerSelection) { selection in
            AssetViewerView(
                assets: selection.assets,
                startIndex: selection.startIndex,
                provider: assetProvider,
                mediaLoader: mediaLoader
            )
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .loading:
            ProgressView()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .needsAuthorization:
            permissionPrompt
        case let .failed(message):
            ContentUnavailableView(
                "Couldn't Load Photos",
                systemImage: "exclamationmark.triangle",
                description: Text(message)
            )
        case .ready:
            if model.sections.isEmpty {
                ContentUnavailableView(
                    "No Photos",
                    systemImage: "photo.on.rectangle",
                    description: Text("Photos in your library will appear here.")
                )
            } else {
                PhotoGridView(
                    sections: model.sections,
                    columnCount: model.columnCount,
                    thumbnails: thumbnails,
                    onSelect: openViewer
                )
                .ignoresSafeArea(edges: .bottom)
            }
        }
    }

    private var densityMenu: some View {
        Menu {
            Picker("Grid Size", selection: $model.columnCount) {
                Label("Large", systemImage: "square.grid.2x2").tag(3)
                Label("Medium", systemImage: "square.grid.3x3").tag(5)
                Label("Small", systemImage: "square.grid.4x3.fill").tag(7)
            }
        } label: {
            Image(systemName: "square.grid.2x2")
        }
    }

    private var permissionPrompt: some View {
        ContentUnavailableView {
            Label("Photo Access Needed", systemImage: "lock.fill")
        } description: {
            Text("Capsule needs access to your photo library to show your timeline.")
        } actions: {
            Button("Open Settings") {
                if let url = URL(string: UIApplication.openSettingsURLString) {
                    UIApplication.shared.open(url)
                }
            }
        }
    }

    /// Open the viewer at the tapped asset, paged across the whole timeline.
    private func openViewer(_ asset: Asset) {
        let assets = model.sections.flatMap(\.assets)
        guard let index = assets.firstIndex(of: asset) else { return }
        viewerSelection = ViewerSelection(assets: assets, startIndex: index)
    }
}

/// The asset list and entry index handed to a presented viewer.
private struct ViewerSelection: Identifiable {
    let id = UUID()
    let assets: [Asset]
    let startIndex: Int
}
