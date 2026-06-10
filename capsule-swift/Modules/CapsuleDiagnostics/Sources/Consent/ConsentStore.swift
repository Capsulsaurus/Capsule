import CapsuleFoundation
import Foundation

/// The writable, persisted store of ``DiagnosticsConsent``.
///
/// Mirrors the app's persisted-store idiom — `actor` + `UserDefaults` + Codable +
/// `AsyncStream` change notifications (see `HiddenStore`). On first launch it
/// returns ``DiagnosticsConsent/privacyDefault`` (local diagnostics on, no
/// uploads).
public actor ConsentStore: ConsentReading {
    private var consent: DiagnosticsConsent
    private var observers: [UUID: AsyncStream<DiagnosticsConsent>.Continuation] = [:]
    private let defaults: UserDefaults
    private static let defaultsKey = "capsule.diagnostics.consent"

    /// - Parameter suiteName: a `UserDefaults` suite to persist into; `nil` uses
    ///   `.standard`. A `Sendable` `String` (rather than a `UserDefaults`) keeps
    ///   the actor boundary clean under strict concurrency.
    public init(suiteName: String? = nil) {
        let defaults = suiteName.flatMap { UserDefaults(suiteName: $0) } ?? .standard
        self.defaults = defaults
        consent = Self.load(from: defaults)
    }

    public func current() -> DiagnosticsConsent { consent }

    /// Apply a mutation, persist it, and notify observers when it changes.
    @discardableResult
    public func update(_ transform: (inout DiagnosticsConsent) -> Void) -> DiagnosticsConsent {
        var updated = consent
        transform(&updated)
        guard updated != consent else { return consent }
        consent = updated
        persist()
        for continuation in observers.values { continuation.yield(updated) }
        return updated
    }

    public nonisolated func changes() -> AsyncStream<DiagnosticsConsent> {
        AsyncStream { continuation in
            let token = UUID()
            Task { await self.register(continuation, token: token) }
            continuation.onTermination = { _ in
                Task { await self.unregister(token) }
            }
        }
    }

    /// Registers an observer and replays the current value immediately, so
    /// subscribers (e.g. the coordinator) converge on live state on subscribe.
    private func register(_ continuation: AsyncStream<DiagnosticsConsent>.Continuation, token: UUID) {
        observers[token] = continuation
        continuation.yield(consent)
    }

    private func unregister(_ token: UUID) {
        observers[token] = nil
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(consent) else { return }
        defaults.set(data, forKey: Self.defaultsKey)
    }

    private static func load(from defaults: UserDefaults) -> DiagnosticsConsent {
        guard let data = defaults.data(forKey: defaultsKey),
              let consent = try? JSONDecoder().decode(DiagnosticsConsent.self, from: data)
        else {
            return .privacyDefault
        }
        return consent
    }
}
