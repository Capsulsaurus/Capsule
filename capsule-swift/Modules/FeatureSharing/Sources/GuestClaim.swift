import CapsuleDomain
import Foundation

// MARK: - GuestClaim

/// Sanitisation for the one string an unauthenticated stranger controls
/// (*Web Upload — Drop and Adoption Lifecycle*).
///
/// `suggested_filename` is guest-supplied and unverified: advisory only, never
/// a filesystem path, and never rendered in a way that could pass for app
/// chrome. Three concrete attacks this closes:
///
/// - **Chrome spoofing.** `"Settings — Capsule.png"` next to real chrome reads
///   as a system label. The caller always renders the result in quotes behind
///   an "unverified" marker, so it can only ever read as a quotation.
/// - **Layout takeover.** Newlines, bidi overrides, and other control
///   characters let a name reflow or reverse the row it sits in. They are
///   removed, not escaped.
/// - **Unbounded length.** A ten-thousand-character name pushes every control
///   off screen. The text is truncated.
///
/// Path traversal (`"../../etc/passwd"`) needs no special case here because the
/// value is never used as a path — but it survives sanitisation intentionally,
/// so the owner sees exactly what the guest claimed.
public enum GuestClaim {
    /// The longest name that is rendered. Past this the tail is dropped.
    public static let maximumLength = 64

    /// A display-safe rendering of a guest-asserted string, or `nil` when
    /// nothing renderable remains.
    public static func sanitized(_ claim: String?) -> String? {
        guard let claim else { return nil }
        let allowed = claim.unicodeScalars.filter {
            !CharacterSet.controlCharacters.contains($0) && !bidiControls.contains($0)
        }
        let stripped = String(String.UnicodeScalarView(allowed))
        let collapsed = stripped.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard !collapsed.isEmpty else { return nil }
        guard collapsed.count > maximumLength else { return collapsed }
        return String(collapsed.prefix(maximumLength)) + "\u{2026}"
    }

    /// The sanitised claim wrapped in typographic quotation marks.
    ///
    /// The quotes are structural, not decorative: they are what makes the
    /// string read as *something a stranger typed* rather than as a title this
    /// app chose.
    public static func quoted(_ claim: String?) -> String? {
        guard let text = sanitized(claim) else { return nil }
        return "\u{201C}\(text)\u{201D}"
    }

    /// Directional formatting characters, which can reverse the visual order of
    /// a row and are never legitimate in a filename.
    private static let bidiControls: CharacterSet = {
        var set = CharacterSet()
        set.insert(charactersIn: "\u{200E}" ... "\u{200F}")
        set.insert(charactersIn: "\u{202A}" ... "\u{202E}")
        set.insert(charactersIn: "\u{2066}" ... "\u{2069}")
        return set
    }()
}
