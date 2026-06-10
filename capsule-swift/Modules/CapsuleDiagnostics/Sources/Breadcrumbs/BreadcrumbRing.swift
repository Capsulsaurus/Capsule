import CapsuleFoundation
import Foundation

/// A bounded, in-memory ring buffer of recent diagnostic breadcrumbs.
///
/// Conforms to ``DiagnosticsSink`` so it can be installed on ``Diagnostics``.
/// The most recent `capacity` events are retained to give a crash or bug report
/// temporal context. Stores only the PII-free event name + a low-cardinality
/// detail + timestamp — never raw event payloads.
public actor BreadcrumbRing {
    /// A single recorded breadcrumb.
    public struct Breadcrumb: Codable, Sendable, Equatable {
        public let name: String
        public let detail: String
        public let timestamp: Date

        public init(name: String, detail: String, timestamp: Date) {
            self.name = name
            self.detail = detail
            self.timestamp = timestamp
        }
    }

    private var entries: [Breadcrumb] = []
    private let capacity: Int

    public init(capacity: Int = 50) {
        self.capacity = max(1, capacity)
    }

    /// The retained breadcrumbs, oldest first.
    public func snapshot() -> [Breadcrumb] { entries }

    func append(_ event: DiagnosticEvent, at time: Date) {
        entries.append(Breadcrumb(name: event.name, detail: Self.detail(for: event), timestamp: time))
        if entries.count > capacity {
            entries.removeFirst(entries.count - capacity)
        }
    }

    /// A low-cardinality, PII-free detail string for the event.
    private static func detail(for event: DiagnosticEvent) -> String {
        switch event {
        case let .appLaunched(coldStart): "coldStart=\(coldStart)"
        case .enteredBackground, .memoryWarning: ""
        case let .photoPermission(granted): "granted=\(granted)"
        case let .operationFailed(operation): operation.rawValue
        case let .metricKitDiagnostic(kind, count): "\(kind.rawValue)×\(count)"
        }
    }
}

extension BreadcrumbRing: DiagnosticsSink {
    /// Records asynchronously: the sink contract is synchronous, so we hop onto
    /// the actor without blocking the caller.
    public nonisolated func record(_ event: DiagnosticEvent, at time: Date) {
        Task { await append(event, at: time) }
    }
}
