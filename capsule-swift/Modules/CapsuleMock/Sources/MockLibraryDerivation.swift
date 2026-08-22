import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockAssetFacets

/// The handful of fields a query filter actually reads.
///
/// Deriving a whole ``LibraryAsset`` allocates a caption, two tag sets, and an
/// LQIP buffer. A filtered `dayCounts(...)` over 250 000 assets that did so
/// would allocate a million objects to answer a question about counts. These
/// facets are the same derivation truncated to the fields
/// ``TimelineQuery`` inspects, so filtered aggregates stay a tight arithmetic
/// loop — and, because they come from the *same* hashes, they cannot disagree
/// with the full asset.
public struct MockAssetFacets: Sendable, Equatable, Hashable {
    public var contentType: ContentType
    public var mediaKind: MediaKind
    public var rating: UInt8
    public var cull: CullFlag
    public var albumOrdinal: Int
    public var captureUTC: Int64

    public init(
        contentType: ContentType,
        mediaKind: MediaKind,
        rating: UInt8,
        cull: CullFlag,
        albumOrdinal: Int,
        captureUTC: Int64
    ) {
        self.contentType = contentType
        self.mediaKind = mediaKind
        self.rating = rating
        self.cull = cull
        self.albumOrdinal = albumOrdinal
        self.captureUTC = captureUTC
    }
}

// MARK: - MockCaptureInstant

/// A capture instant in both of the domain's conventions, plus where it was
/// taken.
public struct MockCaptureInstant: Sendable, Equatable {
    /// The UTC instant — the timeline axis.
    public var utcSeconds: Int64
    /// The device-local wall clock, which differs from UTC exactly when the
    /// photograph was taken away from home.
    public var wallClockSeconds: Int64
    /// The trip the coordinates belong to, or `nil` for the home cluster.
    public var trip: MockTrip?
    /// The day index the instant falls on.
    public var dayIndex: Int
}

// MARK: - Derivation

public extension MockLibrary {
    /// How many container albums the mock world has. Assets are distributed
    /// across them, so an album detail view is never empty and never the whole
    /// library.
    static var albumCount: Int { 6 }

    /// The identifier of the asset at a timeline index.
    func identifier(at index: Int) -> AssetID {
        MockAssetRef(kind: .live, index: index).identifier(seed: profile.seed)
    }

    /// Whether a reference names an asset this library actually derives.
    ///
    /// Range-checked rather than trusted: an identifier decodes structurally
    /// long after the asset it named was purged, and answering for it would
    /// resurrect deleted state.
    func contains(_ ref: MockAssetRef) -> Bool {
        switch ref.kind {
        case .live:
            ref.index >= 0 && ref.index < assetCount
        case .stackMember:
            ref.index >= 0 && ref.index < assetCount
                && ref.memberOrdinal >= 1 && Int(ref.memberOrdinal) < stackMemberCount(at: ref.index)
        case .trashed:
            ref.index >= 0 && ref.index < profile.derivedTrashCount
        case .userHidden:
            ref.index >= 0 && ref.index < profile.derivedHiddenCount
        }
    }

    // MARK: Trips

    /// A trip's window in day indices, derived once from the seed.
    ///
    /// Contiguous by construction, which is what makes the Places surface
    /// pageable: a trip is an index range, not a scattered predicate.
    func tripWindow(_ ordinal: Int) -> (start: Int, length: Int) {
        let hash = MockHash.value(seed: profile.seed, index: ordinal, salt: .trip)
        let length = MockHash.integer(MockHash.mix(hash), in: 5 ... 12)
        let latest = max(0, dayCount - length - 1)
        return (MockHash.integer(hash, in: 0 ... latest), length)
    }

    /// The trip covering a day, or `nil` when the day belongs to the home
    /// cluster. Windows may overlap; the first match wins, which is as
    /// arbitrary and as harmless as it sounds.
    func trip(forDay dayIndex: Int) -> MockTrip? {
        for ordinal in MockTables.trips.indices {
            let window = tripWindow(ordinal)
            guard dayIndex >= window.start, dayIndex < window.start + window.length else { continue }
            return MockTables.trips[ordinal]
        }
        return nil
    }

    // MARK: Capture time

    /// The capture instant for a derived asset.
    ///
    /// For a live asset this is the arithmetic that makes index order and
    /// timeline order the same thing: instants inside a day are strictly
    /// decreasing with index, spread across a plausible waking window, so
    /// ``LibraryAsset/isOrderedNewestFirst(_:_:)`` agrees with the index without
    /// ever reaching the identifier tiebreak.
    func captureInstant(for ref: MockAssetRef) -> MockCaptureInstant {
        switch ref.kind {
        case .live:
            liveCaptureInstant(index: ref.index)
        case .stackMember:
            memberCaptureInstant(ref: ref)
        case .trashed, .userHidden:
            asideCaptureInstant(ref: ref)
        }
    }

    private func liveCaptureInstant(index: Int) -> MockCaptureInstant {
        let dayIndex = self.dayIndex(forAsset: index)
        let dayTotal = max(1, count(forDay: dayIndex))
        let offsetInDay = index - startIndex(forDay: dayIndex)
        // At least nine seconds between consecutive assets, so a collapsed
        // stack's members — which sit one second apart behind their primary —
        // fall strictly between it and the next asset. Without that gap the
        // derived emission order and
        // `LibraryAsset.isOrderedNewestFirst` could disagree once
        // `includeStackHidden` expands a stack, and offsets would drift.
        let step = max(9, 68400 / dayTotal)
        let jitterHash = MockHash.value(seed: profile.seed, index: index, salt: .timeOfDay)
        let jitter = Int(jitterHash % UInt64(step))
        let secondOfDay = max(1, 79200 - offsetInDay * step - jitter)
        return instant(dayIndex: dayIndex, secondOfDay: Int64(secondOfDay))
    }

    private func memberCaptureInstant(ref: MockAssetRef) -> MockCaptureInstant {
        var primary = liveCaptureInstant(index: ref.index)
        let shift = Int64(ref.memberOrdinal)
        primary.utcSeconds -= shift
        primary.wallClockSeconds -= shift
        return primary
    }

    /// Trash and hidden assets are drawn from the recent past rather than the
    /// whole span: a user's trash is what they deleted lately, not a uniform
    /// sample of six years.
    private func asideCaptureInstant(ref: MockAssetRef) -> MockCaptureInstant {
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .timeOfDay)
        let horizon = max(1, min(dayCount, 400)) - 1
        let dayIndex = MockHash.integer(hash, in: 0 ... horizon)
        let secondOfDay = Int64(MockHash.integer(MockHash.mix(hash), in: 10800 ... 79200))
        return instant(dayIndex: dayIndex, secondOfDay: secondOfDay)
    }

    private func instant(dayIndex: Int, secondOfDay: Int64) -> MockCaptureInstant {
        let midnight = MockCalendar.startOfDay(dayNumber: dayNumber(forDay: dayIndex))
        let utcSeconds = midnight + secondOfDay
        let location = trip(forDay: dayIndex)
        let offset = location?.utcOffsetSeconds ?? MockTables.home.utcOffsetSeconds
        return MockCaptureInstant(
            utcSeconds: utcSeconds,
            wallClockSeconds: utcSeconds + offset,
            trip: location,
            dayIndex: dayIndex
        )
    }

    // MARK: Facets

    /// The filter-relevant fields of a live asset, without building one.
    func facets(at index: Int) -> MockAssetFacets {
        facets(for: MockAssetRef(kind: .live, index: index))
    }

    func facets(for ref: MockAssetRef) -> MockAssetFacets {
        let derivation = ref.derivationIndex
        let type = contentType(for: ref)
        return MockAssetFacets(
            contentType: type,
            mediaKind: type.mediaKind,
            rating: rating(derivationIndex: derivation),
            cull: cullFlag(for: ref),
            albumOrdinal: albumOrdinal(derivationIndex: derivation),
            captureUTC: captureInstant(for: ref).utcSeconds
        )
    }

    /// The asset's format.
    ///
    /// An asset in the "newer version" population gets a content type this build
    /// cannot name, preserved verbatim in ``ContentType/unknown(_:)`` — readable,
    /// never writable, and the reason the "created with a newer version"
    /// indicator has something to indicate.
    func contentType(for ref: MockAssetRef) -> ContentType {
        let derivation = ref.derivationIndex
        if isFromNewerVersion(derivationIndex: derivation) {
            return .unknown("image/x-capsule-future")
        }
        let hash = MockHash.value(seed: profile.seed, index: derivation, salt: .contentType)
        let weights = MockTables.contentTypes.map(\.weight)
        guard let position = MockHash.weightedIndex(hash, weights: weights) else { return .heic }
        return MockTables.contentTypes[position].type
    }

    func rating(derivationIndex: Int) -> UInt8 {
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .rating)
        // Most photographs are never rated; a review pass leaves a long tail.
        switch MockHash.integer(hash, in: 0 ... 99) {
        case 0 ..< 62: return 0
        case 62 ..< 74: return 1
        case 74 ..< 86: return 2
        case 86 ..< 94: return 3
        case 94 ..< 99: return 4
        default: return 5
        }
    }

    /// The trinary cull flag, orthogonal to the rating exactly as the domain
    /// insists — a reject here can and does carry three stars.
    func cullFlag(for ref: MockAssetRef) -> CullFlag {
        if isFromNewerVersion(derivationIndex: ref.derivationIndex) {
            return .unknown("shortlist")
        }
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .cull)
        switch MockHash.integer(hash, in: 0 ... 99) {
        case 0 ..< 74: return .neutral
        case 74 ..< 90: return .pick
        default: return .reject
        }
    }

    func albumOrdinal(derivationIndex: Int) -> Int {
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .stacking, sub: 991)
        return MockHash.integer(hash, in: 0 ... (Self.albumCount - 1))
    }

    /// Whether this asset was written by a client newer than this build.
    func isFromNewerVersion(derivationIndex: Int) -> Bool {
        guard profile.newerVersionPerMille > 0 else { return false }
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .schemaAhead)
        return MockHash.occurs(hash, perMille: profile.newerVersionPerMille)
    }

    /// The camera that took the shot, and the lens that was on it.
    func camera(derivationIndex: Int) -> MockCamera {
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .camera)
        return MockHash.element(hash, from: MockTables.cameras) ?? MockTables.cameras[0]
    }

    /// A stable, body-unique serial. A fingerprinting surface in the real
    /// system, which is why it is export-stripped by default; derived here so
    /// the "strip camera serial on export" toggle has something to strip.
    func cameraSerial(derivationIndex: Int) -> String {
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .camera, sub: 7)
        return MockHash.hex(hash, digits: 12).uppercased()
    }
}
