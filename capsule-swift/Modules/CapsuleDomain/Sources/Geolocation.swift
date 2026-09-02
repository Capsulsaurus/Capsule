import Foundation

// MARK: - Geolocation

/// Where a GPS fix came from (*Metadata — Closed Enum Value Sets*).
///
/// A `derived` fix is written to the canonical `gps` field **only on explicit
/// user confirmation** — the same promotion rule as `tags_ai` → `tags_user`, so
/// an automated guess can never silently overwrite capture truth.
public enum GpsSource: ClosedWireEnum {
    /// Written by the capturing device's EXIF.
    case exif
    /// Entered by the user.
    case manual
    /// Client-derived (a paired device's location, an ML suggestion).
    case derived
    case unknown(String)

    public static let knownCases: [GpsSource] = [.exif, .manual, .derived]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .exif: "exif"
        case .manual: "manual"
        case .derived: "derived"
        case let .unknown(raw): raw
        }
    }

    /// Whether this fix needs explicit user confirmation before it may be
    /// promoted into the canonical `gps` field.
    public var requiresUserConfirmation: Bool {
        self == .derived
    }
}

/// The coordinate datum a fix is stored in (*Metadata — Geolocation*).
///
/// The coordinate is stored **verbatim in the datum the source supplied** and
/// never converted at rest: GCJ-02 → WGS-84 has no exact inverse, so converting
/// on input would destroy the user's ground truth. BD-09 is never a storable
/// datum — it is folded to GCJ-02 at the input edge.
public enum GpsDatum: ClosedWireEnum {
    /// The near-universal camera datum, and the **wire-absent default**.
    case wgs84
    /// China's legally mandated obfuscated datum.
    case gcj02
    case unknown(String)

    public static let knownCases: [GpsDatum] = [.wgs84, .gcj02]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .wgs84: "wgs84"
        case .gcj02: "gcj02"
        case let .unknown(raw): raw
        }
    }

    /// Whether this value is wire-absent on encode. `wgs84` omits the key so
    /// every pre-`datum` sidecar and known-answer vector stays byte-identical.
    public var isWireAbsent: Bool {
        self == .wgs84
    }

    /// Whether a coordinate in this datum needs an "approximate" marker
    /// wherever it is displayed on a WGS-84 map — the GCJ-02 inverse is lossy.
    public var displaysAsApproximate: Bool {
        self == .gcj02
    }
}

/// A capture coordinate, datum-tagged (*Metadata — Geolocation*).
///
/// `gps` is a **single atomic value under CRDT merge**: `datum` travels with
/// `lat`/`lon` in one write, so there is no merge rule that could pair a
/// coordinate with the wrong datum.
public struct Gps: Sendable, Equatable, Hashable {
    public var latitude: Double
    public var longitude: Double
    public var source: GpsSource
    /// Wire-absent when ``GpsDatum/wgs84``.
    public var datum: GpsDatum

    public init(latitude: Double, longitude: Double, source: GpsSource, datum: GpsDatum = .wgs84) {
        self.latitude = latitude
        self.longitude = longitude
        self.source = source
        self.datum = datum
    }

    /// The export-safe rounding: 2 decimal places, roughly 1 km
    /// (*Metadata — Privacy on Export*). Applied whenever the asset crosses a
    /// trust boundary and the user has not opted into full precision.
    public var roundedForExport: Gps {
        Gps(
            latitude: (latitude * 100).rounded() / 100,
            longitude: (longitude * 100).rounded() / 100,
            source: source,
            datum: datum
        )
    }
}
