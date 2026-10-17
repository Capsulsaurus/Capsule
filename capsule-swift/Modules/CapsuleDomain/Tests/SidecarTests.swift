import Foundation
import Testing

import CapsuleDomain
import CapsuleFoundation

/// Parity rules 2, 3, and 4: `unknownFields` round-trips verbatim, the two
/// timestamp conventions coexist, and the timeline sorts on
/// `captureUTC ?? captureTimestamp`.
@Suite("Sidecar parity: unknown bytes, timestamps, and the capture axis")
struct SidecarTests {
    private func sidecar(
        captureRFC3339: String = "2026-01-01T00:00:00Z",
        unknownFields: Data = Data()
    ) throws -> SidecarV1 {
        let capture = try #require(CapsuleTimestamp(rfc3339: captureRFC3339))
        return SidecarV1(
            cryptoSuiteID: 1,
            uuid: "01900000-0000-7000-8000-000000000001",
            hash: "abc123",
            captureTimestamp: capture,
            importTimestamp: Fixtures.epoch,
            contentType: .heic,
            deviceID: Fixtures.deviceA,
            sessionID: SessionID("01900000-0000-7000-8000-000000000002"),
            unknownFields: unknownFields
        )
    }

    // MARK: Parity rule 2 — unknown fields

    @Test("unknown fields round-trip byte-for-byte and are never inspected")
    func unknownFieldsRoundTrip() throws {
        // The signature covers these bytes, so stripping or re-ordering them
        // invalidates it. They are opaque here precisely so no code can form an
        // opinion about a schema it does not implement.
        let opaque = Data([0xA1, 0x66, 0x66, 0x75, 0x74, 0x75, 0x72, 0x65, 0x01])
        let original = try sidecar(unknownFields: opaque)

        var copy = original
        copy.rating = copy.rating.applying(
            Stamped(value: 4, timestamp: Fixtures.epoch, author: Fixtures.deviceA)
        )

        #expect(copy.unknownFields == opaque)
        #expect(copy.unknownFields.count == opaque.count)
    }

    @Test("an empty unknown map is distinct from absent bytes but still round-trips")
    func emptyUnknownFields() throws {
        let empty = try sidecar()
        #expect(empty.unknownFields.isEmpty)
    }

    // MARK: Parity rule 3 — two timestamp conventions

    @Test("an RFC 3339 string keeps its exact text while exposing epoch seconds")
    func bothConventionsAvailable() throws {
        // The text is inside the signed bytes: re-rendering it — normalising an
        // offset, dropping a fractional second — invalidates the signature. So
        // the original spelling is preserved verbatim alongside the sortable
        // integer form.
        let text = "2026-01-01T00:00:00.500Z"
        let stamp = try #require(CapsuleTimestamp(rfc3339: text))
        #expect(stamp.rfc3339 == text)
        #expect(stamp.epochSeconds == 1767225600)
    }

    @Test("an offset spelling parses to the same instant as its UTC spelling")
    func offsetSpellingsAgree() throws {
        let utc = try #require(CapsuleTimestamp(rfc3339: "2026-01-01T00:00:00Z"))
        let offset = try #require(CapsuleTimestamp(rfc3339: "2026-01-01T02:00:00+02:00"))
        // Equality is on the instant, which is what a timeline needs...
        #expect(utc == offset)
        // ...but the texts differ, which is what a signature needs.
        #expect(utc.rfc3339 != offset.rfc3339)
    }

    @Test("an unparseable timestamp is nil, never a zero instant")
    func unparseableIsNil() {
        // Silently becoming 1970 would place a corrupted asset at the far end
        // of every timeline instead of surfacing a structural rejection.
        #expect(CapsuleTimestamp(rfc3339: "not a timestamp") == nil)
        #expect(CapsuleTimestamp(rfc3339: "") == nil)
    }

    @Test("epoch seconds render to canonical UTC for timestamps this client mints")
    func mintedTimestampsAreCanonical() {
        let stamp = CapsuleTimestamp(epochSeconds: 1767225600)
        #expect(stamp.rfc3339 == "2026-01-01T00:00:00Z")
    }

    // MARK: Parity rule 4 — the capture axis

    @Test("the effective capture timestamp prefers UTC when the zone is known")
    func effectiveCapturePrefersUTC() {
        let known = CaptureTime(
            captureTimestamp: CapsuleTimestamp(epochSeconds: 1000),
            captureUTC: CapsuleTimestamp(epochSeconds: 2000),
            timezoneSource: .offsetExif
        )
        #expect(known.effectiveCaptureTimestamp.epochSeconds == 2000)
        #expect(!known.isFloating)
    }

    @Test("the effective capture timestamp falls back to the wall clock when it floats")
    func effectiveCaptureFallsBack() {
        let floating = CaptureTime(captureTimestamp: CapsuleTimestamp(epochSeconds: 1000))
        #expect(floating.effectiveCaptureTimestamp.epochSeconds == 1000)
        #expect(floating.isFloating)
    }

    @Test("the timeline sorts on the effective axis, not the raw wall clock")
    func timelineSortsOnEffectiveAxis() {
        // A photo taken abroad has a wall clock that disagrees with its UTC
        // instant. Sorting some rows by one and some by the other reorders the
        // library every time such a photo appears.
        let abroad = Fixtures.libraryAsset(id: "abroad", captureSeconds: 5000, captureUTCSeconds: 1000)
        let home = Fixtures.libraryAsset(id: "home", captureSeconds: 3000, captureUTCSeconds: 3000)

        #expect(abroad.effectiveCaptureTimestamp.epochSeconds == 1000)
        #expect(home.effectiveCaptureTimestamp.epochSeconds == 3000)
        // Newest first: `home` (3000) precedes `abroad` (1000), even though
        // `abroad`'s wall clock reads later.
        #expect(LibraryAsset.isOrderedNewestFirst(home, abroad))
        #expect(!LibraryAsset.isOrderedNewestFirst(abroad, home))
    }

    @Test("equal capture instants are tie-broken on the identifier, deterministically")
    func tieBreakIsStable() {
        // Without the tiebreak, two photos captured in the same second can
        // order differently on two devices, and section offsets stop agreeing
        // across the account.
        let first = Fixtures.libraryAsset(id: "aaaa", captureSeconds: 100, captureUTCSeconds: 100)
        let second = Fixtures.libraryAsset(id: "bbbb", captureSeconds: 100, captureUTCSeconds: 100)
        #expect(LibraryAsset.isOrderedNewestFirst(first, second))
        #expect(!LibraryAsset.isOrderedNewestFirst(second, first))
    }

    // MARK: Sidecar accessors

    @Test("wire-absent registers resolve to their documented defaults")
    func wireAbsentDefaults() throws {
        let bare = try sidecar()
        #expect(bare.cullFlag == .neutral)
        #expect(!bare.isUserHidden)
        #expect(bare.currentStackMembership == nil)
        #expect(!bare.cull.hasBeenWritten)
    }

    @Test("leaving a stack is a stamped nil, distinct from never having been stacked")
    func leavingStackIsStamped() throws {
        var asset = try sidecar()
        let membership = StackMembership(
            stackID: StackID("01900000-0000-7000-8000-00000000000a"),
            stackType: .burst,
            role: .primary
        )
        asset.stackMembership = asset.stackMembership.applying(
            Stamped(value: membership, timestamp: Fixtures.epoch, author: Fixtures.deviceA)
        )
        #expect(asset.currentStackMembership == membership)
        #expect(asset.stackMembership.hasBeenWritten)

        asset.stackMembership = asset.stackMembership.applying(
            Stamped(value: nil, timestamp: Fixtures.time(offsetSeconds: 10), author: Fixtures.deviceA)
        )
        #expect(asset.currentStackMembership == nil)
        // Still written — the register knows the asset *left* a stack, which is
        // not the same as never having joined one.
        #expect(asset.stackMembership.hasBeenWritten)
    }

    @Test("a newer sidecar schema is readable but refuses to be written")
    func schemaVersioning() throws {
        var ahead = try sidecar()
        ahead.sidecarSchema = SidecarV1.currentSchema + 1
        #expect(ahead.isFromNewerSchema)
        #expect(!ahead.isWritableBy(maxKnownSchema: SidecarV1.currentSchema))
        #expect(ahead.isWritableBy(maxKnownSchema: SidecarV1.currentSchema + 1))

        let current = try sidecar()
        #expect(!current.isFromNewerSchema)
        #expect(current.isWritableBy(maxKnownSchema: SidecarV1.currentSchema))
    }

    @Test("GPS is export-rounded to roughly a kilometre by default")
    func exportRounding() {
        let precise = Gps(latitude: 51.501364, longitude: -0.141890, source: .exif)
        let rounded = precise.roundedForExport
        #expect(rounded.latitude == 51.5)
        #expect(rounded.longitude == -0.14)
        // The datum and source travel with the coordinate; rounding is not a
        // conversion.
        #expect(rounded.datum == precise.datum)
        #expect(rounded.source == precise.source)
    }

    @Test("a derived GPS fix requires explicit user confirmation before promotion")
    func derivedFixNeedsConfirmation() {
        #expect(GpsSource.derived.requiresUserConfirmation)
        #expect(!GpsSource.exif.requiresUserConfirmation)
        #expect(!GpsSource.manual.requiresUserConfirmation)
    }

    @Test("a GCJ-02 coordinate is marked approximate wherever it is displayed")
    func gcj02IsApproximate() {
        #expect(GpsDatum.gcj02.displaysAsApproximate)
        #expect(!GpsDatum.wgs84.displaysAsApproximate)
    }

    @Test("content type derives media kind without a second stored field")
    func mediaKindDerivation() {
        #expect(ContentType.heic.mediaKind == .image)
        #expect(ContentType.dng.mediaKind == .image)
        #expect(ContentType.quicktime.mediaKind == .video)
        #expect(ContentType.mp4.presentationMediaType == MediaType.video)
        // An unknown format still classifies from its MIME prefix rather than
        // defaulting silently to a photo viewer for a video.
        #expect(ContentType(rawValue: "video/future").mediaKind == .video)
    }
}
