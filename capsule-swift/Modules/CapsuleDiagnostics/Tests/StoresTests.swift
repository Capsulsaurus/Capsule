import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleDiagnostics

@Suite("BreadcrumbRing")
struct BreadcrumbRingTests {
    @Test("evicts the oldest entries beyond capacity")
    func eviction() async {
        let ring = BreadcrumbRing(capacity: 3)
        for index in 0 ..< 5 {
            await ring.append(.memoryWarning, at: Date(timeIntervalSince1970: TimeInterval(index)))
        }
        let snapshot = await ring.snapshot()
        #expect(snapshot.count == 3)
        #expect(snapshot.first?.timestamp == Date(timeIntervalSince1970: 2))
        #expect(snapshot.last?.timestamp == Date(timeIntervalSince1970: 4))
    }

    @Test("records PII-free summaries via the sink path")
    func sinkPath() async {
        let ring = BreadcrumbRing(capacity: 10)
        ring.record(.operationFailed(operation: .share), at: Date(timeIntervalSince1970: 1))
        let recorded = await poll {
            await ring.snapshot().contains { $0.name == "operation_failed" && $0.detail == "share" }
        }
        #expect(recorded)
    }

    @Test("is lossless under concurrent appends")
    func concurrency() async {
        let ring = BreadcrumbRing(capacity: 1000)
        await withTaskGroup(of: Void.self) { group in
            for index in 0 ..< 300 {
                group.addTask { await ring.append(.memoryWarning, at: Date(timeIntervalSince1970: TimeInterval(index))) }
            }
        }
        #expect(await ring.snapshot().count == 300)
    }
}

@Suite("LastWordsStore")
struct LastWordsStoreTests {
    @Test("a fresh install reports no abnormal termination")
    func fresh() {
        let store = LastWordsStore(suiteName: makeSuiteName())
        #expect(store.previousSessionEndedAbnormally == false)
    }

    @Test("a session without a clean shutdown is flagged on next launch")
    func abnormal() async {
        let suite = makeSuiteName()
        await LastWordsStore(suiteName: suite).beginSession()
        // No markCleanShutdown — simulate a crash.
        let next = LastWordsStore(suiteName: suite)
        #expect(next.previousSessionEndedAbnormally)
    }

    @Test("a clean shutdown is not flagged")
    func clean() async {
        let suite = makeSuiteName()
        let session = LastWordsStore(suiteName: suite)
        await session.beginSession()
        await session.markCleanShutdown()
        let next = LastWordsStore(suiteName: suite)
        #expect(next.previousSessionEndedAbnormally == false)
    }
}

@Suite("CrashDiagnosticStore")
struct CrashDiagnosticStoreTests {
    @Test("stores, reloads, and clears the latest crash")
    func roundTrip() async {
        let suite = makeSuiteName()
        let store = CrashDiagnosticStore(suiteName: suite)
        #expect(await store.lastCrash() == nil)

        let summary = CrashSummary(
            kind: .crash, exceptionType: 1, signal: 11,
            terminationReason: "test", osVersion: "18.0", appBuild: "1",
            timestamp: Date(timeIntervalSince1970: 10)
        )
        await store.store(summary)
        #expect(await store.lastCrash() == summary)

        let reloaded = CrashDiagnosticStore(suiteName: suite)
        #expect(await reloaded.lastCrash() == summary)

        await store.clear()
        #expect(await store.lastCrash() == nil)
    }
}
