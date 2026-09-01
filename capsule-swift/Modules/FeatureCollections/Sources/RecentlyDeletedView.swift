import AssetKit
import SwiftUI

/// Recently Deleted — Capsule-managed soft-deleted assets, with swipe-to-recover
/// and permanent delete.
///
/// System Photos deletions go to the Photos app's own Recently Deleted, which
/// third-party apps can't enumerate, so this lists managed assets. Rendered as a
/// list (by date) rather than a thumbnail grid, since managed-store thumbnails
/// are a separate follow-up.
///
/// Behind a Face ID / passcode gate, like ``HiddenView``: the catalog refuses to
/// enumerate the trash without a fresh-local-auth grant (Local Gallery — SR1),
/// so this screen takes one before it lists. The grant covers a 5-minute grace
/// window, so returning to the screen shortly after is silent. That gate is
/// view-time UX protection against a borrowed-unlocked-phone snoop — it is not a
/// cryptographic boundary and does not protect the files themselves.
struct RecentlyDeletedView: View {
    @State private var unlocked = false
    @State private var assets: [Asset] = []
    @State private var isLoading = false
    let trashProvider: any TrashProvider

    var body: some View {
        Group {
            if !unlocked {
                lockedView
            } else if isLoading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if assets.isEmpty {
                ContentUnavailableView(
                    "ios.recently_deleted.empty.title",
                    systemImage: "trash",
                    description: Text("ios.recently_deleted.empty.description")
                )
            } else {
                list
            }
        }
        .navigationTitle("ios.recently_deleted.title")
        .navigationBarTitleDisplayMode(.inline)
        .task { await authenticate() }
    }

    private var lockedView: some View {
        ContentUnavailableView {
            Label("ios.recently_deleted.title", systemImage: "lock.fill")
        } description: {
            Text("ios.recently_deleted.locked.description")
        } actions: {
            Button("ios.recently_deleted.unlock") { Task { await authenticate() } }
        }
    }

    /// Take the SR1 grant, then list. A grant still inside its grace window is
    /// reused, so this does not re-prompt on every appearance; a refusal leaves
    /// the screen locked with the Unlock action.
    private func authenticate() async {
        if await trashProvider.isTrashUnlocked() {
            unlocked = true
            await reload()
            return
        }
        do {
            try await trashProvider.unlockTrash()
        } catch {
            unlocked = false
            return
        }
        unlocked = true
        await reload()
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
                    Button("ios.common.delete", role: .destructive) {
                        Task { await purge(asset) }
                    }
                    Button("ios.recently_deleted.recover") {
                        Task { await restore(asset) }
                    }
                    .tint(.blue)
                }
            }
        }
    }

    private func reload() async {
        isLoading = true
        assets = (try? await trashProvider.trashedAssets()) ?? []
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
