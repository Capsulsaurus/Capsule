import CapsuleDomain
import Testing

@testable import FeatureImport

/// The picker's one non-negotiable rule: a source the platform cannot provide is
/// **absent**, not disabled.
@Suite("Import source picker")
@MainActor
struct ImportSourcePickerModelTests {
    private func model(
        platform: ImportPlatform,
        scopes: [ImportScope] = []
    ) -> ImportSourcePickerModel {
        ImportSourcePickerModel(
            importing: StubImportPort(scopes: scopes),
            connectivity: StubFixtures.connectivity,
            platform: platform
        )
    }

    @Test("a handheld is never offered a watched folder or a mounted volume")
    func handheldHidesDesktopSources() async {
        let picker = model(platform: .handheld)

        await picker.load()

        #expect(!picker.offers(.watchedDirectory))
        #expect(!picker.offers(.removableVolume))
        #expect(picker.offers(.cameraRoll))
        #expect(picker.offers(.folder))
        #expect(picker.offers(.takeoutArchive))
    }

    @Test("a Mac is offered every source")
    func desktopOffersEverything() async {
        let picker = model(platform: .desktop)

        await picker.load()

        #expect(picker.rows.count == ImportSourceOption.catalog.count)
        #expect(picker.offers(.watchedDirectory))
        #expect(picker.offers(.removableVolume))
    }

    @Test("a discovered scope makes its row scannable on the tap")
    func discoveredScopeIsPaired() async {
        let picker = model(platform: .handheld, scopes: [StubFixtures.cameraRollScope])

        await picker.load()
        guard let row = picker.rows.first(where: { $0.option.kind == .cameraRoll }) else {
            Issue.record("the photo library was not offered")
            return
        }

        #expect(row.discovered == StubFixtures.cameraRollScope)
        #expect(row.scansImmediately)
        #expect(picker.select(row) == StubFixtures.cameraRollScope)
        #expect(picker.selection == StubFixtures.cameraRollScope)
    }

    /// A pickable source has nothing to scan until the user points at
    /// something, so tapping it must not quietly select a different source's
    /// scope.
    @Test("a pickable source refuses to select without a location")
    func pickableSourceNeedsLocation() async {
        let picker = model(platform: .handheld, scopes: [StubFixtures.cameraRollScope])

        await picker.load()
        guard let files = picker.rows.first(where: { $0.option.kind == .folder }) else {
            Issue.record("Files was not offered")
            return
        }

        #expect(files.needsLocation)
        #expect(picker.select(files) == nil)
        #expect(picker.selection == nil)
    }

    /// The scope id comes from the port, never from a Swift derivation: two
    /// devices have to agree on it byte-for-byte.
    @Test("a picked location is resolved into a scope by the port")
    func pickedLocationResolvesThroughPort() async {
        let picker = model(platform: .handheld)
        await picker.load()
        guard let takeout = picker.rows.first(where: { $0.option.kind == .takeoutArchive }) else {
            Issue.record("the Takeout archive was not offered")
            return
        }

        let scope = await picker.choose(takeout, locator: "file:///Downloads/takeout.zip")

        #expect(scope?.sourceKind == .takeoutArchive)
        #expect(scope?.locator == "file:///Downloads/takeout.zip")
        #expect(picker.selection == scope)
    }

    @Test("every offered source states what it will do")
    func everyRowExplainsItself() {
        for option in ImportSourceOption.catalog {
            #expect(!option.titleKey.isEmpty)
            #expect(option.detailKey.hasPrefix("app.import.source."))
        }
    }
}
