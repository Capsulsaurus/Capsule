import Foundation

/// Tracks whether the previous app session shut down cleanly.
///
/// This is deliberately **not** a crash handler: capturing a crash in-process
/// requires async-signal-unsafe work and is fragile. Instead a clean-shutdown
/// sentinel is written when the app backgrounds and cleared while running; its
/// absence on the next launch indicates the last session ended abnormally. The
/// authoritative crash details still come from MetricKit's `MXCrashDiagnostic` —
/// this is only a fast, coarse signal.
public actor LastWordsStore {
    private let defaults: UserDefaults
    private static let cleanKey = "capsule.diagnostics.cleanShutdown"
    private static let activeKey = "capsule.diagnostics.sessionActive"

    /// Whether the previous session terminated without a clean shutdown.
    /// `nonisolated` so callers can read this immutable snapshot synchronously.
    public nonisolated let previousSessionEndedAbnormally: Bool

    /// - Parameter suiteName: a `UserDefaults` suite; `nil` uses `.standard`.
    public init(suiteName: String? = nil) {
        let defaults = suiteName.flatMap { UserDefaults(suiteName: $0) } ?? .standard
        self.defaults = defaults
        // A session that started but never marked a clean shutdown ended abnormally.
        let started = defaults.bool(forKey: Self.activeKey)
        let clean = defaults.bool(forKey: Self.cleanKey)
        previousSessionEndedAbnormally = started && !clean
    }

    /// Mark the current session active and not-yet-clean. Call at launch and
    /// whenever the app returns to the foreground.
    public func beginSession() {
        defaults.set(true, forKey: Self.activeKey)
        defaults.set(false, forKey: Self.cleanKey)
    }

    /// Record a clean shutdown. Call when the app enters the background.
    public func markCleanShutdown() {
        defaults.set(true, forKey: Self.cleanKey)
    }
}
