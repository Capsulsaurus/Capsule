import Foundation

// MARK: - RedactedSecret

/// A string the app is allowed to *show* and never allowed to *keep*.
///
/// The recovery passphrase, a Shamir share, a TOTP seed, a safety code, and an
/// enrollment payload are all in this class. *Backup & Recovery — Local
/// Verification* is explicit that the verification prompt exists to check the
/// **user** still holds the secret; a client that persisted it — or that let it
/// leak into a log, a crash report, or an analytics payload — would auto-pass a
/// check whose entire purpose is that it cannot be auto-passed. The type
/// therefore makes the wrong thing hard rather than trusting a code review:
///
/// - `description` and `debugDescription` are `"<redacted>"`, so string
///   interpolation into an `OSLog` message, an `#expect` failure, or a
///   `print` cannot spill it.
/// - It is deliberately **not** `Codable`, so it cannot be written to
///   `UserDefaults`, a JSON body, a plist, or a `@AppStorage` slot. The
///   compiler refuses; there is nothing to remember.
/// - It is deliberately **not** `Equatable`, so a secret cannot be compared —
///   and therefore brute-forced a character at a time — by accident. The one
///   sanctioned comparison is ``matches(_:atWordIndex:)``, which is what the
///   type-back gate needs and nothing more.
/// - Reading the plaintext is spelled ``reveal()``, so `grep reveal(` is a
///   complete audit of every place the secret becomes an ordinary `String`.
///
/// It is a `struct` holding a `String` rather than locked memory: Swift strings
/// cannot be reliably zeroed, and pretending otherwise would be theatre. The
/// honest guarantee is *no persistence and no diagnostics*, which is what the
/// design docs actually require of the client.
public struct RedactedSecret: Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    private let value: String

    public init(_ value: String) {
        self.value = value
    }

    /// The plaintext. Every call site is a place a secret escapes the type, so
    /// the name is deliberately searchable.
    public func reveal() -> String {
        value
    }

    public var description: String { "<redacted>" }
    public var debugDescription: String { "<redacted>" }

    /// How many characters the secret has, for a length hint that leaks nothing.
    public var characterCount: Int { value.count }

    /// The secret split on its separators, for a passphrase rendered as a grid
    /// of numbered words.
    ///
    /// Both `-` and a space are treated as separators: the generator's exact
    /// spelling is not this layer's contract, and a phrase that arrived
    /// space-separated must still render as words rather than as one long line.
    public var words: [String] {
        value
            .split(whereSeparator: { $0 == "-" || $0 == " " })
            .map(String.init)
    }

    /// Whether `candidate` is the word at `index`, compared the way a human
    /// typing it should be compared: case-folded, whitespace-trimmed.
    ///
    /// The only sanctioned comparison on a secret. It answers one bit about one
    /// word — which is exactly what the type-back gate in *Device Enrollment —
    /// First-Device Enrollment* step 6 needs, and never more.
    public func matches(_ candidate: String, atWordIndex index: Int) -> Bool {
        let words = words
        guard words.indices.contains(index) else { return false }
        let normalized = candidate
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
        return normalized == words[index].lowercased()
    }
}
