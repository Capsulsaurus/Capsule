import Foundation

// MARK: - ChunkedCodeFormatter

/// Renders a code as fixed-length, evenly chunked groups.
///
/// One formatter, used by every surface where a human compares two strings — the
/// server signing-key fingerprint on the connect screen, the safety code on both
/// devices during a cross-device add, and each device's key fingerprint beside
/// it. *Device Enrollment — Safety-code check* requires both devices to show the
/// code "in the same chunked, fixed-length format"; the way to guarantee that is
/// for there to be exactly one function that can produce it, rather than two
/// screens that happen to agree today.
///
/// The chunking is not decoration. An unbroken 32-character hex string is
/// compared by a human roughly as well as a coin flip; four-character groups are
/// what make a single transposed character visible.
public enum ChunkedCodeFormatter {
    /// The group size every Capsule code is displayed in.
    public static let groupSize = 4

    /// The separator between groups. A thin space would look better and compare
    /// worse — a wide, unambiguous gap is the point.
    public static let separator = " "

    /// Chunk a code into fixed-length groups.
    ///
    /// The input is upper-cased and stripped of any grouping it arrived with, so
    /// the same value rendered from two sources — a server's JSON, a QR payload,
    /// a transcript digest — produces byte-identical output. A formatter that
    /// preserved incoming separators would let two devices show the same code
    /// differently, which is exactly the failure this defends against.
    public static func chunked(_ code: String, groupSize: Int = groupSize) -> String {
        guard groupSize > 0 else { return code }
        let normalized = code
            .uppercased()
            .filter { !$0.isWhitespace && $0 != "-" }
        var groups: [String] = []
        var current = ""
        for character in normalized {
            current.append(character)
            if current.count == groupSize {
                groups.append(current)
                current = ""
            }
        }
        if !current.isEmpty { groups.append(current) }
        return groups.joined(separator: separator)
    }

    /// Chunk a code and truncate it to a fixed number of groups, for a short
    /// fingerprint shown beside a device name.
    public static func shortened(_ code: String, groups: Int) -> String {
        let chunks = chunked(code).split(separator: separator).prefix(max(0, groups))
        return chunks.joined(separator: separator)
    }
}
