import CapsuleDomain
import Foundation

// MARK: - SettingsFormat

/// Locale-aware renderings of the *data* a settings screen displays.
///
/// These are deliberately not translatable strings: a byte count, a date, and a
/// percentage are values, and their spelling is the locale's business rather
/// than a translator's. Keeping them here means no screen reaches for
/// `String(format:)` and no screen invents its own byte convention — the
/// difference between 24 GB and 24 GiB in two places in one app is the kind of
/// inconsistency users read as a bug.
public enum SettingsFormat {
    /// A byte count in the platform's file convention.
    public static func bytes(_ value: UInt64) -> String {
        Int64(clamping: value).formatted(.byteCount(style: .file))
    }

    /// A byte count that may be absent — an unset cache budget, an unknown
    /// remaining-disk figure. Renders an em dash rather than a zero, because
    /// "unknown" and "none" are different facts.
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
        value.formatted(.percent.precision(.fractionLength(0)))
    }

    /// An instant, at the granularity a settings row wants: the day plus the
    /// time of day, which is what "last seen" and "last run" need.
    public static func timestamp(_ value: CapsuleTimestamp?) -> String {
        guard let value else { return never }
        return value.date.formatted(date: .abbreviated, time: .shortened)
    }

    /// A day, without a time — for enrollment and escrow dates, where the hour
    /// is noise.
    public static func day(_ value: CapsuleTimestamp?) -> String {
        guard let value else { return never }
        return value.date.formatted(date: .abbreviated, time: .omitted)
    }

    /// An identifier, truncated for a settings row while staying unambiguous.
    ///
    /// Full identifiers are still available to copy; a row that wraps a 36
    /// character UUID onto three lines at large Dynamic Type is unreadable
    /// without telling the user anything the first eight characters did not.
    public static func shortIdentifier(_ value: String, length: Int = 8) -> String {
        value.count <= length ? value : String(value.prefix(length)) + "…"
    }

    /// A `(model_id, model_version)` slot, spelled the way the AI design doc
    /// spells it, so a support report and the screen agree.
    public static func modelSlot(_ slot: ModelSlot) -> String {
        "\(slot.modelID) \(slot.modelVersion)"
    }

    /// A span of seconds, at minute-and-second granularity — the grace window
    /// countdown and nothing longer.
    public static func duration(seconds: Int64) -> String {
        Duration.seconds(seconds).formatted(
            .units(allowed: [.minutes, .seconds], width: .abbreviated)
        )
    }

    /// A whole number of minutes, for a configured window rather than a
    /// countdown.
    public static func minutes(seconds: Int64) -> String {
        Duration.seconds(seconds).formatted(.units(allowed: [.minutes], width: .wide))
    }

    /// A whole number of days.
    public static func days(_ value: Int) -> String {
        Duration.seconds(Int64(value) * 86400).formatted(
            .units(allowed: [.days], width: .wide)
        )
    }

    /// The placeholder for a value that exists but is not known here.
    public static let unknown = "—"
    /// The placeholder for an event that has not happened.
    public static let never = "—"
}
