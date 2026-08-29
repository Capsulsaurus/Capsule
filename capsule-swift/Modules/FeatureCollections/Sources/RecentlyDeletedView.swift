import AssetKit
import CapsuleUI
import SwiftUI

/// Recently Deleted — Capsule-managed soft-deleted assets, with swipe-to-recover
/// and permanent delete.
///
/// System Photos deletions go to the Photos app's own Recently Deleted, which
/// third-party apps can't enumerate, so this lists managed assets. Rendered as a
/// list (by date) rather than a thumbnail grid, since managed-store thumbnails
/// are a separate follow-up.
public struct RecentlyDeletedView: View {
    @State private var assets: [Asset] = []
    @State private var isLoading = true
    let trashProvider: any TrashProvider

    public init(trashProvider: any TrashProvider) {
        self.trashProvider = trashProvider
    }

    public var body: some View {
        Group {
            if isLoading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if assets.isEmpty {
                ContentUnavailableView(
                    "app.recently_deleted.empty.title",
                    systemImage: "trash",
                    description: Text("app.recently_deleted.empty.description")
                )
            } else {
                list
            }
        }
        .navigationTitle("app.recently_deleted.title")
        .capsuleNavigationBarInline()
        .task { await reload() }
    }

    private var list: some View {
        List {
            ForEach(assets) { asset in
                HStack(spacing: 12) {
                    Image(systemName: asset.mediaType == .video ? "video.fill" : "photo.fill")
                        .foregroundStyle(.secondary)
                        .frame(width: 28)
                    Text(asset.captureDate.formatted(date: .abbreviated, time: .shortened))
                    Spacer()
                }
                .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                    Button("app.common.delete", role: .destructive) {
                        Task { await purge(asset) }
                    }
                    Button("app.recently_deleted.recover") {
                        Task { await restore(asset) }
                    }
                    .tint(.blue)
                }
            }
        }
    }

    private func reload() async {
        assets = await (try? trashProvider.trashedAssets()) ?? []
        isLoading = false
    }

    private func restore(_ asset: Asset) async {
        try? await trashProvider.restore(asset.id)
        await reload()
    }

    private func purge(_ asset: Asset) async {
        try? await trashProvider.purge(asset.id)
        await reload()
    }
}
