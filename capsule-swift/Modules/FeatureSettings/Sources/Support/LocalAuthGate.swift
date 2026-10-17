import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - GatedLibraryView

/// The views behind the fresh-local-authentication gate.
///
/// Exactly two, per *Local Gallery — SR1*: "Opening the Recently Deleted
/// (trash) view or the Hidden view requires fresh local authentication."
/// A closed enum rather than a string key so a third gated view is a compile
/// error somewhere rather than a forgotten row in a grace-window table.
public enum GatedLibraryView: String, Sendable, Hashable, CaseIterable, Codable {
    case recentlyDeleted = "recently_deleted"
    case hidden

    /// The catalog key for this view's name.
    public var titleKey: String { "app.settings.security.gate.\(rawValue)" }
}

// MARK: - LocalAuthGate

/// The per-view grace window on the fresh-local-auth gate.
///
/// *Local Gallery — SR1*: "One grant covers a short grace window (default 5
/// minutes, per-view), after which re-auth is required." **Per-view** is the
/// load-bearing word and the reason this holds a dictionary rather than a
/// single instant: unlocking Hidden must not also unlock Recently Deleted, and
/// a shared timestamp would silently make it do exactly that.
///
/// What this is not, in the doc's own words: "view-time UX protection against a
/// borrowed-unlocked-phone snoop; it is not a cryptographic boundary". The
/// Security screen says so on screen, because a user who believes this is
/// encryption will make worse decisions about where their Mac library lives.
@MainActor
@Observable
public final class LocalAuthGate {
    /// The documented default window, in seconds.
    public static let graceWindowSeconds: Int64 = 300

    private var grantedAt: [GatedLibraryView: CapsuleTimestamp] = [:]
    private let windowSeconds: Int64

    public init(windowSeconds: Int64 = LocalAuthGate.graceWindowSeconds) {
        self.windowSeconds = windowSeconds
    }

    /// The configured window, in seconds — what the screen displays, so a
    /// deployment that shortened it is not describing five minutes.
    public var graceWindowSeconds: Int64 { windowSeconds }

    /// Record a successful authentication for one view.
    public func grant(_ view: GatedLibraryView, at now: CapsuleTimestamp) {
        grantedAt[view] = now
    }

    /// Drop one view's grant — the "lock now" affordance.
    public func revoke(_ view: GatedLibraryView) {
        grantedAt[view] = nil
    }

    /// Drop every grant. What backgrounding the app and an explicit lock both
    /// do.
    public func revokeAll() {
        grantedAt.removeAll()
    }

    /// Whether the view would open right now without a fresh challenge.
    ///
    /// The boundary is exclusive: a grant is spent exactly at the window's end,
    /// so a five-minute window grants for 300 seconds and not 301.
    public func isUnlocked(_ view: GatedLibraryView, at now: CapsuleTimestamp) -> Bool {
        remainingSeconds(view, at: now) > 0
    }

    /// Seconds of grace left, floored at zero.
    public func remainingSeconds(_ view: GatedLibraryView, at now: CapsuleTimestamp) -> Int64 {
        guard let granted = grantedAt[view] else { return 0 }
        let elapsed = now.epochSeconds - granted.epochSeconds
        guard elapsed >= 0 else { return windowSeconds }
        return max(0, windowSeconds - elapsed)
    }

    /// When one view's grace expires, or `nil` if it holds no grant.
    public func expiresAt(_ view: GatedLibraryView) -> CapsuleTimestamp? {
        guard let granted = grantedAt[view] else { return nil }
        return CapsuleTimestamp(epochSeconds: granted.epochSeconds + windowSeconds)
    }
}

// MARK: - Drawing a method

public extension LocalAuthMethod {
    /// The tone the row is drawn in. Reinforcement for the text, never a
    /// substitute for it.
    ///
    /// An extension here rather than a property on the port: which
    /// authenticator a device has is a fact about the device, and what colour
    /// to paint it is a fact about this screen.
    var tone: SettingsTone {
        self == .unavailable ? .caution : .positive
    }
}
