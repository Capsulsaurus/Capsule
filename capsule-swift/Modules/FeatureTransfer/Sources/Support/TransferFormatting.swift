import CapsuleDomain
import Foundation

// MARK: - TransferFormat

/// Number, byte, and instant rendering shared by every screen in this module.
///
/// Two rules are enforced here rather than per call site:
///
/// - **No filename ever appears.** No filename crosses the wire by design
///   (*Upload Protocol — What Gets Uploaded*: the manifest carries no path), so
///   a transfer row identifies its asset by capture date. There is deliberately
///   no `filename(_:)` on this type to reach for.
/// - **Units come from the catalog, values from the locale.** A rate is
///   composed from a translatable template (`ios.transfer.unit.per_second`) and
///   a locale-formatted byte count, never from a hardcoded `"/s"`.
public enum TransferFormat {
    /// A byte count in the viewer's locale and unit conventions.
    public static func bytes(_ value: UInt64) -> String {
        Int64(clamping: value).formatted(.byteCount(style: .file))
    }

    /// A transfer rate, as "<bytes>/s" in whatever form the catalog gives.
    public static func rate(bytesPerSecond: Double) -> String {
        let magnitude = bytes(UInt64(max(0, bytesPerSecond)))
        return String(format: String(localized: "ios.transfer.unit.per_second"), magnitude)
    }

    /// A 0…1 fraction as a percentage.
    public static func percent(_ fraction: Double) -> String {
        min(max(fraction, 0), 1).formatted(.percent.precision(.fractionLength(0)))
    }

    /// A plain integer in the viewer's locale.
    public static func count(_ value: Int) -> String {
        value.formatted(.number)
    }

    /// An asset's capture date — the only identity a transfer row shows.
    public static func captureDate(_ instant: CapsuleTimestamp) -> String {
        instant.date.formatted(date: .abbreviated, time: .shortened)
    }

    /// An instant relative to `now` ("3 days ago"), for last-sync and
    /// detected-at stamps.
    /// The formatter is built per call rather than cached in a `static`: it is a
    /// mutable reference type that must not be shared across concurrency
    /// domains, and this runs on a screen refresh, never in a scroll loop —
    /// the same trade-off `CapsuleTimestamp` makes for RFC 3339 parsing.
    public static func relative(_ instant: CapsuleTimestamp, now: CapsuleTimestamp) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter.localizedString(for: instant.date, relativeTo: now.date)
    }

    /// A content address, abbreviated for display. The full value belongs in a
    /// diagnostic export, not on a phone screen.
    public static func shortDigest(_ text: String, keeping: Int = 12) -> String {
        text.count <= keeping ? text : String(text.prefix(keeping)) + "…"
    }

    /// A key fingerprint rendered as lowercase hex.
    public static func fingerprint(_ data: Data, keeping: Int = 12) -> String {
        shortDigest(data.map { String(format: "%02x", $0) }.joined(), keeping: keeping)
    }
}
