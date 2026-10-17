import CapsuleDomain
import Foundation

// MARK: - ImportFormat

/// Locale-aware renderings of the *data* the import screens display.
///
/// These are values, not copy: a byte count, a count, and a date are the
/// locale's business rather than a translator's, and centralising them is what
/// stops one screen saying 24 GB while the next says 24 GiB.
///
/// Unlike the transfer screens, this module **does** render filenames. That is
/// not an inconsistency: no filename crosses the wire once an asset exists, but
/// everything on these screens is still a file on the user's own disk that they
/// picked themselves, and "IMG_4021.HEIC" is the only handle they have on it.
public enum ImportFormat {
    /// A byte count in the platform's file convention.
    public static func bytes(_ value: UInt64) -> String {
        Int64(clamping: value).formatted(.byteCount(style: .file))
    }

    /// A byte count that may be absent — an unknown free-disk figure. Renders an
    /// em dash rather than a zero, because "unknown" and "none" are different
    /// facts.
    public static func bytes(_ value: UInt64?) -> String {
        guard let value else { return unknown }
        return bytes(value)
    }

    /// A whole number, grouped for the locale.
    public static func count(_ value: Int) -> String {
        value.formatted(.number)
    }

    /// A 0…1 fraction as a whole-number percentage.
    public static func percent(_ value: Double) -> String {
        min(max(value, 0), 1).formatted(.percent.precision(.fractionLength(0)))
    }

    /// An instant, at day-plus-time granularity — what "started" and "finished"
    /// need.
    public static func timestamp(_ value: CapsuleTimestamp?) -> String {
        guard let value else { return unknown }
        return value.date.formatted(date: .abbreviated, time: .shortened)
    }

    /// How long a run took, or `nil` while it is still going.
    public static func elapsed(from start: CapsuleTimestamp, to end: CapsuleTimestamp?) -> String {
        guard let end, end.epochSeconds >= start.epochSeconds else { return unknown }
        return Duration.seconds(end.epochSeconds - start.epochSeconds)
            .formatted(.units(allowed: [.hours, .minutes, .seconds], width: .abbreviated))
    }

    /// The last path component of a source locator — the filename a user
    /// recognises.
    ///
    /// Falls back to the whole locator rather than to an empty string: a
    /// PhotoKit local identifier has no path components, and a blank row would
    /// be worse than an opaque one.
    public static func leaf(_ locator: String) -> String {
        let components = locator.split(separator: "/")
        guard let last = components.last, !last.isEmpty else { return locator }
        return String(last)
    }

    /// The placeholder for a value that exists but is not known here.
    public static let unknown = "—"
}
