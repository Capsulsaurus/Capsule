import CapsuleDomain
import Foundation

// MARK: - RestoreMode

/// The three modes of *Backup & Recovery — Backup Verification*, in increasing
/// order of consequence.
///
/// Ordered so a UI can prove it never offers a later mode before an earlier one
/// has been run. Dry run is the default for a restore; commit is never a
/// default.
public enum RestoreMode: String, Sendable, Hashable, CaseIterable, Comparable {
    /// Shape only — counts, sizes, titles where readable. No decrypt, no write.
    case preview
    /// Decrypt, verify hashes and signatures, compute the diff. No write.
    case dryRun = "dry_run"
    /// Apply, after an explicit typed confirmation.
    case commit

    public static func < (lhs: RestoreMode, rhs: RestoreMode) -> Bool {
        guard let left = allCases.firstIndex(of: lhs), let right = allCases.firstIndex(of: rhs) else {
            return false
        }
        return left < right
    }
}

// MARK: - RestorePreview

/// What preview mode can say without decrypting anything.
public struct RestorePreview: Sendable, Equatable, Hashable {
    public var assetCount: Int
    public var totalBytes: Int64
    public var exportedAt: CapsuleTimestamp
    /// The exporting device's model, from `MANIFEST.cbor`.
    public var exporterModel: String
    /// The artifact-format version, so an artifact this build cannot read is
    /// visible before anything else is attempted.
    public var artifactVersion: Int

    public init(
        assetCount: Int,
        totalBytes: Int64,
        exportedAt: CapsuleTimestamp,
        exporterModel: String,
        artifactVersion: Int
    ) {
        self.assetCount = assetCount
        self.totalBytes = totalBytes
        self.exportedAt = exportedAt
        self.exporterModel = exporterModel
        self.artifactVersion = artifactVersion
    }
}

// MARK: - RestoreDiff

/// The dry-run report: what a commit *would* do.
///
/// The four buckets are the four reconciliation outcomes. `conflicting` is the
/// one that matters most: a six-month-old backup must never resurrect an asset
/// the user later deleted, so those manifests go to a quarantine surface for
/// explicit merge rather than being applied.
public struct RestoreDiff: Sendable, Equatable, Hashable {
    /// Absent locally — would be applied.
    public var addedCount: Int
    /// Identical head — already current, would be a no-op.
    public var alreadyPresentCount: Int
    /// Divergent, behind, or locally tombstoned later — would be quarantined,
    /// never applied silently.
    public var conflictingCount: Int
    /// The live copy chains forward from the restored one — offered read-only.
    public var supersededByLocalCount: Int
    /// Whether every referenced AMK version is present in the artifact's own
    /// ledger. A `false` here refuses the restore: some asset would be silently
    /// unrecoverable.
    public var amkLedgerIsComplete: Bool
    /// Whether `MANIFEST.cbor` passed both its HMAC and its exporter signature,
    /// and the exporter is still in the device directory.
    public var signatureChainIsIntact: Bool

    public init(
        addedCount: Int,
        alreadyPresentCount: Int,
        conflictingCount: Int,
        supersededByLocalCount: Int,
        amkLedgerIsComplete: Bool,
        signatureChainIsIntact: Bool
    ) {
        self.addedCount = addedCount
        self.alreadyPresentCount = alreadyPresentCount
        self.conflictingCount = conflictingCount
        self.supersededByLocalCount = supersededByLocalCount
        self.amkLedgerIsComplete = amkLedgerIsComplete
        self.signatureChainIsIntact = signatureChainIsIntact
    }

    /// Whether a commit may even be offered. Both checks are refusals, not
    /// warnings.
    public var isCommittable: Bool {
        amkLedgerIsComplete && signatureChainIsIntact
    }
}

// MARK: - ShamirShareSummary

/// One enrolled Shamir share, without any share material in it.
///
/// The default scheme is 2-of-3: any two reconstruct the seed, one alone
/// reveals nothing. Reconstruction is fully client-side and the server never
/// sees more than one share at a time — so this type describes shares, and the
/// bytes never come near it.
public struct ShamirShareSummary: Sendable, Equatable, Hashable, Identifiable {
    public var id: String
    /// Where the user said they put it — "safe deposit box", "my sister".
    /// User-authored, so it is displayed verbatim and never localised.
    public var label: String
    public var issuedAt: CapsuleTimestamp
    /// Whether this share was invalidated by a rotation. Invalidated shares are
    /// **surfaced as such**, not hidden: a user holding a dead share must learn
    /// it is dead before they need it.
    public var isInvalidated: Bool

    public init(id: String, label: String, issuedAt: CapsuleTimestamp, isInvalidated: Bool = false) {
        self.id = id
        self.label = label
        self.issuedAt = issuedAt
        self.isInvalidated = isInvalidated
    }
}

// MARK: - RestorePort

/// Reading a backup artifact, and applying it.
///
/// **Not yet in `CapsulePorts`.** `capsule-core::backup` owns export, manifest
/// verification, and the inverse restore path; this is the shape the flow needs
/// from it.
public protocol RestorePort: Sendable {
    /// Shape only. Always safe, no decrypt, no write.
    func preview(artifact: URL) async throws -> RestorePreview

    /// Decrypt, verify, and diff against the live library. No write.
    func dryRun(artifact: URL) async throws -> RestoreDiff

    /// Apply the artifact.
    ///
    /// - Parameter confirmationPhrase: the phrase the user typed. The
    ///   implementation checks it too — a UI-only gate is a gate an automated
    ///   caller walks straight past.
    /// - Throws: ``CapsuleError`` when the phrase does not match, when the
    ///   dry-run was not run, or when the artifact fails verification.
    func commit(artifact: URL, confirmationPhrase: String) async throws -> RestoreDiff

    /// The enrolled Shamir shares, if any.
    func shamirShares() async throws -> [ShamirShareSummary]

    /// Reconstruct the recovery secret from a quorum of shares, client-side.
    ///
    /// - Throws: ``CapsuleError`` with `.escrowMalformed` when the quorum is
    ///   short or the shares do not agree.
    func reconstructSecret(fromShareIDs ids: [String]) async throws -> RedactedSecret
}
