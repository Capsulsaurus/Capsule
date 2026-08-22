import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - RestoreFlowViewModel

/// Drives preview → dry run → commit for a backup artifact.
///
/// **Dry run is the default and commit is never the default.** A restore that
/// overwrote live state silently is, in the words of *Backup & Recovery — Backup
/// Verification*, the worst foot-gun a backup system can ship — so the mode
/// ladder is enforced here rather than by button placement: ``canCommit`` is
/// false until a dry run has produced a committable diff *and* the user has
/// typed the confirmation phrase exactly.
///
/// The gate is not the only protection, and the screen says so: even at commit,
/// a restore is a chain reconciliation, never a blind overwrite. Newer local
/// state always wins unless the user deliberately chooses an older version.
@MainActor
@Observable
public final class RestoreFlowViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var mode: RestoreMode = .preview
    public private(set) var preview: RestorePreview?
    public private(set) var diff: RestoreDiff?
    public private(set) var committedDiff: RestoreDiff?
    public private(set) var shares: [ShamirShareSummary] = []
    public private(set) var selectedShareIDs: Set<String> = []
    public private(set) var isWorking = false

    /// What the user has typed into the confirmation field.
    public var confirmationInput = ""

    /// The phrase that must be typed to commit.
    ///
    /// Deliberately **not** localised, and shown with `Text(verbatim:)`. The
    /// phrase is a fixed token the user copies off the screen; translating it
    /// would make the required keystrokes depend on the app's language, which
    /// is exactly the kind of variability a confirmation gate must not have.
    public let requiredPhrase: String

    /// The default 2-of-3 threshold. Any two shares reconstruct the seed; one
    /// alone reveals nothing.
    public static let defaultShamirThreshold = 2

    private let artifact: URL
    private let restore: any RestorePort
    private let recovery: any RecoveryPort

    public init(
        artifact: URL,
        restore: any RestorePort,
        recovery: any RecoveryPort,
        requiredPhrase: String = "RESTORE"
    ) {
        self.artifact = artifact
        self.restore = restore
        self.recovery = recovery
        self.requiredPhrase = requiredPhrase
    }

    // MARK: Derived state

    /// Whether the dry run has been run and came back committable.
    public var hasCommittableDiff: Bool {
        diff?.isCommittable ?? false
    }

    /// Whether the typed phrase matches, compared exactly — no case folding, no
    /// trimming beyond surrounding whitespace. A gate that accepted "restore"
    /// for "RESTORE" is a gate that a user passes without reading it.
    public var confirmationMatches: Bool {
        confirmationInput.trimmingCharacters(in: .whitespacesAndNewlines) == requiredPhrase
    }

    /// The one gate on the destructive action.
    public var canCommit: Bool {
        !isWorking && hasCommittableDiff && confirmationMatches
    }

    /// Whether the artifact was refused outright, and why it cannot be
    /// committed even with a perfect phrase.
    public var isRefused: Bool {
        guard let diff else { return false }
        return !diff.isCommittable
    }

    /// Whether enough shares are selected to reconstruct the seed.
    public var canReconstructFromShares: Bool {
        selectedShareIDs.count >= Self.defaultShamirThreshold
    }

    // MARK: Actions

    /// Read the artifact's shape. Always safe: no decrypt, no write.
    public func runPreview() async {
        mode = .preview
        await perform {
            self.preview = try await self.restore.preview(artifact: self.artifact)
        }
    }

    /// Decrypt, verify, and diff against the live library. Still no write.
    public func runDryRun() async {
        mode = .dryRun
        await perform {
            self.diff = try await self.restore.dryRun(artifact: self.artifact)
        }
    }

    /// Apply the artifact.
    ///
    /// Refuses locally when the gate is not satisfied, and the port checks the
    /// phrase again on its own side — a UI-only gate is one an automated caller
    /// walks straight past.
    @discardableResult
    public func commit() async -> Bool {
        guard canCommit else { return false }
        mode = .commit
        var succeeded = false
        await perform {
            self.committedDiff = try await self.restore.commit(
                artifact: self.artifact,
                confirmationPhrase: self.confirmationInput.trimmingCharacters(in: .whitespacesAndNewlines)
            )
            self.confirmationInput = ""
            succeeded = true
        }
        return succeeded
    }

    /// Load the enrolled Shamir shares.
    public func loadShares() async {
        do {
            shares = try await restore.shamirShares()
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    public func toggleShare(_ identifier: String) {
        if selectedShareIDs.contains(identifier) {
            selectedShareIDs.remove(identifier)
        } else {
            selectedShareIDs.insert(identifier)
        }
    }

    /// Reconstruct the recovery secret from the selected shares and restore the
    /// account with it.
    ///
    /// The reconstructed secret is used and dropped inside this method: it is
    /// never assigned to a property, so it cannot survive the call, be observed,
    /// or be written anywhere.
    @discardableResult
    public func restoreFromSelectedShares() async -> AccountSummary? {
        guard canReconstructFromShares else { return nil }
        isWorking = true
        state = .loading
        defer { isWorking = false }
        do {
            let secret = try await restore.reconstructSecret(fromShareIDs: Array(selectedShareIDs))
            let account = try await recovery.restore(usingRecoverySecret: secret.reveal())
            state = .ready
            return account
        } catch {
            state = .failed(AuthPresentableError(error))
            return nil
        }
    }

    private func perform(_ work: () async throws -> Void) async {
        guard !isWorking else { return }
        isWorking = true
        state = .loading
        defer { isWorking = false }
        do {
            try await work()
            state = .ready
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }
}
