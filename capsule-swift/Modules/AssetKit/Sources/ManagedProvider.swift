import CapsuleCatalog
import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import ManagedStore

/// The ``AssetProvider`` over the Capsule-managed library.
///
/// Reads its timeline from the catalog and maps each ``CatalogAsset`` to the
/// source-agnostic ``Asset``. Delete, restore and purge go through the
/// library's ``ManagedWorkspace``, which owns the trash state machine and its
/// retention window, so on the FFI lane each is a signed record that syncs and
/// survives an index rebuild; the catalog only reads the result. The import
/// flow calls ``refresh()`` once an import completes so the timeline picks up
/// the new assets.
public actor ManagedProvider: AssetProvider, TrashProvider, AssetOwning {
    /// Upper bound on the managed timeline window for the prototype.
    private static let timelineLimit = 10000

    private let library: ManagedLibrary
    private let authGate: any LocalAuthGate
    private let sidecarCoder: any SidecarCoding
    private let uploads: (any UploadPort)?
    private let purgeDelivery: (any PurgeDelivering)?
    private var observers: [UUID: AsyncStream<AssetChange>.Continuation] = [:]
    /// The last queued mutation of each asset; see ``serializingMutation(of:_:)``.
    private var mutationTails: [String: Task<Void, Never>] = [:]
    /// Purges the server refused for good since this provider started, which
    /// ``redeliverPurges()`` skips; see ``PurgeDelivery/failed(permanent:)``.
    private var refusedPurges: Set<String> = []

    /// - Parameter authGate: the SR1 fresh-local-auth adapter used to open the
    ///   Recently Deleted view. Defaults to the real `LAContext` gate; tests and
    ///   previews inject a scripted one, since no test can answer a Face ID
    ///   sheet.
    /// - Parameter sidecarCoder: the format a caption edit rewrites the sidecar
    ///   in — the one the importer wrote it in. Defaults to the mock lane's
    ///   `JSONSidecarCoder`; a catalog with no caption register refuses the edit
    ///   before the sidecar is touched, so no other lane reaches it.
    /// - Parameter uploads: this device's upload sessions, whose in-flight
    ///   upload of an asset ``delete(_:)`` redacts. With none, a delete redacts
    ///   nothing.
    /// - Parameter purgeDelivery: where a purge's `delete` goes to reach the
    ///   server. With none (signed out, or the mock lane), a purge stays local
    ///   and its records wait on the workspace's kept chain.
    public init(
        library: ManagedLibrary,
        authGate: any LocalAuthGate = LocalAuthenticationGate(),
        sidecarCoder: any SidecarCoding = JSONSidecarCoder(),
        uploads: (any UploadPort)? = nil,
        purgeDelivery: (any PurgeDelivering)? = nil
    ) {
        self.library = library
        self.authGate = authGate
        self.sidecarCoder = sidecarCoder
        self.uploads = uploads
        self.purgeDelivery = purgeDelivery
    }

    public func authorizationStatus() -> AssetAuthorizationStatus {
        .authorized // The managed store needs no system permission.
    }

    @discardableResult
    public func requestAuthorization() -> AssetAuthorizationStatus {
        .authorized
    }

    public func loadTimeline() async throws -> any AssetSnapshot {
        let catalog = try await library.catalog()
        let rows = try await catalog.timeline(offset: 0, limit: Self.timelineLimit)
        return InMemoryAssetSnapshot(rows.map(Asset.init(catalogAsset:)))
    }

    public func asset(for id: AssetID) async throws -> Asset? {
        guard case let .managed(uuid) = id else { return nil }
        let catalog = try await library.catalog()
        return try await catalog.asset(id: uuid).map(Asset.init(catalogAsset:))
    }

    public nonisolated func changes() -> AsyncStream<AssetChange> {
        AsyncStream { continuation in
            let token = UUID()
            Task { await self.register(continuation, token: token) }
            continuation.onTermination = { _ in
                Task { await self.unregister(token) }
            }
        }
    }

    public func setFavorite(_ isFavorite: Bool, for id: AssetID) async throws {
        guard case let .managed(uuid) = id else { return }
        try await serializingMutation(of: uuid) {
            try await self.applyFavorite(isFavorite, uuid: uuid)
        }
        await emitReload()
    }

    /// A read-modify-upsert of one row, so it runs only inside
    /// ``serializingMutation(of:_:)``: an upsert writes back every field it read.
    private func applyFavorite(_ isFavorite: Bool, uuid: String) async throws {
        let catalog = try await library.catalog()
        guard var asset = try await catalog.asset(id: uuid) else { return }
        asset.rating = isFavorite ? 1 : 0
        try await catalog.upsertAsset(asset)
    }

    /// Move each managed asset to the trash with the default retention window.
    /// A retried delete keeps the window the first one opened.
    ///
    /// Once the workspace holds an asset's delete, its in-flight upload is
    /// redacted (``redactUpload(of:uuid:)``), as the deletion state machine
    /// requires.
    public func delete(_ ids: [AssetID]) async throws {
        let retainDays = UInt32(TrashEntry.defaultRetentionDays)
        for id in ids {
            guard case let .managed(uuid) = id else { continue }
            try await library.workspace.deleteAsset(id: uuid, retainDays: retainDays)
            CapsuleLog.assetKit.debug("managed delete of \(uuid, privacy: .private)")
            await redactUpload(of: id, uuid: uuid)
        }
        await emitReload()
    }

    /// Stop this device's upload of an asset the workspace has just deleted.
    ///
    /// A failure is logged and swallowed: the signed delete is already held, so
    /// the delete stands either way, and a session left running finalizes into
    /// a create that the delete supersedes once it syncs.
    private func redactUpload(of id: AssetID, uuid: String) async {
        guard let uploads else { return }
        do {
            let redaction = try await uploads.redactUploads(for: id)
            CapsuleLog.assetKit.debug(
                "upload redaction of \(uuid, privacy: .private): \(redaction.cancelled.count) cancelled, \(redaction.finalizing.count) finalizing"
            )
        } catch {
            CapsuleLog.assetKit.error(
                "upload redaction of \(uuid, privacy: .private) failed, the delete stands: \(String(describing: error), privacy: .public)"
            )
        }
    }

    /// Re-publish the timeline — called once an import has added assets.
    public func refresh() async {
        await emitReload()
    }

    /// Capture coordinates from each import's catalog row, which the importer
    /// fills from the original's EXIF. An import with no GPS fix, or with only
    /// half of one, is omitted.
    public func locations(for ids: [AssetID]) async -> [AssetID: AssetCoordinate] {
        let catalog: any AssetCatalog
        do {
            catalog = try await library.catalog()
        } catch {
            CapsuleLog.assetKit.error("managed locations unavailable: \(String(describing: error), privacy: .public)")
            return [:]
        }
        var coordinates: [AssetID: AssetCoordinate] = [:]
        for id in ids {
            guard case let .managed(uuid) = id else { continue }
            do {
                guard let row = try await catalog.asset(id: uuid),
                      let latitude = row.latitude, let longitude = row.longitude
                else { continue }
                coordinates[id] = AssetCoordinate(latitude: latitude, longitude: longitude)
            } catch {
                CapsuleLog.assetKit.error(
                    "managed location read failed for \(uuid, privacy: .private): \(String(describing: error), privacy: .public)"
                )
            }
        }
        CapsuleLog.assetKit.trace("managed locations: \(coordinates.count) of \(ids.count) located")
        return coordinates
    }

    // MARK: Captions

    /// The caption the catalog holds for an import, or `nil` when it has none
    /// or the asset is not this store's.
    public func caption(for id: AssetID) async -> String? {
        guard case let .managed(uuid) = id else { return nil }
        do {
            return try await library.catalog().asset(id: uuid)?.caption
        } catch {
            CapsuleLog.assetKit.error(
                "managed caption read failed for \(uuid, privacy: .private): \(String(describing: error), privacy: .public)"
            )
            return nil
        }
    }

    /// Set, or with `nil` (or an empty string) clear, an import's caption.
    ///
    /// The catalog register is written first and the sidecar second, so a
    /// catalog that has no caption register refuses before anything is written.
    /// When the sidecar rewrite then fails, the register is put back to what it
    /// held, so the index never claims a caption the sidecar does not carry.
    ///
    /// Edits of one asset run one at a time (``serializingMutation(of:_:)``), so
    /// the restore only ever puts back this edit's own predecessor, never a
    /// caption another edit saved while this one was suspended.
    ///
    /// - Throws: ``CatalogError/invalidArgument(message:)`` for an asset this
    ///   store does not hold; the catalog's refusal; the sidecar rewrite's error.
    public func setCaption(_ caption: String?, for id: AssetID) async throws {
        guard case let .managed(uuid) = id else {
            throw CatalogError.invalidArgument(message: "not a managed asset")
        }
        let value = caption?.isEmpty == false ? caption : nil
        try await serializingMutation(of: uuid) {
            try await self.applyCaption(value, uuid: uuid)
        }
    }

    /// The register-then-sidecar write behind ``setCaption(_:for:)``; runs only
    /// inside ``serializingMutation(of:_:)``.
    private func applyCaption(_ value: String?, uuid: String) async throws {
        let catalog = try await library.catalog()
        guard let row = try await catalog.asset(id: uuid) else {
            throw CatalogError.invalidArgument(message: "no asset: \(uuid)")
        }
        try await catalog.setCaption(value, assetID: uuid)
        let captureDate = Date(timeIntervalSince1970: TimeInterval(row.captureTimestamp))
        let modified = Int64(Date().timeIntervalSince1970)
        do {
            try await library.updateSidecar(uuid: uuid, captureDate: captureDate, coder: sidecarCoder) {
                $0.caption = value
                $0.modifiedTimestamp = modified
            }
        } catch {
            CapsuleLog.assetKit.error(
                "caption sidecar write failed for \(uuid, privacy: .private), restoring the register: \(String(describing: error), privacy: .public)"
            )
            do {
                try await catalog.setCaption(row.caption, assetID: uuid)
            } catch let restoreError {
                CapsuleLog.assetKit.fault(
                    "caption register restore failed for \(uuid, privacy: .private): \(String(describing: restoreError), privacy: .public)"
                )
            }
            throw error
        }
        CapsuleLog.assetKit.debug("caption \(value == nil ? "cleared" : "set") for \(uuid, privacy: .private)")
    }

    // MARK: TrashProvider

    public func trashedAssets() async throws -> [Asset] {
        let catalog = try await library.catalog()
        let rows = try await catalog.trash(offset: 0, limit: Self.timelineLimit)
        return rows.map(Asset.init(catalogAsset:))
    }

    public func unlockTrash() async throws {
        let catalog = try await library.catalog()
        try await catalog.unlockView(.recentlyDeleted, using: authGate)
    }

    public func isTrashUnlocked() async -> Bool {
        guard let catalog = try? await library.catalog() else { return false }
        return await catalog.isViewUnlocked(.recentlyDeleted)
    }

    /// Restore a trashed asset. The workspace refuses it once the retention
    /// window has closed (``TrashRefusal/retentionElapsed(retentionUntil:)``),
    /// and the asset stays in Recently Deleted.
    public func restore(_ id: AssetID) async throws {
        guard case let .managed(uuid) = id else { return }
        try await library.workspace.restoreAsset(id: uuid)
        await emitReload()
    }

    /// Permanently remove a trashed asset through the workspace, which removes
    /// the original, its derivatives and cache entries and its sidecar before
    /// the row, and throws — leaving the asset in Recently Deleted for a retry —
    /// when a file cannot be removed.
    ///
    /// An asset the workspace no longer holds is already gone, which is what a
    /// purge asks for, so that refusal is not an error. An early purge hands
    /// back a signed shorter-retention `delete` for the album's ops surface; the
    /// workspace keeps it on the asset's chain until something delivers it.
    ///
    /// Once purged, the asset's `delete` (with that re-issue) goes to the
    /// server through ``PurgeDelivering`` (``deliverPurge(of:)``). A delivery
    /// that fails or is held changes nothing here: the workspace still holds the
    /// records, and ``redeliverPurges()`` sends them later, also after a
    /// relaunch.
    public func purge(_ id: AssetID) async throws {
        guard case let .managed(uuid) = id else { return }
        do {
            let outcome = try await library.workspace.purgeAsset(id: uuid)
            if let record = outcome.reissueRecord {
                CapsuleLog.assetKit.info(
                    "managed purge of \(uuid, privacy: .private) re-issued a \(record.count)-byte delete, kept on its chain"
                )
            } else {
                CapsuleLog.assetKit.debug("managed purge of \(uuid, privacy: .private)")
            }
        } catch TrashRefusal.notFound {
            CapsuleLog.assetKit.debug("managed purge of \(uuid, privacy: .private): already gone")
            await emitReload()
            return
        }
        await emitReload()
        await deliverPurge(of: uuid)
    }

    /// Send every purge the workspace kept whose delivery failed or was held:
    /// the retry a relaunch, a sign-in or a sync that lists new assets runs. A
    /// purge the server already applied answers as a replay, so sending it
    /// again is safe. A purge the server refused for good since this provider
    /// started is skipped: sending it again could only be refused again. With
    /// no delivery port this does nothing.
    public func redeliverPurges() async {
        guard let purgeDelivery else { return }
        let ids: [String]
        do {
            ids = try await purgeDelivery.purgedAssetIDs()
        } catch {
            CapsuleLog.assetKit.error(
                "purge redelivery could not list the purged assets: \(String(describing: error), privacy: .public)"
            )
            return
        }
        let pending = ids.filter { !refusedPurges.contains($0) }
        CapsuleLog.assetKit.info(
            "redelivering \(pending.count) kept purges, skipping \(ids.count - pending.count) refused for good"
        )
        for uuid in pending {
            await deliverPurge(of: uuid)
        }
    }

    /// Deliver one purged asset's `delete` through the port, logging what
    /// became of it. Never throws: the purge stands locally either way, and the
    /// records stay on the workspace's kept chain for ``redeliverPurges()``.
    private func deliverPurge(of uuid: String) async {
        guard let purgeDelivery else { return }
        do {
            switch try await purgeDelivery.deliverPurge(assetID: uuid) {
            case .delivered:
                CapsuleLog.assetKit.info("purge of \(uuid, privacy: .private) delivered")
            case .held:
                CapsuleLog.assetKit.info(
                    "purge of \(uuid, privacy: .private) held: the server has not indexed the asset"
                )
            case let .failed(permanent):
                if permanent { refusedPurges.insert(uuid) }
                CapsuleLog.assetKit.error(
                    "purge of \(uuid, privacy: .private) not delivered, permanent=\(permanent, privacy: .public)"
                )
            }
        } catch {
            CapsuleLog.assetKit.error(
                "purge of \(uuid, privacy: .private) not delivered: \(String(describing: error), privacy: .public)"
            )
        }
    }

    // MARK: AssetOwning

    /// Whether the managed catalog indexes this asset — trashed rows included,
    /// so a restore or purge of an import routes here.
    public func owns(_ id: AssetID) async -> Bool {
        await (try? asset(for: id)) != nil
    }

    // MARK: Private

    /// Run `mutation` once every earlier mutation of the same asset has finished.
    ///
    /// An actor is reentrant at each `await`, and every catalog and sidecar call
    /// is one, so two edits of one asset would otherwise interleave: a favourite's
    /// read-modify-upsert straddling a caption edit writes the stale row back and
    /// drops the caption, a failed caption's restore overwrites a caption saved
    /// meanwhile, and two caption edits race on one sidecar. Each mutation waits
    /// on the asset's previous tail, so edits apply in call order; a failed one
    /// does not block the next. Edits of different assets still run concurrently.
    private func serializingMutation(
        of uuid: String,
        _ mutation: @escaping @Sendable () async throws -> Void
    ) async throws {
        let previous = mutationTails[uuid]
        let operation = Task {
            await previous?.value
            try await mutation()
        }
        let tail = Task { _ = await operation.result }
        mutationTails[uuid] = tail
        defer {
            if mutationTails[uuid] == tail { mutationTails[uuid] = nil }
        }
        if previous != nil {
            CapsuleLog.assetKit.trace("mutation of \(uuid, privacy: .private) queued behind an earlier one")
        }
        try await operation.value
    }

    private func register(_ continuation: AsyncStream<AssetChange>.Continuation, token: UUID) {
        observers[token] = continuation
    }

    private func unregister(_ token: UUID) {
        observers[token] = nil
    }

    private func emitReload() async {
        guard !observers.isEmpty, let snapshot = try? await loadTimeline() else { return }
        for continuation in observers.values {
            continuation.yield(.reload(snapshot))
        }
    }
}

/// Delivers a purged managed asset's `delete` to its album's ops surface.
///
/// On the FFI lane a conformer reads the asset's head off the sync feed and
/// calls `FfiSession.deliverPurge`, which sends what the server lacks of the
/// chain the workspace kept: an early purge's re-issued `delete` together with
/// any `delete` it chains onto. Everything is read from the workspace, so a
/// delivery that failed is retried by calling it again, also after a relaunch.
public protocol PurgeDelivering: Sendable {
    /// Deliver the purged asset `assetID`'s `delete`. Throws only when the
    /// delivery could not be attempted (a refused session, a bad argument).
    func deliverPurge(assetID: String) async throws -> PurgeDelivery

    /// Every asset the workspace has purged (`FfiWorkspace.purgedAssetIds`):
    /// what ``ManagedProvider/redeliverPurges()`` walks.
    func purgedAssetIDs() async throws -> [String]
}

/// What became of one ``PurgeDelivering/deliverPurge(assetID:)``.
public enum PurgeDelivery: Sendable, Equatable {
    /// The server applied the purge's `delete`, or had already.
    case delivered
    /// Nothing was sent: the server has not indexed the asset's create, and it
    /// never sees a `delete` for an asset it has not indexed.
    case held
    /// The server refused it or could not be reached. A `permanent` failure is
    /// one the server judged final, so ``ManagedProvider/redeliverPurges()``
    /// does not send it again for the rest of the provider's life. The refusal
    /// is not persisted: the first walk after a relaunch sends it once more,
    /// and the server refuses it again without applying anything.
    case failed(permanent: Bool)
}
