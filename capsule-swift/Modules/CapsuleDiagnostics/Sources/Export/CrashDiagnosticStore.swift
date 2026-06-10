import Foundation

/// Persists the most recent crash summary delivered by MetricKit, so it can be
/// attached to the next bug report and drive the "crashed last launch?" prompt.
public actor CrashDiagnosticStore {
    private var latest: CrashSummary?
    private let defaults: UserDefaults
    private static let key = "capsule.diagnostics.lastCrash"

    /// - Parameter suiteName: a `UserDefaults` suite; `nil` uses `.standard`.
    public init(suiteName: String? = nil) {
        let defaults = suiteName.flatMap { UserDefaults(suiteName: $0) } ?? .standard
        self.defaults = defaults
        latest = Self.load(from: defaults)
    }

    /// The most recent crash summary, if any.
    public func lastCrash() -> CrashSummary? { latest }

    /// Persist a freshly delivered crash summary as the latest.
    public func store(_ summary: CrashSummary) {
        latest = summary
        if let data = try? JSONEncoder().encode(summary) {
            defaults.set(data, forKey: Self.key)
        }
    }

    /// Clear the stored crash once the user has acknowledged the prompt.
    public func clear() {
        latest = nil
        defaults.removeObject(forKey: Self.key)
    }

    private static func load(from defaults: UserDefaults) -> CrashSummary? {
        guard let data = defaults.data(forKey: Self.key) else { return nil }
        return try? JSONDecoder().decode(CrashSummary.self, from: data)
    }
}
