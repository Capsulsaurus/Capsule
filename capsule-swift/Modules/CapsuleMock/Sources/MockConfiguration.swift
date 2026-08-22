import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockConfiguration

/// The whole world a scenario describes, resolved into values.
///
/// One struct rather than a `MockScenario` switch inside each port, so a
/// scenario is coherent by construction: ``MockScenario/offline`` sets the
/// connection class, stalls the uploads, and degrades the ladder in one place,
/// and there is no way for one port to have heard about it and another not to.
public struct MockConfiguration: Sendable {
    public var scenario: MockScenario
    public var clock: MockClock
    public var profile: MockLibraryProfile

    /// Whether a session exists on this device.
    public var isSignedIn: Bool
    /// Whether the session needs fresh local authentication before a sensitive
    /// action — enrolling a device, opening Trash or Hidden.
    public var requiresLocalAuth: Bool
    public var connectionClass: ConnectionClass
    public var quota: QuotaStatus
    public var uploadPolicy: UploadPolicy
    public var syncScope: SyncScope
    /// Which quarantine surfaces have entries. Several at once for
    /// ``MockScenario/quarantine`` — one row proves nothing about a surface
    /// whose whole job is to distinguish eight kinds of held thing.
    public var quarantineSurfaces: [QuarantineSurface]
    /// Whether federated origins are unreachable and peer circuits are open.
    public var federationIsDegraded: Bool
    /// Whether the recovery cadence has lapsed with its snoozes exhausted.
    public var recoveryIsOverdue: Bool
    /// Whether sessions sit at their declared offset without progressing.
    public var stallsUploads: Bool
    /// Whether a smart-album definition from a newer grammar is present, so the
    /// preserve-never-evaluate path is reachable.
    public var hasSmartAlbumFromNewerGrammar: Bool
    /// Latency and failure injection for the calls that would touch a network.
    public var behaviour: MockBehaviour

    public var seed: UInt64 { profile.seed }

    // MARK: Construction

    /// Resolve a scenario into a world.
    ///
    /// Every branch below changes several fields at once, which is the point: a
    /// screen that reads only the quota state and a screen that reads only the
    /// sync status must both be telling the truth about the same world.
    public static func make(
        scenario: MockScenario,
        clock: MockClock = .reference,
        seed: UInt64 = 0x0C0F_FEE0_1234_5678
    ) -> MockConfiguration {
        var configuration = base(scenario: scenario, clock: clock, seed: seed)
        configuration.apply(scenario: scenario, clock: clock)
        return configuration
    }

    private static func base(
        scenario: MockScenario,
        clock: MockClock,
        seed: UInt64
    ) -> MockConfiguration {
        MockConfiguration(
            scenario: scenario,
            clock: clock,
            profile: MockLibraryProfile(seed: seed, newestDayNumber: clock.todayDayNumber),
            isSignedIn: true,
            requiresLocalAuth: false,
            connectionClass: .unmetered,
            quota: healthyQuota(),
            uploadPolicy: .full,
            syncScope: .metadataAndThumbnails,
            quarantineSurfaces: [],
            federationIsDegraded: false,
            recoveryIsOverdue: false,
            stallsUploads: false,
            hasSmartAlbumFromNewerGrammar: false,
            behaviour: .deterministic
        )
    }

    /// 118 GB of 512, comfortably inside every threshold.
    private static func healthyQuota() -> QuotaStatus {
        QuotaStatus(
            used: 118 * 1073741824,
            softLimit: 409 * 1073741824,
            hardLimit: 512 * 1073741824,
            state: .withinQuota
        )
    }

    // swiftlint:disable:next cyclomatic_complexity
    private mutating func apply(scenario: MockScenario, clock: MockClock) {
        switch scenario {
        case .healthy:
            break
        case .emptyLibrary:
            profile.assetCount = 0
            quota = MockConfiguration.emptyQuota()
        case .neverSignedIn:
            profile.assetCount = 0
            isSignedIn = false
            quota = MockConfiguration.emptyQuota()
        case .offline:
            applyOffline()
        case .hugeLibrary:
            profile.assetCount = 250000
            profile.spanDays = 3650
        case .quotaSoftWarning:
            quota = MockConfiguration.softWarningQuota()
        case .quotaGraceExpired:
            applyGraceExpired(clock: clock)
        case .quarantine:
            applyQuarantine()
        case .degradedFederation:
            federationIsDegraded = true
            connectionClass = .adverse
        case .awaitingOriginals:
            applyAwaitingOriginals()
        case .newerVersionState:
            profile.newerVersionPerMille = 90
            hasSmartAlbumFromNewerGrammar = true
        case .undecodableAssets:
            profile.unreadablePerMille = 110
        case .recoveryOverdue:
            recoveryIsOverdue = true
        case .protocolUpgradeRequired:
            behaviour = .alwaysFailing(.protocolVersionUnsupported)
            stallsUploads = true
        }
    }

    /// Offline degrades what can be *drawn* and stops what needs the network. It
    /// deliberately leaves every local read working — that is the offline-first
    /// contract, and a scenario that broke the gallery would be modelling a
    /// different product.
    private mutating func applyOffline() {
        connectionClass = .offline
        profile.degradesRemoteRepresentations = true
        stallsUploads = true
        behaviour = .alwaysFailing(.blobPendingUpload)
    }

    private mutating func applyGraceExpired(clock: MockClock) {
        let crossed = clock.offset(days: -(QuotaStatus.defaultGraceWindowDays + 9))
        quota = QuotaStatus(
            used: 528 * 1073741824,
            softLimit: 409 * 1073741824,
            hardLimit: 512 * 1073741824,
            state: .graceExpired,
            hardExceededSince: crossed
        )
        stallsUploads = true
    }

    private mutating func applyQuarantine() {
        // Five of the eight surfaces, each with its own holding area, so the
        // "what is preserved and where" question has more than one answer on
        // screen at once.
        quarantineSurfaces = [
            .verifyAssetReject,
            .malformedSidecar,
            .orphanedOriginal,
            .federationSoftFail,
            .pendingDropAwaitingAdoption,
            .albumUpgradeStrandedWrite,
        ]
        profile.quarantinedPerMille = 30
    }

    private mutating func applyAwaitingOriginals() {
        profile.awaitingOriginalPerMille = 380
        uploadPolicy = .staged
        syncScope = .metadataOnly
        connectionClass = .metered
    }

    private static func emptyQuota() -> QuotaStatus {
        QuotaStatus(
            used: 0,
            softLimit: 409 * 1073741824,
            hardLimit: 512 * 1073741824,
            state: .withinQuota
        )
    }

    private static func softWarningQuota() -> QuotaStatus {
        QuotaStatus(
            used: 447 * 1073741824,
            softLimit: 409 * 1073741824,
            hardLimit: 512 * 1073741824,
            state: .softWarning
        )
    }
}
