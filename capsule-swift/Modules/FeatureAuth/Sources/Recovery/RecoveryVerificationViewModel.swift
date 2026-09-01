import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - RecoveryVerificationViewModel

/// Drives the periodic "do you still have your recovery phrase" prompt.
///
/// Three contracts from *Backup & Recovery — Recovery Verification Cadence*
/// shape every line of this type:
///
/// 1. **It never blocks.** ``blocksOtherFlows`` is a constant `false`. There is
///    no method here that refuses anything, and the screen it drives is always
///    dismissible. A prompt that gated sync or unlock would have misread the
///    contract.
/// 2. **It is local-only.** ``RecoveryPort/verify(passphrase:)`` unwraps a
///    cached escrow blob, so this works offline, creates no server-side guessing
///    surface, and a failure cannot lock anything.
/// 3. **The client must not be able to pass it by itself.** Nothing here
///    persists the passphrase: the typed value is cleared the instant it has
///    been checked, and it is never written to defaults, keychain, or disk. The
///    check exists to verify the *user* still holds the secret; a client that
///    could auto-pass it would have converted a safeguard into a placebo.
@MainActor
@Observable
public final class RecoveryVerificationViewModel {
    /// Whether this prompt may gate any other flow. Always `false`.
    public let blocksOtherFlows = RecoveryCadence.isBlocking

    public private(set) var state: ScreenState = .idle
    public private(set) var summary: RecoveryEscrowSummary?
    public private(set) var lastOutcome: RecoveryVerificationOutcome?
    public private(set) var isVerifying = false
    /// Whether the guided re-wrap should be offered — three failures, or an
    /// explicit "I lost it".
    public private(set) var offersGuidedRewrap = false

    /// The typed passphrase. Bound to a `SecureField`, cleared after every
    /// check, and never read by anything but ``verify()``.
    public var passphraseInput = ""

    private let recovery: any RecoveryPort
    private let now: @Sendable () -> CapsuleTimestamp

    public init(
        recovery: any RecoveryPort,
        now: @escaping @Sendable () -> CapsuleTimestamp = {
            CapsuleTimestamp(epochSeconds: Int64(Date().timeIntervalSince1970))
        }
    ) {
        self.recovery = recovery
        self.now = now
    }

    // MARK: Derived state

    /// The cadence state, or a fresh one before the summary has loaded.
    public var verification: RecoveryVerificationState {
        summary?.verification ?? RecoveryVerificationState()
    }

    /// Whether a prompt is due right now.
    public var isDue: Bool {
        verification.isDue(at: now())
    }

    /// Whether the account is even armed for verification.
    ///
    /// A sponsored account holds no root of its own — every path routes through
    /// the sponsor's — so the cadence deliberately never prompts one, and the
    /// screen must not invent a prompt for a user who has nothing to verify.
    public var isArmed: Bool {
        verification.nextDueAt != nil
    }

    /// Whether snoozing is still on offer, or the badge is now persistent.
    public var canSnooze: Bool {
        verification.canSnooze
    }

    /// Whether the non-blocking badge should stay put.
    public var showsPersistentBadge: Bool {
        RecoveryCadence.showsPersistentBadge(verification)
    }

    /// The current backoff step, for the "we will ask again in N days" line.
    public var currentIntervalDays: Int {
        verification.currentIntervalDays
    }

    // MARK: Actions

    public func load() async {
        state = .loading
        do {
            let loaded = try await recovery.summary()
            summary = loaded
            offersGuidedRewrap = loaded.verification.shouldOfferGuidedRewrap
            state = loaded.isConfigured ? .ready : .empty
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    /// Check the typed passphrase, then forget it.
    ///
    /// The `defer` is the load-bearing line: the input is cleared on every exit
    /// path, including a thrown error, so a failed check cannot leave the
    /// plaintext sitting in an observable property for the next screenshot,
    /// state restoration, or crash report to pick up.
    public func verify() async {
        let candidate = passphraseInput
        defer { passphraseInput = "" }
        guard !candidate.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        isVerifying = true
        defer { isVerifying = false }
        do {
            let outcome = try await recovery.verify(passphrase: candidate)
            lastOutcome = outcome
            applyLocally(outcome)
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    /// Snooze for 24 hours or 7 days.
    ///
    /// Refuses past the cap rather than silently doing nothing, so the caller
    /// and the test agree on what "at most three consecutive snoozes" means.
    @discardableResult
    public func snooze(_ snooze: RecoverySnooze) async -> Bool {
        guard canSnooze else { return false }
        let next = RecoveryCadence.snoozed(verification, by: snooze, now: now())
        guard let dueAt = next.nextDueAt else { return false }
        do {
            try await recovery.snoozeVerification(until: dueAt)
            summary?.verification = next
            return true
        } catch {
            state = .failed(AuthPresentableError(error))
            return false
        }
    }

    /// The user says they no longer have the secret. Straight to the guided
    /// re-wrap, with no failed attempts required first.
    public func declareSecretLost() {
        offersGuidedRewrap = true
    }

    /// Re-arm the cadence to the 7-day step after a new device enrolls, the
    /// secret rotates, or a restore completes.
    ///
    /// The prompt lands on the **new** device, which has never seen the
    /// passphrase — which is the case the short interval exists to catch.
    public func rearmAfterEnrollment() {
        summary?.verification = RecoveryCadence.rearmed(now: now())
        offersGuidedRewrap = false
        lastOutcome = nil
    }

    /// Mirror the cadence advance locally so the screen updates without a
    /// round-trip. The port records the authoritative version.
    private func applyLocally(_ outcome: RecoveryVerificationOutcome) {
        let next = RecoveryCadence.advanced(verification, after: outcome, now: now())
        summary?.verification = next
        offersGuidedRewrap = next.shouldOfferGuidedRewrap
    }
}
