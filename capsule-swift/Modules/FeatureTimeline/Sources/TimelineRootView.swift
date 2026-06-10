import AssetKit
import CapsuleUI
import FeatureViewer
import ImagePipeline
import ManagedStore
import SwiftUI
import UIKit

/// The photo timeline grid — the app's primary screen.
///
/// Renders, by load state, a spinner, a permission prompt, an empty state, or
/// the ``PhotoGridView``. Tapping a tile opens the full-screen viewer; the
/// toolbar's import action brings photos into the Capsule-managed library.
public struct TimelineRootView: View {
    @State private var model: TimelineViewModel
    @State private var importer: LibraryImporter
    @State private var viewerSelection: ViewerSelection?
    private let assetProvider: any AssetProvider
    private let albumProvider: any AlbumProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader

    public init(
        assetProvider: any AssetProvider,
        albumProvider: any AlbumProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader,
        importer: LibraryImporter
    ) {
        _model = State(wrappedValue: TimelineViewModel(provider: assetProvider))
        _importer = State(wrappedValue: importer)
        self.assetProvider = assetProvider
        self.albumProvider = albumProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        NavigationStack {
            content
                .navigationTitle("Library")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) { importButton }
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
                mediaLoader: mediaLoader,
                albumProvider: albumProvider
            )
        }
        .sheet(isPresented: $importer.isPickerPresented) {
            PhotoPickerView { sources in
                Task { await importer.importPicked(sources) }
            }
            .ignoresSafeArea()
        }
        .overlay {
            if importer.isImporting { importProgressOverlay }
        }
        .alert(
            "Import Complete",
            isPresented: importResultBinding,
            presenting: importer.lastResult
        ) { _ in
            Button("OK") {}
        } message: { result in
            Text(Self.importSummary(result))
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
                    description: Text("Tap the import button to add photos to Capsule.")
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

    private var importButton: some View {
        Button {
            importer.presentPicker()
        } label: {
            Image(systemName: "square.and.arrow.down")
        }
        .accessibilityLabel("Import Photos")
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
            Text("Grant photo access to see your library, or import photos directly into Capsule.")
        } actions: {
            Button("Open Settings") {
                if let url = URL(string: UIApplication.openSettingsURLString) {
                    UIApplication.shared.open(url)
                }
            }
        }
    }

    private var importProgressOverlay: some View {
        ZStack {
            Color.black.opacity(0.3).ignoresSafeArea()
            ProgressView("Importing…")
                .padding(CapsuleTheme.Spacing.xLarge)
                .capsuleGlass(in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.medium))
        }
    }

    private var importResultBinding: Binding<Bool> {
        Binding(
            get: { importer.lastResult != nil },
            set: { presented in
                if !presented { importer.lastResult = nil }
            }
        )
    }

    /// Open the viewer at the tapped asset, paged across the whole timeline.
    private func openViewer(_ asset: Asset) {
        let assets = model.sections.flatMap(\.assets)
        guard let index = assets.firstIndex(of: asset) else { return }
        viewerSelection = ViewerSelection(assets: assets, startIndex: index)
    }

    private static func importSummary(_ result: ImportResult) -> String {
        var lines: [String] = []
        if result.importedCount > 0 {
            lines.append("\(result.importedCount) imported into Capsule.")
        }
        if result.duplicateCount > 0 {
            lines.append("\(result.duplicateCount) already in your library.")
        }
        if result.failureCount > 0 {
            lines.append("\(result.failureCount) couldn't be imported.")
        }
        return lines.isEmpty ? "Nothing to import." : lines.joined(separator: "\n")
    }
}

/// The asset list and entry index handed to a presented viewer.
private struct ViewerSelection: Identifiable {
    let id = UUID()
    let assets: [Asset]
    let startIndex: Int
}
