import Foundation

/// A redacted, self-contained diagnostics report a user can share via the system
/// share sheet or — when opted in — upload to a self-hosted endpoint.
///
/// Everything in here is PII-free by construction: the metadata carries no
/// identifiers, breadcrumbs and the crash summary are typed/numeric, and the log
/// excerpt is run through ``Redactor`` before assembly.
public struct DiagnosticsBundle: Codable, Sendable, Equatable {
    /// A single redacted log line.
    public struct LogEntry: Codable, Sendable, Equatable {
        public let timestamp: Date
        public let category: String
        public let level: String
        public let message: String

        public init(timestamp: Date, category: String, level: String, message: String) {
            self.timestamp = timestamp
            self.category = category
            self.level = level
            self.message = message
        }
    }

    public let createdAt: Date
    public let metadata: DeviceMetadata
    public let breadcrumbs: [BreadcrumbRing.Breadcrumb]
    public let crash: CrashSummary?
    public let logExcerpt: [LogEntry]

    public init(
        createdAt: Date,
        metadata: DeviceMetadata,
        breadcrumbs: [BreadcrumbRing.Breadcrumb],
        crash: CrashSummary?,
        logExcerpt: [LogEntry]
    ) {
        self.createdAt = createdAt
        self.metadata = metadata
        self.breadcrumbs = breadcrumbs
        self.crash = crash
        self.logExcerpt = logExcerpt
    }

    /// Serialise to stable, pretty-printed JSON for the share sheet or upload body.
    public func jsonData() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        encoder.dateEncodingStrategy = .iso8601
        return try encoder.encode(self)
    }
}
