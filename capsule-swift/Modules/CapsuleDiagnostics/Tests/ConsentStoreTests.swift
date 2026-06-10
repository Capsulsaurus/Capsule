import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleDiagnostics

@Suite("ConsentStore")
struct ConsentStoreTests {
    @Test("defaults to the privacy default on first launch")
    func defaultsToPrivacyDefault() async {
        let store = ConsentStore(suiteName: makeSuiteName())
        #expect(await store.current() == .privacyDefault)
    }

    @Test("update persists and reloads across instances")
    func persistsAndReloads() async {
        let suite = makeSuiteName()
        let store = ConsentStore(suiteName: suite)
        await store.update {
            $0.remoteUploadEnabled = true
            $0.uploadEndpoint = URL(string: "https://capsule.example/v1/telemetry")
        }

        let reloaded = await ConsentStore(suiteName: suite).current()
        #expect(reloaded.remoteUploadEnabled)
        #expect(reloaded.uploadEndpoint == URL(string: "https://capsule.example/v1/telemetry"))
    }

    @Test("changes() replays current then yields updates")
    func changesReplayThenYield() async {
        let store = ConsentStore(suiteName: makeSuiteName())
        var iterator = store.changes().makeAsyncIterator()

        let initial = await iterator.next()
        #expect(initial == .privacyDefault)

        await store.update { $0.diagnosticsEnabled = false }
        let updated = await iterator.next()
        #expect(updated?.diagnosticsEnabled == false)
    }
}
