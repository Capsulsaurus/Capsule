import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

// MARK: - ImportAndScopesSettingsTests

/// The screen's job is to make a destination *explainable*: every row carries
/// the rule that produced it, and the ladder is drawn in full.
@Suite("Import destinations are explained rung by rung")
@MainActor
struct ImportAndScopesSettingsTests {
    private static func model(
        settings: StubSettingsPort = StubSettingsPort(defaultAlbum: LibraryFixture.defaultAlbumID),
        importing: StubImportPort = StubImportPort(),
        albums: StubAlbumPort = StubAlbumPort(),
        sourceKindDefaults: [SourceKind: AlbumID] = [:],
        connection: ConnectionClass? = .unmetered
    ) -> ImportAndScopesSettingsModel {
        ImportAndScopesSettingsModel(
            settings: settings,
            importing: importing,
            albums: albums,
            connectivity: .stub(connection: connection),
            sourceKindDefaults: sourceKindDefaults
        )
    }

    @Test("loading reads the sources, the albums, and the pointer")
    func loadReadsEverythingTheLadderNeeds() async {
        let model = Self.model()

        await model.load()

        #expect(model.phase == .ready)
        #expect(model.scopes.count == 3)
        #expect(model.albums.count == 3)
        #expect(model.ownerDefaultAlbumID == LibraryFixture.defaultAlbumID)
        #expect(model.derivedDefaultAlbumID == LibraryFixture.defaultAlbumID)
        #expect(model.resolutions.count == 3)
    }

    @Test("a library with no sources and no albums is empty, not ready")
    func emptyLibraryIsItsOwnPhase() async {
        let model = Self.model(
            settings: StubSettingsPort(),
            importing: StubImportPort(scopes: []),
            albums: StubAlbumPort(albums: [])
        )

        await model.load()

        #expect(model.phase == .empty)
        #expect(model.resolutions.isEmpty)
        #expect(model.derivedDefaultAlbumID == nil)
    }

    @Test("a read that fails is classified by its code")
    func failedReadIsClassified() async {
        let model = Self.model(settings: StubSettingsPort(readFailure: StubError.failure(.syncUnauthenticated)))

        await model.load()

        #expect(model.phase == .failed(.syncUnauthenticated))
        #expect(model.scopes.isEmpty)
    }

    @Test("a read that fails while offline reads as offline")
    func offlineReadIsOffline() async {
        let model = Self.model(
            settings: StubSettingsPort(readFailure: StubError.failure(.syncUnauthenticated)),
            connection: .offline
        )

        await model.load()

        #expect(model.phase == .offline)
    }

    @Test("with only the owner pointer set, every source resolves by the pointer")
    func pointerIsTheDefaultRung() async {
        let model = Self.model()
        await model.load()

        let rules = model.resolutions.map(\.rule)
        let albums = model.resolutions.map(\.albumID)

        #expect(rules.allSatisfy { $0 == .ownerDefaultPointer })
        #expect(albums.allSatisfy { $0 == LibraryFixture.defaultAlbumID })
    }

    @Test("a per-source-kind default outranks the owner pointer for that kind only")
    func sourceKindDefaultOutranksThePointer() async {
        let model = Self.model(sourceKindDefaults: [.screenshots: LibraryFixture.screenshotsAlbumID])
        await model.load()

        #expect(model.rule(for: LibraryFixture.screenshots) == .sourceKindDefault)
        #expect(model.rule(for: LibraryFixture.cameraRoll) == .ownerDefaultPointer)
        let screenshots = model.resolutions.first { $0.scope == LibraryFixture.screenshots }
        #expect(screenshots?.albumID == LibraryFixture.screenshotsAlbumID)
    }

    @Test("a scope override outranks both the kind default and the pointer")
    func scopeOverrideOutranksLowerRungs() async {
        let model = Self.model(sourceKindDefaults: [.screenshots: LibraryFixture.screenshotsAlbumID])
        await model.load()

        await model.setOverride(LibraryFixture.travelAlbumID, for: LibraryFixture.screenshots)

        #expect(model.rule(for: LibraryFixture.screenshots) == .scopeOverride)
        let screenshots = model.resolutions.first { $0.scope == LibraryFixture.screenshots }
        #expect(screenshots?.albumID == LibraryFixture.travelAlbumID)
        #expect(model.rule(for: LibraryFixture.folder) == .ownerDefaultPointer, "one source, not all of them")
    }

    @Test("clearing an override drops the row back to the rung below it")
    func clearingAnOverrideFallsBack() async {
        let model = Self.model()
        await model.load()
        await model.setOverride(LibraryFixture.travelAlbumID, for: LibraryFixture.folder)
        #expect(model.rule(for: LibraryFixture.folder) == .scopeOverride)

        await model.setOverride(nil, for: LibraryFixture.folder)

        #expect(model.rule(for: LibraryFixture.folder) == .ownerDefaultPointer)
    }

    @Test("re-pointing the owner default is recorded and read back")
    func ownerDefaultCanBeRepointed() async {
        let port = StubSettingsPort(defaultAlbum: LibraryFixture.defaultAlbumID)
        let model = Self.model(settings: port)
        await model.load()

        await model.setOwnerDefault(LibraryFixture.travelAlbumID)

        #expect(model.ownerDefaultAlbumID == LibraryFixture.travelAlbumID)
        let stored = await port.storedDefaultAlbum
        #expect(stored == LibraryFixture.travelAlbumID)
    }

    @Test("a write that fails is surfaced rather than silently dropped")
    func failedWriteIsSurfaced() async {
        let port = StubSettingsPort(
            defaultAlbum: LibraryFixture.defaultAlbumID,
            writeFailure: StubError.failure(.albumNotAvailable)
        )
        let model = Self.model(settings: port)
        await model.load()

        await model.setOverride(LibraryFixture.travelAlbumID, for: LibraryFixture.folder)

        #expect(model.phase == .failed(.albumNotAvailable))
        #expect(model.rule(for: LibraryFixture.folder) == .ownerDefaultPointer)
    }

    /// The default album is nameless by design, so the view renders the
    /// catalog's word for it rather than an empty row.
    @Test("the nameless default album has no name to show")
    func namelessDefaultAlbumHasNoName() async {
        let model = Self.model()
        await model.load()

        #expect(model.albumName(LibraryFixture.defaultAlbumID) == nil)
        #expect(model.albumName(LibraryFixture.travelAlbumID) == "Travel")
        #expect(model.albumName(AlbumID.managed(uuid: "album-missing")) == nil)
    }

    /// An explicit pick is made *during* an import, and a settings screen has no
    /// import in flight — the rung is still drawn because it outranks everything
    /// the user can configure here.
    @Test("the explicit-pick rung never fires on this screen but is still in the ladder")
    func explicitPickIsNeverTheAnswerHere() async {
        let model = Self.model()
        await model.load()

        let rules = model.resolutions.map(\.rule)

        #expect(!rules.contains(.explicitUserPick))
        #expect(DestinationResolution.order.first == .explicitUserPick)
    }

    @Test("a row is identified by its scope, so the table has no duplicates")
    func rowsAreIdentifiedByScope() async {
        let model = Self.model()
        await model.load()

        let identifiers = model.resolutions.map(\.id)

        #expect(Set(identifiers).count == identifiers.count)
        #expect(identifiers.contains(LibraryFixture.cameraRoll.scopeID))
    }
}
