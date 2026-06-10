import CapsuleCatalog
import Foundation

/// An in-memory ``AssetCatalog`` for tests.
///
/// `MockCatalog` is a *faithful* implementation of the catalog contract — same
/// timeline ordering, soft-delete visibility, and album semantics as the real
/// SQLite-backed ``CapsuleCatalog`` — so any consumer tested against it sees
/// realistic behaviour without an FFI dependency. It is an `actor`, matching
/// the contract's concurrency model.
public actor MockCatalog: AssetCatalog {
    private var assets: [String: CatalogAsset] = [:]
    private var albumsByID: [String: CatalogAlbum] = [:]
    private var stacks: [String: CatalogStack] = [:]
    private var membersByStack: [String: [CatalogStackMember]] = [:]
    private let schemaVersionValue: UInt32

    /// The current wall-clock, overridable so trash-expiry tests are deterministic.
    public var now = Int64(Date().timeIntervalSince1970)

    /// When `true`, ``insertAsset(_:)`` throws — to exercise import rollback.
    public var failInserts = false

    public init(schemaVersion: UInt32 = 2) {
        schemaVersionValue = schemaVersion
    }

    /// Pin the mock's notion of "now" — used by ``expiredTrash(olderThanSeconds:)``.
    public func setNow(_ value: Int64) {
        now = value
    }

    /// Make ``insertAsset(_:)`` throw, to exercise import rollback.
    public func setFailInserts(_ value: Bool) {
        failInserts = value
    }

    public func schemaVersion() -> UInt32 {
        schemaVersionValue
    }

    // MARK: Assets

    public func insertAsset(_ asset: CatalogAsset) throws {
        if failInserts {
            throw CatalogError.Database(message: "mock: insert failure injected")
        }
        guard assets[asset.id] == nil else {
            throw CatalogError.Database(message: "asset already exists: \(asset.id)")
        }
        assets[asset.id] = asset
    }

    public func upsertAsset(_ asset: CatalogAsset) {
        assets[asset.id] = asset
    }

    public func asset(id: String) -> CatalogAsset? {
        assets[id]
    }

    public func asset(hashSHA256: String) -> CatalogAsset? {
        assets.values.first { $0.hashSHA256 == hashSHA256 }
    }

    public func timeline(filter: TimelineFilter, offset: Int, limit: Int) -> [CatalogAsset] {
        let visible = assets.values
            .filter { !$0.isDeleted && !$0.isStackHidden }
            .filter { Self.filter(filter, matches: $0) }
        return Self.page(Self.timelineSorted(visible), offset: offset, limit: limit)
    }

    public func softDeleteAsset(id: String, deletedAt: Int64) {
        assets[id]?.isDeleted = true
        assets[id]?.deletedAt = deletedAt
    }

    public func restoreAsset(id: String) {
        assets[id]?.isDeleted = false
        assets[id]?.deletedAt = nil
    }

    public func expiredTrash(olderThanSeconds: Int64) -> [CatalogAsset] {
        let threshold = now - olderThanSeconds
        return assets.values
            .filter { $0.isDeleted && ($0.deletedAt ?? 0) < threshold }
            .sorted { ($0.deletedAt ?? 0) < ($1.deletedAt ?? 0) }
    }

    public func trash(offset: Int, limit: Int) -> [CatalogAsset] {
        let deleted = assets.values
            .filter(\.isDeleted)
            .sorted { ($0.deletedAt ?? 0) > ($1.deletedAt ?? 0) }
        return Self.page(deleted, offset: offset, limit: limit)
    }

    public func purgeAsset(id: String) {
        assets[id] = nil
    }

    // MARK: Stacks

    public func insertStack(_ stack: CatalogStack) {
        stacks[stack.id] = stack
    }

    public func insertStackMember(_ member: CatalogStackMember) {
        membersByStack[member.stackID, default: []].append(member)
    }

    public func updateStackHidden(assetID: String, hidden: Bool) {
        assets[assetID]?.isStackHidden = hidden
    }

    public func updateStackPrimary(stackID: String, primaryAssetID: String) {
        stacks[stackID]?.primaryAssetID = primaryAssetID
    }

    public func stackMembers(stackID: String) -> [CatalogStackMember] {
        (membersByStack[stackID] ?? []).sorted { $0.sequenceOrder < $1.sequenceOrder }
    }

    // MARK: Albums

    public func insertAlbum(_ album: CatalogAlbum) throws {
        guard albumsByID[album.id] == nil else {
            throw CatalogError.Database(message: "album already exists: \(album.id)")
        }
        albumsByID[album.id] = album
    }

    public func updateAlbum(_ album: CatalogAlbum) {
        albumsByID[album.id] = album
    }

    public func deleteAlbum(id: String) {
        albumsByID[id] = nil
        for assetID in assets.keys where assets[assetID]?.albumID == id {
            assets[assetID]?.albumID = nil
        }
    }

    public func album(id: String) -> CatalogAlbum? {
        albumsByID[id]
    }

    public func albums() -> [CatalogAlbum] {
        albumsByID.values.sorted { $0.createdAt > $1.createdAt }
    }

    public func setAssetAlbum(assetID: String, albumID: String?) {
        assets[assetID]?.albumID = albumID
    }

    public func albumAssets(albumID: String, offset: Int, limit: Int) -> [CatalogAsset] {
        let visible = assets.values
            .filter { $0.albumID == albumID && !$0.isDeleted && !$0.isStackHidden }
        return Self.page(Self.timelineSorted(visible), offset: offset, limit: limit)
    }

    // MARK: Helpers

    /// The catalog's stable timeline order: effective capture instant
    /// descending, ties broken by `id` ascending.
    private static func timelineSorted(_ values: some Sequence<CatalogAsset>) -> [CatalogAsset] {
        values.sorted { lhs, rhs in
            lhs.effectiveCaptureTimestamp != rhs.effectiveCaptureTimestamp
                ? lhs.effectiveCaptureTimestamp > rhs.effectiveCaptureTimestamp
                : lhs.id < rhs.id
        }
    }

    private static func filter(_ filter: TimelineFilter, matches asset: CatalogAsset) -> Bool {
        if let type = filter.assetType, asset.assetType != type { return false }
        if let after = filter.capturedAfter, asset.effectiveCaptureTimestamp < after { return false }
        if let before = filter.capturedBefore, asset.effectiveCaptureTimestamp > before { return false }
        return true
    }

    private static func page<Element>(_ items: [Element], offset: Int, limit: Int) -> [Element] {
        guard offset >= 0, limit > 0, offset < items.count else { return [] }
        return Array(items[offset ..< min(offset + limit, items.count)])
    }
}
