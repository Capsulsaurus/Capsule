@testable import CapsuleCatalogFFI
import Foundation
import Testing

extension FFISyncDriverFixture {
    @Suite("FFISyncDriver — an unheld asset adopted from its history, or refused by it")
    struct FFISyncDriverHistoryTests {
        @Test("an unheld asset's create is opened on its own: its history is empty")
        func unheldCreateOpensWithoutHistory() async throws {
            let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))])

            try await driver(verbs, sink: DrainSink()).sync()

            #expect(verbs.historyFetches == [Data([1])])
            #expect(verbs.historyOpens.isEmpty)
            #expect(verbs.opens.map(\.manifest) == [Data([1])])
        }

        @Test("an unheld asset first seen past its create is opened against its fetched history and adopted")
        func adoptsFromHistory() async throws {
            let record = Data([3])
            let history = [Data([1]), Data([2])]
            let kept = FFISyncState(cursor: Data([7]), heads: [assetA: FFIPurgeDelivery.headHash(Data([2]))])
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([8])))],
                histories: [record: .success(history)]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore(kept)
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.applied == 1)
            #expect(verbs.historyFetches == [record])
            #expect(verbs.opens.isEmpty)
            #expect(verbs.applies.isEmpty)
            #expect(verbs.historyOpens == [
                HistoryOpenCall(
                    call: ApplyCall(
                        manifest: record,
                        metadata: Data(metadataHash.utf8),
                        localChainHead: FFIPurgeDelivery.headHash(Data([2])),
                        ciphertext: Data(originalHash.utf8)
                    ),
                    history: history
                ),
            ])
            #expect(verbs.fetches == [metadataHash, originalHash])
            #expect(catalog.writes.map(\.action) == ["metadata-update"])
            #expect(catalog.writes.first?.original == plaintextBytes)
            #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(record))
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("an asset adopted from its history is held for the rest of the drain: its next entry reads no history")
        func adoptedAssetIsHeldAfter() async throws {
            let verbs = ScriptedVerbs(
                pages: [
                    .success(page(
                        [
                            entry(assetA, record: Data([3]), kind: .updated, metadata: true),
                            entry(assetA, record: Data([4]), kind: .updated, metadata: true),
                        ],
                        next: Data([1])
                    )),
                ],
                histories: [Data([3]): .success([Data([1]), Data([2])])]
            )

            let drain = try await driver(verbs, sink: DrainSink()).sync()

            #expect(drain.applied == 2)
            #expect(verbs.historyFetches == [Data([3])])
            #expect(verbs.historyOpens.map(\.call.manifest) == [Data([3])])
            #expect(verbs.opens.map(\.manifest) == [Data([4])])
            #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(Data([3])))
        }

        @Test("an asset its create adopted is held for the rest of the drain: its next entry reads no history")
        func adoptedCreateIsHeldAfter() async throws {
            let verbs = ScriptedVerbs(
                pages: [
                    .success(page(
                        [
                            entry(assetA, record: Data([1]), metadata: true),
                            entry(assetA, record: Data([2]), kind: .updated, metadata: true),
                        ],
                        next: Data([1])
                    )),
                ],
                openings: [Data([1]): .opened(facts: facts(Data([1]), metadata: metadataFacts, adopted: true))]
            )

            let drain = try await driver(verbs, sink: DrainSink()).sync()

            #expect(drain.applied == 2)
            #expect(verbs.historyFetches == [Data([1])])
            #expect(verbs.historyOpens.isEmpty)
            #expect(verbs.opens.map(\.manifest) == [Data([1]), Data([2])])
            #expect(verbs.opens.last?.localChainHead == FFIPurgeDelivery.headHash(Data([1])))
        }

        @Test("a held asset's entry is opened against its own chain, reading no history")
        func heldAssetReadsNoHistory() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: Data([2]), kind: .updated, metadata: true)], next: Data([1])))],
                chainHeads: [assetA: Data([1])]
            )

            try await driver(verbs, sink: DrainSink()).sync()

            #expect(verbs.historyFetches.isEmpty)
            #expect(verbs.historyOpens.isEmpty)
            #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(Data([1])))
        }

        @Test("an unheld asset first seen at its delete in trash is adopted from its history with its live metadata blob, and its restore opens against the adopted head")
        func trashedDeleteIsAdoptedAndRestored() async throws {
            let delete = Data([3])
            let restore = Data([4])
            let history = [Data([1]), Data([2])]
            let verbs = ScriptedVerbs(
                pages: [
                    .success(page(
                        [
                            entry(assetA, record: delete, kind: .deleted, metadata: true),
                            entry(assetA, record: restore, kind: .updated, metadata: true),
                        ],
                        next: Data([1])
                    )),
                ],
                openings: [restore: .opened(facts: facts(restore, action: "trash-restore", metadata: metadataFacts, adopted: true))],
                histories: [delete: .success(history)],
                historyOpenings: [delete: .opened(facts: facts(delete, action: "delete", metadata: metadataFacts, adopted: true))]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.applied == 2)
            #expect(drain.skipped == 0)
            #expect(verbs.fetches == [metadataHash, originalHash, metadataHash, originalHash])
            #expect(verbs.historyFetches == [delete])
            #expect(verbs.historyOpens == [
                HistoryOpenCall(
                    call: ApplyCall(
                        manifest: delete,
                        metadata: Data(metadataHash.utf8),
                        localChainHead: nil,
                        ciphertext: Data(originalHash.utf8)
                    ),
                    history: history
                ),
            ])
            #expect(verbs.opens.map(\.manifest) == [restore])
            #expect(verbs.opens.first?.metadata == Data(metadataHash.utf8))
            #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(delete))
            #expect(verbs.applies.isEmpty)
            #expect(catalog.writes.map(\.action) == ["delete", "trash-restore"])
            #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(restore))
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("a history the chokepoint refuses is passed as its verdict and writes nothing")
        func refusedHistoryWritesNothing() async throws {
            let record = Data([3])
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([1])))],
                histories: [record: .success([Data([1]), Data([2])])],
                historyOpenings: [record: .refused(outcome: .quarantined(reason: "Rejected.ForgedChain", detail: nil))]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.quarantined == 1)
            #expect(drain.applied == 0)
            #expect(catalog.writes.isEmpty)
            #expect(store.state?.cursor == Data([1]))
            #expect(store.state?.heads.isEmpty == true)
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("a history whose records do not decode quarantines its entry before any blob is fetched")
        func unreadableHistoryQuarantines() async throws {
            let record = Data([3])
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([1])))],
                histories: [record: .failure(FfiError.Sync(code: nil, detail: "a record does not decode"))]
            )
            let store = MemoryStore()
            let catalog = RecordingCatalog()

            let drain = try await driver(verbs, store: store, catalog: catalog, sink: DrainSink()).sync()

            #expect(drain.quarantined == 1)
            #expect(verbs.fetches.isEmpty)
            #expect(verbs.historyOpens.isEmpty)
            #expect(verbs.opens.isEmpty)
            #expect(catalog.writes.isEmpty)
            #expect(store.state?.cursor == Data([1]))
        }

        @Test("a record that cannot be fetched stops the drain before its page is kept, and the next drain reads it again")
        func failedRecordFetchKeepsNothing() async throws {
            let record = Data([3])
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([1])))],
                histories: [record: .failure(FfiError.Fetch(code: nil, detail: "provenance record: unreachable"))]
            )
            let store = MemoryStore()
            let staging = stagingDirectory()
            let subject = driver(verbs, store: store, staging: staging, sink: DrainSink())

            await #expect(throws: FfiError.self) { try await subject.sync() }

            #expect(store.saves == 0)
            #expect(verbs.fetches.isEmpty)
            #expect(try leftovers(in: staging).isEmpty)

            verbs.enqueue([.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([1])))])
            await #expect(throws: FfiError.self) { try await subject.sync() }

            #expect(verbs.cursors == [Data(), Data()])
            #expect(verbs.historyFetches == [record, record])
        }
    }
}
