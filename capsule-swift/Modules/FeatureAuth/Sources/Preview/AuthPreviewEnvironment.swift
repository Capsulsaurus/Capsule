import CapsuleDomain
import CapsuleMock
import CapsulePorts
import Foundation

// MARK: - AuthPreviewEnvironment

/// Every port these screens need, wired into one coherent world.
///
/// The same argument ``MockEnvironment`` makes for building the whole graph in
/// one place applies to the identity screens specifically: a scenario must not
/// be half-applied. There is no configuration in which the welcome screen
/// believes there is no session while the session ledger lists three, because
/// both read stores built from one ``MockConfiguration`` — and the ports this
/// module had to declare for itself are built from the same one.
///
/// The clock comes from the scenario too, so a snooze that lands "in 7 days" and
/// a session that expires "in 180 days" are measured against the same instant
/// and a preview renders identically today and in a year.
public struct AuthPreviewEnvironment: Sendable {
    public let environment: MockEnvironment
    public let discovery: PreviewServerDiscovery
    public let credentials: PreviewCredentials
    public let secondFactor: PreviewSecondFactor
    public let ceremony: PreviewEnrollmentCeremony
    public let crossDevice: PreviewCrossDeviceCeremony
    public let restore: PreviewRestore
    /// A discovered server, for the screens that come after discovery.
    public let server: ServerInfo

    public init(
        scenario: MockScenario = .healthy,
        credentialBehaviour: PreviewCredentialBehaviour = .healthy,
        ceremonyBehaviour: PreviewCeremonyBehaviour = .healthy,
        safetyCodesDiverge: Bool = false,
        restoreLedgerIsComplete: Bool = true,
        restoreSignatureChainIsIntact: Bool = true
    ) {
        let environment = MockEnvironment(scenario: scenario)
        self.environment = environment
        discovery = PreviewServerDiscovery(environment: environment)
        credentials = PreviewCredentials(environment: environment, behaviour: credentialBehaviour)
        secondFactor = PreviewSecondFactor(environment: environment)
        ceremony = PreviewEnrollmentCeremony(behaviour: ceremonyBehaviour)
        crossDevice = PreviewCrossDeviceCeremony(
            environment: environment,
            producesMismatch: safetyCodesDiverge
        )
        restore = PreviewRestore(
            environment: environment,
            ledgerIsComplete: restoreLedgerIsComplete,
            signatureChainIsIntact: restoreSignatureChainIsIntact
        )
        server = PreviewServerDiscovery.server(
            domain: "capsule.example",
            seed: environment.configuration.seed
        )
    }

    // MARK: Ports from the shared world

    public var auth: any AuthPort { environment.auth }
    public var devices: any DevicePort { environment.devices }
    public var enrollment: any EnrollmentPort { environment.enrollment }
    public var recovery: any RecoveryPort { environment.recovery }

    /// The scenario's instant, so every countdown in a preview is reproducible.
    public var now: @Sendable () -> CapsuleTimestamp {
        let instant = environment.configuration.clock.now
        return { instant }
    }

    /// A file URL standing in for a chosen backup artifact. Never read — the
    /// preview restore port answers from the scenario, not from disk.
    public var artifactURL: URL {
        URL(fileURLWithPath: "/Backups/capsule-2026-02-22.tar")
    }

    // MARK: The interesting worlds

    /// A healthy, signed-in library.
    public static let healthy = AuthPreviewEnvironment()

    /// No session on this device — the first-run fork, and the whole
    /// never-signed-in mode behind it.
    public static let neverSignedIn = AuthPreviewEnvironment(scenario: .neverSignedIn)

    /// The verification cadence has lapsed and every snooze is spent, so the
    /// persistent badge and the guided re-wrap are both reachable.
    public static let recoveryOverdue = AuthPreviewEnvironment(scenario: .recoveryOverdue)

    /// No usable network. Every local read still answers; everything that would
    /// touch the server refuses.
    public static let offline = AuthPreviewEnvironment(scenario: .offline)
}
