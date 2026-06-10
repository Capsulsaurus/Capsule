import Foundation
import os
import Testing

@testable import CapsuleFoundation

@Suite("Diagnostics emit API")
struct DiagnosticsEmitTests {
    /// A `Sendable` sink that records every event for assertions.
    final class RecordingSink: DiagnosticsSink {
        private let entries = OSAllocatedUnfairLock<[(DiagnosticEvent, Date)]>(initialState: [])

        func record(_ event: DiagnosticEvent, at time: Date) {
            entries.withLock { $0.append((event, time)) }
        }

        var events: [DiagnosticEvent] { entries.withLock { $0.map(\.0) } }
        var times: [Date] { entries.withLock { $0.map(\.1) } }
    }

    /// A deterministic clock for time-stamp assertions.
    struct StubClock: TimeSource {
        let now: Date
    }

    @Test("record fans out to installed sinks with the injected time")
    func recordFansOut() {
        let fixed = Date(timeIntervalSince1970: 1000)
        let diagnostics = Diagnostics(time: StubClock(now: fixed))
        let sink = RecordingSink()
        diagnostics.install(sink)

        diagnostics.record(.memoryWarning)
        diagnostics.recordError(operation: .timelineLoad)

        #expect(sink.events == [.memoryWarning, .operationFailed(operation: .timelineLoad)])
        #expect(sink.times == [fixed, fixed])
    }

    @Test("every installed sink receives the event")
    func multipleSinks() {
        let diagnostics = Diagnostics(time: StubClock(now: .init(timeIntervalSince1970: 0)))
        let first = RecordingSink()
        let second = RecordingSink()
        diagnostics.install(first)
        diagnostics.install(second)

        diagnostics.record(.appLaunched(coldStart: true))

        #expect(first.events == [.appLaunched(coldStart: true)])
        #expect(second.events == [.appLaunched(coldStart: true)])
    }

    @Test("removeAll stops delivery")
    func removeAll() {
        let diagnostics = Diagnostics(time: StubClock(now: .init(timeIntervalSince1970: 0)))
        let sink = RecordingSink()
        diagnostics.install(sink)
        diagnostics.removeAll()

        diagnostics.record(.memoryWarning)

        #expect(sink.events.isEmpty)
    }

    @Test("record is safe and lossless under concurrent emit")
    func concurrentRecord() async {
        let diagnostics = Diagnostics(time: SystemTimeSource())
        let sink = RecordingSink()
        diagnostics.install(sink)

        await withTaskGroup(of: Void.self) { group in
            for _ in 0 ..< 200 {
                group.addTask { diagnostics.record(.memoryWarning) }
            }
        }

        #expect(sink.events.count == 200)
    }
}

@Suite("DiagnosticsConsent model")
struct DiagnosticsConsentTests {
    @Test("privacy default keeps uploads off")
    func privacyDefault() {
        let consent = DiagnosticsConsent.privacyDefault
        #expect(consent.diagnosticsEnabled)
        #expect(consent.remoteUploadEnabled == false)
        #expect(consent.uploadEndpoint == nil)
        #expect(consent.canUpload == false)
    }

    @Test("canUpload requires both the flag and an endpoint")
    func canUpload() {
        var consent = DiagnosticsConsent(diagnosticsEnabled: true, remoteUploadEnabled: true)
        #expect(consent.canUpload == false)
        consent.uploadEndpoint = URL(string: "https://capsule.example/v1/telemetry")
        #expect(consent.canUpload)
    }

    @Test("consent round-trips through Codable")
    func codable() throws {
        let consent = DiagnosticsConsent(
            diagnosticsEnabled: false,
            remoteUploadEnabled: true,
            uploadEndpoint: URL(string: "https://capsule.example")
        )
        let data = try JSONEncoder().encode(consent)
        let decoded = try JSONDecoder().decode(DiagnosticsConsent.self, from: data)
        #expect(decoded == consent)
    }
}
