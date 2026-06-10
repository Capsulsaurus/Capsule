import Foundation
import Testing

import AssetKit
import CapsuleFoundation
import CapsuleTestSupport
import ManagedStore

@Suite("ManagedAlbumProvider")
struct ManagedAlbumProviderTests {
    private func makeProvider() -> (ManagedAlbumProvider, MockCatalog) {
        let catalog = MockCatalog()
        let library = ManagedLibrary(
            layout: ManagedLibraryLayout(root: URL(filePath: "/capsule/Library")),
            fileStore: MockFileStore(),
            catalog: catalog
        )
        return (ManagedAlbumProvider(library: library), catalog)
    }

    @Test("a created user album appears in the listing")
    func createsAndLists() async throws {
        let (provider, _) = makeProvider()
        #expect(await provider.loadAlbums().isEmpty)

        try await provider.createUserAlbum(named: "Trip")

        let albums = await provider.loadAlbums()
        #expect(albums.count == 1)
        #expect(albums.first?.title == "Trip")
        #expect(albums.first?.isUserAlbum == true)
    }

    @Test("an added asset becomes the album's content")
    func addsAsset() async throws {
        let (provider, catalog) = makeProvider()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "asset-1"))
        try await provider.createUserAlbum(named: "Trip")
        guard let albumID = await provider.loadAlbums().first?.id else {
            Issue.record("the album was not created")
            return
        }

        try await provider.addAsset(.managed(uuid: "asset-1"), to: albumID)

        let assets = try await provider.assets(in: albumID)
        #expect(assets.map(\.id) == [.managed(uuid: "asset-1")])
    }

    @Test("editing a read-only smart album throws")
    func smartAlbumIsReadOnly() async {
        let provider = PhotoKitAlbumProvider()
        await #expect(throws: AlbumError.self) {
            try await provider.createUserAlbum(named: "Nope")
        }
    }

    @Test("the composite provider surfaces managed user albums")
    func compositeMergesManagedAlbums() async throws {
        let (managed, _) = makeProvider()
        try await managed.createUserAlbum(named: "Merged")
        let composite = CompositeAlbumProvider(providers: [PhotoKitAlbumProvider(), managed])

        let albums = await composite.loadAlbums()

        #expect(albums.contains { $0.title == "Merged" })
    }
}
