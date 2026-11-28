import CapsuleCatalog
import CapsuleFoundation
import Foundation
import ManagedStore

// MARK: Purge and lifecycle delivery

/// How ``ManagedProvider`` gets a purge's `delete`, and a delete's or
/// restore's signed record, to the server: once when the verb runs, and again
/// on every redelivery walk until the server holds it or refuses it for good.
public extension ManagedProvider {
    /// Permanently remove a trashed asset through the workspace, which removes
    /// the original, its derivatives and cache entries and its sidecar before
    /// the row, and throws — leaving the asset in Recently Deleted for a retry —
    /// when a file cannot be removed.
    ///
    /// An asset the workspace no longer holds and the catalog has no row for is
    /// already gone, which is what a purge asks for, so that refusal is not an
    /// error. A row the catalog still holds without the workspace holding the
    /// asset (one written by sync for another device's asset) was not purged,
    /// so that refusal is thrown and the row stays in Recently Deleted.
    ///
    /// An early purge hands back a signed shorter-retention `delete` for the
    /// album's ops surface; the workspace keeps it on the asset's chain until
    /// something delivers it.
    ///
    /// Once purged, the asset's `delete` (with that re-issue) goes to the
    /// server through ``PurgeDelivering`` (``deliverPurge(of:)``). A delivery
    /// that fails or is held changes nothing here: the workspace still holds the
    /// records, and ``redeliverPurges()`` sends them later, also after a
    /// relaunch.
    func purge(_ id: AssetID) async throws {
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
            if try await library.catalog().asset(id: uuid) != nil {
                // A row the workspace does not hold: one the sync driver wrote
                // for another device's asset. Nothing was purged, so the
                // refusal is the caller's, and the row stays in the trash.
                CapsuleLog.assetKit.error(
                    "managed purge of \(uuid, privacy: .private) refused: the catalog row is not held by the workspace"
                )
                throw TrashRefusal.notFound
            }
            CapsuleLog.assetKit.debug("managed purge of \(uuid, privacy: .private): already gone")
            await emitReload()
            return
        }
        await emitReload()
        await deliverPurge(of: uuid)
    }

    /// Send every purge the workspace kept whose delivery failed or was held:
    /// the retry a relaunch, a sign-in or a sync that lists new assets runs. A
    /// purge whose head the server already holds sends nothing (the port
    /// answers it delivered off the feed), so the walk's network cost is the
    /// port's feed read plus the purges still undelivered. A purge the server refused for good since this provider
    /// started is skipped: sending it again could only be refused again. With
    /// no delivery port this does nothing.
    func redeliverPurges() async {
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

    /// Send the held lifecycle records of every asset the workspace still
    /// holds: the deletes and restores whose delivery failed or was held, the
    /// retry a relaunch, a sign-in or a sync that lists new assets runs beside
    /// ``redeliverPurges()``. An asset whose head the server already holds
    /// sends nothing (the port answers it delivered off the feed), so the
    /// walk's network cost is the port's feed read plus the records still
    /// undelivered. An asset the server refused for good, or whose too-late
    /// restore was withheld, since this provider started is skipped: neither
    /// can go any other way. With no delivery port this does nothing.
    func redeliverLifecycleRecords() async {
        guard let lifecycleDelivery else { return }
        let ids: [String]
        do {
            ids = try await lifecycleDelivery.heldAssetIDs()
        } catch {
            CapsuleLog.assetKit.error(
                "lifecycle redelivery could not list the held assets: \(String(describing: error), privacy: .public)"
            )
            return
        }
        let pending = ids.filter { !settledLifecycles.contains($0) }
        CapsuleLog.assetKit.info(
            "redelivering the held records of \(pending.count) assets, skipping \(ids.count - pending.count) settled for good"
        )
        for uuid in pending {
            await deliverLifecycle(of: uuid)
        }
    }

    /// Deliver one held asset's lifecycle records through the port, logging
    /// what became of them. Never throws: the delete or restore stands
    /// locally either way, and its record stays on the asset's chain for
    /// ``redeliverLifecycleRecords()``.
    internal func deliverLifecycle(of uuid: String) async {
        guard let lifecycleDelivery else { return }
        do {
            switch try await lifecycleDelivery.deliverLifecycle(assetID: uuid) {
            case .delivered:
                CapsuleLog.assetKit.info("lifecycle records of \(uuid, privacy: .private) delivered")
            case .held:
                CapsuleLog.assetKit.info(
                    "lifecycle records of \(uuid, privacy: .private) held: the server has not indexed the asset"
                )
            case let .withheld(retentionUntil):
                settledLifecycles.insert(uuid)
                CapsuleLog.assetKit.info(
                    "lifecycle records of \(uuid, privacy: .private) withheld: the restore came after the held delete's window closed at \(retentionUntil, privacy: .public)"
                )
            case let .failed(permanent):
                if permanent { settledLifecycles.insert(uuid) }
                CapsuleLog.assetKit.error(
                    "lifecycle records of \(uuid, privacy: .private) not delivered, permanent=\(permanent, privacy: .public)"
                )
            }
        } catch {
            CapsuleLog.assetKit.error(
                "lifecycle records of \(uuid, privacy: .private) not delivered: \(String(describing: error), privacy: .public)"
            )
        }
    }
}

// MARK: Redelivery walk

public extension ManagedProvider {
    /// Run ``redeliverPurges()`` and then ``redeliverLifecycleRecords()``: the
    /// walk a relaunch, a sign-in and ``syncDrained(newlyIndexed:)`` run. The
    /// purges go first, then the held assets, one after the other so both
    /// read one feed drain.
    ///
    /// Walks run one at a time, in call order. A walk asked for while one is
    /// running starts once that one ends, so a sync that lands mid-walk is
    /// never lost to a walk that listed the held records before it, and two
    /// walks never send one asset's records side by side. With no delivery
    /// port this does nothing.
    func redeliverHeldRecords() async {
        guard purgeDelivery != nil || lifecycleDelivery != nil else { return }
        let previous = walkTail
        let walk = Task {
            await previous?.value
            await self.redeliverPurges()
            await self.redeliverLifecycleRecords()
        }
        walkTail = walk
        if previous != nil {
            CapsuleLog.assetKit.debug("redelivery walk queued behind the one in flight")
        }
        await walk.value
        if walkTail == walk { walkTail = nil }
    }

    /// Run the redelivery walk after a sync that drained the feed, when the
    /// drain listed assets the server had not indexed before it.
    ///
    /// A purge, delete or restore is held while the feed does not list its
    /// asset, so a drain that lists newly indexed assets is what can release
    /// one before the next launch. The walk is ``redeliverHeldRecords()``,
    /// the one a launch runs: an asset whose head the feed already lists sends
    /// nothing. A drain that lists nothing new releases nothing, so it walks
    /// nothing.
    ///
    /// - Parameter assetIDs: the assets the drain listed that the feed had not
    ///   listed before it, as the sync driver applied them.
    func syncDrained(newlyIndexed assetIDs: [String]) async {
        guard !assetIDs.isEmpty else {
            CapsuleLog.assetKit.debug("sync drain listed no newly indexed assets: no redelivery")
            return
        }
        let count = assetIDs.count
        CapsuleLog.assetKit.info(
            "sync drain listed \(count, privacy: .public) newly indexed assets: redelivering held records"
        )
        await redeliverHeldRecords()
    }
}
