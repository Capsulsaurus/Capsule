import CapsuleFoundation
import Foundation

/// A redacted, PII-free summary of a MetricKit diagnostic.
///
/// Holds only numeric exception/signal codes and coarse environment facts
/// produced by the OS — never call-stack symbols tied to user data, file paths,
/// or content. Persisted so it can drive the "crashed last launch?" prompt and
/// be attached to the next bug report.
public struct CrashSummary: Codable, Sendable, Equatable {
    public let kind: MetricDiagnosticKind
    public let exceptionType: Int?
    public let exceptionCode: Int?
    public let signal: Int?
    public let terminationReason: String?
    public let osVersion: String?
    public let appBuild: String?
    public let timestamp: Date

    public init(
        kind: MetricDiagnosticKind,
        exceptionType: Int? = nil,
        exceptionCode: Int? = nil,
        signal: Int? = nil,
        terminationReason: String? = nil,
        osVersion: String? = nil,
        appBuild: String? = nil,
        timestamp: Date
    ) {
        self.kind = kind
        self.exceptionType = exceptionType
        self.exceptionCode = exceptionCode
        self.signal = signal
        self.terminationReason = terminationReason
        self.osVersion = osVersion
        self.appBuild = appBuild
        self.timestamp = timestamp
    }
}
