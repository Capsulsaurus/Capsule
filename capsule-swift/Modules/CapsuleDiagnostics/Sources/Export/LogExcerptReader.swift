import CapsuleFoundation
import Foundation
import OSLog

/// Reads recent on-device log entries for inclusion in a report. Abstracted so
/// the exporter is testable without the (environment-dependent) log store.
public protocol LogExcerptReader: Sendable {
    func recentEntries(within interval: TimeInterval, limit: Int) async -> [DiagnosticsBundle.LogEntry]
}

/// Production reader over `OSLogStore`, scoped to the **current process** (no
/// special entitlement needed) and filtered to Capsule's subsystems. Returns raw
/// messages; redaction is applied downstream by the exporter.
public struct OSLogExcerptReader: LogExcerptReader {
    public init() {}

    public func recentEntries(within interval: TimeInterval, limit: Int) async -> [DiagnosticsBundle.LogEntry] {
        do {
            let store = try OSLogStore(scope: .currentProcessIdentifier)
            let since = store.position(date: Date().addingTimeInterval(-interval))
            // Matches both "com.justin13888.capsule" and ".capsule.core".
            let predicate = NSPredicate(format: "subsystem BEGINSWITH %@", CapsuleLog.subsystem)
            let entries = try store.getEntries(at: since, matching: predicate)
            var result: [DiagnosticsBundle.LogEntry] = []
            for case let entry as OSLogEntryLog in entries {
                result.append(
                    DiagnosticsBundle.LogEntry(
                        timestamp: entry.date,
                        category: entry.category,
                        level: Self.label(for: entry.level),
                        message: entry.composedMessage
                    )
                )
            }
            // Keep the most recent `limit` entries.
            return Array(result.suffix(limit))
        } catch {
            CapsuleLog.diagnostics.error(
                "oslog excerpt unavailable: \(error.localizedDescription, privacy: .public)"
            )
            return []
        }
    }

    private static func label(for level: OSLogEntryLog.Level) -> String {
        switch level {
        case .debug: "debug"
        case .info: "info"
        case .notice: "notice"
        case .error: "error"
        case .fault: "fault"
        case .undefined: "undefined"
        @unknown default: "unknown"
        }
    }
}
