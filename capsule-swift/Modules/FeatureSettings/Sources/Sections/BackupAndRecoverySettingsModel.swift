import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - RestoreReconciliationRule

/// What a restore will do to local state it disagrees with.
///
/// *Backup & Recovery*: "a restore **never silently overwrites newer local
/// state**". The four outcomes are the dry-run report this screen shows before
/// the typed-phrase gate, because "you are about to restore" is not informed
/// consent — "anything your device has that the backup does not will be kept,
/// and anything that diverged goes to quarantine rather than being replaced"
/// is.
public enum RestoreReconciliationRule: String, Sendable, Hashable, CaseIterable {
    /// Local head matches the backup: nothing happens.
    case identicalHead = "identical_head"
    /// Local state is ahead of the backup: offered read-only, never overwritten.
    case localAhead = "local_ahead"
    /// The chains diverged, or the asset is tombstoned locally: quarantined for
    /// a human, never merged.
    case divergent
    /// Present in the backup, absent locally: applied.
    case absentLocally = "absent_locally"

    public var titleKey: String { "ios.settings.recovery.restore.rule.\(rawValue)" }
    public var detailKey: String { "ios.settings.recovery.restore.outcome.\(rawValue)" }
}

// MARK: - BackupAndRecoverySettingsModel

/// Drives Backup & Recovery: the escrow, the verification cadence, guided
/// re-wrap, and the restore ceremony.
///
/// The **single-root invariant** shapes the copy: there is exactly one root —
/// the recovery secret — and "the server escrow, the backup artifact's wrap
/// key, and any Shamir shares are all *wraps reachable from* it, not additional
/// backups". The screen therefore presents one root with wraps under it, rather
/// than a list of independent safety nets, because the list would be false.
@MainActor
@Observable
public final class BackupAndRecoverySettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var summary: RecoveryEscrowSummary?
    /// A freshly minted secret, held only until the user dismisses it. Never
    /// persisted here — the port shows it once and the app must not be the
    /// second place it lives.
    public private(set) var mintedSecret: String?
    public private(set) var lastVerification: RecoveryVerificationOutcome?
    /// The passphrase the user is typing into the verification check.
    public var passphraseInput = ""
    /// The secret typed into the restore ceremony.
    public var restoreSecretInput = ""
    public private(set) var restoredAccount: AccountSummary?
    public private(set) var isWorking = false

    private let recovery: any RecoveryPort
    private let connectivity: SettingsConnectivity
    private let clock: SettingsClock

    public init(
        recovery: any RecoveryPort,
        connectivity: SettingsConnectivity,
        clock: SettingsClock = .system
    ) {
        self.recovery = recovery
        self.connectivity = connectivity
        self.clock = clock
    }

    public func load() async {
        phase = .loading
        do {
            summary = try await recovery.summary()
            phase = .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Whether recovery is configured at all — the one thing worth nagging
    /// about, since an account without it can lose everything to one lost phone.
    public var isConfigured: Bool { summary?.isConfigured == true }

    /// Whether the verification prompt is due right now.
    public var isVerificationDue: Bool {
        summary?.verification.isDue(at: clock.now()) == true
    }

    /// Whether further snoozing is available, or the badge is now permanent.
    public var canSnooze: Bool { summary?.verification.canSnooze == true }

    /// Whether the guided re-wrap should be offered after repeated failures.
    public var shouldOfferGuidedRewrap: Bool {
        summary?.verification.shouldOfferGuidedRewrap == true
    }

    /// The reconciliation rules the dry-run report enumerates.
    public var restoreRules: [RestoreReconciliationRule] {
        RestoreReconciliationRule.allCases
    }

    public func setUpRecovery() async {
        await perform { self.mintedSecret = try await self.recovery.setUpRecovery() }
    }

    public func dismissMintedSecret() {
        mintedSecret = nil
    }

    /// Check a passphrase against the cached escrow. Local-only, so it works
    /// offline and a failure cannot lock anything.
    public func verify() async {
        let candidate = passphraseInput
        await perform {
            self.lastVerification = try await self.recovery.verify(passphrase: candidate)
            self.passphraseInput = ""
            self.summary = try await self.recovery.summary()
        }
    }

    /// Push the prompt out. Advisory: it never blocks sync or unlock.
    public func snooze(days: Int) async {
        let until = CapsuleTimestamp(epochSeconds: clock.now().epochSeconds + Int64(days) * 86400)
        await perform {
            try await self.recovery.snoozeVerification(until: until)
            self.summary = try await self.recovery.summary()
        }
    }

    /// Guided rotation — a **wrap** rotation, not a key rotation. The same
    /// master key is re-wrapped under a fresh secret, so nothing is
    /// re-encrypted and no blob hash changes.
    public func rotateSecret() async {
        await perform {
            self.mintedSecret = try await self.recovery.rotateRecoverySecret()
            self.summary = try await self.recovery.summary()
        }
    }

    /// Commit a restore.
    ///
    /// - Parameter gate: the typed-phrase gate. Commit is refused outright when
    ///   it is unsatisfied — the check lives here, not only in a disabled
    ///   button, because a disabled button is a rendering decision and this is a
    ///   contract.
    /// - Returns: whether the restore actually succeeded. A refusal and a
    ///   failure are both `false` and are distinguished by ``phase`` — but they
    ///   must not both be `true`. Returning `true` for a restore that threw
    ///   would tell a caller the account had been restored when it had not,
    ///   which on this particular screen means dismissing the flow and leaving
    ///   the user believing their library is back.
    @discardableResult
    public func commitRestore(gate: TypedPhraseGate) async -> Bool {
        guard gate.isSatisfied else { return false }
        let secret = restoreSecretInput
        guard !secret.isEmpty else { return false }
        return await perform {
            self.restoredAccount = try await self.recovery.restore(usingRecoverySecret: secret)
            self.restoreSecretInput = ""
            self.summary = try await self.recovery.summary()
        }
    }

    /// Run `work`, turning a throw into a rendered ``phase``.
    ///
    /// - Returns: whether `work` completed without throwing. Callers that report
    ///   an outcome must use this rather than assuming success, since the throw
    ///   is deliberately swallowed into `phase` here.
    @discardableResult
    private func perform(_ work: @escaping () async throws -> Void) async -> Bool {
        isWorking = true
        defer { isWorking = false }
        do {
            try await work()
            return true
        } catch {
            phase = await connectivity.phase(for: error)
            return false
        }
    }
}
