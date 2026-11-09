import Foundation
import Testing

import CapsuleCatalog
import CapsuleTestSupport

/// Verifies that `MockCatalog` is a *faithful* ``AssetCatalog`` — it must obey
/// the same documented semantics as the real catalog, or every consumer test
/// built on it is testing against a fiction.
@Suite("MockCatalog conforms to the AssetCatalog contract")
struct MockCatalogTests {
    @Test("the timeline is newest-first and excludes soft-deleted assets")
    func timelineOrderAndDeletion() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "old", captureTimestamp: 100))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "new", captureTimestamp: 300))
        try await catalog.insertAsset(Fixtures.catalogAsset(
            id: "gone",
            captureTimestamp: 200,
            isDeleted: true,
            deletedAt: 250
        ))

        let timeline = await catalog.timeline(filter: .all, offset: 0, limit: 100)
        #expect(timeline.map(\.id) == ["new", "old"])
    }

    @Test("stack-hidden assets are excluded from the timeline")
    func stackHiddenExcluded() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "shown"))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "hidden", isStackHidden: true))

        let timeline = await catalog.timeline(filter: .all, offset: 0, limit: 100)
        #expect(timeline.map(\.id) == ["shown"])
    }

    @Test("a duplicate insert throws, matching the real catalog")
    func duplicateInsertThrows() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "x"))
        await #expect(throws: (any Error).self) {
            try await catalog.insertAsset(Fixtures.catalogAsset(id: "x"))
        }
    }

    @Test("a type filter restricts the timeline")
    func typeFilter() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "p", assetType: "photo"))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "v", assetType: "video"))

        let videos = await catalog.timeline(
            filter: TimelineFilter(assetType: "video"),
            offset: 0,
            limit: 100
        )
        #expect(videos.map(\.id) == ["v"])
    }

    @Test("deleting an album clears membership but keeps the assets")
    func albumDeletionClearsMembership() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAlbum(Fixtures.catalogAlbum(id: "alb"))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "member"))
        await catalog.setAssetAlbum(assetID: "member", albumID: "alb")
        #expect(await catalog.albumAssets(albumID: "alb", offset: 0, limit: 10).count == 1)

        await catalog.deleteAlbum(id: "alb")
        let asset = await catalog.asset(id: "member")
        #expect(asset != nil)
        #expect(asset?.albumID == nil)
    }

    // MARK: Gated views (SR1)

    //
    // These mirror the core's `gated_hidden_query_refuses_without_grant_and_serves_with_one`
    // and `locked_until_opened_then_refuses_after_grace_expiry`. The mock is only useful to
    // its consumers if it refuses the same reads the real catalog refuses.

    @Test("the trash listing refuses without a grant and serves with one")
    func trashRefusesWithoutGrant() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "gone", isDeleted: true, deletedAt: 100))

        await #expect(throws: CatalogError.viewLocked) {
            try await catalog.trash(offset: 0, limit: 10)
        }

        try await catalog.unlockView(.recentlyDeleted, using: MockLocalAuthGate())
        #expect(try await catalog.trash(offset: 0, limit: 10).map(\.id) == ["gone"])
    }

    @Test("a refused challenge mints nothing and leaves the view locked")
    func refusedChallengeMintsNothing() async throws {
        let catalog = MockCatalog()
        let gate = MockLocalAuthGate(refusingWith: .cancelled)

        await #expect(throws: LocalAuthError.cancelled) {
            try await catalog.unlockView(.recentlyDeleted, using: gate)
        }
        #expect(await catalog.isViewUnlocked(.recentlyDeleted) == false)
        await #expect(throws: CatalogError.viewLocked) {
            try await catalog.trash(offset: 0, limit: 10)
        }
    }

    @Test("a grant for one view is not a grant for the other")
    func grantsDoNotCrossViews() async throws {
        let catalog = MockCatalog()
        try await catalog.unlockView(.hidden, using: MockLocalAuthGate())

        #expect(await catalog.isViewUnlocked(.hidden))
        #expect(await catalog.isViewUnlocked(.recentlyDeleted) == false)
        await #expect(throws: CatalogError.viewLocked) {
            try await catalog.trash(offset: 0, limit: 10)
        }
    }

    @Test("a grant is reused inside its grace window and expires at the end of it")
    func grantExpiresAfterGrace() async throws {
        let catalog = MockCatalog()
        let gate = MockLocalAuthGate()
        await catalog.setNow(1000)

        try await catalog.unlockView(.recentlyDeleted, using: gate)
        #expect(gate.challengeCount == 1)

        // Re-entering inside the window reuses the grant — no second prompt — and
        // does not slide the window forward: it runs from the original mint.
        await catalog.setNow(1000 + MockCatalog.graceSeconds - 1)
        try await catalog.unlockView(.recentlyDeleted, using: gate)
        #expect(gate.challengeCount == 1)
        #expect(await catalog.isViewUnlocked(.recentlyDeleted))

        await catalog.setNow(1000 + MockCatalog.graceSeconds)
        #expect(await catalog.isViewUnlocked(.recentlyDeleted) == false)
        try await catalog.unlockView(.recentlyDeleted, using: gate)
        #expect(gate.challengeCount == 2)
    }

    @Test("relockView drops one grant and lockViews drops every grant")
    func revocation() async throws {
        let catalog = MockCatalog()
        try await catalog.unlockView(.recentlyDeleted, using: MockLocalAuthGate())
        try await catalog.unlockView(.hidden, using: MockLocalAuthGate())

        await catalog.relockView(.recentlyDeleted)
        #expect(await catalog.isViewUnlocked(.recentlyDeleted) == false)
        #expect(await catalog.isViewUnlocked(.hidden))

        await catalog.lockViews()
        #expect(await catalog.isViewUnlocked(.hidden) == false)
    }

    @Test("removeAsset drops the row from every listing")
    func removeAssetDropsTheRow() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "gone", isDeleted: true, deletedAt: 100))
        try await catalog.unlockView(.recentlyDeleted, using: MockLocalAuthGate())

        await catalog.removeAsset(id: "gone")

        #expect(await catalog.asset(id: "gone") == nil)
        #expect(try await catalog.trash(offset: 0, limit: 10).isEmpty)
    }

    @Test("the caption register is set, survives an upsert of the row, and clears with nil")
    func captionRegister() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "a"))

        try await catalog.setCaption("harbour", assetID: "a")
        var row = try #require(await catalog.asset(id: "a"))
        #expect(row.caption == "harbour")

        row.rating = 1
        await catalog.upsertAsset(row)
        #expect(await catalog.asset(id: "a")?.caption == "harbour")

        try await catalog.setCaption(nil, assetID: "a")
        #expect(await catalog.asset(id: "a")?.caption == nil)
    }

    @Test("a caption for an asset the catalog does not hold is refused")
    func captionForUnknownAssetIsRefused() async {
        let catalog = MockCatalog()
        await #expect(throws: CatalogError.invalidArgument(message: "no asset: ghost")) {
            try await catalog.setCaption("harbour", assetID: "ghost")
        }
        #expect(await catalog.asset(id: "ghost") == nil)
    }
}

@Suite("JSONSidecarCoder")
struct JSONSidecarCoderTests {
    private func sidecar(caption: String?) -> CatalogSidecar {
        CatalogSidecar(
            version: 1,
            uuid: "u",
            assetType: "photo",
            originalFilename: "a.jpg",
            importTimestamp: 1,
            modifiedTimestamp: 1,
            hashSHA256: String(repeating: "0", count: 64),
            fileSize: 3,
            importerVersion: "test",
            rawshiftVersion: "0.0.0",
            gpsLatitude: 48.8584,
            gpsLongitude: 2.2945,
            caption: caption
        )
    }

    @Test("a caption and a GPS fix round-trip")
    func captionAndGPSRoundTrip() throws {
        let coder = JSONSidecarCoder()
        let original = sidecar(caption: "harbour")
        #expect(try coder.decode(coder.encode(original)) == original)
    }

    @Test("a sidecar written before the caption field decodes with no caption")
    func olderSidecarDecodes() throws {
        let coder = JSONSidecarCoder()
        let encoded = try coder.encode(sidecar(caption: nil))
        var object = try #require(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        object["caption"] = nil
        let older = try JSONSerialization.data(withJSONObject: object)

        #expect(try coder.decode(older) == sidecar(caption: nil))
    }
}
