import CapsuleDiagnostics
import CapsuleFoundation
import Foundation
import os

/// A deterministic ``TimeSource`` for tests.
public struct FixedTimeSource: TimeSource {
    public let now: Date
    public init(now: Date = Date(timeIntervalSince1970: 0)) { self.now = now }
}

/// A ``DiagnosticsSink`` that records every event it receives, for assertions.
public final class MockDiagnosticsSink: DiagnosticsSink {
    private let storage = OSAllocatedUnfairLock<[DiagnosticEvent]>(initialState: [])
    public init() {}

    public func record(_ event: DiagnosticEvent, at _: Date) {
        storage.withLock { $0.append(event) }
    }

    /// Every event recorded so far, in order.
    public var recorded: [DiagnosticEvent] { storage.withLock { $0 } }
}

/// A settable ``ConsentReading`` for tests, with a `send(_:)` hook to drive
/// runtime consent changes through ``changes()``.
public actor MockConsentReading: ConsentReading {
    private var consent: DiagnosticsConsent
    private var continuation: AsyncStream<DiagnosticsConsent>.Continuation?

    public init(_ consent: DiagnosticsConsent = .privacyDefault) {
        self.consent = consent
    }

    public func current() -> DiagnosticsConsent { consent }

    public nonisolated func changes() -> AsyncStream<DiagnosticsConsent> {
        AsyncStream { continuation in
            Task { await self.attach(continuation) }
        }
    }

    /// Update the value and notify observers.
    public func send(_ consent: DiagnosticsConsent) {
        self.consent = consent
        continuation?.yield(consent)
    }

    /// Replays the latest value on subscribe (CurrentValueSubject semantics) so
    /// observers always converge on the newest consent regardless of timing.
    private func attach(_ continuation: AsyncStream<DiagnosticsConsent>.Continuation) {
        self.continuation = continuation
        continuation.yield(consent)
    }
}

/// A ``LogExcerptReader`` returning a fixed set of (possibly unredacted) entries.
public struct MockLogExcerptReader: LogExcerptReader {
    private let entries: [DiagnosticsBundle.LogEntry]
    public init(_ entries: [DiagnosticsBundle.LogEntry]) { self.entries = entries }

    public func recentEntries(within _: TimeInterval, limit: Int) async -> [DiagnosticsBundle.LogEntry] {
        Array(entries.prefix(limit))
    }
}

/// A ``DiagnosticsExporter`` returning a fixed bundle and recording its inputs.
public actor MockDiagnosticsExporter: DiagnosticsExporter {
    public private(set) var lastBreadcrumbs: [BreadcrumbRing.Breadcrumb] = []
    public private(set) var lastCrash: CrashSummary?
    private let bundle: DiagnosticsBundle

    public init(returning bundle: DiagnosticsBundle) { self.bundle = bundle }

    public func exportBundle(
        metadata _: DeviceMetadata,
        breadcrumbs: [BreadcrumbRing.Breadcrumb],
        crash: CrashSummary?
    ) -> DiagnosticsBundle {
        lastBreadcrumbs = breadcrumbs
        lastCrash = crash
        return bundle
    }
}

/// A ``TelemetryUploader`` that records attempts and can be configured to fail a
/// number of times before succeeding.
public final class MockTelemetryUploader: TelemetryUploader {
    private struct State {
        var attempts: [URL] = []
        var remainingFailures: Int
    }

    private let state: OSAllocatedUnfairLock<State>

    public init(failuresBeforeSuccess: Int = 0) {
        state = OSAllocatedUnfairLock(initialState: State(remainingFailures: failuresBeforeSuccess))
    }

    public func upload(_: DiagnosticsBundle, to endpoint: URL) async throws {
        try state.withLock { state in
            state.attempts.append(endpoint)
            if state.remainingFailures > 0 {
                state.remainingFailures -= 1
                throw UploadError.server(status: 500)
            }
        }
    }

    /// The number of upload attempts made.
    public var uploadCount: Int { state.withLock { $0.attempts.count } }
}

/// A ``MetricsCollecting`` recording start/stop, so consent-gating can be tested
/// without registering a real `MXMetricManager` subscriber.
public final class MockMetricsCollector: MetricsCollecting, @unchecked Sendable {
    private struct Counts {
        var started = false
        var startCount = 0
        var stopCount = 0
    }

    private let state = OSAllocatedUnfairLock(initialState: Counts())
    public init() {}

    public func start() { state.withLock { $0.started = true; $0.startCount += 1 } }
    public func stop() { state.withLock { $0.started = false; $0.stopCount += 1 } }

    public var isStarted: Bool { state.withLock { $0.started } }
    public var startCount: Int { state.withLock { $0.startCount } }
    public var stopCount: Int { state.withLock { $0.stopCount } }
}

/// A ``UploadTransport`` that fails a configurable number of times before
/// returning a success status, recording every send.
public final class MockUploadTransport: UploadTransport {
    private struct State {
        var sends = 0
        var remainingFailures: Int
        var successStatus: Int
    }

    private let state: OSAllocatedUnfairLock<State>

    public init(failuresBeforeSuccess: Int = 0, successStatus: Int = 200) {
        state = OSAllocatedUnfairLock(
            initialState: State(remainingFailures: failuresBeforeSuccess, successStatus: successStatus)
        )
    }

    public func send(_: URLRequest) async throws -> Int {
        try state.withLock { state in
            state.sends += 1
            if state.remainingFailures > 0 {
                state.remainingFailures -= 1
                throw UploadError.server(status: 503)
            }
            return state.successStatus
        }
    }

    /// The number of send attempts made.
    public var sendCount: Int { state.withLock { $0.sends } }
}
