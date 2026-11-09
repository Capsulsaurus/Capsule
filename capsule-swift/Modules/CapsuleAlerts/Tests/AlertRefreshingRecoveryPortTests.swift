import CapsuleDomain
import CapsuleMock
import CapsulePorts
import Testing

@testable import CapsuleAlerts

/// Recovery has no change stream, so the wrapper is what keeps the recovery
/// alarm and its badge current after a verify or snooze anywhere in the app.
@Suite("Recovery calls refresh the alerts")
struct AlertRefreshingRecoveryPortTests {
    private actor RefreshCounter {
        private(set) var count = 0

        func increment() {
            count += 1
        }
    }

    private func wrapped(_ scenario: MockScenario) -> (AlertRefreshingRecoveryPort, RefreshCounter) {
        let counter = RefreshCounter()
        let port = AlertRefreshingRecoveryPort(MockEnvironment(scenario: scenario).recovery) {
            await counter.increment()
        }
        return (port, counter)
    }

    @Test("reading the summary changes nothing, so it refreshes nothing")
    func summaryDoesNotRefresh() async throws {
        let (port, counter) = wrapped(.healthy)

        _ = try await port.summary()

        #expect(await counter.count == 0)
    }

    @Test("a verification refreshes once, whatever its outcome")
    func verifyRefreshes() async throws {
        let (port, counter) = wrapped(.healthy)

        _ = try await port.verify(passphrase: "not the secret")

        #expect(await counter.count == 1)
    }

    @Test("a snooze refreshes once and is passed through")
    func snoozeRefreshes() async throws {
        let (port, counter) = wrapped(.recoveryOverdue)
        let until = CapsuleTimestamp(epochSeconds: 1900000000)

        try await port.snoozeVerification(until: until)

        #expect(await counter.count == 1)
        #expect(try await port.summary().verification.nextDueAt == until)
    }

    @Test("a call that throws still refreshes, and the error reaches the caller")
    func failureRefreshes() async {
        let (port, counter) = wrapped(.healthy)

        await #expect(throws: CapsuleError.self) {
            _ = try await port.restore(usingRecoverySecret: "not the secret")
        }
        #expect(await counter.count == 1)
    }
}
