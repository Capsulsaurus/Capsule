import CapsuleFoundation
import Foundation

/// The app-level façade that wires diagnostics together.
///
/// Responsibilities:
/// - install ``DiagnosticsSink``s on ``Diagnostics`` according to consent,
///   reconciling live as the user toggles preferences;
/// - drive MetricKit collection (gated on consent);
/// - track clean shutdown for the "crashed last launch?" prompt;
/// - build redacted report bundles and, when opted in, upload them.
///
/// Main-actor isolated: it is owned by the (`@MainActor`) `AppEnvironment` and
/// reads `UIDevice` when assembling metadata.
@MainActor
public final class DiagnosticsCoordinator {
    private let diagnostics: Diagnostics
    private let consentReader: any ConsentReading
    private let breadcrumbs: BreadcrumbRing
    private let lastWords: LastWordsStore
    private let crashStore: CrashDiagnosticStore
    private let exporter: any DiagnosticsExporter
    private let uploader: any TelemetryUploader
    private let metrics: any MetricsCollecting

    private let osLogSink = OSLogDiagnosticsSink()
    private var metricsRunning = false
    private var consentTask: Task<Void, Never>?

    public init(
        consent: any ConsentReading,
        diagnostics: Diagnostics = .shared,
        breadcrumbs: BreadcrumbRing = BreadcrumbRing(),
        lastWords: LastWordsStore = LastWordsStore(),
        crashStore: CrashDiagnosticStore = CrashDiagnosticStore(),
        exporter: any DiagnosticsExporter = DefaultDiagnosticsExporter(),
        uploader: any TelemetryUploader = RemoteTelemetryUploader(),
        metrics: (any MetricsCollecting)? = nil
    ) {
        consentReader = consent
        self.diagnostics = diagnostics
        self.breadcrumbs = breadcrumbs
        self.lastWords = lastWords
        self.crashStore = crashStore
        self.exporter = exporter
        self.uploader = uploader
        self.metrics = metrics ?? MetricKitSubscriber(
            diagnostics: diagnostics,
            crashStore: crashStore,
            time: SystemTimeSource()
        )
    }

    deinit { consentTask?.cancel() }

    /// Boot diagnostics: install sinks per consent, begin the session, observe
    /// consent changes, and record the launch.
    public func start() async {
        let consent = await consentReader.current()
        reconcile(consent)
        await lastWords.beginSession()
        observeConsent()
        diagnostics.record(.appLaunched(coldStart: true))
    }

    // MARK: Scene lifecycle

    /// Call when the app enters the background — marks a clean shutdown.
    public func noteEnteredBackground() async {
        diagnostics.record(.enteredBackground)
        await lastWords.markCleanShutdown()
    }

    /// Call when the app returns to the foreground — re-arms crash detection.
    public func noteBecameActive() async {
        await lastWords.beginSession()
    }

    // MARK: Crash prompt

    /// Whether to offer a "we crashed last time" report — true only when
    /// diagnostics are enabled and MetricKit delivered a crash since last launch.
    public func shouldOfferCrashReport() async -> Bool {
        guard await consentReader.current().diagnosticsEnabled else { return false }
        return await crashStore.lastCrash() != nil
    }

    /// Forget the stored crash once the user has acted on the prompt.
    public func acknowledgeCrashReport() async {
        await crashStore.clear()
    }

    // MARK: Reporting

    /// Assemble a redacted diagnostics bundle for sharing or upload.
    public func makeReportBundle() async -> DiagnosticsBundle {
        let metadata = DeviceMetadata.current()
        let crumbs = await breadcrumbs.snapshot()
        let crash = await crashStore.lastCrash()
        return await exporter.exportBundle(metadata: metadata, breadcrumbs: crumbs, crash: crash)
    }

    /// Upload a bundle to the configured endpoint. Throws ``UploadError/disabled``
    /// when the user has not opted into uploads — callers fall back to the share
    /// sheet in that case.
    public func submitReport(_ bundle: DiagnosticsBundle) async throws {
        let consent = await consentReader.current()
        guard consent.canUpload, let endpoint = consent.uploadEndpoint else {
            throw UploadError.disabled
        }
        try await uploader.upload(bundle, to: endpoint)
    }

    // MARK: Internals

    private func observeConsent() {
        consentTask = Task { [weak self] in
            guard let self else { return }
            for await consent in consentReader.changes() {
                reconcile(consent)
            }
        }
    }

    /// Rebuild the sink set and MetricKit subscription for the given consent.
    /// Idempotent: the on-device OSLog sink is always present; breadcrumbs and
    /// MetricKit are present only while diagnostics are enabled.
    private func reconcile(_ consent: DiagnosticsConsent) {
        diagnostics.removeAll()
        diagnostics.install(osLogSink)
        if consent.diagnosticsEnabled {
            diagnostics.install(breadcrumbs)
            if !metricsRunning { metrics.start(); metricsRunning = true }
        } else if metricsRunning {
            metrics.stop()
            metricsRunning = false
        }
    }
}
