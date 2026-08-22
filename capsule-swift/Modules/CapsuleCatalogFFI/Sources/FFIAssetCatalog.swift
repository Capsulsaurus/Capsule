import CapsuleFoundation
import Foundation

/// The production ``AssetCatalog`` — a Swift `actor` over the Rust `Catalog`
/// UniFFI object.
///
/// Every SQLite call is synchronous Rust behind a `Mutex`; confining the
/// `Catalog` to this actor moves that blocking work off the main thread and
/// serialises it, so a catalog query can never stall a frame. The raw FFI
/// `Catalog` and the generated record types never escape this type — callers
/// see only the `AssetCatalog` contract and the native `Catalog*` models.
///
/// - Important: ``init(openingCatalogAt:)`` runs schema migration synchronously;
///   construct it off the main actor (e.g. inside a `Task`).
public actor CapsuleCatalog: AssetCatalog {
    private let catalog: Catalog

    /// Routes Rust `log` records into Apple unified logging. A `static let` so
    /// the `oslog` backend is installed exactly once per process.
    private static let loggingBootstrap: Void = {
        initLogging()
        CapsuleLog.catalog.info("rust core logging bridged into oslog")
    }()

    /// Open (creating and migrating if necessary) the catalog at `url`.
    public init(openingCatalogAt url: URL) throws {
        _ = Self.loggingBootstrap
        CapsuleLog.catalog.info("opening catalog at \(url.path, privacy: .public)")
        catalog = try Catalog.open(path: url.path)
    }

    private init(catalog: Catalog) {
        _ = Self.loggingBootstrap
        self.catalog = catalog
    }

    /// Open an ephemeral in-memory catalog, for tests and SwiftUI previews.
    public static func inMemory() throws -> CapsuleCatalog {
        CapsuleLog.catalog.debug("opening in-memory catalog")
        return try CapsuleCatalog(catalog: Catalog.openInMemory())
    }

    public func schemaVersion() throws -> UInt32 {
        try catalog.schemaVersion()
    }

    // MARK: Assets

    public func insertAsset(_ asset: CatalogAsset) throws {
        CapsuleLog.catalog.debug("insertAsset id=\(asset.id, privacy: .public)")
        try catalog.insertAsset(asset: asset.ffiRecord)
    }

    public func upsertAsset(_ asset: CatalogAsset) throws {
        CapsuleLog.catalog.debug("upsertAsset id=\(asset.id, privacy: .public)")
        try catalog.upsertAsset(asset: asset.ffiRecord)
    }

    public func asset(id: String) throws -> CatalogAsset? {
        CapsuleLog.catalog.trace("asset id=\(id, privacy: .public)")
        return try catalog.findByUuid(uuid: id).map(CatalogAsset.init)
    }

    public func asset(hashSHA256: String) throws -> CatalogAsset? {
        CapsuleLog.catalog.trace("asset hash lookup")
        return try catalog.findByHash(hash: hashSHA256).map(CatalogAsset.init)
    }

    public func timeline(filter: TimelineFilter, offset: Int, limit: Int) throws -> [CatalogAsset] {
        let signposter = CapsuleSignpost.catalog
        let interval = signposter.beginInterval("timeline")
        defer { signposter.endInterval("timeline", interval) }
        CapsuleLog.catalog.trace("timeline offset=\(offset) limit=\(limit) filtered=\(!filter.isUnfiltered)")
        let records = try catalog.queryTimelineFiltered(
            assetType: filter.assetType,
            after: filter.capturedAfter,
            before: filter.capturedBefore,
            offset: pageValue(offset),
            limit: pageValue(limit)
        )
        return records.map(CatalogAsset.init)
    }

    public func softDeleteAsset(id: String, deletedAt: Int64) throws {
        CapsuleLog.catalog.debug("softDeleteAsset id=\(id, privacy: .public)")
        try catalog.softDelete(uuid: id, deletedAt: deletedAt)
    }

    public func restoreAsset(id: String) throws {
        CapsuleLog.catalog.debug("restoreAsset id=\(id, privacy: .public)")
        try catalog.restoreAsset(uuid: id)
    }

    public func expiredTrash(olderThanSeconds: Int64) throws -> [CatalogAsset] {
        try catalog.queryExpiredTrash(olderThanSecs: olderThanSeconds).map(CatalogAsset.init)
    }

    public func trash(offset: Int, limit: Int) throws -> [CatalogAsset] {
        CapsuleLog.catalog.trace("trash offset=\(offset) limit=\(limit)")
        return try catalog.queryTrash(offset: pageValue(offset), limit: pageValue(limit))
            .map(CatalogAsset.init)
    }

    public func purgeAsset(id: String) throws {
        CapsuleLog.catalog.debug("purgeAsset id=\(id, privacy: .public)")
        try catalog.purgeAsset(uuid: id)
    }

    // MARK: Stacks

    public func insertStack(_ stack: CatalogStack) throws {
        CapsuleLog.catalog.debug("insertStack id=\(stack.id, privacy: .public)")
        try catalog.insertStack(stack: stack.ffiRecord)
    }

    public func insertStackMember(_ member: CatalogStackMember) throws {
        CapsuleLog.catalog.debug("insertStackMember stack=\(member.stackID, privacy: .public)")
        try catalog.insertStackMember(member: member.ffiRecord)
    }

    public func updateStackHidden(assetID: String, hidden: Bool) throws {
        CapsuleLog.catalog.debug("updateStackHidden asset=\(assetID, privacy: .public) hidden=\(hidden)")
        try catalog.updateStackHidden(uuid: assetID, hidden: hidden)
    }

    public func updateStackPrimary(stackID: String, primaryAssetID: String) throws {
        CapsuleLog.catalog.debug("updateStackPrimary stack=\(stackID, privacy: .public)")
        try catalog.updateStackPrimary(stackId: stackID, primaryUuid: primaryAssetID)
    }

    public func stackMembers(stackID: String) throws -> [CatalogStackMember] {
        try catalog.listStackMembers(stackId: stackID).map(CatalogStackMember.init)
    }

    // MARK: Albums

    public func insertAlbum(_ album: CatalogAlbum) throws {
        CapsuleLog.catalog.debug("insertAlbum id=\(album.id, privacy: .public)")
        try catalog.insertAlbum(album: album.ffiRecord)
    }

    public func updateAlbum(_ album: CatalogAlbum) throws {
        CapsuleLog.catalog.debug("updateAlbum id=\(album.id, privacy: .public)")
        try catalog.updateAlbum(album: album.ffiRecord)
    }

    public func deleteAlbum(id: String) throws {
        CapsuleLog.catalog.debug("deleteAlbum id=\(id, privacy: .public)")
        try catalog.deleteAlbum(id: id)
    }

    public func album(id: String) throws -> CatalogAlbum? {
        try catalog.findAlbum(id: id).map(CatalogAlbum.init)
    }

    public func albums() throws -> [CatalogAlbum] {
        try catalog.listAlbums().map(CatalogAlbum.init)
    }

    public func setAssetAlbum(assetID: String, albumID: String?) throws {
        CapsuleLog.catalog.debug("setAssetAlbum asset=\(assetID, privacy: .public) album=\(albumID ?? "nil", privacy: .public)")
        try catalog.setAssetAlbum(uuid: assetID, albumId: albumID)
    }

    public func albumAssets(albumID: String, offset: Int, limit: Int) throws -> [CatalogAsset] {
        CapsuleLog.catalog.trace("albumAssets album=\(albumID, privacy: .public) offset=\(offset) limit=\(limit)")
        let records = try catalog.queryAlbumAssets(
            albumId: albumID,
            offset: pageValue(offset),
            limit: pageValue(limit)
        )
        return records.map(CatalogAsset.init)
    }

    /// Clamp a Swift signed pagination value to the unsigned FFI domain.
    private func pageValue(_ value: Int) -> UInt64 {
        UInt64(max(0, value))
    }
}
