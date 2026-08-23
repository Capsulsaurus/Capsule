import CapsuleDomain
@testable import CapsuleUI
import Testing

@Suite("Asset cell badges")
struct AssetCellOverlayTests {
    @Test("duration reads as m:ss below an hour", arguments: [
        (Int64(0), "0:00"),
        (Int64(999), "0:00"),
        (Int64(1000), "0:01"),
        (Int64(59000), "0:59"),
        (Int64(60000), "1:00"),
        (Int64(3599000), "59:59"),
    ])
    func shortDurations(milliseconds: Int64, expected: String) {
        #expect(AssetCellOverlay.durationText(milliseconds: milliseconds) == expected)
    }

    @Test("duration grows a leading hour field only when it needs one", arguments: [
        (Int64(3600000), "1:00:00"),
        (Int64(3661000), "1:01:01"),
        (Int64(36000000), "10:00:00"),
    ])
    func longDurations(milliseconds: Int64, expected: String) {
        #expect(AssetCellOverlay.durationText(milliseconds: milliseconds) == expected)
    }

    /// A negative duration is not a thing a video has, but it *is* something a
    /// bad sidecar can carry, and a cell that renders `-1:-1` looks like a bug
    /// in the app rather than in the data.
    @Test("a nonsense duration clamps instead of rendering nonsense")
    func negativeDuration() {
        #expect(AssetCellOverlay.durationText(milliseconds: -5000) == "0:00")
    }

    @Test("every stack kind has a glyph, including one this build does not know")
    func stackSymbolsAreTotal() {
        let kinds: [StackType] = [
            .rawJpeg, .burst, .livePhoto, .portrait, .smartSelection,
            .hdrBracket, .focusStack, .pixelShift, .panorama,
            .proxy, .chaptered, .dualAudio, .custom,
            // The load-bearing case: a stack written by a newer client is a
            // real stack, and rendering its cover as a lone photo would
            // misrepresent the library.
            .unknown("time-lapse"),
        ]
        for kind in kinds {
            #expect(!AssetCellOverlay.symbol(for: kind).isEmpty)
        }
    }

    /// The mapping is the whole contract of the badge: a state's *role* decides
    /// its colour and whether it interrupts, so getting one wrong is a
    /// user-visible misstatement about their photos.
    @Test("sync states map to the role that matches how they should read")
    func syncStateRoles() {
        #expect(AssetSyncState.durable.role == .settled)
        #expect(AssetSyncState.uploading(tier: .original, transferred: 1, total: 2).role == .inFlight)
        #expect(AssetSyncState.awaitingOriginal(heldBy: nil).role == .waiting)
        #expect(AssetSyncState.fullResolutionUnavailable(bestAvailable: .preview).role == .degraded)
        #expect(AssetSyncState.quarantined(QuarantineID("q")).role == .attention)
        #expect(AssetSyncState.unreadableOnThisDevice(.localBytesCorrupt).role == .attention)
    }

    /// Written-by-a-newer-version is intact data, not damaged data — but it is
    /// still the one degrade-shaped state the user has to be told about,
    /// because this build will not write it back.
    @Test("a document from a newer client asks for attention without claiming damage")
    func newerVersionIsAttentionNotDegraded() {
        let ahead = SchemaAhead(surface: .sidecarSchema, found: "2", maxKnown: "1")
        let state = AssetSyncState.writtenByNewerVersion(ahead)
        #expect(state.role == .attention)
        #expect(state.needsUserAttention)
        // And it must never be mistaken for a state that permits dropping bytes.
        #expect(!state.permitsLocalRelease)
    }

    @Test("only a durable asset may have its local copy released")
    func onlyDurableReleases() {
        let states: [AssetSyncState] = [
            .uploading(tier: .index, transferred: 0, total: 1),
            .awaitingOriginal(heldBy: nil),
            .quarantined(QuarantineID("q")),
            .unreadableOnThisDevice(.albumKeyNotDelivered),
            .writtenByNewerVersion(SchemaAhead(surface: .protocolVersion, found: "9", maxKnown: "1")),
            .fullResolutionUnavailable(bestAvailable: .thumbnail),
        ]
        for state in states {
            #expect(!state.permitsLocalRelease)
        }
        #expect(AssetSyncState.durable.permitsLocalRelease)
    }

    /// Culling flags deliberately do **not** borrow the sync roles: a rejected
    /// photo is a decision, not a fault, and drawing it in the alarm colour
    /// would make a review pass look like a failure report.
    @Test("cull flags keep their own vocabulary")
    func cullTintsAreDistinct() {
        #expect(CullFlag.pick.tint != CullFlag.reject.tint)
        #expect(CullFlag.neutral.tint != CullFlag.pick.tint)
    }
}
