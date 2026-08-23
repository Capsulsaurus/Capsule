import AssetKit
import CapsuleFoundation
import CapsuleUI
import FeatureViewer
import ImagePipeline
import ManagedStore
import SwiftUI

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
    @Environment(\.openURL) private var openURL
    private let assetProvider: any AssetProvider
    private let albumProvider: any AlbumProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader
    private let hiddenStore: HiddenStore

    public init(
        assetProvider: any AssetProvider,
        albumProvider: any AlbumProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader,
        importer: LibraryImporter,
        hiddenStore: HiddenStore
    ) {
        _model = State(wrappedValue: TimelineViewModel(provider: assetProvider, hiddenStore: hiddenStore))
        _importer = State(wrappedValue: importer)
        self.assetProvider = assetProvider
        self.albumProvider = albumProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
        self.hiddenStore = hiddenStore
    }

    public var body: some View {
        content
            .navigationTitle("ios.tab.library")
            .capsuleNavigationBarInline()
            // `.navigation` / `.primaryAction` rather than `.topBarLeading`
            // / `.topBarTrailing`: the topBar placements exist only where
            // there is a navigation bar, while these two resolve to the same
            // leading/trailing slots on iOS and to the window toolbar on macOS.
            .toolbar {
                if isSelecting {
                    ToolbarItem(placement: .navigation) {
                        Button("ios.common.cancel") { exitSelection() }
                    }
                    ToolbarItem(placement: .principal) {
                        Text(selectionTitle).font(.headline)
                    }
                } else {
                    ToolbarItem(placement: .navigation) { importButton }
                    if model.state == .ready, !model.sections.isEmpty {
                        ToolbarItem(placement: .principal) { levelPicker }
                        if model.level == .all {
                            ToolbarItem(placement: .primaryAction) { densityMenu }
                            ToolbarItem(placement: .primaryAction) { selectButton }
                        }
                    }
                }
            }
            .task { await model.load() }
            .capsuleFullScreenCover(item: $viewerSelection) { selection in
                AssetViewerView(
                    assets: selection.assets,
                    startIndex: selection.startIndex,
                    provider: assetProvider,
                    mediaLoader: mediaLoader,
                    albumProvider: albumProvider
                )
            }
            .photoImportPicker(isPresented: $importer.isPickerPresented) { sources in
                Task { await importer.importPicked(sources) }
            }
            .overlay {
                if importer.isImporting { importProgressOverlay }
            }
            .alert(
                "ios.timeline.import_complete.title",
                isPresented: importResultBinding,
                presenting: importer.lastResult
            ) { _ in
                Button("ios.common.ok") {}
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
                "ios.add_to_album.title",
                isPresented: $isAddToAlbumPresented,
                titleVisibility: .visible
            ) {
                ForEach(userAlbums) { album in
                    Button(album.title) { Task { await addSelectedToAlbum(album) } }
                }
            } message: {
                Text(userAlbums.isEmpty
                    ? LocalizedStringKey("ios.add_to_album.empty_collections")
                    : LocalizedStringKey("ios.add_to_album.choose"))
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
                "ios.timeline.load_failed.title",
                systemImage: "exclamationmark.triangle",
                description: Text(message)
            )
        case .ready:
            if model.sections.isEmpty {
                ContentUnavailableView(
                    "ios.timeline.empty.title",
                    systemImage: "photo.on.rectangle",
                    description: Text("ios.timeline.empty.description")
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
        .accessibilityLabel("ios.timeline.import.accessibility")
    }

    private var levelPicker: some View {
        Picker("ios.timeline.view_picker", selection: levelBinding) {
            Text("ios.timeline.level.years").tag(TimelineViewModel.TimelineLevel.years)
            Text("ios.timeline.level.months").tag(TimelineViewModel.TimelineLevel.months)
            Text("ios.timeline.level.all").tag(TimelineViewModel.TimelineLevel.all)
        }
        .pickerStyle(.segmented)
        .frame(maxWidth: 260)
    }

    private var levelBinding: Binding<TimelineViewModel.TimelineLevel> {
        Binding(get: { model.level }, set: { model.setLevel($0) })
    }

    private var densityMenu: some View {
        Menu {
            Picker("ios.timeline.grid_size", selection: $model.columnCount) {
                Label("ios.timeline.grid.large", systemImage: "square.grid.2x2").tag(3)
                Label("ios.timeline.grid.medium", systemImage: "square.grid.3x3").tag(5)
                Label("ios.timeline.grid.small", systemImage: "square.grid.4x3.fill").tag(7)
            }
        } label: {
            Image(systemName: "square.grid.2x2")
        }
    }

    private var permissionPrompt: some View {
        ContentUnavailableView {
            Label("ios.timeline.permission.title", systemImage: "lock.fill")
        } description: {
            Text("ios.timeline.permission.description")
        } actions: {
            if let settingsURL = PhotoLibrarySettings.url {
                Button("ios.timeline.permission.open_settings") { openURL(settingsURL) }
            }
        }
    }

    private var importProgressOverlay: some View {
        ZStack {
            Color.black.opacity(0.3).ignoresSafeArea()
            ProgressView("ios.timeline.importing")
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
        Button("ios.timeline.select") { isSelecting = true }
    }

    var selectionTitle: String {
        selectedIDs.isEmpty ? "Select Items" : "\(selectedIDs.count) Selected"
    }

    var selectionActionBar: some View {
        HStack(spacing: 0) {
            shareSelectionAction
            selectionAction("heart") { Task { await favoriteSelected() } }
            selectionAction("rectangle.stack.badge.plus") { Task { await presentAddToAlbum() } }
            selectionAction("eye.slash") { Task { await hideSelected() } }
            selectionAction("trash", role: .destructive) { isDeleteConfirmPresented = true }
        }
        .padding(.vertical, CapsuleTheme.Spacing.medium)
        .padding(.horizontal, CapsuleTheme.Spacing.small)
        .capsuleGlass(in: Capsule())
        .padding(.horizontal, CapsuleTheme.Spacing.large)
        .padding(.bottom, CapsuleTheme.Spacing.small)
        .disabled(selectedIDs.isEmpty)
    }

    /// Share every selected asset.
    ///
    /// A `ShareLink` rather than a button that pre-loads images into state:
    /// ``ShareableAsset`` decodes each original only once the user has chosen a
    /// destination, so selecting two hundred photos costs nothing until then —
    /// and `ShareLink` is the one share affordance both platforms have.
    var shareSelectionAction: some View {
        ShareLink(
            items: selectedAssets.map { ShareableAsset(asset: $0, mediaLoader: mediaLoader) },
            preview: { SharePreview($0.previewTitle) },
            label: { selectionActionLabel("square.and.arrow.up") }
        )
    }

    func selectionAction(
        _ symbol: String,
        role: ButtonRole? = nil,
        action: @escaping () -> Void
    ) -> some View {
        Button(role: role, action: action) {
            selectionActionLabel(symbol)
        }
    }

    func selectionActionLabel(_ symbol: String) -> some View {
        Image(systemName: symbol)
            .font(.title3)
            .frame(maxWidth: .infinity)
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

    func hideSelected() async {
        await hiddenStore.setHidden(true, for: Array(selectedIDs))
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
}

/// The asset list and entry index handed to a presented viewer.
private struct ViewerSelection: Identifiable {
    let id = UUID()
    let assets: [Asset]
    let startIndex: Int
}
