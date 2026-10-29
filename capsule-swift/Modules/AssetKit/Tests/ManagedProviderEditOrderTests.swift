import Foundation
import Testing

import AssetKit
import CapsuleCatalog
import CapsuleFoundation
import CapsuleTestSupport
import ManagedStore

@Suite("ManagedProvider edit order")
struct ManagedProviderEditOrderTests {
    /// Yield often enough that an edit which is not queued would run to its end.
    private static func letOtherTasksRun() async {
        for _ in 0 ..< 200 {
            await Task.yield()
        }
    }

    @Test("a caption edit queued behind one that fails is kept, not overwritten by the failed edit's restore")
    func queuedCaptionSurvivesFailedRestore() async throws {
        let base = MockFileStore()
        let held = HeldFileStore(base: base)
        let fixture = try await ManagedCaptionFixture(fileStore: base, providerFileStore: held)
        let provider = fixture.provider
        await held.holdNextRead()

        let failing = Task { try await provider.setCaption("first", for: .managed(uuid: "import")) }
        await held.waitUntilHeld()
        let queued = Task { try await provider.setCaption("second", for: .managed(uuid: "import")) }
        await Self.letOtherTasksRun()

        #expect(await held.reads == 1, "the second edit must wait for the first, not read the sidecar under it")
        await held.release(throwing: MockFileStore.MockError.injected(.read))
        await #expect(throws: MockFileStore.MockError.injected(.read)) { try await failing.value }
        try await queued.value

        #expect(await fixture.catalog.asset(id: "import")?.caption == "second")
        #expect(try await fixture.sidecarCaption() == "second")
    }

    @Test("a favourite toggled during a caption edit waits for it, so its upsert keeps the caption")
    func favoriteQueuedBehindCaptionKeepsIt() async throws {
        let base = MockFileStore()
        let held = HeldFileStore(base: base)
        let fixture = try await ManagedCaptionFixture(fileStore: base, providerFileStore: held)
        let provider = fixture.provider
        await held.holdNextRead()

        let captioning = Task { try await provider.setCaption("beach", for: .managed(uuid: "import")) }
        await held.waitUntilHeld()
        let favoriting = Task { try await provider.setFavorite(true, for: .managed(uuid: "import")) }
        await Self.letOtherTasksRun()

        #expect(await fixture.catalog.asset(id: "import")?.rating == 0, "the favourite must wait for the caption edit")
        await held.release()
        try await captioning.value
        try await favoriting.value

        let row = try #require(await fixture.catalog.asset(id: "import"))
        #expect(row.caption == "beach")
        #expect(row.rating == 1)
        #expect(try await fixture.sidecarCaption() == "beach")
    }
}

/// A managed provider over one catalogued import (`"import"`), with its sidecar
/// on disk unless `withSidecar` is false.
struct ManagedCaptionFixture {
    static let layout = ManagedLibraryLayout(root: URL(filePath: "/capsule/Library"))
    static let coder = JSONSidecarCoder()

    let provider: ManagedProvider
    let catalog: MockCatalog
    let fileStore: MockFileStore
    let sidecarURL: URL

    /// - Parameter providerFileStore: what the provider reads and writes
    ///   through, when it is not `fileStore` itself (a wrapper over it).
    init(
        withSidecar: Bool = true,
        fileStore: MockFileStore = MockFileStore(),
        providerFileStore: (any FileStore)? = nil
    ) async throws {
        let catalog = MockCatalog()
        let asset = Fixtures.catalogAsset(id: "import")
        try await catalog.insertAsset(asset)
        let captureDate = Date(timeIntervalSince1970: TimeInterval(asset.captureTimestamp))
        let sidecarURL = Self.layout.sidecarFile(uuid: "import", captureDate: captureDate)
        if withSidecar {
            let sidecar = CatalogSidecar(
                version: 1,
                uuid: "import",
                assetType: "photo",
                originalFilename: "import.jpg",
                importTimestamp: 1,
                modifiedTimestamp: 1,
                hashSHA256: asset.hashSHA256,
                fileSize: 5,
                importerVersion: "test",
                rawshiftVersion: "0.0.0"
            )
            try await fileStore.write(Self.coder.encode(sidecar), to: sidecarURL)
        }
        provider = ManagedProvider(
            library: ManagedLibrary(layout: Self.layout, fileStore: providerFileStore ?? fileStore, catalog: catalog),
            authGate: MockLocalAuthGate(),
            sidecarCoder: Self.coder
        )
        self.catalog = catalog
        self.fileStore = fileStore
        self.sidecarURL = sidecarURL
    }

    /// The caption the sidecar on disk carries.
    func sidecarCaption() async throws -> String? {
        let bytes = try #require(await fileStore.data(at: sidecarURL))
        return try Self.coder.decode(bytes).caption
    }
}

/// A ``FileStore`` over a ``MockFileStore`` whose next `read` can be held open,
/// so a test can suspend one sidecar rewrite mid-flight and start a second edit
/// of the same asset while the first waits.
private actor HeldFileStore: FileStore {
    private let base: MockFileStore
    private var holdsNextRead = false
    private var heldRead: CheckedContinuation<Void, Never>?
    private var heldReadFailure: Error?
    private var holdObserver: CheckedContinuation<Void, Never>?
    private(set) var reads = 0

    init(base: MockFileStore) {
        self.base = base
    }

    /// Make the next `read` wait until ``release(throwing:)``.
    func holdNextRead() {
        holdsNextRead = true
    }

    /// Return once the held `read` has begun.
    func waitUntilHeld() async {
        guard heldRead == nil else { return }
        await withCheckedContinuation { holdObserver = $0 }
    }

    /// Let the held `read` finish — with `error` when one is given.
    func release(throwing error: Error? = nil) {
        heldReadFailure = error
        heldRead?.resume()
        heldRead = nil
    }

    func read(at url: URL) async throws -> Data {
        reads += 1
        if holdsNextRead {
            holdsNextRead = false
            await withCheckedContinuation { continuation in
                heldRead = continuation
                holdObserver?.resume()
                holdObserver = nil
            }
            if let heldReadFailure { throw heldReadFailure }
        }
        return try await base.read(at: url)
    }

    func fileExists(at url: URL) async -> Bool {
        await base.fileExists(at: url)
    }

    func createDirectory(at url: URL) async throws {
        try await base.createDirectory(at: url)
    }

    func write(_ data: Data, to url: URL) async throws {
        try await base.write(data, to: url)
    }

    func copyItem(at source: URL, to destination: URL) async throws {
        try await base.copyItem(at: source, to: destination)
    }

    func moveItem(at source: URL, to destination: URL) async throws {
        try await base.moveItem(at: source, to: destination)
    }

    func removeItem(at url: URL) async throws {
        try await base.removeItem(at: url)
    }

    func contentsOfDirectory(at url: URL) async throws -> [URL] {
        try await base.contentsOfDirectory(at: url)
    }

    func fileSize(at url: URL) async throws -> Int64 {
        try await base.fileSize(at: url)
    }
}
