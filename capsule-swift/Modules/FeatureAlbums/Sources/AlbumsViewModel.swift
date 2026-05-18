import AssetKit
import Foundation
import Observation

/// Drives the albums screen: loads albums, splits them into the user and smart
/// sections, observes changes, and creates new user albums.
@MainActor
@Observable
public final class AlbumsViewModel {
    public private(set) var userAlbums: [AlbumSummary] = []
    public private(set) var smartAlbums: [AlbumSummary] = []
    public private(set) var isLoading = true

    private let albumProvider: any AlbumProvider
    // `nonisolated(unsafe)` so `deinit` can cancel it; see `TimelineViewModel`.
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(albumProvider: any AlbumProvider) {
        self.albumProvider = albumProvider
    }

    deinit {
        observation?.cancel()
    }

    /// Load albums and begin observing changes. Call once, on appear.
    public func load() async {
        await reload()
        observeChanges()
    }

    /// Create a user album, ignoring blank names.
    public func createAlbum(named name: String) async {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        try? await albumProvider.createUserAlbum(named: trimmed)
        await reload()
    }

    private func reload() async {
        let all = await albumProvider.loadAlbums()
        userAlbums = all.filter(\.isUserAlbum)
        smartAlbums = all.filter { !$0.isUserAlbum }
        isLoading = false
    }

    private func observeChanges() {
        observation?.cancel()
        let provider = albumProvider
        observation = Task { [weak self] in
            for await _ in provider.changes() {
                guard !Task.isCancelled else { return }
                await self?.reload()
            }
        }
    }
}
