import CapsuleFoundation
import Foundation

/// An ``AlbumProvider`` merging several providers — the albums screen's view
/// over both system smart albums and Capsule user albums.
public final class CompositeAlbumProvider: AlbumProvider {
    private let providers: [any AlbumProvider]

    public init(providers: [any AlbumProvider]) {
        self.providers = providers
    }

    public func loadAlbums() async -> [AlbumSummary] {
        var all: [AlbumSummary] = []
        for provider in providers {
            all += await provider.loadAlbums()
        }
        return all
    }

    public func assets(in albumID: AlbumID) async throws -> [Asset] {
        for provider in providers {
            if let assets = try? await provider.assets(in: albumID), !assets.isEmpty {
                return assets
            }
        }
        return []
    }

    public func createUserAlbum(named name: String) async throws {
        var created = false
        for provider in providers
            where (try? await provider.createUserAlbum(named: name)) != nil {
            created = true
        }
        if !created {
            throw AlbumError.readOnly
        }
    }

    public func addAsset(_ assetID: AssetID, to albumID: AlbumID) async throws {
        for provider in providers {
            try? await provider.addAsset(assetID, to: albumID)
        }
    }

    public func changes() -> AsyncStream<Void> {
        let providers = providers
        return AsyncStream { continuation in
            let task = Task {
                await withTaskGroup(of: Void.self) { group in
                    for provider in providers {
                        group.addTask {
                            for await _ in provider.changes() {
                                continuation.yield(())
                            }
                        }
                    }
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
