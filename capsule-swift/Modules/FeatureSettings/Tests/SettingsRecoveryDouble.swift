import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - StubRecoveryPort

/// A ``RecoveryPort`` over an in-memory escrow.
///
/// The cadence lives in the summary, so a screen's "due now", "out of snoozes",
/// and "offer the guided re-wrap" states are all reachable by handing this a
/// different ``RecoveryVerificationState`` rather than by waiting for a clock.
actor StubRecoveryPort: RecoveryPort {
    /// The phrase the stub mints and the only one it accepts.
    static let phrase = "harbor-lantern-quartz-meadow-cobalt-thistle-ember-willow-granite-cinder-marlin-juniper"
    /// The phrase a guided rotation mints instead — a new **wrap**, not a new
    /// master key.
    static let rotatedPhrase = "beacon-pumice-fathom-cedar-onyx-plover-tundra-basalt-verdant-quill-saffron-halyard"

    private var escrow: RecoveryEscrowSummary
    private var generation = 0
    private let readFailure: CapsuleError?
    private let writeFailure: CapsuleError?
    private(set) var restoreAttempts: [String] = []
    private(set) var snoozedUntil: [CapsuleTimestamp] = []

    init(
        escrow: RecoveryEscrowSummary = StubRecoveryPort.configuredEscrow,
        readFailure: CapsuleError? = nil,
        writeFailure: CapsuleError? = nil
    ) {
        self.escrow = escrow
        self.readFailure = readFailure
        self.writeFailure = writeFailure
    }

    static let configuredEscrow = RecoveryEscrowSummary(
        hasServerEscrow: true,
        escrowUpdatedAt: SettingsInstant.days(-40),
        shamirShareCount: 5,
        shamirThreshold: 3,
        verification: RecoveryVerificationState(nextDueAt: SettingsInstant.days(51))
    )

    static let unconfiguredEscrow = RecoveryEscrowSummary(hasServerEscrow: false)

    /// An escrow whose prompt is overdue and whose snoozes are spent.
    static let overdueEscrow = RecoveryEscrowSummary(
        hasServerEscrow: true,
        escrowUpdatedAt: SettingsInstant.days(-400),
        verification: RecoveryVerificationState(
            nextDueAt: SettingsInstant.days(-37),
            currentIntervalDays: RecoveryVerificationState.steadyIntervalDays,
            snoozeCount: RecoveryVerificationState.maximumConsecutiveSnoozes,
            consecutiveFailures: RecoveryVerificationState.failuresBeforeGuidedRewrap
        )
    )

    var currentGeneration: Int { generation }

    func summary() async throws -> RecoveryEscrowSummary {
        if let readFailure { throw readFailure }
        return escrow
    }

    func setUpRecovery() async throws -> String {
        if let writeFailure { throw writeFailure }
        escrow.hasServerEscrow = true
        escrow.escrowUpdatedAt = SettingsInstant.reference
        escrow.verification = RecoveryVerificationState(
            nextDueAt: SettingsInstant.days(RecoveryVerificationState.initialIntervalDays)
        )
        return Self.phrase
    }

    func verify(passphrase: String) async throws -> RecoveryVerificationOutcome {
        if let readFailure { throw readFailure }
        let expected = generation == 0 ? Self.phrase : Self.rotatedPhrase
        guard passphrase == expected else {
            escrow.verification.consecutiveFailures += 1
            escrow.verification.consecutiveSuccesses = 0
            return .mismatch
        }
        escrow.verification.consecutiveFailures = 0
        escrow.verification.consecutiveSuccesses += 1
        escrow.verification.snoozeCount = 0
        return .verified
    }

    func snoozeVerification(until: CapsuleTimestamp) async throws {
        if let writeFailure { throw writeFailure }
        snoozedUntil.append(until)
        escrow.verification.nextDueAt = until
        escrow.verification.snoozeCount += 1
    }

    /// Wrap rotation, not key rotation: the same master key, re-wrapped.
    func rotateRecoverySecret() async throws -> String {
        if let writeFailure { throw writeFailure }
        generation += 1
        escrow.escrowUpdatedAt = SettingsInstant.reference
        escrow.verification = RecoveryVerificationState(
            nextDueAt: SettingsInstant.days(RecoveryVerificationState.initialIntervalDays)
        )
        return Self.rotatedPhrase
    }

    func restore(usingRecoverySecret secret: String) async throws -> AccountSummary {
        restoreAttempts.append(secret)
        guard secret == Self.phrase else {
            throw CapsuleError(code: .escrowMalformed, detail: "stub: the secret does not unwrap the escrow")
        }
        return StubAuthPort.account
    }
}
