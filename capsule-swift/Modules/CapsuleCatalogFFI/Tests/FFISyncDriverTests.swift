import Foundation
import Testing

@testable import CapsuleCatalogFFI

extension FFISyncDriverFixture {
    @Suite("FFISyncDriver — the managed lane's sync over a persisted cursor")
    struct FFISyncDriverTests {
        @Test("a drain across pages hands over exactly the assets the feed newly listed, and keeps the last cursor")
        func handsOverNewlyListedAssets() async throws {
            let verbs = ScriptedVerbs(pages: [
                .success(page([entry(assetA, record: Data([1]))], next: Data([1]), more: true)),
                .success(page([entry(assetB, record: Data([2])), entry(assetA, record: Data([3]), kind: .updated)], next: Data([2]))),
            ])
            let store = MemoryStore()
            let sink = DrainSink()

            let drain = try await driver(verbs, store: store, sink: sink).sync()

            #expect(drain.newlyIndexed == [assetA, assetB])
            #expect(drain.pages == 2)
            #expect(drain.applied == 3)
            #expect(verbs.cursors == [Data(), Data([1])])
            #expect(store.state?.cursor == Data([2]))
            #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(Data([3])))
            #expect(await sink.drains == [drain])
        }

        @Test("an applied entry's head is the local chain head its asset's next entry is applied with")
        func chainsTheAppliedHead() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: Data([1])), entry(assetA, record: Data([2]), kind: .updated)], next: Data([1])))],
                chainHeads: [assetA: Data([0])]
            )

            try await driver(verbs, sink: DrainSink()).sync()

            #expect(verbs.applies.map(\.localChainHead) == [FFIPurgeDelivery.headHash(Data([0])), FFIPurgeDelivery.headHash(Data([1]))])
            #expect(verbs.applies.allSatisfy { $0.ciphertext == Data(originalHash.utf8) })
        }

        @Test("a later drain that lists nothing new hands over an empty list")
        func emptyWhenNothingIsNew() async throws {
            let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]))], next: Data([1])))])
            let sink = DrainSink()
            let subject = driver(verbs, sink: sink)
            try await subject.sync()
            verbs.enqueue([.success(page([entry(assetA, record: Data([2]), kind: .updated)], next: Data([2])))])

            let second = try await subject.sync()

            #expect(second.newlyIndexed.isEmpty)
            #expect(second.applied == 1)
            #expect(verbs.cursors == [Data(), Data([1])])
            #expect(await sink.drains.map(\.newlyIndexed) == [[assetA], []])
        }

        @Test("an asset with a kept head is not newly listed after a relaunch")
        func keptHeadsAreNotNew() async throws {
            let kept = FFISyncState(cursor: Data([7]), heads: [assetA: FFIPurgeDelivery.headHash(Data([1]))])
            let verbs = ScriptedVerbs(pages: [
                .success(page([entry(assetA, record: Data([2]), kind: .updated), entry(assetB, record: Data([3]))], next: Data([8]))),
            ])

            let drain = try await driver(verbs, store: MemoryStore(kept), sink: DrainSink()).sync()

            #expect(verbs.cursors == [Data([7])])
            #expect(drain.newlyIndexed == [assetB])
            #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(Data([1])))
        }

        @Test("a failed page keeps the cursor and heads from before it, and hands over what the kept pages listed")
        func failedPageKeepsTheCursor() async throws {
            let verbs = ScriptedVerbs(pages: [
                .success(page([entry(assetA, record: Data([1]))], next: Data([1]), more: true)),
                .failure(FfiError.Sync(code: nil, detail: "unreachable")),
            ])
            let store = MemoryStore()
            let sink = DrainSink()
            let subject = driver(verbs, store: store, sink: sink)

            await #expect(throws: FfiError.self) { try await subject.sync() }

            #expect(store.state?.cursor == Data([1]))
            #expect(await sink.drains.map(\.newlyIndexed) == [[assetA]])

            verbs.enqueue([.success(page([entry(assetB, record: Data([2]))], next: Data([2])))])
            let retry = try await subject.sync()

            #expect(verbs.cursors.last == Data([1]))
            #expect(retry.newlyIndexed == [assetB])
        }

        @Test("a page whose apply fails partway is not kept, and lists nothing as indexed")
        func failedApplyKeepsNothingOfItsPage() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: Data([1])), entry(assetB, record: Data([2]))], next: Data([1])))],
                openings: [Data([2]): .refused(outcome: .pending(reason: "AmkNotYetLocal"))]
            )
            let store = MemoryStore()
            let sink = DrainSink()

            await #expect(throws: FFISyncError.pending(assetID: assetB, reason: "AmkNotYetLocal")) {
                try await driver(verbs, store: store, sink: sink).sync()
            }

            #expect(store.state == nil)
            #expect(store.saves == 0)
            #expect(await sink.drains == [FFISyncDrain()])
        }

        @Test("an entry level with the workspace's own chain head is neither fetched nor applied, but is newly listed")
        func ownHeadIsLevel() async throws {
            let ownRecord = Data([9])
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: ownRecord)], next: Data([1])))],
                chainHeads: [assetA: ownRecord]
            )

            let drain = try await driver(verbs, sink: DrainSink()).sync()

            #expect(drain.level == 1)
            #expect(drain.newlyIndexed == [assetA])
            #expect(verbs.fetches.isEmpty)
            #expect(verbs.applies.isEmpty)
        }

        @Test("a quarantined entry is passed and its page kept; an entry awaiting its original is not applied")
        func quarantineAndAwaitingPass() async throws {
            let entries = [entry(assetA, record: Data([1]), metadata: true), entry(assetB, record: Data([2]), metadata: true, held: false)]
            let verbs = ScriptedVerbs(
                pages: [.success(page(entries, next: Data([1])))],
                openings: [Data([1]): .refused(outcome: .quarantined(reason: "Rejected.ForgedChain", detail: nil))]
            )
            let store = MemoryStore()
            let catalog = RecordingCatalog()

            let drain = try await driver(verbs, store: store, catalog: catalog, sink: DrainSink()).sync()

            #expect(catalog.writes.isEmpty)
            #expect(drain.quarantined == 1)
            #expect(drain.awaitingOriginal == 1)
            #expect(drain.newlyIndexed == [assetA, assetB])
            #expect(verbs.opens.count == 1)
            #expect(store.state?.cursor == Data([1]))
            #expect(store.state?.heads.isEmpty == true)
        }

        @Test("an entry naming a metadata blob is opened with the blob's fetched bytes")
        func fetchesTheMetadataBlob() async throws {
            let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))])

            try await driver(verbs, sink: DrainSink()).sync()

            #expect(verbs.fetches == [metadataHash, originalHash])
            #expect(verbs.opens.first?.metadata == Data(metadataHash.utf8))
            #expect(verbs.opens.first?.ciphertext == Data(originalHash.utf8))
            #expect(verbs.applies.isEmpty)
        }

        @Test("an applied entry with metadata hands the catalog its facts and the verified plaintext, removed after")
        func writesAnOpenedEntry() async throws {
            let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))])
            let catalog = RecordingCatalog()
            let store = MemoryStore()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.applied == 1)
            #expect(catalog.writes.map(\.action) == ["create"])
            #expect(catalog.writes.first?.hasMetadata == true)
            #expect(catalog.writes.first?.original == plaintextBytes)
            #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(Data([1])))
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("a held asset's applied delete is only verified, and the catalog is handed its facts with no original")
        func writesAVerifiedLifecycleEntry() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: Data([1]), kind: .deleted)], next: Data([1])))],
                verdicts: [Data([1]): .applied(facts: facts(Data([1]), action: "delete"))],
                chainHeads: [assetA: Data([0])]
            )
            let catalog = RecordingCatalog()

            let drain = try await driver(verbs, catalog: catalog, sink: DrainSink()).sync()

            #expect(drain.applied == 1)
            #expect(verbs.opens.isEmpty)
            #expect(verbs.applies.count == 1)
            #expect(catalog.writes == [CatalogCall(action: "delete", hasMetadata: false, original: nil, originalPath: nil)])
        }

        @Test("an opened entry the chokepoint refuses is passed as its verdict and writes nothing")
        func refusedOpenWritesNothing() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))],
                openings: [Data([1]): .refused(outcome: .quarantined(reason: "Rejected.ForgedChain", detail: nil))]
            )
            let catalog = RecordingCatalog()
            let staging = stagingDirectory()

            let drain = try await driver(verbs, catalog: catalog, staging: staging, sink: DrainSink()).sync()

            #expect(drain.quarantined == 1)
            #expect(drain.applied == 0)
            #expect(catalog.writes.isEmpty)
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test(
            "an applied entry whose original does not open is quarantined and writes nothing",
            arguments: [
                FfiOpenedOriginal.undecryptable(facts: facts(Data([1]), metadata: metadataFacts)),
                FfiOpenedOriginal.digestMismatch(facts: facts(Data([1]), metadata: metadataFacts)),
            ]
        )
        func unopenedOriginalWritesNothing(opened: FfiOpenedOriginal) async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))],
                openings: [Data([1]): opened]
            )
            let catalog = RecordingCatalog()
            let store = MemoryStore()

            let drain = try await driver(verbs, store: store, catalog: catalog, sink: DrainSink()).sync()

            #expect(drain.quarantined == 1)
            #expect(catalog.writes.isEmpty)
            #expect(store.state?.heads.isEmpty == true)
            #expect(store.state?.cursor == Data([1]))
        }

        @Test("a catalog write that fails keeps nothing of its page, and removes the staged plaintext")
        func failedWriteKeepsNothingOfItsPage() async throws {
            let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))])
            let catalog = RecordingCatalog(failing: [assetA])
            let store = MemoryStore()
            let staging = stagingDirectory()

            await #expect(throws: FfiError.self) {
                try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()
            }

            #expect(catalog.writes.count == 1)
            #expect(store.saves == 0)
            #expect(try leftovers(in: staging).isEmpty)
        }

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
            #expect(verbs.historyOpens == [HistoryOpenCall(
                call: ApplyCall(
                    manifest: record,
                    metadata: Data(metadataHash.utf8),
                    localChainHead: FFIPurgeDelivery.headHash(Data([2])),
                    ciphertext: Data(originalHash.utf8)
                ),
                history: history
            )])
            #expect(verbs.fetches == [metadataHash, originalHash])
            #expect(catalog.writes.map(\.action) == ["metadata-update"])
            #expect(catalog.writes.first?.original == plaintextBytes)
            #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(record))
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("an asset adopted from its history is held for the rest of the drain: its next entry reads no history")
        func adoptedAssetIsHeldAfter() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page(
                    [
                        entry(assetA, record: Data([3]), kind: .updated, metadata: true),
                        entry(assetA, record: Data([4]), kind: .updated, metadata: true),
                    ],
                    next: Data([1])
                ))],
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
                pages: [.success(page(
                    [
                        entry(assetA, record: Data([1]), metadata: true),
                        entry(assetA, record: Data([2]), kind: .updated, metadata: true),
                    ],
                    next: Data([1])
                ))],
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
                pages: [.success(page(
                    [
                        entry(assetA, record: delete, kind: .deleted, metadata: true),
                        entry(assetA, record: restore, kind: .updated, metadata: true),
                    ],
                    next: Data([1])
                ))],
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
            #expect(verbs.historyOpens == [HistoryOpenCall(
                call: ApplyCall(
                    manifest: delete,
                    metadata: Data(metadataHash.utf8),
                    localChainHead: nil,
                    ciphertext: Data(originalHash.utf8)
                ),
                history: history
            )])
            #expect(verbs.opens.map(\.manifest) == [restore])
            #expect(verbs.opens.first?.metadata == Data(metadataHash.utf8))
            #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(delete))
            #expect(verbs.applies.isEmpty)
            #expect(catalog.writes.map(\.action) == ["delete", "trash-restore"])
            #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(restore))
            #expect(try leftovers(in: staging).isEmpty)
        }

        @Test("an entry whose metadata blob address is not a content address is skipped, fetching nothing")
        func malformedMetadataAddressIsSkipped() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page(
                    [entry(assetA, record: Data([1]), metadata: true, metadataAddress: Data("../../etc/passwd".utf8))],
                    next: Data([1])
                ))]
            )
            let store = MemoryStore()

            let drain = try await driver(verbs, store: store, sink: DrainSink()).sync()

            #expect(drain.skipped == 1)
            #expect(drain.applied == 0)
            #expect(verbs.fetches.isEmpty)
            #expect(verbs.historyFetches.isEmpty)
            #expect(verbs.opens.isEmpty)
            #expect(store.state?.cursor == Data([1]))
        }

        @Test("a metadata blob that cannot be fetched stops the drain before its page is kept, staging nothing")
        func failedMetadataFetchKeepsNothing() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))],
                metadataFailure: FfiError.Fetch(code: nil, detail: "blob exceeds the limit for a blob of unlisted size")
            )
            let store = MemoryStore()
            let staging = stagingDirectory()

            await #expect(throws: FfiError.self) {
                try await driver(verbs, store: store, staging: staging, sink: DrainSink()).sync()
            }

            #expect(store.saves == 0)
            #expect(verbs.fetches == [metadataHash])
            #expect(verbs.opens.isEmpty)
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

        @Test("a stale cursor restarts the drain from the feed's start once, keeping the heads")
        func staleCursorResyncs() async throws {
            let kept = FFISyncState(cursor: Data([7]), heads: [assetA: FFIPurgeDelivery.headHash(Data([1]))])
            let verbs = ScriptedVerbs(pages: [
                .failure(FfiError.Sync(code: FFISyncDriver.staleCursorCode, detail: "new epoch")),
                .success(page([entry(assetA, record: Data([1])), entry(assetC, record: Data([2]))], next: Data([1]))),
            ])

            let drain = try await driver(verbs, store: MemoryStore(kept), sink: DrainSink()).sync()

            #expect(drain.resynced)
            #expect(verbs.cursors == [Data([7]), Data()])
            #expect(drain.level == 1)
            #expect(drain.newlyIndexed == [assetC])
        }

        @Test("a page that claims more without advancing the cursor stops the drain")
        func stalledCursorStops() async throws {
            let verbs = ScriptedVerbs(pages: [.success(page([], next: Data(), more: true))])

            await #expect(throws: FeedDrainError.cursorStalled) {
                try await driver(verbs, sink: DrainSink()).sync()
            }
        }

        @Test("the state file round-trips and an absent file loads as none")
        func stateFileRoundTrips() throws {
            let file = FFISyncStateFile(url: stagingDirectory().appending(path: "state.json"))
            #expect(try file.load() == nil)
            let state = FFISyncState(cursor: Data([1, 2]), heads: [assetA: Data([3])])

            try file.save(state)

            #expect(try file.load() == state)
        }

        @Test("the blob root is the API origin's /v1 root")
        func blobRoot() {
            #expect(FFISessionSyncVerbs.blobBaseURL(apiBaseURL: "https://capsule.test:8443") == "https://capsule.test:8443/v1")
            #expect(FFISessionSyncVerbs.blobBaseURL(apiBaseURL: "https://capsule.test/") == "https://capsule.test/v1")
        }
    }
}
