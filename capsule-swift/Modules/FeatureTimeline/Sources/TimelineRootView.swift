import AssetKit
import CapsuleUI
import ImagePipeline
import SwiftUI
import UIKit

/// The photo timeline grid — the app's primary screen.
///
/// Owns a ``TimelineViewModel`` and renders, by load state, a spinner, a
/// permission prompt, an empty state, or the ``PhotoGridView``.
public struct TimelineRootView: View {
    @State private var model: TimelineViewModel
    private let thumbnails: any ThumbnailProvider
    private let onOpenViewer: (Asset) -> Void

    public init(
        assetProvider: any AssetProvider,
        thumbnails: any ThumbnailProvider,
        onOpenViewer: @escaping (Asset) -> Void = { _ in }
    ) {
        _model = State(wrappedValue: TimelineViewModel(provider: assetProvider))
        self.thumbnails = thumbnails
        self.onOpenViewer = onOpenViewer
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
                    onSelect: onOpenViewer
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
}
