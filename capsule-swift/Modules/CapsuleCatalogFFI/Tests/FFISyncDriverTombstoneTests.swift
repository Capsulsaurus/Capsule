import Foundation
import Testing

@testable import CapsuleCatalogFFI

extension FFISyncDriverFixture {
    /// An album page serves a `delete` with no metadata blob, no blob and
    /// `original_held: false`: what the drain does with one, held or not.
    @Suite("FFISyncDriver — an album page's tombstones")
    struct FFISyncDriverTombstoneTests {
        @Test("an unheld asset first seen at its delete on an album page is adopted from the blobs its history names, and its restore opens against the adopted head")
        func albumTombstoneIsAdoptedFromItsRecords() async throws {
            let delete = Data([3])
            let restore = Data([4])
            let history = [Data([1]), Data([2])]
            let verbs = ScriptedVerbs(
                pages: [.success(page(
                    [
                        tombstone(assetA, record: delete),
                        entry(assetA, record: restore, kind: .updated, metadata: true),
                    ],
                    next: Data([1])
                ))],
                openings: [restore: .opened(facts: facts(restore, action: "trash-restore", metadata: metadataFacts, adopted: true))],
                histories: [delete: .success(history)],
                historyOpenings: [delete: .opened(facts: facts(delete, action: "delete", metadata: metadataFacts, adopted: true))],
                sources: [delete: .success(liveSources)]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.applied == 2)
            #expect(drain.awaitingOriginal == 0)
            #expect(verbs.retentionJudgements == [RetentionCall(manifest: delete, changedAt: tombstoneChangedAt)])
            #expect(verbs.historyFetches == [delete])
            #expect(verbs.sourceNamings == [SourcesCall(manifest: delete, history: history)])
            #expect(verbs.fetches == [liveMetadataHash, liveOriginalHash, metadataHash, originalHash])
            #expect(verbs.fetchedSizes == [liveSources.original.size, 64])
            #expect(verbs.historyOpens == [HistoryOpenCall(
                call: ApplyCall(
                    manifest: delete,
                    metadata: Data(liveMetadataHash.utf8),
                    localChainHead: nil,
                    ciphertext: Data(liveOriginalHash.utf8)
                ),
                history: history
            )])
            #expect(verbs.opens.map(\.manifest) == [restore])
            #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(delete))
            #expect(verbs.applies.isEmpty)
            #expect(verbs.heldApplies.isEmpty)
            #expect(catalog.writes.map(\.action) == ["delete", "trash-restore"])
            #expect(catalog.writes.first?.original == plaintextBytes)
            #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(restore))
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("an album page's tombstone whose history names no live metadata blob is quarantined, fetching no blob")
        func unboundTombstoneIsRefused() async throws {
            let delete = Data([3])
            let history = [Data([1]), Data([2])]
            let verbs = ScriptedVerbs(
                pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
                histories: [delete: .success(history)],
                sources: [delete: .failure(FfiError.Sync(code: nil, detail: "no record of the chain binds a metadata blob"))]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.quarantined == 1)
            #expect(drain.applied == 0)
            #expect(verbs.sourceNamings == [SourcesCall(manifest: delete, history: history)])
            #expect(verbs.fetches.isEmpty)
            #expect(verbs.historyOpens.isEmpty)
            #expect(verbs.opens.isEmpty)
            #expect(verbs.applies.isEmpty)
            #expect(catalog.writes.isEmpty)
            #expect(store.state?.cursor == Data([1]))
            #expect(store.state?.heads.isEmpty == true)
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("an album page's tombstone whose records do not decode is quarantined before its sources are named")
        func unreadableTombstoneHistoryQuarantines() async throws {
            let delete = Data([3])
            let verbs = ScriptedVerbs(
                pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
                histories: [delete: .failure(FfiError.Sync(code: nil, detail: "a record does not decode"))]
            )

            let drain = try await driver(verbs, sink: DrainSink()).sync()

            #expect(drain.quarantined == 1)
            #expect(verbs.sourceNamings.isEmpty)
            #expect(verbs.fetches.isEmpty)
        }

        @Test("an unheld asset's tombstone whose window had closed at its changed_at is passed and its page kept, fetching nothing")
        func closedWindowTombstoneIsPassed() async throws {
            let delete = Data([3])
            let verbs = ScriptedVerbs(
                pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
                closedWindows: [delete]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.skipped == 1)
            #expect(drain.applied == 0)
            #expect(drain.quarantined == 0)
            #expect(drain.newlyIndexed == [assetA])
            #expect(verbs.retentionJudgements == [RetentionCall(manifest: delete, changedAt: tombstoneChangedAt)])
            #expect(verbs.historyFetches.isEmpty)
            #expect(verbs.sourceNamings.isEmpty)
            #expect(verbs.fetches.isEmpty)
            #expect(catalog.writes.isEmpty)
            #expect(store.state?.cursor == Data([1]))
            #expect(store.state?.heads.isEmpty == true)
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("an unheld asset's tombstone whose records the server refuses for good is passed and its page kept, and the next sync moves past it")
        func goneHistoryTombstoneIsPassed() async throws {
            let delete = Data([3])
            let verbs = ScriptedVerbs(
                pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
                histories: [delete: .failure(FfiError.Fetch(
                    code: FFISyncDriver.blobGoneCode,
                    detail: "provenance record: blob permanently unavailable"
                ))]
            )
            let store = MemoryStore()
            let staging = stagingDirectory()
            let subject = driver(verbs, store: store, staging: staging, sink: DrainSink())

            let drain = try await subject.sync()

            #expect(drain.skipped == 1)
            #expect(drain.quarantined == 0)
            #expect(verbs.historyFetches == [delete])
            #expect(verbs.sourceNamings.isEmpty)
            #expect(verbs.fetches.isEmpty)
            #expect(store.state?.cursor == Data([1]))
            #expect(store.state?.heads.isEmpty == true)
            #expect(try leftovers(in: staging).isEmpty)

            try await subject.sync()

            #expect(verbs.cursors == [Data(), Data([1])])
        }

        @Test("an unheld asset's tombstone whose live metadata blob the server refuses for good is passed, staging nothing")
        func goneBlobTombstoneIsPassed() async throws {
            let delete = Data([3])
            let verbs = ScriptedVerbs(
                pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
                histories: [delete: .success([Data([1])])],
                metadataFailure: FfiError.Fetch(code: FFISyncDriver.blobGoneCode, detail: "blob permanently unavailable")
            )
            let store = MemoryStore()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, staging: staging, sink: DrainSink()).sync()

            #expect(drain.skipped == 1)
            #expect(verbs.fetches == [metadataHash])
            #expect(verbs.historyOpens.isEmpty)
            #expect(store.state?.cursor == Data([1]))
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("an unheld asset's delete in the owner's own feed is passed the same way: a closed window fetches nothing, and records the server refuses for good pass the entry")
        func unservedOwnFeedDeleteIsPassed() async throws {
            let closed = Data([3])
            let gone = Data([5])
            let verbs = ScriptedVerbs(
                pages: [.success(page(
                    [
                        entry(assetA, record: closed, kind: .deleted, metadata: true),
                        entry(assetB, record: gone, kind: .deleted, metadata: true),
                    ],
                    next: Data([1])
                ))],
                histories: [gone: .failure(FfiError.Fetch(code: FFISyncDriver.blobGoneCode, detail: "blob permanently unavailable"))],
                closedWindows: [closed]
            )
            let store = MemoryStore()

            let drain = try await driver(verbs, store: store, sink: DrainSink()).sync()

            #expect(drain.skipped == 2)
            #expect(drain.applied == 0)
            #expect(verbs.historyFetches == [gone])
            #expect(verbs.fetches.isEmpty)
            #expect(verbs.opens.isEmpty)
            #expect(verbs.historyOpens.isEmpty)
            #expect(store.state?.cursor == Data([1]))
        }

        @Test(
            "a tombstone's history fetch that fails for any other reason stops the drain: its page is not kept and the cursor stays where it was",
            arguments: [
                FfiError.Fetch(code: nil, detail: "provenance record: transient fetch failure"),
                FfiError.Fetch(code: "error.blob.access_revoked", detail: "provenance record: blob authorization changed"),
                FfiError.Auth(code: nil, detail: "the server refused the refreshed credential"),
            ]
        )
        func failedTombstoneHistoryFetchStops(error: FfiError) async throws {
            let delete = Data([3])
            let kept = FFISyncState(cursor: Data([7]), heads: [:])
            let verbs = ScriptedVerbs(
                pages: [.success(page([tombstone(assetA, record: delete)], next: Data([8])))],
                histories: [delete: .failure(error)]
            )
            let store = MemoryStore(kept)
            let catalog = RecordingCatalog()
            let staging = stagingDirectory()

            await #expect(throws: error) {
                try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()
            }

            #expect(store.saves == 0)
            #expect(store.state?.cursor == Data([7]))
            #expect(verbs.historyFetches == [delete])
            #expect(verbs.sourceNamings.isEmpty)
            #expect(verbs.fetches.isEmpty)
            #expect(catalog.writes.isEmpty)
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("a held asset's album-page tombstone applies its delete against the held chain, fetching nothing, and keeps its head")
        func heldTombstoneAppliesTheDelete() async throws {
            let delete = Data([2])
            let verbs = ScriptedVerbs(
                pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
                chainHeads: [assetA: Data([1])]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.applied == 1)
            #expect(drain.awaitingOriginal == 0)
            #expect(verbs.heldApplies == [HeldApplyCall(
                manifest: delete,
                metadata: Data(),
                localChainHead: FFIPurgeDelivery.headHash(Data([1]))
            )])
            #expect(verbs.retentionJudgements.isEmpty)
            #expect(verbs.historyFetches.isEmpty)
            #expect(verbs.sourceNamings.isEmpty)
            #expect(verbs.fetches.isEmpty)
            #expect(verbs.applies.isEmpty)
            #expect(verbs.opens.isEmpty)
            #expect(catalog.writes == [CatalogCall(action: "delete", hasMetadata: false, original: nil, originalPath: nil)])
            #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(delete))
            #expect(store.state?.cursor == Data([1]))
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("a held asset's album-page tombstone the chokepoint refuses is quarantined, and keeps no head")
        func refusedHeldTombstoneQuarantines() async throws {
            let delete = Data([2])
            let verbs = ScriptedVerbs(
                pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
                verdicts: [delete: .quarantined(reason: "Rejected.ForgedChain", detail: nil)],
                chainHeads: [assetA: Data([1])]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore()

            let drain = try await driver(verbs, store: store, catalog: catalog, sink: DrainSink()).sync()

            #expect(drain.quarantined == 1)
            #expect(drain.applied == 0)
            #expect(verbs.heldApplies.map(\.manifest) == [delete])
            #expect(verbs.fetches.isEmpty)
            #expect(catalog.writes.isEmpty)
            #expect(store.state?.heads.isEmpty == true)
            #expect(store.state?.cursor == Data([1]))
        }
    }
}
