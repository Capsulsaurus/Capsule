import CapsuleFoundation
import Foundation

/// Uploads a diagnostics bundle to a self-hosted endpoint.
public protocol TelemetryUploader: Sendable {
    func upload(_ bundle: DiagnosticsBundle, to endpoint: URL) async throws
}

/// Errors surfaced by ``RemoteTelemetryUploader``.
public enum UploadError: Error, Sendable, Equatable {
    /// Upload was requested while disabled / unconfigured.
    case disabled
    /// The server rejected the upload with a non-2xx status.
    case server(status: Int)
    /// All retry attempts failed.
    case exhaustedRetries
}

/// The transport seam the uploader posts through — the actual `URLSession` call.
/// Abstracted so retry/backoff logic is testable without real networking.
public protocol UploadTransport: Sendable {
    /// Send the request and return the HTTP status code.
    func send(_ request: URLRequest) async throws -> Int
}

/// Production transport over `URLSession`.
public struct URLSessionTransport: UploadTransport {
    private let session: URLSession
    public init(session: URLSession = .shared) { self.session = session }

    public func send(_ request: URLRequest) async throws -> Int {
        let (_, response) = try await session.data(for: request)
        return (response as? HTTPURLResponse)?.statusCode ?? -1
    }
}

/// The app's **only** network egress: POSTs a bundle as JSON with bounded
/// exponential-backoff retry. Compiled in, but invoked only when the user has
/// opted into uploads and configured an endpoint (enforced by the coordinator).
public struct RemoteTelemetryUploader: TelemetryUploader {
    private let transport: any UploadTransport
    private let maxAttempts: Int
    private let baseBackoff: Duration

    public init(
        transport: any UploadTransport = URLSessionTransport(),
        maxAttempts: Int = 3,
        baseBackoff: Duration = .milliseconds(500)
    ) {
        self.transport = transport
        self.maxAttempts = max(1, maxAttempts)
        self.baseBackoff = baseBackoff
    }

    public func upload(_ bundle: DiagnosticsBundle, to endpoint: URL) async throws {
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try bundle.jsonData()

        var attempt = 0
        while true {
            attempt += 1
            do {
                let status = try await transport.send(request)
                if (200 ..< 300).contains(status) { return }
                throw UploadError.server(status: status)
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                try Task.checkCancellation()
                guard attempt < maxAttempts else { throw UploadError.exhaustedRetries }
                // Exponential backoff: base × 2^(attempt-1).
                try? await Task.sleep(for: baseBackoff * (1 << (attempt - 1)))
            }
        }
    }
}
