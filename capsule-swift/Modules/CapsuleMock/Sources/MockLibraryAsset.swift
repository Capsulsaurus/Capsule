import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Asset projection

public extension MockLibrary {
    /// The asset at a timeline index. The whole point of the module: an O(1)
    /// derivation, with nothing stored behind it.
    func asset(at index: Int) -> LibraryAsset {
        asset(for: MockAssetRef(kind: .live, index: index))
    }

    /// The asset for a derived reference.
    ///
    /// Split into named parts not for tidiness but because each part is a rule
    /// the real adapter also has to follow — the three exclusion flags, the
    /// ladder, the stack cover — and a single 200-line initializer would hide a
    /// disagreement with any of them.
    func asset(for ref: MockAssetRef) -> LibraryAsset {
        let derivation = ref.derivationIndex
        let instant = captureInstant(for: ref)
        let type = contentType(for: ref)
        let ladder = representationState(for: ref)
        let size = dimensions(for: ref, contentType: type)
        return LibraryAsset(
            id: ref.identifier(seed: profile.seed),
            mediaType: mediaType(for: ref, contentType: type),
            contentType: type,
            captureTime: captureTime(instant),
            importTimestamp: CapsuleTimestamp(epochSeconds: importSeconds(instant: instant, ref: ref)),
            dimensions: size,
            lqip: lqip(derivationIndex: derivation),
            durationMilliseconds: durationMilliseconds(for: ref, contentType: type),
            cull: cullFlag(for: ref),
            rating: rating(derivationIndex: derivation),
            tagsUser: userTags(derivationIndex: derivation),
            tagsAI: aiTags(derivationIndex: derivation),
            caption: caption(derivationIndex: derivation),
            hasSupersededCaptions: hasSupersededCaption(derivationIndex: derivation),
            stackMembership: stackMembership(for: ref),
            albumID: MockIdentifiers.albumID(
                seed: profile.seed,
                ordinal: albumOrdinal(derivationIndex: derivation)
            ),
            isDeleted: ref.kind == .trashed,
            deletedAt: ref.kind == .trashed ? CapsuleTimestamp(epochSeconds: deletedSeconds(ref: ref)) : nil,
            isStackHidden: ref.kind == .stackMember,
            isUserHidden: ref.kind == .userHidden,
            representations: ladder.representations,
            syncState: ladder.state
        )
    }

    /// Both timestamp conventions. The wall clock differs from UTC exactly when
    /// the photograph was taken away from home, and the zone source says how the
    /// offset was known — `floating` for the assets whose zone never was.
    private func captureTime(_ instant: MockCaptureInstant) -> CaptureTime {
        let hash = MockHash.mix(UInt64(bitPattern: instant.utcSeconds))
        let isFloating = MockHash.occurs(hash, perMille: 60)
        guard !isFloating else {
            // A floating asset has no zone, so its wall clock *is* the instant
            // the timeline sorts on. Handing it a shifted local clock would put
            // it on a different day from the one its section header claims, and
            // the aggregate and the page would stop agreeing.
            return CaptureTime(
                captureTimestamp: CapsuleTimestamp(epochSeconds: instant.utcSeconds),
                timezoneSource: .floating
            )
        }
        return CaptureTime(
            captureTimestamp: CapsuleTimestamp(epochSeconds: instant.wallClockSeconds),
            captureUTC: CapsuleTimestamp(epochSeconds: instant.utcSeconds),
            timezoneSource: instant.trip == nil ? .offsetExif : .gpsLookup
        )
    }

    /// Imports land somewhere between minutes and a few days after capture —
    /// a phone syncs quickly, a memory card comes home at the weekend.
    private func importSeconds(instant: MockCaptureInstant, ref: MockAssetRef) -> Int64 {
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .duration, sub: 11)
        return instant.utcSeconds + Int64(MockHash.integer(hash, in: 120 ... 320_000))
    }

    private func deletedSeconds(ref: MockAssetRef) -> Int64 {
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .duration, sub: 13)
        let base = MockCalendar.startOfDay(dayNumber: profile.newestDayNumber)
        return base - Int64(MockHash.integer(hash, in: 0 ... 25) * 86400)
    }

    // MARK: Pixels

    /// Pixel dimensions, with a portrait orientation applied by transposing —
    /// which is what an orientation flag does in practice.
    func dimensions(for ref: MockAssetRef, contentType: ContentType) -> Dimensions {
        let derivation = ref.derivationIndex
        let table = contentType.mediaKind == .video ? MockTables.videoDimensions : MockTables.stillDimensions
        let hash = MockHash.value(seed: profile.seed, index: derivation, salt: .dimensions)
        let chosen = MockHash.element(hash, from: table) ?? Dimensions(width: 4032, height: 3024)
        let isPortrait = MockHash.occurs(
            MockHash.value(seed: profile.seed, index: derivation, salt: .orientation),
            perMille: 340
        )
        guard isPortrait, chosen.width != chosen.height else { return chosen }
        return Dimensions(width: chosen.height, height: chosen.width)
    }

    func durationMilliseconds(for ref: MockAssetRef, contentType: ContentType) -> Int64? {
        let isMotion = contentType.mediaKind == .video
            || (ref.kind == .live && stackType(at: ref.index) == .livePhoto)
        guard isMotion else { return nil }
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .duration)
        if ref.kind == .live, stackType(at: ref.index) == .livePhoto {
            return Int64(MockHash.integer(hash, in: 1500 ... 3000))
        }
        return Int64(MockHash.integer(hash, in: 3000 ... 184_000))
    }

    /// The embedded placeholder — the bottom of the degrade ladder, and the
    /// reason a tile is never blank.
    ///
    /// The bytes are opaque to this layer in the real system, so they are opaque
    /// here too: deterministic filler of the right shape. What matters is
    /// ``Lqip/dominantColor``, which ``MockThumbnailRenderer`` draws its gradient
    /// around — the placeholder and the thumbnail agree because both read the
    /// same derivation.
    func lqip(derivationIndex: Int) -> Lqip {
        var bytes = [UInt8]()
        bytes.reserveCapacity(24)
        for step in 0 ..< 3 {
            let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .colour, sub: step)
            withUnsafeBytes(of: hash.bigEndian) { bytes.append(contentsOf: $0) }
        }
        return Lqip(
            chromahash: Data(bytes),
            formatVersion: 1,
            dominantColor: MockPalette.dominantColour(seed: profile.seed, derivationIndex: derivationIndex)
        )
    }

    /// The approximate stored size of an asset's original, for the storage and
    /// quota surfaces. Format-aware, because a DNG and a HEIC of the same scene
    /// differ by an order of magnitude and a storage breakdown that ignores that
    /// teaches the user nothing.
    func byteSize(for ref: MockAssetRef, contentType: ContentType) -> UInt64 {
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .byteSize)
        let range: ClosedRange<Int>
        switch contentType {
        case .dng, .tiff: range = 22_000_000 ... 58_000_000
        case .mp4, .quicktime, .matroska, .webm: range = 18_000_000 ... 620_000_000
        case .png: range = 900_000 ... 9_000_000
        default: range = 1_400_000 ... 6_800_000
        }
        return UInt64(MockHash.integer(hash, in: range))
    }

    // MARK: Metadata

    func userTags(derivationIndex: Int) -> Set<String> {
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .userTags)
        guard MockHash.occurs(hash, perMille: 280) else { return [] }
        let count = MockHash.integer(MockHash.mix(hash), in: 1 ... 3)
        var tags = Set<String>()
        for step in 0 ..< count {
            let pick = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .userTags, sub: step)
            if let tag = MockHash.element(pick, from: MockTables.userTags) { tags.insert(tag) }
        }
        return tags
    }

    /// AI tags, with their model slots intact.
    ///
    /// Some assets carry a **superseded** slot on purpose: a term over a stale
    /// slot must evaluate as stale-excluded rather than being compared across
    /// model versions, and a mock with only current slots makes that path
    /// unreachable.
    func aiTags(derivationIndex: Int) -> Set<AiTag> {
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .aiTags)
        guard MockHash.occurs(hash, perMille: 720) else { return [] }
        let count = MockHash.integer(MockHash.mix(hash), in: 1 ... 3)
        let isStale = MockHash.occurs(MockHash.mix(hash &+ 17), perMille: 180)
        let slot = isStale ? MockTables.staleTaggingSlot : MockTables.sceneTaggingSlot
        var tags = Set<AiTag>()
        for step in 0 ..< count {
            let pick = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .aiTags, sub: step)
            guard let tag = MockHash.element(pick, from: MockTables.aiTags) else { continue }
            tags.insert(AiTag(tag: tag, modelID: slot.modelID, modelVersion: slot.modelVersion))
        }
        return tags
    }

    /// A caption, as a pair of vocabulary tokens rather than a sentence.
    ///
    /// Captions are user *content*, not chrome, so synthesising them is not the
    /// hardcoded-string rule being bent — but prose would still be prose to
    /// translate if it ever leaked into a fixture, and tokens make the intent
    /// unambiguous.
    func caption(derivationIndex: Int) -> String? {
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .caption)
        guard MockHash.occurs(hash, perMille: 140) else { return nil }
        let first = MockHash.element(hash, from: MockTables.aiTags) ?? "photo"
        let second = MockHash.element(MockHash.mix(hash), from: MockTables.userTags) ?? "archive"
        return "\(first) / \(second)"
    }

    /// Whether a displaced caption exists to offer restoring — the surface the
    /// LWW superseded log is there for.
    func hasSupersededCaption(derivationIndex: Int) -> Bool {
        guard caption(derivationIndex: derivationIndex) != nil else { return false }
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .caption, sub: 2)
        return MockHash.occurs(hash, perMille: 300)
    }

    /// The capture coordinate, in **its stored datum** and never converted.
    /// Around three assets in ten have no fix at all, which is what an indoor
    /// library actually looks like.
    func geolocation(for ref: MockAssetRef) -> Gps? {
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .geolocation)
        guard MockHash.occurs(hash, perMille: 700) else { return nil }
        let instant = captureInstant(for: ref)
        let place = instant.trip ?? MockTables.home
        let latitudeJitter = (MockHash.fraction(hash) - 0.5) * place.spread
        let longitudeJitter = (MockHash.fraction(MockHash.mix(hash)) - 0.5) * place.spread
        return Gps(
            latitude: place.latitude + latitudeJitter,
            longitude: place.longitude + longitudeJitter,
            source: MockHash.occurs(MockHash.mix(hash &+ 3), perMille: 60) ? .manual : .exif,
            datum: place.datum
        )
    }
}
