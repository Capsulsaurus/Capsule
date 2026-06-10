import Foundation

/// Scrubs free-form log text of likely-sensitive tokens before it can leave the
/// device: absolute file paths and UUID / long-hex identifiers (asset UUIDs,
/// content hashes, PhotoKit local identifiers).
///
/// This is a defence-in-depth net over the structured event enum, which is
/// already PII-free. It is intentionally aggressive — over-redaction in a
/// diagnostics excerpt is preferable to a leak.
enum Redactor {
    /// The placeholder substituted for any matched token.
    static let placeholder = "‹redacted›"

    private static let patterns: [NSRegularExpression] = {
        let expressions = [
            #"/[A-Za-z0-9._/\-]{3,}"#, // absolute file paths
            #"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}"#, // UUIDs
            #"[0-9a-fA-F]{32,}"#, // long hex (hashes / identifiers)
        ]
        return expressions.compactMap { try? NSRegularExpression(pattern: $0) }
    }()

    static func redact(_ input: String) -> String {
        var output = input
        for regex in patterns {
            let range = NSRange(output.startIndex..., in: output)
            output = regex.stringByReplacingMatches(in: output, options: [], range: range, withTemplate: placeholder)
        }
        return output
    }
}
