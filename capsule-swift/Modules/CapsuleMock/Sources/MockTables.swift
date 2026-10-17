import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockCamera

/// A camera body paired with the lens most often on it.
///
/// The domain has no lens field — ``CameraID`` carries model and serial only —
/// so the lens travels here for the surfaces that want to show one (the info
/// panel, search suggestions) without inventing a sidecar field the Rust mirror
/// does not have.
public struct MockCamera: Sendable, Equatable, Hashable {
    public var model: String
    public var lens: String
    /// Whether this body writes RAW, which decides whether its assets are
    /// eligible to be the DNG half of a RAW+JPEG stack.
    public var producesRaw: Bool

    public init(model: String, lens: String, producesRaw: Bool) {
        self.model = model
        self.lens = lens
        self.producesRaw = producesRaw
    }
}

// MARK: - MockTrip

/// A cluster of capture coordinates occupying a contiguous run of days.
///
/// Trips are day-ranged rather than hash-scattered because that is what makes
/// the Places surface pageable: a trip is a contiguous index range, so its
/// assets can be windowed without scanning the library. It is also simply true
/// of real libraries — nobody takes one photo in Lisbon between two in their
/// kitchen.
public struct MockTrip: Sendable, Equatable, Hashable {
    public var identifier: String
    public var latitude: Double
    public var longitude: Double
    /// Degrees of jitter applied around the centroid — a city-sized spread.
    public var spread: Double
    /// The datum the source supplied. One trip is GCJ-02 so the "approximate"
    /// marker on a WGS-84 map is reachable without a contrived fixture.
    public var datum: GpsDatum
    /// Offset from UTC in seconds, so an asset shot abroad has a wall clock that
    /// differs from its UTC instant — the case the timeline axis exists for.
    public var utcOffsetSeconds: Int64

    public init(
        identifier: String,
        latitude: Double,
        longitude: Double,
        spread: Double,
        datum: GpsDatum = .wgs84,
        utcOffsetSeconds: Int64
    ) {
        self.identifier = identifier
        self.latitude = latitude
        self.longitude = longitude
        self.spread = spread
        self.datum = datum
        self.utcOffsetSeconds = utcOffsetSeconds
    }
}

// MARK: - MockTables

/// The small fixed tables every derived field draws from.
///
/// Values, not strings shown to a user: a camera model and a place id are data
/// that happens to be text, in the same way a MIME type is. Nothing here is
/// display copy — that lives in the i18n catalog.
public enum MockTables {
    /// Camera bodies, weighted by the order they are picked in — phones
    /// dominate a real library, so they appear more than once.
    public static let cameras: [MockCamera] = [
        MockCamera(model: "iPhone 17 Pro", lens: "Main Camera 24mm f/1.6", producesRaw: true),
        MockCamera(model: "iPhone 17 Pro", lens: "Telephoto 120mm f/2.8", producesRaw: true),
        MockCamera(model: "iPhone 15", lens: "Main Camera 26mm f/1.6", producesRaw: false),
        MockCamera(model: "Pixel 9 Pro", lens: "Wide 25mm f/1.7", producesRaw: false),
        MockCamera(model: "SONY ILCE-7RM5", lens: "FE 35mm F1.4 GM", producesRaw: true),
        MockCamera(model: "SONY ILCE-7RM5", lens: "FE 70-200mm F2.8 GM OSS II", producesRaw: true),
        MockCamera(model: "FUJIFILM X-T5", lens: "XF23mmF1.4 R LM WR", producesRaw: true),
        MockCamera(model: "Canon EOS R6m2", lens: "RF24-70mm F2.8 L IS USM", producesRaw: true),
        MockCamera(model: "DJI Mini 4 Pro", lens: "FC8482 24mm f/1.7", producesRaw: false),
        MockCamera(model: "RICOH GR IIIx", lens: "GR Lens 40mm F2.8", producesRaw: true),
    ]

    /// Twelve trips plus the home cluster the rest of the library falls back to.
    public static let trips: [MockTrip] = [
        MockTrip(identifier: "trip-lisbon", latitude: 38.7223, longitude: -9.1393, spread: 0.06, utcOffsetSeconds: 3600),
        MockTrip(identifier: "trip-kyoto", latitude: 35.0116, longitude: 135.7681, spread: 0.05, utcOffsetSeconds: 32400),
        MockTrip(identifier: "trip-reykjavik", latitude: 64.1466, longitude: -21.9426, spread: 0.4, utcOffsetSeconds: 0),
        MockTrip(identifier: "trip-shanghai", latitude: 31.2304, longitude: 121.4737, spread: 0.08, datum: .gcj02, utcOffsetSeconds: 28800),
        MockTrip(identifier: "trip-banff", latitude: 51.1784, longitude: -115.5708, spread: 0.3, utcOffsetSeconds: -25200),
        MockTrip(identifier: "trip-marrakesh", latitude: 31.6295, longitude: -7.9811, spread: 0.05, utcOffsetSeconds: 3600),
        MockTrip(identifier: "trip-queenstown", latitude: -45.0312, longitude: 168.6626, spread: 0.25, utcOffsetSeconds: 43200),
        MockTrip(identifier: "trip-patagonia", latitude: -50.9423, longitude: -73.4068, spread: 0.5, utcOffsetSeconds: -10800),
        MockTrip(identifier: "trip-svalbard", latitude: 78.2232, longitude: 15.6267, spread: 0.6, utcOffsetSeconds: 7200),
        MockTrip(identifier: "trip-hoi-an", latitude: 15.8801, longitude: 108.3380, spread: 0.04, utcOffsetSeconds: 25200),
        MockTrip(identifier: "trip-dolomites", latitude: 46.4102, longitude: 11.8440, spread: 0.35, utcOffsetSeconds: 7200),
        MockTrip(identifier: "trip-big-sur", latitude: 36.2704, longitude: -121.8081, spread: 0.4, utcOffsetSeconds: -28800),
    ]

    /// Where the library lives when it is not on a trip.
    public static let home = MockTrip(
        identifier: "place-home",
        latitude: 43.6532,
        longitude: -79.3832,
        spread: 0.09,
        utcOffsetSeconds: -18000
    )

    /// Tags a person actually types. Short, lowercase, reused across assets so
    /// a tag filter returns a meaningful set rather than one photo each.
    public static let userTags = [
        "family", "travel", "portfolio", "print", "birthday", "hiking",
        "food", "architecture", "night", "film-scan", "client-work", "pets",
    ]

    /// What a scene-tagging model emits. Deliberately generic — a model's
    /// vocabulary is not a person's.
    public static let aiTags = [
        "dog", "cat", "beach", "mountain", "food", "document", "screenshot",
        "sunset", "snow", "crowd", "vehicle", "flower", "building", "water",
        "indoor", "night-sky",
    ]

    /// The pixel dimensions a real library actually contains, landscape-first.
    /// Portrait assets are these transposed, which is what an orientation flag
    /// does in practice.
    public static let stillDimensions: [Dimensions] = [
        Dimensions(width: 4032, height: 3024),
        Dimensions(width: 8064, height: 6048),
        Dimensions(width: 6000, height: 4000),
        Dimensions(width: 7008, height: 4672),
        Dimensions(width: 9504, height: 6336),
        Dimensions(width: 3024, height: 3024),
    ]

    /// Video frame sizes.
    public static let videoDimensions: [Dimensions] = [
        Dimensions(width: 1920, height: 1080),
        Dimensions(width: 3840, height: 2160),
        Dimensions(width: 1080, height: 1920),
    ]

    /// Content types with their relative frequency. HEIC and JPEG dominate; RAW
    /// and video are present but not so common that every screen is full of
    /// them.
    public static let contentTypes: [(type: ContentType, weight: Int)] = [
        (.heic, 470),
        (.jpeg, 250),
        (.png, 40),
        (.dng, 70),
        (.mp4, 90),
        (.quicktime, 70),
        (.webp, 10),
    ]

    /// The canonical model slots the AI surfaces report.
    public static let sceneTaggingSlot = ModelSlot(modelID: "capsule-scene", modelVersion: "2.1.0")
    /// A superseded scene-tagging slot, so "stale, pending regeneration" is a
    /// state the UI can actually be walked through.
    public static let staleTaggingSlot = ModelSlot(modelID: "capsule-scene", modelVersion: "1.4.0")
    public static let faceEmbeddingSlot = ModelSlot(modelID: "capsule-face", modelVersion: "3.0.1")
    public static let imageEmbeddingSlot = ModelSlot(modelID: "capsule-clip", modelVersion: "1.2.0")
}
