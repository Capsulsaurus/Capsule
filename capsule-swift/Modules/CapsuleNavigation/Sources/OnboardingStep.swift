import Foundation

// MARK: - OnboardingStep

/// The closed, ordered set of first-run steps.
///
/// Ordered because onboarding is the one flow where "back" means the previous
/// *step*, not the previous route: ``previous`` and ``next`` are what the flow
/// advances on, and a step's position is what a progress indicator renders.
/// Modelling the order here rather than in the view keeps the three shells —
/// which present this as a sheet, a full-screen cover, and a modal window
/// respectively — agreeing on what step 3 of 7 is.
public enum OnboardingStep: String, Sendable, Hashable, Codable, CaseIterable {
    /// What Capsule is, before asking for anything.
    case welcome
    /// Which server this library lives on.
    case server
    /// Sign in or create an account.
    case signIn = "sign_in"
    /// Generate or enter the recovery phrase.
    case recovery
    /// Enrol this device and mint its keys.
    case enrollment
    /// Ask for photo-library access, with the reason first.
    case photoAccess = "photo_access"
    /// Choose what gets backed up.
    case backupScope = "backup_scope"
    /// Confirmation, and the hand-off into the library.
    case finish
}

public extension OnboardingStep {
    /// The catalog key for this step's title. Derived, for the same reason as
    /// ``SettingsSection/titleKey``.
    var titleKey: String { "app.onboarding.step.\(rawValue)" }

    /// Zero-based position, for "step N of M" progress.
    var index: Int { Self.allCases.firstIndex(of: self) ?? 0 }

    /// The next step, or `nil` at the end of the flow.
    var next: OnboardingStep? { Self.allCases[safe: index + 1] }

    /// The previous step, or `nil` at the start of the flow.
    var previous: OnboardingStep? { Self.allCases[safe: index - 1] }
}

private extension Array {
    /// Bounds-checked access, so stepping off either end of the flow is `nil`
    /// rather than a trap — `force_unwrapping` and index traps are the same
    /// class of bug and neither belongs in a navigation model.
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
