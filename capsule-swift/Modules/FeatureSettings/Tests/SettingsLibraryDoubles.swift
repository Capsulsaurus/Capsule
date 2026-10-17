import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - Library fixtures

/// Albums and import scopes built by hand, so a resolution test states its own
/// world rather than inheriting one from a scenario.
enum LibraryFixture {
    static let defaultAlbumID = AlbumID.managed(uuid: "album-default")
    static let screenshotsAlbumID = AlbumID.managed(uuid: "album-screenshots")
    static let travelAlbumID = AlbumID.managed(uuid: "album-travel")

    static let policy = AlbumPolicy(historyPolicy: .full, retentionDays: 30, protocolVersion: "2026-05-01")

    /// The de facto default album is nameless by design.
    static let albums: [ContainerAlbum] = [
        ContainerAlbum(
            id: defaultAlbumID,
            name: nil,
            count: 4000,
            epoch: 1,
            policy: policy,
            isDefault: true
        ),
        ContainerAlbum(id: screenshotsAlbumID, name: "Screenshots", count: 120, epoch: 1, policy: policy),
        ContainerAlbum(id: travelAlbumID, name: "Travel", count: 300, epoch: 2, policy: policy),
    ]

    static func scope(_ kind: SourceKind, locator: String) -> ImportScope {
        ImportScope(scopeID: "scope-\(kind.rawValue)", platform: .ios, sourceKind: kind, locator: locator)
    }

    static let cameraRoll = scope(.cameraRoll, locator: "photokit://camera-roll")
    static let screenshots = scope(.screenshots, locator: "photokit://screenshots")
    static let folder = scope(.folder, locator: "file:///Volumes/Photos/2026")

    static let scopes = [cameraRoll, screenshots, folder]
}

// MARK: - StubAlbumPort

/// An ``AlbumPort`` that answers the one question a settings screen asks: which
/// container albums exist. Everything else on the protocol is a membership
/// commit, which no settings screen performs.
actor StubAlbumPort: AlbumPort {
    private let albums: [ContainerAlbum]
    private let readFailure: CapsuleError?

    init(albums: [ContainerAlbum] = LibraryFixture.albums, readFailure: CapsuleError? = nil) {
        self.albums = albums
        self.readFailure = readFailure
    }

    func containerAlbums() async throws -> [ContainerAlbum] {
        if let readFailure { throw readFailure }
        return albums
    }

    func containerAlbum(_ identifier: AlbumID) async throws -> ContainerAlbum? {
        albums.first { $0.id == identifier }
    }

    func viewAlbums() async throws -> [ViewAlbum] { [] }

    func resolveDefaultAlbum(
        for _: ImportScope?
    ) async throws -> (album: ContainerAlbum, rule: ImportPlan.DestinationRule) {
        guard let album = albums.first(where: \.isDefault) else { throw StubError.unimplemented }
        return (album, .derivedDefaultAlbum)
    }

    func createAlbum(name _: String, policy _: AlbumPolicy) async throws -> ContainerAlbum {
        throw StubError.unimplemented
    }

    func renameAlbum(_: AlbumID, to _: String) async throws {}

    func setCoverAsset(_: AssetID?, for _: AlbumID) async throws {}

    func deleteAlbum(_: AlbumID) async throws {}

    func move(_: [AssetID], to _: AlbumID) async throws {}

    func inviteMember(handle _: String, role _: AlbumRole, to _: AlbumID) async throws {}

    func setMemberRole(_: AlbumRole, for _: String, in _: AlbumID) async throws {}

    func removeMember(handle _: String, from _: AlbumID) async throws {}

    nonisolated func changes() -> AsyncStream<Void> {
        AsyncStream { $0.finish() }
    }
}

// MARK: - StubImportPort

/// An ``ImportPort`` that lists sources. A settings screen runs no imports, so
/// everything that would move bytes refuses rather than pretending.
actor StubImportPort: ImportPort {
    private let scopes: [ImportScope]
    private let readFailure: CapsuleError?

    init(scopes: [ImportScope] = LibraryFixture.scopes, readFailure: CapsuleError? = nil) {
        self.scopes = scopes
        self.readFailure = readFailure
    }

    func availableScopes() async throws -> [ImportScope] {
        if let readFailure { throw readFailure }
        return scopes
    }

    func scan(_: ImportScope) async throws -> ImportScan {
        throw StubError.unimplemented
    }

    func plan(
        _: ImportScan,
        destination _: AlbumID?,
        mode _: ImportMode,
        uploadPolicy _: UploadPolicy,
        streaming _: Bool
    ) async throws -> ImportPlan {
        throw StubError.unimplemented
    }

    nonisolated func execute(_: ImportPlan) -> AsyncStream<ImportProgressEvent> {
        AsyncStream { $0.finish() }
    }

    func cancel(_: ImportID) async throws {}

    func resolveScope(sourceKind _: SourceKind, locator _: String) async throws -> ImportScope {
        throw StubError.unimplemented
    }

    nonisolated func scanStream(_: ImportScope) -> AsyncStream<ImportScanEvent> {
        AsyncStream { $0.finish() }
    }

    func retry(_: ImportID, locators _: [String]) async throws -> [ImportResult] { [] }

    func history(limit _: Int) async throws -> [ImportSessionRecord] { [] }

    func replan(_: ImportID) async throws -> ImportPlan {
        throw StubError.unimplemented
    }

    func dismissSession(_: ImportID) async throws {}
}
