import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - RecoveryPort

/// The recovery secret, its escrow, and the verification cadence.
///
/// The **single-root invariant** shapes everything here: there is exactly one
/// root — the recovery secret. The server escrow, a backup artifact's wrap key,
/// and any Shamir shares are wraps *reachable from* it, not additional roots. A
/// settings screen that presents them as independent backups is telling the user
/// something false, so the summary carries them as attributes of one escrow
/// rather than as a list of equals.
extension MockIdentityStore: RecoveryPort {
    public func summary() async throws -> RecoveryEscrowSummary {
        escrowSummary
    }

    /// Mint a recovery secret and store the wrapped master key in escrow.
    ///
    /// The returned secret is shown **once** and never persisted by the app.
    /// Deriving it rather than randomising it is what lets a test assert on it;
    /// in the real client it comes from the CSPRNG and this comment would be a
    /// bug report.
    public func setUpRecovery() async throws -> String {
        try await behaviourGate.admit()
        var summary = escrowSummary
        summary.hasServerEscrow = true
        summary.escrowUpdatedAt = configuration.clock.now
        summary.verification = RecoveryVerificationState(
            nextDueAt: configuration.clock.offset(days: RecoveryVerificationState.initialIntervalDays)
        )
        setEscrow(summary)
        return Self.passphrase(seed: configuration.seed, generation: 0)
    }

    /// Verify a passphrase against the cached escrow blob.
    ///
    /// **Local-only** — no server round-trip — so it works offline, creates no
    /// guessing surface, and a failure cannot lock anything. The refresh-and-
    /// retry is not an optimisation: the escrow may have been rotated from
    /// another device, and without the retry every rotation would manufacture
    /// false failures across the user's other devices.
    public func verify(passphrase: String) async throws -> RecoveryVerificationOutcome {
        let generations = [currentGeneration, max(0, currentGeneration - 1)]
        let matches = generations.contains {
            passphrase == Self.passphrase(seed: configuration.seed, generation: $0)
        }
        await recordVerification(succeeded: matches)
        return matches ? .verified : .mismatch
    }

    /// Snooze the verification prompt.
    ///
    /// Advisory by design: the check **never** blocks sync, unlock, or any
    /// critical flow. A UI that gated anything on it has misread the contract.
    public func snoozeVerification(until: CapsuleTimestamp) async throws {
        var summary = escrowSummary
        summary.verification.nextDueAt = until
        summary.verification.snoozeCount += 1
        setEscrow(summary)
    }

    /// Run the guided rotation after repeated failures or an explicit "I lost
    /// it".
    ///
    /// **Wrap rotation, not key rotation**: the same master key is re-wrapped
    /// under a fresh secret. An O(1) escrow replacement with no data
    /// re-encryption and no blob-hash changes — which is why it is offered
    /// casually rather than as a migration. The old escrow is deleted, so the
    /// lost secret unwraps nothing.
    public func rotateRecoverySecret() async throws -> String {
        try await behaviourGate.admit()
        let generation = currentGeneration + 1
        var summary = escrowSummary
        summary.escrowUpdatedAt = configuration.clock.now
        summary.verification = RecoveryVerificationState(
            nextDueAt: configuration.clock.offset(days: RecoveryVerificationState.initialIntervalDays)
        )
        setEscrow(summary)
        setGeneration(generation)
        return Self.passphrase(seed: configuration.seed, generation: generation)
    }

    /// Restore an account from a recovery secret on a fresh device.
    public func restore(usingRecoverySecret secret: String) async throws -> AccountSummary {
        try await behaviourGate.admit()
        guard secret == Self.passphrase(seed: configuration.seed, generation: currentGeneration) else {
            throw CapsuleError(code: .escrowMalformed, detail: "CapsuleMock: the secret does not unwrap the escrow")
        }
        let account = Self.account(configuration: configuration)
        setState(.signedIn(account))
        await authChanges.send(currentState)
        return account
    }

    // MARK: Cadence

    /// Advance the backoff ladder: 7 days → 90 days → a 180-day cap after two
    /// consecutive successes.
    private func recordVerification(succeeded: Bool) async {
        var summary = escrowSummary
        var verification = summary.verification
        if succeeded {
            let interval = verification.nextIntervalAfterSuccess
            verification.consecutiveSuccesses += 1
            verification.consecutiveFailures = 0
            verification.snoozeCount = 0
            verification.currentIntervalDays = interval
            verification.nextDueAt = configuration.clock.offset(days: interval)
        } else {
            verification.consecutiveFailures += 1
            verification.consecutiveSuccesses = 0
        }
        summary.verification = verification
        setEscrow(summary)
    }

    /// A passphrase of the shape the real generator produces — words a person
    /// can write down, not hex.
    private static func passphrase(seed: UInt64, generation: Int) -> String {
        let words = [
            "harbor", "lantern", "quartz", "meadow", "cobalt", "thistle",
            "ember", "willow", "granite", "cinder", "marlin", "juniper",
        ]
        return (0 ..< 6).map { position -> String in
            let hash = MockHash.value(seed: seed, index: generation &* 16 &+ position, salt: .identity, sub: 5150)
            return MockHash.element(hash, from: words) ?? "harbor"
        }.joined(separator: "-")
    }
}
