import SwiftUI

// The two places the identity screens genuinely differ across destinations.
//
// Both live here rather than as an inline `#if os(...)` in a view body, which is
// what `.swiftlint.yml` asks for: the difference stays named, reviewable, and in
// one file instead of being rediscovered on every new screen.

extension View {
    /// Configure a field that accepts digits only — a TOTP code, an enrollment
    /// fallback code.
    ///
    /// The number pad and the submit label are software-keyboard affordances, so
    /// they do not exist on macOS, where the same field is typed into with a
    /// hardware keyboard. The *validation* is deliberately not here: a keyboard
    /// is an affordance, never a constraint, and every one of these fields
    /// filters its own input regardless of how the characters arrived.
    func authNumericField() -> some View {
        #if os(iOS)
            autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .keyboardType(.numberPad)
                .submitLabel(.done)
        #else
            autocorrectionDisabled()
        #endif
    }

    /// Size a sheet that hosts a confirmation ceremony.
    ///
    /// iPhone and iPad sheets size themselves against the screen and the
    /// detents; a Mac sheet has no such anchor and opens at whatever its content
    /// happens to measure, which for a short form is a strip too small to read
    /// the consequences in.
    func authSheetFrame() -> some View {
        #if os(macOS)
            frame(minWidth: 460, minHeight: 360)
        #else
            self
        #endif
    }
}
