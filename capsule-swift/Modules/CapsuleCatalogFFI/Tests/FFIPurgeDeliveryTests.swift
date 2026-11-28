import CapsuleCatalog
@testable import CapsuleCatalogFFI
import Foundation
import Testing

extension FFIPurgeDeliveryFixture {
    @Suite("FFIPurgeDelivery — a purge delivered with the head the sync feed names")
    struct FFIPurgeDeliveryTests {
        @Test(
            "each session outcome becomes the PurgeDelivery it names",
            arguments: [
                (applied, PurgeDelivery.delivered),
                (FfiLifecycleOpOutcome.applied(assetId: assetA, action: "delete", syncSeq: 9, replayed: true), .delivered),
                (FfiLifecycleOpOutcome.notIndexed, .held),
                (FfiLifecycleOpOutcome.restoreTooLate(retentionUntil: "2026-01-02T00:00:00Z"), .held),
                (failure(permanent: true), .failed(permanent: true)),
                (failure(permanent: false), .failed(permanent: false)),
            ]
        )
        func mapsOutcomes(outcome: FfiLifecycleOpOutcome, expected: PurgeDelivery) async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, head: Data([1]))], next: Data([1])))],
                outcomes: [assetA: outcome]
            )

            #expect(try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA) == expected)
        }

        @Test("the head sent is the latest entry's record across every page of the drain")
        func sendsTheLatestHead() async throws {
            let verbs = ScriptedVerbs(
                pages: [
                    .success(page([entry(assetA, head: Data([0x01]), seq: 1)], next: Data([1]), more: true)),
                    .success(page([entry(assetB, head: Data([0x02]), seq: 2)], next: Data([2]), more: true)),
                    .success(page([entry(assetA, head: Data([0x03]), seq: 3)], next: Data([3]))),
                ],
                outcomes: [assetA: applied]
            )

            _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

            #expect(verbs.cursors == [Data(), Data([1]), Data([2])])
            #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: hashed(0x03))])
        }

        @Test("an asset the feed does not list is sent with no head, so the session holds it")
        func unlistedAssetHasNoHead() async throws {
            let verbs = ScriptedVerbs(pages: [.success(page([entry(assetB, head: Data([0x02]))], next: Data([1])))])

            let delivery = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

            #expect(delivery == .held)
            #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: nil)])
        }

        @Test("a listed entry that carries no record is sent as an empty head, not as unlisted")
        func emptyHeadIsListed() async throws {
            let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, head: Data())], next: Data([1])))])

            _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

            #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: Data())])
        }

        @Test("a purge whose kept head the feed already lists is delivered without a send")
        func levelPurgeSendsNothing() async throws {
            let kept = Data([0x09, 0x0A])
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, head: kept), entry(assetB, head: Data([0x01]))], next: Data([1])))],
                outcomes: [assetB: applied],
                keptHeads: [assetA: kept, assetB: Data([0x02])]
            )
            let delivery = FFIPurgeDelivery(verbs: verbs)

            #expect(try await delivery.deliverPurge(assetID: assetA) == .delivered)
            #expect(try await delivery.deliverPurge(assetID: assetB) == .delivered)

            // The server holds A's kept head, so only B, whose head it lacks, is sent.
            #expect(verbs.deliveries == [.init(assetID: assetB, serverHeadHash: hashed(0x01))])
        }

        @Test("a listed entry that carries no record is never taken as level")
        func emptyHeadIsNotLevel() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, head: Data())], next: Data([1])))],
                keptHeads: [assetA: Data()]
            )

            _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

            #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: Data())])
        }

        @Test("a head's hash is the SHA-256 of its bytes, 32 bytes whatever the record's size")
        func headHashIsSHA256() {
            let hex = FFIPurgeDelivery.headHash(Data("abc".utf8)).map { String(format: "%02x", $0) }.joined()
            #expect(hex == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            #expect(FFIPurgeDelivery.headHash(Data(repeating: 7, count: 4096)).count == 32)
        }

        @Test("a feed id in another case names the same asset; one that is not a UUID is ignored")
        func feedIdsAreNormalised() async throws {
            let verbs = ScriptedVerbs(pages: [
                .success(page(
                    [entry(assetA.uppercased(), head: Data([0x07])), entry("not-a-uuid", head: Data([0x08]))],
                    next: Data([1])
                )),
            ])

            _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

            #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: hashed(0x07))])
            #expect(FFIPurgeDelivery.feedKey("not-a-uuid") == nil)
        }

        @Test("a later delivery resumes from the cursor the last drain ended at and folds what changed")
        func laterDeliveryResumes() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, head: Data([0x01]))], next: Data([1])))],
                outcomes: [assetA: applied]
            )
            let delivery = FFIPurgeDelivery(verbs: verbs)
            _ = try await delivery.deliverPurge(assetID: assetA)

            verbs.enqueue([.success(page([entry(assetA, head: Data([0x05]))], next: Data([2])))])
            _ = try await delivery.deliverPurge(assetID: assetA)

            #expect(verbs.cursors == [Data(), Data([1])])
            #expect(verbs.deliveries.map(\.serverHeadHash) == [hashed(0x01), hashed(0x05)])
        }

        @Test("a feed read that fails sends nothing, and the next drain starts where the last good one ended")
        func failedDrainCommitsNothing() async throws {
            let verbs = ScriptedVerbs(pages: [
                .success(page([entry(assetA, head: Data([0x01]))], next: Data([1]), more: true)),
                .failure(FfiError.Sync(code: nil, detail: "unreachable")),
            ])
            let delivery = FFIPurgeDelivery(verbs: verbs)

            await #expect(throws: FfiError.self) { _ = try await delivery.deliverPurge(assetID: assetA) }
            #expect(verbs.deliveries.isEmpty)

            verbs.enqueue([.success(page([entry(assetB, head: Data([0x02]))], next: Data([2])))])
            _ = try await delivery.deliverPurge(assetID: assetA)

            // The first page's head was read in the drain that failed, so it is not trusted.
            #expect(verbs.cursors == [Data(), Data([1]), Data()])
            #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: nil)])
        }

        @Test("a page that claims more without advancing the cursor stops the drain")
        func stalledCursorStops() async {
            let verbs = ScriptedVerbs(pages: [
                .success(page([], next: Data([1]), more: true)),
                .success(page([], next: Data([1]), more: true)),
            ])

            await #expect(throws: FeedDrainError.cursorStalled) {
                _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)
            }
            #expect(verbs.deliveries.isEmpty)
        }

        @Test("the purged assets are the workspace's list, unchanged")
        func listsPurgedAssets() async throws {
            let verbs = ScriptedVerbs(pages: [], purged: [assetA, assetB])

            #expect(try await FFIPurgeDelivery(verbs: verbs).purgedAssetIDs() == [assetA, assetB])
            #expect(verbs.cursors.isEmpty)
        }
    }

    @Suite("FFIPurgeDelivery — a held asset's lifecycle records delivered with the head the sync feed names")
    struct FFILifecycleDeliveryTests {
        @Test(
            "each session outcome becomes the LifecycleDelivery it names, a too-late restore distinctly",
            arguments: [
                (applied, LifecycleDelivery.delivered),
                (FfiLifecycleOpOutcome.notIndexed, .held),
                (
                    FfiLifecycleOpOutcome.restoreTooLate(retentionUntil: "2026-01-02T00:00:00Z"),
                    .withheld(retentionUntil: "2026-01-02T00:00:00Z")
                ),
                (failure(permanent: true), .failed(permanent: true)),
                (failure(permanent: false), .failed(permanent: false)),
            ]
        )
        func mapsOutcomes(outcome: FfiLifecycleOpOutcome, expected: LifecycleDelivery) async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, head: Data([1]))], next: Data([1])))],
                outcomes: [assetA: outcome],
                chainHeads: [assetA: Data([2])]
            )

            #expect(try await FFIPurgeDelivery(verbs: verbs).deliverLifecycle(assetID: assetA) == expected)
        }

        @Test("an asset whose chain head the feed lists is delivered without a send; one the server lacks is sent")
        func levelAssetSendsNothing() async throws {
            let head = Data([0x09, 0x0A])
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, head: head), entry(assetB, head: Data([0x01]))], next: Data([1])))],
                outcomes: [assetB: applied],
                chainHeads: [assetA: head, assetB: Data([0x02])]
            )
            let delivery = FFIPurgeDelivery(verbs: verbs)

            #expect(try await delivery.deliverLifecycle(assetID: assetA) == .delivered)
            #expect(try await delivery.deliverLifecycle(assetID: assetB) == .delivered)

            #expect(verbs.lifecycleDeliveries.map(\.assetID) == [assetB])
            #expect(verbs.lifecycleDeliveries.map(\.serverHeadHash) == [hashed(0x01)])
            #expect(verbs.deliveries.isEmpty, "no purge verb is driven")
        }

        @Test("an unlisted asset is sent with no head, and a listed entry with no record as an empty one")
        func unlistedAndEmptyHeads() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetB, head: Data())], next: Data([1])))],
                chainHeads: [assetA: Data([0x01]), assetB: Data()]
            )
            let delivery = FFIPurgeDelivery(verbs: verbs)

            #expect(try await delivery.deliverLifecycle(assetID: assetA) == .held)
            _ = try await delivery.deliverLifecycle(assetID: assetB)

            #expect(verbs.lifecycleDeliveries.map(\.serverHeadHash) == [nil, Data()])
        }

        @Test("the session is handed the latest changed_at the drain saw, by instant rather than feed order")
        func passesTheLatestServerInstant() async throws {
            let verbs = ScriptedVerbs(
                pages: [
                    .success(page(
                        [entry(assetB, head: Data([0x01]), changedAt: "2026-10-03T00:00:00.250000Z")],
                        next: Data([1]),
                        more: true
                    )),
                    .success(page(
                        [
                            entry(assetA, head: Data([0x02]), changedAt: "2026-10-01T00:00:00Z"),
                            entry(assetB, head: Data([0x03]), changedAt: "not a time"),
                        ],
                        next: Data([2])
                    )),
                ],
                chainHeads: [assetA: Data([0x04])]
            )

            _ = try await FFIPurgeDelivery(verbs: verbs).deliverLifecycle(assetID: assetA)

            #expect(verbs.lifecycleDeliveries == [
                .init(
                    assetID: assetA,
                    serverHeadHash: hashed(0x02),
                    latestChangedAt: "2026-10-03T00:00:00.250000Z"
                ),
            ])
        }

        @Test("with no readable changed_at in the feed the session gets no floor")
        func noFloorWithoutAReadableInstant() async throws {
            let verbs = ScriptedVerbs(
                pages: [.success(page([entry(assetA, head: Data([0x01]), changedAt: "")], next: Data([1])))],
                chainHeads: [assetA: Data([0x02])]
            )

            _ = try await FFIPurgeDelivery(verbs: verbs).deliverLifecycle(assetID: assetA)

            #expect(verbs.lifecycleDeliveries.map(\.latestChangedAt) == [nil])
        }

        @Test("an RFC 3339 instant keeps its fractional second; anything else is no instant")
        func parsesServerInstants() throws {
            let whole = try #require(ServerInstant("2026-10-03T00:00:00Z"))
            let fractional = try #require(ServerInstant("2026-10-03T00:00:00.250000Z"))
            #expect(fractional.date.timeIntervalSince(whole.date) == 0.25)
            #expect(ServerInstant.later(whole, fractional) == fractional)
            #expect(ServerInstant.later(nil, whole) == whole)
            #expect(ServerInstant("2026-10-03") == nil)
            #expect(ServerInstant("2026-10-03T00:00:00.Z") == nil)
        }

        @Test("a chain head the workspace cannot read is thrown, and nothing is sent")
        func unreadableHeadThrows() async {
            let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, head: Data([0x01]))], next: Data([1])))])

            await #expect(throws: FfiError.self) {
                _ = try await FFIPurgeDelivery(verbs: verbs).deliverLifecycle(assetID: assetA)
            }
            #expect(verbs.lifecycleDeliveries.isEmpty)
        }

        @Test("the held assets are the workspace's list, unchanged")
        func listsHeldAssets() async throws {
            let verbs = ScriptedVerbs(pages: [], held: [assetA, assetB])

            #expect(try await FFIPurgeDelivery(verbs: verbs).heldAssetIDs() == [assetA, assetB])
            #expect(verbs.cursors.isEmpty)
        }
    }
}
