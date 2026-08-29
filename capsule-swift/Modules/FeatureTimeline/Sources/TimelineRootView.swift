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
    /// The capture date of the topmost tile on screen, which is the only place
    /// an unsectioned grid can say where in the library the reader is.
    @State private var visibleDate: Date?
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
            .navigationTitle(navigationTitle)
            .capsuleNavigationBarInline()
            // `.navigation` / `.primaryAction` rather than `.topBarLeading`
            // / `.topBarTrailing`: the topBar placements exist only where
            // there is a navigation bar, while these two resolve to the same
            // leading/trailing slots on iOS and to the window toolbar on macOS.
            .toolbar {
                if isSelecting {
                    ToolbarItem(placement: .navigation) {
                        Button("app.common.cancel") { exitSelection() }
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
                            // Two adjacent trailing items share one glass
                            // capsule without this, which says they are the same
                            // kind of thing. Changing how the grid is *drawn*
                            // and entering a mode that changes what a tap
                            // *does* are not.
                            ToolbarSpacer(.fixed, placement: .primaryAction)
                            ToolbarItem(placement: .primaryAction) { selectButton }
                        }
                    }
                }
            }
            .task { await model.load() }
            .onChange(of: model.level) { _, _ in visibleDate = nil }
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
                "app.timeline.import_complete.title",
                isPresented: importResultBinding,
                presenting: importer.lastResult
            ) { _ in
                Button("app.common.ok") {}
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
                "app.add_to_album.title",
                isPresented: $isAddToAlbumPresented,
                titleVisibility: .visible
            ) {
                ForEach(userAlbums) { album in
                    Button(album.title) { Task { await addSelectedToAlbum(album) } }
                }
            } message: {
                Text(userAlbums.isEmpty
                    ? LocalizedStringKey("app.add_to_album.empty_collections")
                    : LocalizedStringKey("app.add_to_album.choose"))
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
                "app.timeline.load_failed.title",
                systemImage: "exclamationmark.triangle",
                description: Text(message)
            )
        case .ready:
            if model.sections.isEmpty {
                ContentUnavailableView(
                    "app.timeline.empty.title",
                    systemImage: "photo.on.rectangle",
                    description: Text("app.timeline.empty.description")
                )
            } else {
                PhotoGridView(
                    sections: model.sections,
                    style: model.gridStyle,
                    thumbnails: thumbnails,
                    // All Photos is one continuous run, so it draws no day
                    // headers; Months and Years are card levels whose cards
                    // carry their own titles.
                    showsSectionHeaders: false,
                    scrollToSectionID: model.focusSectionID,
                    scrollToAsset: model.focusAsset,
                    isSelecting: isSelecting,
                    selectedIDs: selectedIDs,
                    onSelect: openViewer,
                    onSelectSection: { model.drillDown(into: $0) },
                    onZoomLevelChange: { model.zoom(in: $0) },
                    onToggleSelection: { toggleSelection($0) },
                    onLeadingVisibleAsset: { visibleDate = $0.captureDate },
                    onColumnsChange: { model.columnCount = $0 }
                )
                .ignoresSafeArea(edges: .bottom)
            }
        }
    }

    /// The library's own name until the reader has scrolled into a date.
    ///
    /// A grid with no section headers has nowhere else to say *when* you are
    /// looking at, and losing your place in a quarter of a million photos is the
    /// failure that costs. Apple Photos puts the same answer in the same place.
    private var navigationTitle: Text {
        guard model.level == .all, let visibleDate else {
            return Text("app.tab.library")
        }
        return Text(visibleDate, format: .dateTime.month(.wide).year())
    }

    private var importButton: some View {
        Button {
            importer.presentPicker()
        } label: {
            Image(systemName: "square.and.arrow.down")
        }
        .accessibilityLabel("app.timeline.import.accessibility")
    }

    private var levelPicker: some View {
        Picker("app.timeline.view_picker", selection: levelBinding) {
            Text("app.timeline.level.years").tag(TimelineViewModel.TimelineLevel.years)
            Text("app.timeline.level.months").tag(TimelineViewModel.TimelineLevel.months)
            Text("app.timeline.level.all").tag(TimelineViewModel.TimelineLevel.all)
        }
        .pickerStyle(.segmented)
        .frame(maxWidth: 260)
    }

    private var levelBinding: Binding<TimelineViewModel.TimelineLevel> {
        Binding(get: { model.level }, set: { model.setLevel($0) })
    }

    /// The three named densities, spanning the pinch ladder's range.
    ///
    /// A shortcut, not the whole vocabulary: pinching reaches all six rungs of
    /// ``PhotoGridZoom/ladder``, and this names the three worth naming. It also
    /// exists so density is reachable *without* a pinch, which matters for
    /// anyone who cannot make one.
    private static let namedDensities = [3, 5, 10]

    private var densityMenu: some View {
        Menu {
            Picker("app.timeline.grid_size", selection: namedDensityBinding) {
                Label("app.timeline.grid.large", systemImage: "square.grid.2x2")
                    .tag(Self.namedDensities[0])
                Label("app.timeline.grid.medium", systemImage: "square.grid.3x3")
                    .tag(Self.namedDensities[1])
                Label("app.timeline.grid.small", systemImage: "square.grid.4x3.fill")
                    .tag(Self.namedDensities[2])
            }
        } label: {
            Image(systemName: "square.grid.2x2")
        }
    }

    /// The density menu's selection, snapped to a rung the menu actually offers.
    ///
    /// A pinch can settle on 2, 4, or 7 — rungs with no menu entry — and a
    /// `Picker` whose selection matches none of its tags renders as though
    /// nothing is chosen. Reading through the nearest named rung means the menu
    /// always shows where the grid is, even when a pinch put it between names.
    private var namedDensityBinding: Binding<Int> {
        Binding(
            get: {
                Self.namedDensities.min {
                    abs($0 - model.columnCount) < abs($1 - model.columnCount)
                } ?? PhotoGridZoom.defaultColumns
            },
            set: { model.columnCount = $0 }
        )
    }

    private var permissionPrompt: some View {
        ContentUnavailableView {
            Label("app.timeline.permission.title", systemImage: "lock.fill")
        } description: {
            Text("app.timeline.permission.description")
        } actions: {
            if let settingsURL = PhotoLibrarySettings.url {
                Button("app.timeline.permission.open_settings") { openURL(settingsURL) }
            }
        }
    }

    private var importProgressOverlay: some View {
        ZStack {
            Color.black.opacity(0.3).ignoresSafeArea()
            ProgressView("app.timeline.importing")
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
        Button("app.timeline.select") { isSelecting = true }
    }

    var selectionTitle: String {
        selectedIDs.isEmpty ? "Select Items" : "\(selectedIDs.count) Selected"
    }

    var selectionActionBar: some View {
        SelectionActionBar(
            assets: selectedAssets,
            mediaLoader: mediaLoader,
            onFavorite: { Task { await favoriteSelected() } },
            onAddToAlbum: { Task { await presentAddToAlbum() } },
            onHide: { Task { await hideSelected() } },
            onDelete: { isDeleteConfirmPresented = true }
        )
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
