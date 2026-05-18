import Foundation
import Testing

import AssetKit
import CapsuleTestSupport
import FeatureViewer

@Suite("AssetViewerModel")
@MainActor
struct AssetViewerModelTests {
    @Test("clamps an out-of-range start index")
    func clampsStartIndex() {
        let model = AssetViewerModel(
            assets: Fixtures.assets(count: 3),
            startIndex: 99,
            provider: MockAssetProvider(),
            albumProvider: MockAlbumProvider()
        )
        #expect(model.currentIndex == 2)
    }

    @Test("toggleFavorite flips the asset and persists through the provider")
    func togglesFavorite() async {
        let assets = Fixtures.assets(count: 2)
        let provider = MockAssetProvider(assets: assets)
        let model = AssetViewerModel(
            assets: assets, startIndex: 0, provider: provider, albumProvider: MockAlbumProvider()
        )
        #expect(model.currentAsset?.isFavorite == false)

        await model.toggleFavorite()

        #expect(model.currentAsset?.isFavorite == true)
        let stored = await provider.asset(for: assets[0].id)
        #expect(stored?.isFavorite == true)
    }

    @Test("deleting a middle asset removes it and keeps the viewer open")
    func deletesMiddleAsset() async {
        let assets = Fixtures.assets(count: 3)
        let provider = MockAssetProvider(assets: assets)
        let model = AssetViewerModel(
            assets: assets, startIndex: 1, provider: provider, albumProvider: MockAlbumProvider()
        )

        let shouldDismiss = await model.deleteCurrentAsset()

        #expect(shouldDismiss == false)
        #expect(model.assets.count == 2)
        #expect(model.currentIndex == 1)
    }

    @Test("deleting the last remaining asset signals dismissal")
    func deletesLastAsset() async {
        let assets = Fixtures.assets(count: 1)
        let provider = MockAssetProvider(assets: assets)
        let model = AssetViewerModel(
            assets: assets, startIndex: 0, provider: provider, albumProvider: MockAlbumProvider()
        )

        let shouldDismiss = await model.deleteCurrentAsset()

        #expect(shouldDismiss == true)
        #expect(model.assets.isEmpty)
    }
}
