import Foundation
import os

/// The process-wide entry point features use to record diagnostics.
///
/// `record` fans an event out to every installed ``DiagnosticsSink`` under a
/// lightweight unfair lock; it is safe to call from any isolation domain and
/// never blocks on IO. Callers already importing `CapsuleFoundation` reach it
/// via ``shared`` with no extra dependency — mirroring how ``CapsuleLog`` is
/// used. The default install is empty; the app installs the on-device OSLog
/// sink at launch and further sinks (breadcrumbs, upload) once consent resolves.
public final class Diagnostics: Sendable {
    /// The shared instance used across the app.
    public static let shared = Diagnostics()

    private let sinks = OSAllocatedUnfairLock<[any DiagnosticsSink]>(initialState: [])
    private let time: any TimeSource

    public init(time: any TimeSource = SystemTimeSource()) {
        self.time = time
    }

    /// Add a sink to the fan-out set.
    public func install(_ sink: any DiagnosticsSink) {
        sinks.withLock { $0.append(sink) }
    }

    /// Remove every installed sink (e.g. when the user revokes consent).
    public func removeAll() {
        sinks.withLock { $0.removeAll() }
    }

    /// Record a diagnostic event, fanning it out to every installed sink.
    ///
    /// The sink list is copied out of the lock before delivery so a sink's
    /// `record` never runs while the lock is held.
    public func record(_ event: DiagnosticEvent) {
        let now = time.now
        let current = sinks.withLock { $0 }
        for sink in current {
            sink.record(event, at: now)
        }
    }

    /// Convenience for the common non-fatal-failure case. Records only the
    /// operation category — never the underlying error's text — so callers keep
    /// logging the error itself through ``CapsuleLog`` alongside this call.
    public func recordError(operation: DiagnosticOp) {
        record(.operationFailed(operation: operation))
    }
}
