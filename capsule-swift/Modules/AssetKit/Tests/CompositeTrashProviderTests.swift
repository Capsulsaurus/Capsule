import Foundation
import Testing

import AssetKit
import CapsuleCatalog
import CapsuleFoundation
import CapsuleTestSupport
import ManagedStore

@Suite("Composite trash provider")
struct CompositeTrashProviderTests {
    private let layout = ManagedLibraryLayout(root: URL(filePath: "/capsule/Library"))

    /// A managed provider over an in-memory catalog holding one trashed row,
    /// built over `gate` exactly as the composition root builds it.
    private func makeManaged(gate: SecondaryTrashGate, trashedID: String) async throws -> ManagedProvider {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(
            id: trashedID,
            captureTimestamp: 2000,
            isDeleted: true,
            deletedAt: 2001
        ))
        return ManagedProvider(
            library: ManagedLibrary(layout: layout, fileStore: MockFileStore(), catalog: catalog),
            authGate: gate
        )
    }

    @Test("one unlock opens every source, and the listing merges them newest first")
    func unlockOpensEverySource() async throws {
        let gate = SecondaryTrashGate()
        let managed = try await makeManaged(gate: gate, trashedID: "import")
        let primary = FakeTrash(trashed: [Self.asset("old", at: 1000), Self.asset("new", at: 3000)])
        let trash = CompositeTrashProvider(primary: primary, secondaries: [managed], secondaryGate: gate)

        #expect(await trash.isTrashUnlocked() == false)
        await #expect(throws: CatalogError.self) { try await trash.trashedAssets() }

        try await trash.unlockTrash()

        #expect(await trash.isTrashUnlocked())
        #expect(await primary.unlockCount == 1)
        let ids = try await trash.trashedAssets().map(\.id)
        #expect(ids == [.managed(uuid: "new"), .managed(uuid: "import"), .managed(uuid: "old")])
    }

    @Test("a refused primary challenge is thrown and opens nothing")
    func refusalOpensNothing() async throws {
        let gate = SecondaryTrashGate()
        let managed = try await makeManaged(gate: gate, trashedID: "import")
        let primary = FakeTrash(trashed: [], refusal: LocalAuthError.cancelled)
        let trash = CompositeTrashProvider(primary: primary, secondaries: [managed], secondaryGate: gate)

        await #expect(throws: LocalAuthError.cancelled) { try await trash.unlockTrash() }

        #expect(await managed.isTrashUnlocked() == false)
    }

    @Test("a secondary cannot be opened except through the composite")
    func secondaryRefusesDirectUnlock() async throws {
        let gate = SecondaryTrashGate()
        let managed = try await makeManaged(gate: gate, trashedID: "import")

        await #expect(throws: LocalAuthError.failed) { try await managed.unlockTrash() }
        #expect(throws: LocalAuthError.failed) { try gate.authenticate(view: .recentlyDeleted) }
    }

    @Test("restore and purge go to the store that owns the asset")
    func routesByOwnership() async throws {
        let gate = SecondaryTrashGate()
        let managed = try await makeManaged(gate: gate, trashedID: "import")
        let primary = FakeTrash(trashed: [Self.asset("library", at: 1000)])
        let trash = CompositeTrashProvider(primary: primary, secondaries: [managed], secondaryGate: gate)
        try await trash.unlockTrash()

        try await trash.restore(.managed(uuid: "import"))
        try await trash.restore(.managed(uuid: "library"))

        #expect(await primary.restored == [.managed(uuid: "library")])
        #expect(try await managed.loadTimeline().count == 1)

        try await trash.purge(.managed(uuid: "import"))
        try await trash.purge(.managed(uuid: "library"))

        #expect(await primary.purged == [.managed(uuid: "library")])
        #expect(await managed.owns(.managed(uuid: "import")) == false)
    }

    private static func asset(_ uuid: String, at seconds: TimeInterval) -> Asset {
        Asset(id: .managed(uuid: uuid), mediaType: .photo, captureDate: Date(timeIntervalSince1970: seconds))
    }
}

@Suite("Composite caption store")
struct CompositeCaptionStoreTests {
    @Test("an owned asset's caption goes to its owner, everything else to the primary")
    func routesByOwnership() async throws {
        let primary = FakeCaptions(owned: [])
        let secondary = FakeCaptions(owned: [.managed(uuid: "import")])
        let store = CompositeCaptionStore(primary: primary, secondaries: [secondary])

        try await store.setCaption("beach", for: .managed(uuid: "import"))
        try await store.setCaption("harbour", for: .managed(uuid: "library"))

        #expect(await secondary.captions == [.managed(uuid: "import"): "beach"])
        #expect(await primary.captions == [.managed(uuid: "library"): "harbour"])
        #expect(await store.caption(for: .managed(uuid: "import")) == "beach")
    }

    @Test("an import's caption is kept in the catalog register and its sidecar, not the primary")
    func managedKeepsCaption() async throws {
        let fixture = try await ManagedCaptionFixture()
        let primary = FakeCaptions(owned: [])
        let store = CompositeCaptionStore(
            primary: primary,
            secondaries: [ManagedCaptionStore(provider: fixture.provider)]
        )

        try await store.setCaption("beach", for: .managed(uuid: "import"))

        #expect(await primary.captions.isEmpty)
        #expect(await store.caption(for: .managed(uuid: "import")) == "beach")
        #expect(await fixture.catalog.asset(id: "import")?.caption == "beach")
        #expect(try await fixture.sidecarCaption() == "beach")
    }

    @Test("clearing an import's caption clears both the register and the sidecar")
    func managedClearsCaption() async throws {
        let fixture = try await ManagedCaptionFixture()
        let store = ManagedCaptionStore(provider: fixture.provider)

        try await store.setCaption("beach", for: .managed(uuid: "import"))
        try await store.setCaption("", for: .managed(uuid: "import"))

        #expect(await store.caption(for: .managed(uuid: "import")) == nil)
        #expect(await fixture.catalog.asset(id: "import")?.caption == nil)
        #expect(try await fixture.sidecarCaption() == nil)
    }

    @Test("a caption whose sidecar cannot be rewritten is refused, and the register is put back")
    func managedRefusesWithoutSidecar() async throws {
        let fixture = try await ManagedCaptionFixture(withSidecar: false)
        let store = ManagedCaptionStore(provider: fixture.provider)

        await #expect(throws: CatalogError.sidecar(message: "no sidecar for import")) {
            try await store.setCaption("beach", for: .managed(uuid: "import"))
        }
        #expect(await fixture.catalog.asset(id: "import")?.caption == nil)
        #expect(await store.caption(for: .managed(uuid: "import")) == nil)
    }
}

@Suite("ManagedProvider locations")
struct ManagedProviderLocationTests {
    @Test("an import's GPS fix is its location; an import without one, or another store's asset, is omitted")
    func locationsFromCatalogRows() async throws {
        let catalog = MockCatalog()
        var located = Fixtures.catalogAsset(id: "located")
        located.latitude = 48.8584
        located.longitude = 2.2945
        var halfLocated = Fixtures.catalogAsset(id: "half")
        halfLocated.latitude = 10
        try await catalog.insertAsset(located)
        try await catalog.insertAsset(halfLocated)
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "unlocated"))
        let provider = ManagedProvider(
            library: ManagedLibrary(
                layout: ManagedLibraryLayout(root: URL(filePath: "/capsule/Library")),
                fileStore: MockFileStore(),
                catalog: catalog
            ),
            authGate: MockLocalAuthGate()
        )

        let locations = await provider.locations(for: [
            .managed(uuid: "located"),
            .managed(uuid: "half"),
            .managed(uuid: "unlocated"),
            .managed(uuid: "not-mine"),
            .photoKit(localIdentifier: "elsewhere"),
        ])

        #expect(locations == [.managed(uuid: "located"): AssetCoordinate(latitude: 48.8584, longitude: 2.2945)])
    }
}

// MARK: - Fakes

/// A trash source with a scripted gate and a record of what it was asked to do.
private actor FakeTrash: TrashProvider {
    private var trashed: [Asset]
    private let refusal: LocalAuthError?
    private var unlocked = false
    private(set) var unlockCount = 0
    private(set) var restored: [AssetID] = []
    private(set) var purged: [AssetID] = []

    init(trashed: [Asset], refusal: LocalAuthError? = nil) {
        self.trashed = trashed
        self.refusal = refusal
    }

    func trashedAssets() throws -> [Asset] {
        guard unlocked else { throw CatalogError.viewLocked }
        return trashed
    }

    func restore(_ id: AssetID) {
        restored.append(id)
    }

    func purge(_ id: AssetID) {
        purged.append(id)
    }

    func unlockTrash() throws {
        unlockCount += 1
        if let refusal { throw refusal }
        unlocked = true
    }

    func isTrashUnlocked() -> Bool {
        unlocked
    }
}

/// A caption store that owns a fixed set of ids and keeps what it is given.
private actor FakeCaptions: CaptionStore, AssetOwning {
    private let owned: Set<AssetID>
    private(set) var captions: [AssetID: String] = [:]

    init(owned: Set<AssetID>) {
        self.owned = owned
    }

    func owns(_ id: AssetID) -> Bool {
        owned.contains(id)
    }

    func caption(for id: AssetID) -> String? {
        captions[id]
    }

    func setCaption(_ caption: String?, for id: AssetID) {
        captions[id] = caption
    }
}
