import CapsuleFoundation
import CapsuleTestSupport
import Foundation
import Testing

@testable import CapsuleDiagnostics

@Suite("RemoteTelemetryUploader retry")
struct UploaderTests {
    private let endpoint = testEndpoint()

    @Test("succeeds after transient failures")
    func retriesThenSucceeds() async throws {
        let transport = MockUploadTransport(failuresBeforeSuccess: 2)
        let uploader = RemoteTelemetryUploader(transport: transport, maxAttempts: 3, baseBackoff: .milliseconds(1))

        try await uploader.upload(makeBundle(), to: endpoint)

        #expect(transport.sendCount == 3)
    }

    @Test("throws exhaustedRetries after the attempt budget")
    func exhaustsRetries() async {
        let transport = MockUploadTransport(failuresBeforeSuccess: 10)
        let uploader = RemoteTelemetryUploader(transport: transport, maxAttempts: 3, baseBackoff: .milliseconds(1))

        await #expect(throws: UploadError.exhaustedRetries) {
            try await uploader.upload(makeBundle(), to: endpoint)
        }
        #expect(transport.sendCount == 3)
    }

    @Test("a non-2xx status is treated as a failure")
    func nonSuccessStatusFails() async {
        let transport = MockUploadTransport(failuresBeforeSuccess: 0, successStatus: 418)
        let uploader = RemoteTelemetryUploader(transport: transport, maxAttempts: 2, baseBackoff: .milliseconds(1))

        await #expect(throws: UploadError.exhaustedRetries) {
            try await uploader.upload(makeBundle(), to: endpoint)
        }
        #expect(transport.sendCount == 2)
    }
}
