import Foundation
import Testing

import AssetKit
import CapsuleDomain
import CapsuleFoundation
import CapsulePorts

// MARK: - Fixtures

/// Deliberately explicit library rows. Named apart from `CapsuleTestSupport`'s
/// `Fixtures` so a failure never leaves it ambiguous which fixture built the
/// value under test.
enum BridgeFixtures {
    /// 2026-08-22T12:00:00Z.
    static let noon = CapsuleTimestamp(epochSeconds: 1787400000)

    static func libraryAsset(
        uuid: String = "asset-0",
        mediaType: MediaType = .photo,
        captureTime: CaptureTime? = nil,
        dimensions: Dimensions? = Dimensions(width: 4032, height: 3024),
        durationMilliseconds: Int64? = nil,
        rating: UInt8 = 0,
        tagsUser: Set<String> = [],
        isDeleted: Bool = false,
        isUserHidden: Bool = false
    ) -> LibraryAsset {
        LibraryAsset(
            id: .managed(uuid: uuid),
            mediaType: mediaType,
            contentType: mediaType == .video ? .quicktime : .heic,
            captureTime: captureTime ?? CaptureTime(captureTimestamp: noon),
            importTimestamp: noon,
            dimensions: dimensions,
            durationMilliseconds: durationMilliseconds,
            rating: rating,
            tagsUser: tagsUser,
            isDeleted: isDeleted,
            isUserHidden: isUserHidden
        )
    }

    /// `count` rows, one second apart, newest last.
    static func libraryAssets(count: Int) -> [LibraryAsset] {
        (0 ..< count).map { index in
            libraryAsset(
                uuid: "asset-\(index)",
                captureTime: CaptureTime(
                    captureTimestamp: CapsuleTimestamp(epochSeconds: noon.epochSeconds + Int64(index))
                )
            )
        }
    }
}

// MARK: - Projection

@Suite("LibraryAsset → Asset projection")
struct LibraryAssetProjectionTests {
    @Test("identity, dimensions, and media type project unchanged")
    func projectsIdentity() {
        let source = BridgeFixtures.libraryAsset(uuid: "abc", mediaType: .livePhoto)
        let asset = Asset(libraryAsset: source)
        #expect(asset.id == .managed(uuid: "abc"))
        #expect(asset.mediaType == .livePhoto)
        #expect(asset.pixelWidth == 4032)
        #expect(asset.pixelHeight == 3024)
        #expect(asset.aspectRatio == 4032.0 / 3024.0)
    }

    @Test("absent dimensions become an unknown, square aspect rather than a crash")
    func projectsMissingDimensions() {
        let asset = Asset(libraryAsset: BridgeFixtures.libraryAsset(dimensions: nil))
        #expect(asset.pixelWidth == 0)
        #expect(asset.pixelHeight == 0)
        #expect(asset.aspectRatio == 1)
    }

    @Test("captureDate is the effective capture instant, not the wall clock")
    func projectsEffectiveCaptureDate() {
        // A photograph taken abroad: the wall clock and the resolved UTC
        // instant are three hours apart, and the timeline sorts on the latter.
        let wallClock = CapsuleTimestamp(epochSeconds: 1787400000)
        let utc = CapsuleTimestamp(epochSeconds: 1787389200)
        let source = BridgeFixtures.libraryAsset(
            captureTime: CaptureTime(
                captureTimestamp: wallClock,
                captureUTC: utc,
                timezoneSource: .offsetExif
            )
        )
        #expect(Asset(libraryAsset: source).captureDate == utc.date)
        #expect(Asset(libraryAsset: source).captureDate != wallClock.date)
    }

    @Test("a video's duration converts from milliseconds to seconds")
    func projectsVideoDuration() {
        let video = BridgeFixtures.libraryAsset(
            mediaType: .video,
            durationMilliseconds: 12500
        )
        #expect(Asset(libraryAsset: video).duration == 12.5)
        #expect(Asset(libraryAsset: BridgeFixtures.libraryAsset()).duration == 0)
    }

    @Test("isFavorite comes from the reserved tag, never from the star rating")
    func derivesFavouriteFromTag() {
        let starred = BridgeFixtures.libraryAsset(rating: 5)
        let favourited = BridgeFixtures.libraryAsset(tagsUser: [Asset.favoriteTag])
        let both = BridgeFixtures.libraryAsset(rating: 5, tagsUser: [Asset.favoriteTag])
        // A five-star photograph is not thereby a favourite: conflating the two
        // is what makes un-favouriting destroy a rating.
        #expect(Asset(libraryAsset: starred).isFavorite == false)
        #expect(Asset(libraryAsset: favourited).isFavorite)
        #expect(Asset(libraryAsset: both).isFavorite)
        #expect(Asset(libraryAsset: BridgeFixtures.libraryAsset(tagsUser: ["travel"])).isFavorite == false)
    }
}

// MARK: - Authorization

@Suite("Port-backed authorization")
struct PortBackedAuthorizationTests {
    @Test("authorizationStatus is authorized and asks the library nothing")
    func authorizesWithoutAsking() async {
        let library = FakeLibrary(assets: [])
        let provider = PortBackedAssetProvider(library: library, organize: library)
        #expect(await provider.authorizationStatus() == .authorized)
        #expect(await provider.requestAuthorization() == .authorized)
        // Not merely "no prompt": no read of any kind. There is nothing to
        // authorize, so there is nothing to ask.
        #expect(await library.callCount == 0)
        #expect(await library.pageRequests.isEmpty)
    }
}

// MARK: - Paging

@Suite("Paged library snapshot")
struct PagedLibrarySnapshotTests {
    @Test("a 250 000-asset library reports its count without being materialized")
    func pagesRatherThanMaterializing() async throws {
        let library = SyntheticLibrary(totalAssets: 250000, assetsPerDay: 250)
        let provider = PortBackedAssetProvider(
            library: library,
            organize: FakeLibrary(assets: []),
            warmLimit: 400
        )
        let snapshot = try await provider.loadTimeline()

        #expect(snapshot.count == 250000)
        // Two windows warmed, and not one row more: the count came from the
        // aggregate, not from walking the library.
        #expect(await library.rowsFetched == 400)
        #expect(await library.pageRequests.count == 2)
    }

    @Test("a warmed index answers with the real row")
    func servesWarmedRows() async throws {
        let library = SyntheticLibrary(totalAssets: 250000, assetsPerDay: 250)
        let provider = PortBackedAssetProvider(
            library: library,
            organize: FakeLibrary(assets: []),
            warmLimit: 400
        )
        let snapshot = try await provider.loadTimeline()
        let first = snapshot.asset(at: 0)
        #expect(first.id == .managed(uuid: "synthetic-0"))
        #expect(PagedLibrarySnapshot.isProvisional(first) == false)
        #expect(first.pixelWidth == 4000)
    }

    @Test("an unloaded index answers provisionally, on the right day")
    func answersUnloadedIndexProvisionally() async throws {
        let library = SyntheticLibrary(totalAssets: 250000, assetsPerDay: 250)
        let provider = PortBackedAssetProvider(
            library: library,
            organize: FakeLibrary(assets: []),
            warmLimit: 400
        )
        let snapshot = try await provider.loadTimeline()
        let far = snapshot.asset(at: 200000)

        // Obviously provisional, and detectable as such.
        #expect(PagedLibrarySnapshot.isProvisional(far))
        #expect(far.pixelWidth == 0)
        // But sectioned correctly, because the day histogram already knows
        // which day row 200 000 falls on. A wrong date here would put the row
        // in the wrong month and make the grid jump when the real row landed.
        let expected = await library.dayKey(at: 200000)
        #expect(DayKey(epochSeconds: Int64(far.captureDate.timeIntervalSince1970)) == expected)
        // Distinct per index, so a ForEach cannot collapse a partial window.
        #expect(snapshot.asset(at: 200001).id != far.id)
    }

    @Test("reading an unloaded index fetches its window for the next read")
    func schedulesTheWindowItCouldNotAnswer() async throws {
        let library = SyntheticLibrary(totalAssets: 250000, assetsPerDay: 250)
        let provider = PortBackedAssetProvider(
            library: library,
            organize: FakeLibrary(assets: []),
            warmLimit: 400
        )
        guard let snapshot = try await provider.loadTimeline() as? PagedLibrarySnapshot else {
            Issue.record("expected a PagedLibrarySnapshot")
            return
        }
        _ = snapshot.asset(at: 100000)
        // The window is fetched by a detached task that hops onto the port's
        // actor, so `Task.yield()` is not enough to let it finish — yielding
        // reschedules on the same executor and can spin without the fetch ever
        // being resumed. Poll on a real clock instead, with a deadline generous
        // enough for a loaded CI machine.
        let deadline = ContinuousClock.now + .seconds(5)
        while !snapshot.isLoaded(at: 100000), ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(snapshot.isLoaded(at: 100000))
        #expect(PagedLibrarySnapshot.isProvisional(snapshot.asset(at: 100000)) == false)
    }

    @Test("residency is bounded — a full walk does not retain the library")
    func boundsResidency() async throws {
        let library = SyntheticLibrary(totalAssets: 40000, assetsPerDay: 250)
        let provider = PortBackedAssetProvider(
            library: library,
            organize: FakeLibrary(assets: []),
            warmLimit: 40000
        )
        guard let snapshot = try await provider.loadTimeline() as? PagedLibrarySnapshot else {
            Issue.record("expected a PagedLibrarySnapshot")
            return
        }
        // 200 windows would cover the library; the cache keeps 40.
        #expect(snapshot.loadedPageCount <= 40)
        #expect(snapshot.count == 40000)
    }
}

// MARK: - Change propagation

@Suite("Port-backed change propagation")
struct PortBackedChangeTests {
    @Test("a trash mutation through OrganizePort surfaces on changes()")
    func trashSurfacesOnChangeStream() async throws {
        let assets = BridgeFixtures.libraryAssets(count: 3)
        let library = FakeLibrary(assets: assets)
        let provider = PortBackedAssetProvider(library: library, organize: library)

        let stream = provider.changes()
        try await waitForSubscriber(on: library)

        try await library.moveToTrash([assets[0].id], retentionDays: nil)

        let change = await firstChange(from: stream)
        let snapshot = try #require(change?.snapshot)
        #expect(snapshot.count == 2)
        #expect(change?.isIncremental == false)
    }

    @Test("hiding an asset removes it from the live slice the provider publishes")
    func hidingSurfacesOnChangeStream() async throws {
        let assets = BridgeFixtures.libraryAssets(count: 4)
        let library = FakeLibrary(assets: assets)
        let provider = PortBackedAssetProvider(library: library, organize: library)

        let stream = provider.changes()
        try await waitForSubscriber(on: library)

        try await library.setHidden(true, for: [assets[1].id, assets[2].id])

        let snapshot = try #require(await firstChange(from: stream)?.snapshot)
        #expect(snapshot.count == 2)
    }

    @Test("favouriting round-trips through the reserved tag and back off again")
    func favouriteRoundTrips() async throws {
        let assets = BridgeFixtures.libraryAssets(count: 1)
        let library = FakeLibrary(assets: assets)
        let provider = PortBackedAssetProvider(library: library, organize: library)
        let identifier = assets[0].id

        try await provider.setFavorite(true, for: identifier)
        let favourited = try await provider.asset(for: identifier)
        #expect(favourited?.isFavorite == true)

        // The un-favourite has to find the add id on the sidecar; a remove that
        // names an unobserved add is rejected, so this failing would be silent
        // data the user could not clear.
        try await provider.setFavorite(false, for: identifier)
        let cleared = try await provider.asset(for: identifier)
        #expect(cleared?.isFavorite == false)
    }

    @Test("favouriting twice does not accumulate duplicate tag entries")
    func favouriteIsIdempotent() async throws {
        let assets = BridgeFixtures.libraryAssets(count: 1)
        let library = FakeLibrary(assets: assets)
        let provider = PortBackedAssetProvider(library: library, organize: library)
        let identifier = assets[0].id

        try await provider.setFavorite(true, for: identifier)
        try await provider.setFavorite(true, for: identifier)
        try await provider.setFavorite(false, for: identifier)
        let cleared = try await provider.asset(for: identifier)
        #expect(cleared?.isFavorite == false)
    }
}

// MARK: - Helpers

/// Wait until the adapter's relay has actually registered on the library's
/// change stream.
///
/// Registration hops onto an actor, so a mutation issued before it lands is
/// legitimately missed — the port's contract is "re-read the window you care
/// about", not "replay what you slept through".
private func waitForSubscriber(on library: FakeLibrary, attempts: Int = 1000) async throws {
    for _ in 0 ..< attempts {
        if await library.subscriberCount > 0 { return }
        await Task.yield()
    }
    throw BridgeTestTimeout()
}

/// The first change on a stream, or `nil` after a generous deadline — so a
/// broken adapter fails the suite instead of hanging it.
private func firstChange(from stream: AsyncStream<AssetChange>) async -> AssetChange? {
    await withTaskGroup(of: AssetChange?.self) { group in
        group.addTask {
            for await change in stream {
                return change
            }
            return nil
        }
        group.addTask {
            try? await Task.sleep(for: .seconds(5))
            return nil
        }
        var first: AssetChange?
        if let produced = await group.next() { first = produced }
        group.cancelAll()
        return first
    }
}

private struct BridgeTestTimeout: Error {}
