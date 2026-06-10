import CapsuleFoundation
import Foundation
import MetricKit

/// Subscribes to Apple's MetricKit to receive crash, hang, CPU and disk-write
/// diagnostics plus daily performance metrics — the privacy-safe, OS-symbolicated
/// path to crash reporting, with no in-process (async-signal-unsafe) handler.
///
/// MetricKit delivers payloads on its own queue; this subscriber holds only
/// immutable references and hops every mutation onto the breadcrumb / crash
/// actors, so it is safe to mark `@unchecked Sendable`.
final class MetricKitSubscriber: NSObject, MXMetricManagerSubscriber, MetricsCollecting, @unchecked Sendable {
    private let diagnostics: Diagnostics
    private let crashStore: CrashDiagnosticStore
    private let time: any TimeSource

    init(diagnostics: Diagnostics, crashStore: CrashDiagnosticStore, time: any TimeSource) {
        self.diagnostics = diagnostics
        self.crashStore = crashStore
        self.time = time
    }

    func start() { MXMetricManager.shared.add(self) }
    func stop() { MXMetricManager.shared.remove(self) }

    // MARK: MXMetricManagerSubscriber

    func didReceive(_ payloads: [MXMetricPayload]) {
        guard !payloads.isEmpty else { return }
        CapsuleLog.diagnostics.info("metrickit metric payloads received: \(payloads.count, privacy: .public)")
    }

    func didReceive(_ payloads: [MXDiagnosticPayload]) {
        for payload in payloads { handle(payload) }
    }

    private func handle(_ payload: MXDiagnosticPayload) {
        if let crashes = payload.crashDiagnostics, !crashes.isEmpty {
            let now = time.now
            for crash in crashes {
                let summary = CrashSummary(from: crash, at: now)
                Task { await crashStore.store(summary) }
            }
            diagnostics.record(.metricKitDiagnostic(kind: .crash, count: crashes.count))
        }
        if let hangs = payload.hangDiagnostics, !hangs.isEmpty {
            diagnostics.record(.metricKitDiagnostic(kind: .hang, count: hangs.count))
        }
        if let cpu = payload.cpuExceptionDiagnostics, !cpu.isEmpty {
            diagnostics.record(.metricKitDiagnostic(kind: .cpuException, count: cpu.count))
        }
        if let disk = payload.diskWriteExceptionDiagnostics, !disk.isEmpty {
            diagnostics.record(.metricKitDiagnostic(kind: .diskWriteException, count: disk.count))
        }
    }
}
