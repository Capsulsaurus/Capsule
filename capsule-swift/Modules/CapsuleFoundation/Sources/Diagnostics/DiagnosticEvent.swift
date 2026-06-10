import Foundation

/// A privacy-safe diagnostic signal — counts and categories only, never photo
/// content, asset identifiers, file paths, or free-form text.
///
/// Events feed two destinations: Apple unified logging (always, on-device) and,
/// when the user has opted in, a bounded breadcrumb ring attached to bug
/// reports. Keeping the payloads to a closed enum makes accidental PII leakage
/// a compile error rather than a review catch.
public enum DiagnosticEvent: Sendable, Equatable {
    /// The app finished launching. `coldStart` is `false` for warm resumes.
    case appLaunched(coldStart: Bool)
    /// The app moved to the background — used to mark a clean shutdown.
    case enteredBackground
    /// The system delivered a low-memory warning.
    case memoryWarning
    /// The user granted or denied photo-library access.
    case photoPermission(granted: Bool)
    /// A non-fatal failure in a named operation. Carries no error text.
    case operationFailed(operation: DiagnosticOp)
    /// A summarised MetricKit diagnostic (crash / hang / CPU / disk-write).
    case metricKitDiagnostic(kind: MetricDiagnosticKind, count: Int)

    /// A stable, low-cardinality identifier, safe to log and serialise.
    public var name: String {
        switch self {
        case .appLaunched: "app_launched"
        case .enteredBackground: "entered_background"
        case .memoryWarning: "memory_warning"
        case .photoPermission: "photo_permission"
        case .operationFailed: "operation_failed"
        case .metricKitDiagnostic: "metrickit_diagnostic"
        }
    }
}

/// The closed set of operations that can report a non-fatal failure.
public enum DiagnosticOp: String, Sendable, CaseIterable, Equatable, Codable {
    case timelineLoad
    case importRun
    case delete
    case share
    case albumCreate
    case search
    case viewerLoad
}

/// The category of an Apple MetricKit diagnostic payload.
public enum MetricDiagnosticKind: String, Sendable, CaseIterable, Equatable, Codable {
    case crash
    case hang
    case cpuException
    case diskWriteException
}
