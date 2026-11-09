import CapsuleDomain
import CapsuleNavigation
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
