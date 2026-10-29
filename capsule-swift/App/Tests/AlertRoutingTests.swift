import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import Foundation
import Testing

@testable import Capsule

/// Where a tapped alert lands. The re-wrap is the one that must never share a
/// destination with the routine check: the class set is closed, so the
/// `recovery` parameter is all that tells them apart.
@Suite("Alert taps route to their screens")
struct AlertRoutingTests {
    @Test("a re-wrap alert presents the guided re-wrap; a routine check opens Backup & Recovery")
    func rewrapIsNotTheCheck() {
        #expect(AlertDestination.guidedRewrap.tapAction == .presentGuidedRewrap)
        #expect(AlertDestination.recoveryVerification.tapAction == .navigate(.settings(.backupAndRecovery)))
    }

    @Test("every server-held class opens the screen that resolves it")
    func serverHeldClasses() {
        #expect(AlertDestination.syncStatus.tapAction == .navigate(.transferCenter))
        #expect(AlertDestination.quota.tapAction == .navigate(.quota))
        #expect(AlertDestination.quarantine.tapAction == .navigate(.quarantine))
        #expect(AlertDestination.drops.tapAction == .navigate(.drops))
    }

    @Test("an armed re-wrap alarm, decoded from its notification, presents the re-wrap")
    func endToEnd() {
        let input = NotifyInput(recoveryNextDue: CapsuleTimestamp(epochSeconds: 1767225600), recoveryRewrapDue: true)
        let alert = input.armedAlert(for: ClassDeadline(
            alertClass: .recoveryCheckDue,
            deadline: CapsuleTimestamp(epochSeconds: 1767225600)
        ))
        #expect(alert.destination.tapAction == .presentGuidedRewrap)
    }
}

/// Which adapters the alert loop is composed on, and the clock each measures
/// against. The clock is the load-bearing half: an alarm armed from facts
/// dated on one clock and fired on another is weeks wrong.
@MainActor
@Suite("Alert adapters compose per lane")
struct AlertAdapterCompositionTests {
    @Test("the mock lane arms nothing with the OS and keeps the reference instant")
    func mockLane() {
        let world = MockEnvironment(scenario: .healthy)
        #expect(!AlertAdapters.mock.armsWithSystem)
        #expect(AlertAdapters.mock.worldClock().now == MockClock.reference.now)
        #expect(AlertAdapters.mock.alertClock(world: world).now() == world.configuration.clock.now)
    }

    @Test("the system lane dates the world at launch and measures on the wall clock")
    func systemLane() {
        let adapters = AlertAdapters.system(policy: MockAlertPolicy())
        let launchedAt = Date(timeIntervalSince1970: 1_800_000_000)
        #expect(adapters.armsWithSystem)
        #expect(adapters.worldClock(launchedAt: launchedAt).now == CapsuleTimestamp(epochSeconds: 1_800_000_000))

        let wall = Int64(Date().timeIntervalSince1970)
        let measured = adapters.alertClock(world: MockEnvironment(scenario: .healthy)).now().epochSeconds
        #expect(abs(measured - wall) <= 5)
    }

    @Test("this build launches on the lane its linked core selects")
    func launchLane() {
        #if canImport(CapsuleCatalogFFI)
            #expect(AlertAdapters.launch.armsWithSystem)
        #else
            #expect(!AlertAdapters.launch.armsWithSystem)
        #endif
    }

    @Test("an environment built without naming a lane composes the mock adapters")
    func defaultEnvironmentIsMock() {
        let environment = AppEnvironment(mock: MockEnvironment(scenario: .healthy))
        #expect(!environment.alertAdapters.armsWithSystem)
    }
}
