import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - MockIdentityStore

/// Sessions, the device directory, enrollment, and recovery.
///
/// One actor because the four are one question — *who is allowed to decrypt this
/// library, from where* — and every write to one is visible to the others.
/// Revoking a device removes it from the album groups it belonged to; revoking
/// every session must not leave a stale "signed in" state behind; enrollment
/// adds a directory row the session ledger then groups by cohort.
///
/// **No token ever leaves this actor**, because no token ever enters it. That is
/// the point of ``AuthState``: a view model renders every screen from it and can
/// never hold, log, or serialise a credential.
public actor MockIdentityStore {
    nonisolated let configuration: MockConfiguration
    private nonisolated let gate: MockGate

    private var authState: AuthState
    private var deviceRecords: [DeviceRecord]
    private var sessionRecords: [SessionRecord]
    private var escrow: RecoveryEscrowSummary
    private var activeChannels: Set<String> = []
    /// Which wrap generation the escrow currently holds. Rotation replaces the
    /// wrap, never the master key, so this is a counter rather than a new root.
    private var escrowGeneration = 0

    nonisolated let authChanges = ChangeBroadcaster<AuthState>()
    nonisolated let directoryChanges = ChangeBroadcaster<Void>()

    public init(configuration: MockConfiguration) {
        self.configuration = configuration
        gate = MockGate(behaviour: configuration.behaviour)
        let account = Self.account(configuration: configuration)
        authState = Self.initialState(configuration: configuration, account: account)
        deviceRecords = Self.seedDevices(configuration: configuration)
        sessionRecords = Self.seedSessions(configuration: configuration)
        escrow = Self.seedEscrow(configuration: configuration)
    }

    static func account(configuration: MockConfiguration) -> AccountSummary {
        AccountSummary(
            handle: MockSidecarFactory.ownerHandle,
            userID: MockHash.hex(MockHash.mix(configuration.seed), digits: 16),
            displayName: "avery",
            homeServer: "capsule.example",
            accountType: .registered
        )
    }

    private static func initialState(configuration: MockConfiguration, account: AccountSummary) -> AuthState {
        guard configuration.isSignedIn else { return .signedOut }
        return configuration.requiresLocalAuth ? .requiresLocalAuth(account) : .signedIn(account)
    }

    /// The device directory.
    ///
    /// A revoked device is **listed, not hidden**: its public half stays in the
    /// directory forever so everything it ever signed remains verifiable. A user
    /// auditing their account should see the same history the cryptography does,
    /// which is why one seeded device is revoked.
    private static func seedDevices(configuration: MockConfiguration) -> [DeviceRecord] {
        let seed = configuration.seed
        let clock = configuration.clock
        return [
            DeviceRecord(
                id: MockIdentifiers.deviceID(seed: seed, ordinal: 0),
                model: PlatformEnvironment.hardwareModel,
                platform: PlatformTag(rawValue: PlatformEnvironment.platformTag),
                firstSeen: clock.offset(days: -420),
                lastSeen: clock.now,
                cohortHash: MockIdentifiers.cohortHash(seed: seed, ordinal: 0),
                isCurrent: true
            ),
            DeviceRecord(
                id: MockIdentifiers.deviceID(seed: seed, ordinal: 1),
                model: "iPhone17,2",
                platform: .ios,
                firstSeen: clock.offset(days: -300),
                lastSeen: clock.offset(days: -2),
                cohortHash: MockIdentifiers.cohortHash(seed: seed, ordinal: 1)
            ),
            DeviceRecord(
                id: MockIdentifiers.deviceID(seed: seed, ordinal: 2),
                model: "Pixel 9 Pro",
                platform: .android,
                firstSeen: clock.offset(days: -700),
                lastSeen: clock.offset(days: -190),
                cohortHash: MockIdentifiers.cohortHash(seed: seed, ordinal: 2),
                revokedAt: clock.offset(days: -188)
            ),
        ]
    }

    private static func seedSessions(configuration: MockConfiguration) -> [SessionRecord] {
        let seed = configuration.seed
        let clock = configuration.clock
        return (0 ..< 3).map { ordinal in
            SessionRecord(
                id: MockIdentifiers.sessionID(seed: seed, ordinal: ordinal),
                deviceID: MockIdentifiers.deviceID(seed: seed, ordinal: ordinal),
                cohortHash: MockIdentifiers.cohortHash(seed: seed, ordinal: ordinal),
                createdAt: clock.offset(days: -60 * (ordinal + 1)),
                lastUsedAt: clock.offset(days: -ordinal),
                // The sliding window refreshes on use; the hard ceiling does
                // not, because its job is to bound an exfiltrated token's
                // lifetime regardless of how busy the thief is.
                inactivityExpiresAt: clock.offset(days: 180 - ordinal * 30),
                hardExpiresAt: clock.offset(days: 365 - 60 * (ordinal + 1)),
                isCurrent: ordinal == 0,
                revokedAt: ordinal == 2 ? clock.offset(days: -188) : nil
            )
        }
    }

    private static func seedEscrow(configuration: MockConfiguration) -> RecoveryEscrowSummary {
        let clock = configuration.clock
        let overdue = configuration.recoveryIsOverdue
        return RecoveryEscrowSummary(
            hasServerEscrow: true,
            escrowUpdatedAt: clock.offset(days: overdue ? -400 : -40),
            shamirShareCount: 5,
            shamirThreshold: 3,
            verification: RecoveryVerificationState(
                nextDueAt: clock.offset(days: overdue ? -37 : 51),
                currentIntervalDays: overdue
                    ? RecoveryVerificationState.steadyIntervalDays
                    : RecoveryVerificationState.maximumIntervalDays,
                snoozeCount: overdue ? RecoveryVerificationState.maximumConsecutiveSnoozes : 0,
                consecutiveFailures: overdue ? RecoveryVerificationState.failuresBeforeGuidedRewrap : 0,
                consecutiveSuccesses: overdue ? 0 : 2
            )
        )
    }

    // MARK: State

    var currentState: AuthState { authState }
    var deviceList: [DeviceRecord] { deviceRecords }
    var sessionList: [SessionRecord] { sessionRecords }
    var escrowSummary: RecoveryEscrowSummary { escrow }
    var behaviourGate: MockGate { gate }
    var channels: Set<String> { activeChannels }

    func setState(_ value: AuthState) { authState = value }
    func setDevices(_ value: [DeviceRecord]) { deviceRecords = value }
    func setSessions(_ value: [SessionRecord]) { sessionRecords = value }
    func setEscrow(_ value: RecoveryEscrowSummary) { escrow = value }
    func openChannel(_ handle: String) { activeChannels.insert(handle) }
    func closeChannel(_ handle: String) { activeChannels.remove(handle) }
    var currentGeneration: Int { escrowGeneration }
    func setGeneration(_ value: Int) { escrowGeneration = value }
}
