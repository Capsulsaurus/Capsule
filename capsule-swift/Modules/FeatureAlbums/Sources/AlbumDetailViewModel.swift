import AssetKit
import Foundation
import Observation

/// Loads and holds the assets of a single album for ``AlbumDetailView``.
@MainActor
@Observable
public final class AlbumDetailViewModel {
    public private(set) var assets: [Asset] = []
    public private(set) var isLoading = true

    private let album: AlbumSummary
    private let albumProvider: any AlbumProvider

    public init(album: AlbumSummary, albumProvider: any AlbumProvider) {
        self.album = album
        self.albumProvider = albumProvider
    }

    /// Load the album's assets. Call on appear.
    public func load() async {
        assets = (try? await albumProvider.assets(in: album.id)) ?? []
        isLoading = false
    }
}
