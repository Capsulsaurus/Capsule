import CapsuleFoundation
import Foundation

/// Assembles a redacted ``DiagnosticsBundle`` from the app's diagnostic state.
public protocol DiagnosticsExporter: Sendable {
    func exportBundle(
        metadata: DeviceMetadata,
        breadcrumbs: [BreadcrumbRing.Breadcrumb],
        crash: CrashSummary?
    ) async -> DiagnosticsBundle
}

/// The production exporter. Reads a recent log excerpt via an injected
/// ``LogExcerptReader``, **redacts every log message** (defence in depth over
/// the already-PII-free structured data), and packages everything into a bundle.
public struct DefaultDiagnosticsExporter: DiagnosticsExporter {
    private let logReader: any LogExcerptReader
    private let time: any TimeSource
    private let window: TimeInterval
    private let logLimit: Int

    public init(
        logReader: any LogExcerptReader = OSLogExcerptReader(),
        time: any TimeSource = SystemTimeSource(),
        window: TimeInterval = 600,
        logLimit: Int = 500
    ) {
        self.logReader = logReader
        self.time = time
        self.window = window
        self.logLimit = logLimit
    }

    public func exportBundle(
        metadata: DeviceMetadata,
        breadcrumbs: [BreadcrumbRing.Breadcrumb],
        crash: CrashSummary?
    ) async -> DiagnosticsBundle {
        let raw = await logReader.recentEntries(within: window, limit: logLimit)
        let redacted = raw.map { entry in
            DiagnosticsBundle.LogEntry(
                timestamp: entry.timestamp,
                category: entry.category,
                level: entry.level,
                message: Redactor.redact(entry.message)
            )
        }
        return DiagnosticsBundle(
            createdAt: time.now,
            metadata: metadata,
            breadcrumbs: breadcrumbs,
            crash: crash,
            logExcerpt: redacted
        )
    }
}
