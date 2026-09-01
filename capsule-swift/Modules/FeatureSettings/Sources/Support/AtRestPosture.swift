import CapsuleFoundation
import Foundation

// MARK: - AtRestPosture

/// One row of the at-rest posture table, for the platform actually running.
///
/// *Local Gallery — SR2* tabulates where the library lives and what protects it
/// per platform, and the two rows this app can be on say materially different
/// things:
///
/// - iOS and iPadOS: an app-private container, where "the sandbox denies other
///   apps" and file-level Data Protection covers it at rest.
/// - macOS: an ordinary user-directory library, where the protection is "OS
///   user permissions only — any process of the same user can read it", and
///   full-disk encryption is the at-rest story.
///
/// The screen shows **one** row — the running platform's — rather than the
/// whole table, because a Mac user reading the iOS row will come away believing
/// a guarantee their machine does not make. The doc's own words on the desktop
/// row are "we do not pretend otherwise", and a settings screen that printed
/// both rows and let the user work out which applied would be pretending.
public struct AtRestPosture: Sendable, Equatable, Hashable {
    /// Where the library lives.
    public let storeKey: String
    /// What that protects it from, stated without euphemism.
    public let protectionKey: String
    /// The one-sentence summary shown as the section footer.
    public let summaryKey: String
    /// Reinforcement for the summary, never the signal itself.
    public let tone: SettingsTone

    /// The posture for a platform, selected by the only fact that distinguishes
    /// the two rows: whether the library sits in a sandboxed container or in
    /// the user's own directory.
    ///
    /// A parameter rather than a `#if` so both rows are reachable from a test
    /// on either platform — "the copy differs by platform" is exactly the sort
    /// of claim that rots silently when only one branch is ever executed.
    public static func forPlatform(isSandboxPrivate: Bool) -> AtRestPosture {
        isSandboxPrivate ? sandboxPrivate : userDirectory
    }

    /// The posture of the platform this build is running on.
    public static var current: AtRestPosture {
        forPlatform(isSandboxPrivate: PlatformEnvironment.libraryIsSandboxPrivate)
    }

    /// iOS and iPadOS.
    public static let sandboxPrivate = AtRestPosture(
        storeKey: "app.settings.security.atrest.sandbox.store",
        protectionKey: "app.settings.security.atrest.sandbox.protection",
        summaryKey: "app.settings.security.atrest.sandbox.summary",
        tone: .positive
    )

    /// macOS.
    public static let userDirectory = AtRestPosture(
        storeKey: "app.settings.security.atrest.userdir.store",
        protectionKey: "app.settings.security.atrest.userdir.protection",
        summaryKey: "app.settings.security.atrest.userdir.summary",
        tone: .caution
    )
}
