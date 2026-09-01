import Foundation
import Testing

import CapsuleDomain

/// The degrade ladder, per-asset sync states, quarantine surfaces, and paging —
/// parity rule 6.
@Suite("The degrade ladder never bottoms out below a colour")
struct RepresentationTests {
    @Test("the ladder is ordered cheapest first")
    func ladderOrder() {
        #expect(RepresentationTier.allCases == [.dominantColour, .lqip, .thumbnail, .preview, .original])
        #expect(RepresentationTier.dominantColour < RepresentationTier.original)
        #expect(RepresentationTier.preview.degraded == .thumbnail)
        #expect(RepresentationTier.dominantColour.degraded == nil)
    }

    @Test("the dominant colour is always held, so a tile is never blank")
    func colourIsAlwaysHeld() {
        let empty = LocalRepresentations()
        #expect(empty.holds(.dominantColour))
        #expect(empty.best == .dominantColour)
    }

    @Test("the best representation is the highest rung actually held")
    func bestIsHighestHeld() {
        let held = LocalRepresentations(heldTiers: [.lqip, .thumbnail])
        #expect(held.best == .thumbnail)
        #expect(!held.isFullResolutionAvailable)

        let full = held.adding(.original)
        #expect(full.best == .original)
        #expect(full.isFullResolutionAvailable)
    }

    @Test("eviction degrades down the ladder rather than emptying it")
    func evictionDegrades() {
        let evicted = LocalRepresentations(heldTiers: [.lqip, .thumbnail, .preview, .original])
            .removing(.original)
            .removing(.preview)
        #expect(evicted.best == .thumbnail)
        #expect(evicted.holds(.dominantColour))
    }

    @Test("an unknown sync scope fetches nothing eagerly rather than over-fetching")
    func unknownScopeIsConservative() {
        // Guessing high on a metered plan spends a user's data on a policy this
        // build does not understand.
        #expect(SyncScope(rawValue: "metadata_and_something_new").eagerTier == nil)
        #expect(SyncScope.metadataOnly.eagerTier == .lqip)
        #expect(SyncScope.metadataThumbnailsAndOriginals.eagerTier == .original)
    }

    @Test("only a durable asset may have its local bytes released")
    func onlyDurablePermitsRelease() {
        #expect(AssetSyncState.durable.permitsLocalRelease)
        #expect(!AssetSyncState.awaitingOriginal(heldBy: Fixtures.deviceA).permitsLocalRelease)
        #expect(!AssetSyncState.uploading(tier: .original, transferred: 10, total: 100).permitsLocalRelease)
        #expect(!AssetSyncState.quarantined(QuarantineID("q")).permitsLocalRelease)
    }

    @Test("awaiting-original is a badge, not something needing attention")
    func awaitingOriginalIsInformational() {
        // The asset is visible everywhere the moment its manifest and metadata
        // finalize; its original may legitimately still be on the phone that
        // took it. Rendering that as an error teaches users to distrust a
        // working system.
        #expect(!AssetSyncState.awaitingOriginal(heldBy: Fixtures.deviceA).needsUserAttention)
        #expect(!AssetSyncState.fullResolutionUnavailable(bestAvailable: .preview).needsUserAttention)
        #expect(AssetSyncState.quarantined(QuarantineID("q")).needsUserAttention)
        #expect(AssetSyncState.unreadableOnThisDevice(.albumKeyNotDelivered).needsUserAttention)
    }

    @Test("a staged upload's index tier escapes on any usable connection")
    func indexTierIsPermissive() {
        // T0 is a few KB and is what turns a lost phone into a *known* loss.
        for connection in [ConnectionClass.unmetered, .metered, .constrained, .adverse] {
            #expect(UploadTier.index.canOpen(on: connection))
        }
        #expect(!UploadTier.index.canOpen(on: .offline))
    }

    @Test("the original tier waits for an unmetered link unless the user forces it")
    func originalTierNeedsUnmetered() {
        #expect(UploadTier.original.canOpen(on: .unmetered))
        #expect(!UploadTier.original.canOpen(on: .metered))
        #expect(UploadTier.original.canOpen(on: .metered, forceSync: true))
        // A force sync still cannot invent a network.
        #expect(!UploadTier.original.canOpen(on: .offline, forceSync: true))
    }

    @Test("the tier ladder is strictly ordered")
    func tierLadderOrder() {
        #expect(UploadTier.ladder == [.index, .preview, .original])
        #expect(UploadTier.index < UploadTier.original)
    }
}

@Suite("Quarantine surfaces are exactly the eight from the threat model")
struct QuarantineTests {
    @Test("there are exactly eight surfaces")
    func eightSurfaces() {
        // The union exists so the UI and the operator audit share one
        // inventory. A ninth here without a row in the threat model would give
        // the app a category no owner doc defends.
        #expect(QuarantineSurface.knownCases.count == 8)
        #expect(Set(QuarantineSurface.knownCases.map(\.rawValue)).count == 8)
    }

    @Test("each surface names where its bytes live")
    func surfacesNameTheirStorage() {
        #expect(QuarantineSurface.malformedSidecar.storage == .quarantineDirectory)
        #expect(QuarantineSurface.federationSoftFail.storage == .rejectedHashTable)
        #expect(QuarantineSurface.albumUpgradeStrandedWrite.storage == .pendingUntilUpgradeQueue)
        #expect(QuarantineSurface.pendingDropAwaitingAdoption.storage == .serverInbox)
    }

    @Test("only some holding areas preserve the original bytes")
    func preservationDiffersByStorage() {
        // The difference between "you can still get this back" and "we recorded
        // that it happened" is the first thing a user asks.
        #expect(QuarantineStorage.quarantineDirectory.preservesOriginalBytes)
        #expect(QuarantineStorage.serverInbox.preservesOriginalBytes)
        #expect(!QuarantineStorage.auditLog.preservesOriginalBytes)
        #expect(!QuarantineStorage.rejectedHashTable.preservesOriginalBytes)
    }

    @Test("there are exactly three explicit resolutions and only discard is destructive")
    func threeResolutions() {
        #expect(QuarantineResolution.allCases.count == 3)
        #expect(QuarantineResolution.discard.isDestructive)
        #expect(!QuarantineResolution.inspect.isDestructive)
        #expect(!QuarantineResolution.repair.isDestructive)
    }

    @Test("an item is recoverable only when its bytes survive and repair is offered")
    func recoverability() {
        let repairable = QuarantineItem(
            id: QuarantineID("q1"),
            surface: .malformedSidecar,
            reason: .malformedEncoding,
            detectedAt: Fixtures.epoch,
            preservedBytes: 4096,
            resolutions: [.inspect, .repair, .discard]
        )
        #expect(repairable.isRecoverable)

        // A soft-fail records a hash, not bytes — there is nothing to repair.
        let recorded = QuarantineItem(
            id: QuarantineID("q2"),
            surface: .federationSoftFail,
            reason: .staleProvenanceChain,
            detectedAt: Fixtures.epoch,
            resolutions: [.inspect, .discard]
        )
        #expect(!recorded.isRecoverable)
    }

    @Test("a verify rejection quarantines while a pending outcome retries")
    func verifyOutcomeRouting() {
        // Pending is not a soft rejection: without it, a client racing MLS key
        // delivery would quarantine its own perfectly valid assets.
        #expect(VerifyOutcome.terminalReject(.badWriteSig).isQuarantining)
        #expect(!VerifyOutcome.terminalReject(.badWriteSig).isRetryable)
        #expect(VerifyOutcome.pending(.amkNotYetLocal).isRetryable)
        #expect(!VerifyOutcome.pending(.amkNotYetLocal).isQuarantining)
        #expect(VerifyOutcome.accept.isAccepted)
        #expect(!VerifyOutcome.accept.isQuarantining)
    }

    @Test("the reject reason set is complete and every case is distinct")
    func rejectReasonsComplete() {
        #expect(RejectReason.allCases.count == 13)
        #expect(Set(RejectReason.allCases).count == 13)
    }
}

/// Parity rule 6: everything is paged.
@Suite("Reads are windows, never whole arrays")
struct PagingTests {
    @Test("a short page is the end of the collection")
    func shortPageEnds() {
        let page = Page(items: [1, 2, 3], request: PageRequest(offset: 0, limit: 10))
        #expect(!page.hasMore)
        #expect(page.nextRequest == nil)
    }

    @Test("a full page implies more, absent a known total")
    func fullPageImpliesMore() {
        let page = Page(items: Array(0 ..< 10), request: PageRequest(offset: 0, limit: 10))
        #expect(page.hasMore)
        #expect(page.nextRequest == PageRequest(offset: 10, limit: 10))
    }

    @Test("a known total decides, even when the page is full")
    func totalOverridesHeuristic() {
        let page = Page(items: Array(0 ..< 10), request: PageRequest(offset: 0, limit: 10), totalCount: 10)
        #expect(!page.hasMore)
    }

    @Test("an unknown total means unknown, never zero")
    func unknownTotalIsNotZero() {
        // A smart album's count is a full evaluation. A UI that read `nil` as
        // zero would render an empty state over a populated album.
        let page = Page(items: [1], request: PageRequest(offset: 0, limit: 1))
        #expect(page.totalCount == nil)
        #expect(page.hasMore)
    }

    @Test("a negative offset or limit is clamped rather than trapping")
    func requestClamps() {
        let request = PageRequest(offset: -5, limit: -1)
        #expect(request.offset == 0)
        #expect(request.limit == 0)
    }

    @Test("day counts give a grid its section offsets in one read")
    func sectionOffsets() {
        // Without this aggregate a virtualized grid must either load every
        // asset to know how tall it is, or guess and then jump.
        let counts = [
            DayCount(dayKey: DayKey("2026-01-03"), count: 4),
            DayCount(dayKey: DayKey("2026-01-02"), count: 2),
            DayCount(dayKey: DayKey("2026-01-01"), count: 7),
        ]
        #expect(counts.sectionOffsets == [0, 4, 6])
        #expect(counts.totalCount == 13)
    }

    @Test("a day key floors to UTC midnight regardless of the instant within the day")
    func dayKeyFloors() {
        // Arithmetic, not `Calendar`: two devices in different zones must
        // section the same library identically.
        let morning = CapsuleTimestamp(epochSeconds: 1767225600) // 2026-01-01T00:00:00Z
        let evening = CapsuleTimestamp(epochSeconds: 1767225600 + 86399)
        #expect(morning.dayKey == evening.dayKey)
        #expect(morning.dayKey.rawValue == "2026-01-01")

        let nextDay = CapsuleTimestamp(epochSeconds: 1767225600 + 86400)
        #expect(nextDay.dayKey.rawValue == "2026-01-02")
    }

    @Test("day keys sort chronologically as strings")
    func dayKeysSortChronologically() {
        let keys = [DayKey("2026-01-10"), DayKey("2026-01-02"), DayKey("2025-12-31")]
        #expect(keys.sorted().map(\.rawValue) == ["2025-12-31", "2026-01-02", "2026-01-10"])
    }
}
