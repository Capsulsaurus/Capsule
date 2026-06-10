import CapsuleFoundation
import CapsuleTestSupport
import Foundation
import Testing

@testable import CapsuleDiagnostics

@MainActor
@Suite("DiagnosticsCoordinator consent gating")
struct DiagnosticsCoordinatorTests {
    private func makeCoordinator(
        consent: DiagnosticsConsent,
        diagnostics: Diagnostics,
        breadcrumbs: BreadcrumbRing,
        metrics: MockMetricsCollector,
        uploader: MockTelemetryUploader = MockTelemetryUploader()
    ) -> (DiagnosticsCoordinator, MockConsentReading) {
        let reader = MockConsentReading(consent)
        let suite = makeSuiteName()
        let coordinator = DiagnosticsCoordinator(
            consent: reader,
            diagnostics: diagnostics,
            breadcrumbs: breadcrumbs,
            lastWords: LastWordsStore(suiteName: suite),
            crashStore: CrashDiagnosticStore(suiteName: suite),
            exporter: DefaultDiagnosticsExporter(logReader: MockLogExcerptReader([])),
            uploader: uploader,
            metrics: metrics
        )
        return (coordinator, reader)
    }

    @Test("enabled: installs breadcrumbs and starts MetricKit")
    func enabledInstallsSinks() async {
        let diagnostics = Diagnostics(time: FixedTimeSource())
        let breadcrumbs = BreadcrumbRing(capacity: 16)
        let metrics = MockMetricsCollector()
        let (coordinator, _) = makeCoordinator(
            consent: DiagnosticsConsent(diagnosticsEnabled: true),
            diagnostics: diagnostics, breadcrumbs: breadcrumbs, metrics: metrics
        )

        await coordinator.start()
        #expect(metrics.isStarted)

        diagnostics.record(.memoryWarning)
        let recorded = await poll { await breadcrumbs.snapshot().contains { $0.name == "memory_warning" } }
        #expect(recorded)
        withExtendedLifetime(coordinator) {}
    }

    @Test("disabled: MetricKit stays off and no breadcrumbs are kept")
    func disabledKeepsNothing() async {
        let diagnostics = Diagnostics(time: FixedTimeSource())
        let breadcrumbs = BreadcrumbRing(capacity: 16)
        let metrics = MockMetricsCollector()
        let (coordinator, _) = makeCoordinator(
            consent: DiagnosticsConsent(diagnosticsEnabled: false),
            diagnostics: diagnostics, breadcrumbs: breadcrumbs, metrics: metrics
        )

        await coordinator.start()
        #expect(metrics.isStarted == false)

        diagnostics.record(.memoryWarning)
        // Give any stray async hop a chance, then assert nothing landed.
        _ = await poll(timeout: .milliseconds(200)) { await !breadcrumbs.snapshot().isEmpty }
        #expect(await breadcrumbs.snapshot().isEmpty)
        withExtendedLifetime(coordinator) {}
    }

    @Test("toggling diagnostics off at runtime stops MetricKit")
    func runtimeToggleStopsMetricKit() async {
        let diagnostics = Diagnostics(time: FixedTimeSource())
        let breadcrumbs = BreadcrumbRing(capacity: 16)
        let metrics = MockMetricsCollector()
        let (coordinator, reader) = makeCoordinator(
            consent: DiagnosticsConsent(diagnosticsEnabled: true),
            diagnostics: diagnostics, breadcrumbs: breadcrumbs, metrics: metrics
        )

        await coordinator.start()
        #expect(metrics.isStarted)

        await reader.send(DiagnosticsConsent(diagnosticsEnabled: false))
        let stopped = await poll { metrics.stopCount >= 1 }
        #expect(stopped)
        #expect(metrics.isStarted == false)
        withExtendedLifetime(coordinator) {}
    }

    @Test("submitReport without upload consent throws and never uploads")
    func submitWithoutConsent() async {
        let uploader = MockTelemetryUploader()
        let (coordinator, _) = makeCoordinator(
            consent: DiagnosticsConsent(diagnosticsEnabled: true),
            diagnostics: Diagnostics(time: FixedTimeSource()),
            breadcrumbs: BreadcrumbRing(), metrics: MockMetricsCollector(), uploader: uploader
        )
        await coordinator.start()

        await #expect(throws: UploadError.disabled) {
            try await coordinator.submitReport(makeBundle())
        }
        #expect(uploader.uploadCount == 0)
        withExtendedLifetime(coordinator) {}
    }

    @Test("submitReport with upload consent uploads once")
    func submitWithConsent() async throws {
        let uploader = MockTelemetryUploader()
        let consent = DiagnosticsConsent(
            diagnosticsEnabled: true,
            remoteUploadEnabled: true,
            uploadEndpoint: URL(string: "https://capsule.example/v1/telemetry")
        )
        let (coordinator, _) = makeCoordinator(
            consent: consent,
            diagnostics: Diagnostics(time: FixedTimeSource()),
            breadcrumbs: BreadcrumbRing(), metrics: MockMetricsCollector(), uploader: uploader
        )
        await coordinator.start()

        try await coordinator.submitReport(makeBundle())
        #expect(uploader.uploadCount == 1)
        withExtendedLifetime(coordinator) {}
    }
}
