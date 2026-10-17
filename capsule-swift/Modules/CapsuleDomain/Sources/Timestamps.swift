import Foundation

// MARK: - CapsuleTimestamp

/// One instant, carrying **both** of Capsule's timestamp conventions.
///
/// Two conventions coexist across the system and neither can be dropped:
///
/// - **RFC 3339 strings** in the signed sidecar and every manifest. They are
///   inside the signed bytes, so re-rendering one — dropping a fractional
///   second, normalising `Z` to `+00:00` — invalidates a signature. The
///   original text is therefore kept verbatim in ``rfc3339`` and echoed back
///   unchanged on write.
/// - **`Int64` Unix epoch seconds** in catalog records and every query the
///   local index runs. Sorting and windowing a timeline over strings would be
///   both wrong and slow.
///
/// Normalising here — once, at the domain boundary — is what stops each feature
/// module from inventing its own conversion. Both forms stay accessible.
///
/// Equality is on the **instant**, not the text: two spellings of the same
/// moment are the same timestamp, which is what a timeline needs. Where byte
/// fidelity matters, compare ``rfc3339`` directly.
public struct CapsuleTimestamp: Sendable, Hashable, Comparable, Codable {
    /// Unix epoch seconds — the catalog and local-index convention.
    public let epochSeconds: Int64

    /// The RFC 3339 rendering, **verbatim as it arrived** when this timestamp
    /// was decoded from a signed document. Echo this back on write; never
    /// re-render it from ``epochSeconds``, which would lose sub-second
    /// precision and offset spelling.
    public let rfc3339: String

    /// Build from epoch seconds, rendering a canonical UTC RFC 3339 string.
    /// Use this only for timestamps *this* client mints.
    public init(epochSeconds: Int64) {
        self.epochSeconds = epochSeconds
        rfc3339 = Self.render(epochSeconds: epochSeconds)
    }

    /// Decode an RFC 3339 string from a signed document, preserving the exact
    /// text. Returns `nil` when the text is not parseable — which the caller
    /// must treat as a structural rejection
    /// (``RejectReason/badTimestamp``), never as a zero instant.
    public init?(rfc3339: String) {
        guard let seconds = Self.parse(rfc3339: rfc3339) else { return nil }
        epochSeconds = seconds
        self.rfc3339 = rfc3339
    }

    /// The instant as a `Date`, for the rare view that needs system date
    /// formatting. Presentation formatting itself belongs to the feature layer.
    public var date: Date {
        Date(timeIntervalSince1970: TimeInterval(epochSeconds))
    }

    public static func == (lhs: CapsuleTimestamp, rhs: CapsuleTimestamp) -> Bool {
        lhs.epochSeconds == rhs.epochSeconds
    }

    public func hash(into hasher: inout Hasher) {
        hasher.combine(epochSeconds)
    }

    public static func < (lhs: CapsuleTimestamp, rhs: CapsuleTimestamp) -> Bool {
        lhs.epochSeconds < rhs.epochSeconds
    }

    /// The UTC day this instant falls on, as the timeline's grouping key.
    public var dayKey: DayKey {
        DayKey(epochSeconds: epochSeconds)
    }
}

private extension CapsuleTimestamp {
    /// Parse an RFC 3339 instant to epoch seconds, or `nil` if the text is not
    /// a valid RFC 3339 timestamp.
    ///
    /// The formatter is built per call rather than cached in a `static`: RFC
    /// 3339 admits fractional seconds and either `Z` or a numeric offset, so
    /// two option sets are needed, and a locally-owned formatter avoids sharing
    /// a mutable reference type across concurrency domains. Parsing happens at
    /// the FFI boundary, never inside a scroll loop, so the allocation is not
    /// on a hot path.
    static func parse(rfc3339: String) -> Int64? {
        let formatter = ISO8601DateFormatter()
        let optionSets: [ISO8601DateFormatter.Options] = [
            [.withInternetDateTime, .withFractionalSeconds],
            [.withInternetDateTime],
        ]
        for options in optionSets {
            formatter.formatOptions = options
            if let date = formatter.date(from: rfc3339) {
                return Int64(date.timeIntervalSince1970.rounded(.down))
            }
        }
        return nil
    }

    /// Render epoch seconds as canonical UTC RFC 3339 (`…Z`, whole seconds).
    /// Only ever applied to a timestamp this client mints — a decoded one keeps
    /// its original text.
    static func render(epochSeconds: Int64) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(epochSeconds)))
    }
}

// MARK: - DayKey

/// A UTC calendar day, the axis a virtualized photo grid is sectioned on.
///
/// A day is `YYYY-MM-DD` in UTC, deliberately not in the viewer's local zone:
/// the grid's section offsets are computed from ``LibraryPort`` day counts and
/// must be identical on every device that renders the same library, exactly like
/// the determinism the smart-album and aggregated-album views rely on.
public struct DayKey: Sendable, Hashable, Comparable, Codable, Identifiable, CustomStringConvertible {
    /// The `YYYY-MM-DD` text. Lexicographic order is chronological order.
    public let rawValue: String

    public var id: String { rawValue }
    public var description: String { rawValue }

    public init(_ rawValue: String) {
        self.rawValue = rawValue
    }

    /// The UTC day containing this instant.
    ///
    /// The floor is computed arithmetically rather than through `Calendar` so
    /// it cannot pick up the viewer's locale or timezone: two devices must
    /// section the same library identically.
    public init(epochSeconds: Int64) {
        let secondsPerDay = Int64(86400)
        let midnight = epochSeconds - (((epochSeconds % secondsPerDay) + secondsPerDay) % secondsPerDay)
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withFullDate, .withDashSeparatorInDate]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        rawValue = formatter.string(from: Date(timeIntervalSince1970: TimeInterval(midnight)))
    }

    public static func < (lhs: DayKey, rhs: DayKey) -> Bool {
        lhs.rawValue < rhs.rawValue
    }
}

// MARK: - CaptureTime

/// How the capture timezone for an asset was resolved.
///
/// Mirrors the catalog's `capture_timezone_source` column and the Rust
/// `CaptureTzSource`. `floating` means no zone is known — the wall clock is all
/// there is, and ``CaptureTime/captureUTC`` is therefore absent.
public enum CaptureTimezoneSource: ClosedWireEnum {
    /// An explicit UTC offset was present in the file's EXIF.
    case offsetExif
    /// The zone was derived from the capture coordinates.
    case gpsLookup
    /// No zone is known; the wall clock floats.
    case floating
    /// A value written by a newer client.
    case unknown(String)

    public static let knownCases: [CaptureTimezoneSource] = [.offsetExif, .gpsLookup, .floating]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .offsetExif: "offset_exif"
        case .gpsLookup: "gps_lookup"
        case .floating: "floating"
        case let .unknown(raw): raw
        }
    }
}

/// An asset's capture instant, in both the forms the system stores it.
///
/// The timeline sorts and sections on ``effectiveCaptureTimestamp`` and nothing
/// else. Mixing the two fields — sorting some rows by wall clock and others by
/// UTC — reorders a library every time a photo taken abroad appears, which is
/// exactly the bug the single derived accessor exists to prevent.
public struct CaptureTime: Sendable, Equatable, Hashable {
    /// Device-local wall clock at capture. Always present.
    public var captureTimestamp: CapsuleTimestamp

    /// The same instant in UTC, present only when the capture timezone was
    /// resolved. Absent for a floating wall clock.
    public var captureUTC: CapsuleTimestamp?

    /// How the zone was resolved, when it was.
    public var timezoneSource: CaptureTimezoneSource?

    public init(
        captureTimestamp: CapsuleTimestamp,
        captureUTC: CapsuleTimestamp? = nil,
        timezoneSource: CaptureTimezoneSource? = nil
    ) {
        self.captureTimestamp = captureTimestamp
        self.captureUTC = captureUTC
        self.timezoneSource = timezoneSource
    }

    /// **The canonical timeline axis**: the UTC capture instant when known,
    /// else the device-local wall clock.
    ///
    /// Mirrors the catalog's `COALESCE(capture_utc, capture_timestamp)`
    /// ordering, and the aggregated album's merge order
    /// (*Federation — Membership and Rendering*: `capture_timestamp`, tie-broken
    /// by `asset_id`). Every sort, every section boundary, every date filter
    /// reads this and never the raw fields.
    public var effectiveCaptureTimestamp: CapsuleTimestamp {
        captureUTC ?? captureTimestamp
    }

    /// Whether the capture zone is unknown, so the instant floats. A viewer may
    /// want to mark this; it never changes the sort.
    public var isFloating: Bool {
        captureUTC == nil
    }
}
