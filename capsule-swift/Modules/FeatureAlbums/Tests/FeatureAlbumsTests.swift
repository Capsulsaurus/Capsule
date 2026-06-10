import Foundation
import Testing

import AssetKit
import CapsuleFoundation
import CapsuleTestSupport
import FeatureAlbums

@Suite("AlbumsViewModel")
@MainActor
struct AlbumsViewModelTests {
    @Test("splits albums into the user and smart sections")
    func splitsAlbums() async {
        let provider = MockAlbumProvider(albums: [
            AlbumSummary(id: .managed(uuid: "u1"), title: "Trip", count: 3),
            AlbumSummary(id: .smart(localIdentifier: "s1"), title: "Favorites", count: 9),
        ])
        let model = AlbumsViewModel(albumProvider: provider)

        await model.load()

        #expect(model.userAlbums.map(\.title) == ["Trip"])
        #expect(model.smartAlbums.map(\.title) == ["Favorites"])
    }

    @Test("creating an album adds it to the user section")
    func createsAlbum() async {
        let model = AlbumsViewModel(albumProvider: MockAlbumProvider())
        await model.load()
        #expect(model.userAlbums.isEmpty)

        await model.createAlbum(named: "Summer")

        #expect(model.userAlbums.contains { $0.title == "Summer" })
    }

    @Test("a blank album name is ignored")
    func ignoresBlankName() async {
        let model = AlbumsViewModel(albumProvider: MockAlbumProvider())
        await model.load()

        await model.createAlbum(named: "   ")

        #expect(model.userAlbums.isEmpty)
    }
}
