import CapsuleDomain
import CapsulePorts

// MARK: - AlertRefreshingRecoveryPort

/// A ``RecoveryPort`` that refreshes the alerts after every call that can move
/// the recovery facts they read.
///
/// `RecoveryPort` has no change stream, so the alert loop cannot subscribe to
/// it the way it does to sync, quota, quarantine, drops and settings. Every
/// screen that verifies, snoozes, sets up, rotates or restores reaches the port
/// through this wrapper instead, so a verify on the settings screen or a snooze
/// on the verification prompt re-arms the recovery alarm and clears its badge
/// as surely as the guided re-wrap does. The refresh runs whether the call
/// succeeded or threw: a failed call may still have recorded something (a
/// counted verification attempt), and an extra refresh is idempotent.
public struct AlertRefreshingRecoveryPort: RecoveryPort {
    private let base: any RecoveryPort
    private let refresh: @Sendable () async -> Void

    /// Wrap `base`, calling `refresh` after each call that changes its state.
    public init(_ base: any RecoveryPort, refresh: @escaping @Sendable () async -> Void) {
        self.base = base
        self.refresh = refresh
    }

    public func summary() async throws -> RecoveryEscrowSummary {
        try await base.summary()
    }

    public func setUpRecovery() async throws -> String {
        try await refreshing { try await base.setUpRecovery() }
    }

    public func verify(passphrase: String) async throws -> RecoveryVerificationOutcome {
        try await refreshing { try await base.verify(passphrase: passphrase) }
    }

    public func snoozeVerification(until: CapsuleTimestamp) async throws {
        try await refreshing { try await base.snoozeVerification(until: until) }
    }

    public func rotateRecoverySecret() async throws -> String {
        try await refreshing { try await base.rotateRecoverySecret() }
    }

    public func restore(usingRecoverySecret secret: String) async throws -> AccountSummary {
        try await refreshing { try await base.restore(usingRecoverySecret: secret) }
    }

    private func refreshing<Value>(_ body: () async throws -> Value) async throws -> Value {
        do {
            let value = try await body()
            await refresh()
            return value
        } catch {
            await refresh()
            throw error
        }
    }
}
