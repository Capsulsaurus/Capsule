import Foundation

/// Generates time-ordered UUIDv7 identifiers (RFC 9562).
///
/// Capsule keys managed assets by UUIDv7 rather than the random UUIDv4 so the
/// identifiers sort chronologically — catalog primary keys, on-disk filenames,
/// and import order all stay naturally time-ordered.
public enum UUIDv7 {
    /// A fresh UUIDv7 for `date`, formatted as a lowercase hyphenated string.
    public static func string(date: Date = Date()) -> String {
        var bytes = [UInt8](repeating: 0, count: 16)

        // Bytes 0–5: 48-bit Unix timestamp in milliseconds, big-endian.
        let milliseconds = UInt64((date.timeIntervalSince1970 * 1000).rounded())
        bytes[0] = UInt8((milliseconds >> 40) & 0xFF)
        bytes[1] = UInt8((milliseconds >> 32) & 0xFF)
        bytes[2] = UInt8((milliseconds >> 24) & 0xFF)
        bytes[3] = UInt8((milliseconds >> 16) & 0xFF)
        bytes[4] = UInt8((milliseconds >> 8) & 0xFF)
        bytes[5] = UInt8(milliseconds & 0xFF)

        // Bytes 6–15: random, then stamped with the version and variant fields.
        for index in 6 ..< 16 {
            bytes[index] = UInt8.random(in: 0 ... 255)
        }
        bytes[6] = (bytes[6] & 0x0F) | 0x70 // version 7
        bytes[8] = (bytes[8] & 0x3F) | 0x80 // RFC 4122 variant

        let identifier = UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11],
            bytes[12], bytes[13], bytes[14], bytes[15]
        ))
        return identifier.uuidString.lowercased()
    }
}
