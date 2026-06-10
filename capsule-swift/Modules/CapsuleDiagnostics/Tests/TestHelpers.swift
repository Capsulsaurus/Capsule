import CapsuleDiagnostics
import Foundation

/// A fresh, isolated `UserDefaults` suite name per call, so persisted-store
/// tests don't collide.
func makeSuiteName() -> String {
    "capsule.test.\(UUID().uuidString)"
}

/// A stable upload endpoint for uploader tests (force-unwrap free).
func testEndpoint() -> URL {
    URL(string: "https://capsule.example/v1/telemetry") ?? URL(filePath: "/")
}

/// A minimal, identifier-free bundle for tests that just need a payload.
func makeBundle() -> DiagnosticsBundle {
    DiagnosticsBundle(
        createdAt: Date(timeIntervalSince1970: 0),
        metadata: DeviceMetadata(
            appVersion: "1.0",
            appBuild: "1",
            systemName: "iOS",
            systemVersion: "18.0",
            model: "iPhone",
            locale: "en_US",
            freeDiskSpace: .ample
        ),
        breadcrumbs: [],
        crash: nil,
        logExcerpt: []
    )
}

/// Polls `condition` until it holds or the timeout elapses. Used to await the
/// effects of fire-and-forget actor hops (sinks, observers) deterministically.
@discardableResult
func poll(timeout: Duration = .seconds(2), _ condition: @Sendable () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + timeout
    while ContinuousClock.now < deadline {
        if await condition() { return true }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return await condition()
}
