import Foundation

/// A ``DiagnosticsSink`` that writes each event to Apple unified logging under
/// ``CapsuleLog/diagnostics``.
///
/// Always installed: the data stays in the on-device unified log (queryable via
/// Console.app or `log show`) and never leaves the device, so it is independent
/// of upload consent. All interpolations are `.public` because the event enum
/// is PII-free by construction.
public struct OSLogDiagnosticsSink: DiagnosticsSink {
    public init() {}

    public func record(_ event: DiagnosticEvent, at _: Date) {
        switch event {
        case let .appLaunched(coldStart):
            CapsuleLog.diagnostics.info("app launched (coldStart: \(coldStart, privacy: .public))")
        case .enteredBackground:
            CapsuleLog.diagnostics.debug("entered background")
        case .memoryWarning:
            CapsuleLog.diagnostics.notice("memory warning")
        case let .photoPermission(granted):
            CapsuleLog.diagnostics.info("photo permission (granted: \(granted, privacy: .public))")
        case let .operationFailed(operation):
            CapsuleLog.diagnostics.error("operation failed: \(operation.rawValue, privacy: .public)")
        case let .metricKitDiagnostic(kind, count):
            CapsuleLog.diagnostics.error(
                "metrickit diagnostic: \(kind.rawValue, privacy: .public) ×\(count, privacy: .public)"
            )
        }
    }
}
