import Foundation

/// The user's diagnostics & telemetry preferences.
///
/// Privacy-first defaults: on-device diagnostics are on (they never leave the
/// device), while any network upload is off until the user explicitly enables
/// it and provides an endpoint.
public struct DiagnosticsConsent: Codable, Sendable, Equatable {
    /// Collect MetricKit + on-device diagnostics. Stays on the device.
    public var diagnosticsEnabled: Bool
    /// Upload bug reports / diagnostics to ``uploadEndpoint``.
    public var remoteUploadEnabled: Bool
    /// The self-hosted endpoint reports are uploaded to, when enabled.
    public var uploadEndpoint: URL?

    public init(
        diagnosticsEnabled: Bool = true,
        remoteUploadEnabled: Bool = false,
        uploadEndpoint: URL? = nil
    ) {
        self.diagnosticsEnabled = diagnosticsEnabled
        self.remoteUploadEnabled = remoteUploadEnabled
        self.uploadEndpoint = uploadEndpoint
    }

    /// The privacy-first default: local diagnostics on, no uploads.
    public static let privacyDefault = DiagnosticsConsent()

    /// Whether a remote upload may actually be attempted — both opted in and
    /// pointed at an endpoint.
    public var canUpload: Bool {
        remoteUploadEnabled && uploadEndpoint != nil
    }
}

/// A read-only view of consent, consulted by sinks and the coordinator.
public protocol ConsentReading: Sendable {
    /// The current consent snapshot.
    func current() async -> DiagnosticsConsent
    /// A stream that yields the latest consent whenever it changes.
    func changes() -> AsyncStream<DiagnosticsConsent>
}
