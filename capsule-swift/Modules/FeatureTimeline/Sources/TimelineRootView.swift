import AssetKit
import CapsuleFoundation
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
    @State private var isSelecting = false
    @State private var selectedIDs: Set<AssetID> = []
    @State private var userAlbums: [AlbumSummary] = []
    @State private var isAddToAlbumPresented = false
    @State private var isDeleteConfirmPresented = false
    @State private var shareItems: [UIImage] = []
    @State private var isSharePresented = false
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
                    if isSelecting {
                        ToolbarItem(placement: .topBarLeading) {
                            Button("Cancel") { exitSelection() }
                        }
                        ToolbarItem(placement: .principal) {
                            Text(selectionTitle).font(.headline)
                        }
                    } else {
                        ToolbarItem(placement: .topBarLeading) { importButton }
                        if model.state == .ready, !model.sections.isEmpty {
                            ToolbarItem(placement: .principal) { levelPicker }
                            if model.level == .all {
                                ToolbarItem(placement: .topBarTrailing) { densityMenu }
                                ToolbarItem(placement: .topBarTrailing) { selectButton }
                            }
                        }
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
        .overlay(alignment: .bottom) {
            if isSelecting { selectionActionBar }
        }
        .confirmationDialog(
            "Delete \(selectedIDs.count) Items?",
            isPresented: $isDeleteConfirmPresented,
            titleVisibility: .visible
        ) {
            Button("Delete \(selectedIDs.count) Items", role: .destructive) {
                Task { await deleteSelected() }
            }
        }
        .confirmationDialog(
            "Add to Album",
            isPresented: $isAddToAlbumPresented,
            titleVisibility: .visible
        ) {
            ForEach(userAlbums) { album in
                Button(album.title) { Task { await addSelectedToAlbum(album) } }
            }
        } message: {
            Text(userAlbums.isEmpty
                ? "Create an album in Collections first."
                : "Choose a Capsule album.")
        }
        .sheet(isPresented: $isSharePresented) {
            if !shareItems.isEmpty { TimelineActivityView(items: shareItems) }
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
                    style: model.gridStyle,
                    thumbnails: thumbnails,
                    scrollToSectionID: model.focusSectionID,
                    isSelecting: isSelecting,
                    selectedIDs: selectedIDs,
                    onSelect: openViewer,
                    onSelectSection: { model.drillDown(into: $0) },
                    onZoomLevelChange: { model.zoom(in: $0) },
                    onToggleSelection: { toggleSelection($0) }
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

    private var levelPicker: some View {
        Picker("View", selection: levelBinding) {
            Text("Years").tag(TimelineViewModel.TimelineLevel.years)
            Text("Months").tag(TimelineViewModel.TimelineLevel.months)
            Text("All").tag(TimelineViewModel.TimelineLevel.all)
        }
        .pickerStyle(.segmented)
        .frame(maxWidth: 260)
    }

    private var levelBinding: Binding<TimelineViewModel.TimelineLevel> {
        Binding(get: { model.level }, set: { model.setLevel($0) })
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

// MARK: - Multi-select

private extension TimelineRootView {
    var selectButton: some View {
        Button("Select") { isSelecting = true }
    }

    var selectionTitle: String {
        selectedIDs.isEmpty ? "Select Items" : "\(selectedIDs.count) Selected"
    }

    var selectionActionBar: some View {
        HStack(spacing: 0) {
            selectionAction("square.and.arrow.up") { Task { await shareSelected() } }
            selectionAction("heart") { Task { await favoriteSelected() } }
            selectionAction("rectangle.stack.badge.plus") { Task { await presentAddToAlbum() } }
            selectionAction("trash", role: .destructive) { isDeleteConfirmPresented = true }
        }
        .padding(.vertical, CapsuleTheme.Spacing.medium)
        .padding(.horizontal, CapsuleTheme.Spacing.small)
        .capsuleGlass(in: Capsule())
        .padding(.horizontal, CapsuleTheme.Spacing.large)
        .padding(.bottom, CapsuleTheme.Spacing.small)
        .disabled(selectedIDs.isEmpty)
    }

    func selectionAction(
        _ symbol: String,
        role: ButtonRole? = nil,
        action: @escaping () -> Void
    ) -> some View {
        Button(role: role, action: action) {
            Image(systemName: symbol)
                .font(.title3)
                .frame(maxWidth: .infinity)
        }
    }

    var selectedAssets: [Asset] {
        model.sections.flatMap(\.assets).filter { selectedIDs.contains($0.id) }
    }

    func toggleSelection(_ id: AssetID) {
        if selectedIDs.contains(id) { selectedIDs.remove(id) } else { selectedIDs.insert(id) }
    }

    func exitSelection() {
        isSelecting = false
        selectedIDs = []
    }

    func deleteSelected() async {
        let ids = Array(selectedIDs)
        guard !ids.isEmpty else { return }
        try? await assetProvider.delete(ids)
        exitSelection()
    }

    func favoriteSelected() async {
        for id in selectedIDs {
            try? await assetProvider.setFavorite(true, for: id)
        }
        exitSelection()
    }

    func presentAddToAlbum() async {
        userAlbums = await albumProvider.loadAlbums().filter(\.isUserAlbum)
        isAddToAlbumPresented = true
    }

    func addSelectedToAlbum(_ album: AlbumSummary) async {
        for id in selectedIDs {
            try? await albumProvider.addAsset(id, to: album.id)
        }
        exitSelection()
    }

    func shareSelected() async {
        var images: [UIImage] = []
        for asset in selectedAssets {
            if let image = await mediaLoader.fullImage(
                for: asset, targetSize: CGSize(width: 2048, height: 2048)
            ) {
                images.append(image)
            }
        }
        guard !images.isEmpty else { return }
        shareItems = images
        isSharePresented = true
    }
}

/// The asset list and entry index handed to a presented viewer.
private struct ViewerSelection: Identifiable {
    let id = UUID()
    let assets: [Asset]
    let startIndex: Int
}

/// A `UIActivityViewController` bridged into SwiftUI for the bulk share sheet.
private struct TimelineActivityView: UIViewControllerRepresentable {
    let items: [UIImage]

    func makeUIViewController(context _: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_: UIActivityViewController, context _: Context) {}
}
