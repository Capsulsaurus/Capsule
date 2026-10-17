import Foundation

// MARK: - QuarantineSurface

/// The eight "don't apply, surface it" code paths
/// (*Threat Model — Quarantine Surfaces*).
///
/// This is the **complete** inventory from the threat model's table — exactly
/// eight rows, no more. The union exists so the UI surface and the operator
/// audit have a single list of "things that need a human to look at"; adding a
/// ninth here without adding a row there would give the app a category no
/// owner doc defends.
///
/// The invariant that binds all eight: a quarantined item is **never silently
/// dropped and never silently applied**. The user or operator inspects, repairs,
/// or discards, explicitly.
public enum QuarantineSurface: ClosedWireEnum {
    /// A `verify_asset` reject — any signature or chain failure.
    /// Owner: *Cryptography — Write Authorization*.
    case verifyAssetReject
    /// A federated event that failed validation. Rejected locally, but its hash
    /// is **remembered** in a bounded LRU so Capsule's view cannot silently
    /// diverge from peers that wrongly accepted it.
    /// Owner: *Federation — Soft-Fail Semantics*.
    case federationSoftFail
    /// An original with no sidecar, after a failed recovery.
    /// Owner: *Filesystem — Repair*.
    case orphanedOriginal
    /// A sidecar whose CBOR will not parse. The **unparseable bytes are
    /// preserved** — discarding them would destroy the only evidence of what
    /// went wrong.
    /// Owner: *Filesystem — Repair*.
    case malformedSidecar
    /// A peer or a restore proposed a manifest behind the local chain head —
    /// an attempt to resurrect state this device has already moved past.
    /// Owner: *Cryptography — Provenance*.
    case staleRevival
    /// A write stranded by an in-progress album upgrade ceremony, held in the
    /// local pending-until-upgrade queue.
    /// Owner: *Versioning — Album Upgrade Ceremony*.
    case albumUpgradeStrandedWrite
    /// A restore whose chain conflicts with newer local state. Never silently
    /// overwritten.
    /// Owner: *Backup & Recovery — Backup Verification*.
    case backupRestoreChainConflict
    /// A web-upload drop awaiting the provisioning user's review.
    /// Owner: *Web Upload — Drop and Adoption Lifecycle*.
    case pendingDropAwaitingAdoption

    case unknown(String)

    public static let knownCases: [QuarantineSurface] = [
        .verifyAssetReject,
        .federationSoftFail,
        .orphanedOriginal,
        .malformedSidecar,
        .staleRevival,
        .albumUpgradeStrandedWrite,
        .backupRestoreChainConflict,
        .pendingDropAwaitingAdoption,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .verifyAssetReject: "verify_asset_reject"
        case .federationSoftFail: "federation_soft_fail"
        case .orphanedOriginal: "orphaned_original"
        case .malformedSidecar: "malformed_sidecar"
        case .staleRevival: "stale_revival"
        case .albumUpgradeStrandedWrite: "album_upgrade_stranded_write"
        case .backupRestoreChainConflict: "backup_restore_chain_conflict"
        case .pendingDropAwaitingAdoption: "pending_drop_awaiting_adoption"
        case let .unknown(raw): raw
        }
    }

    /// Where the preserved bytes live, per the threat model's table. This is
    /// what a diagnostic export and a support report need to point at.
    public var storage: QuarantineStorage {
        switch self {
        case .verifyAssetReject: .auditLog
        case .federationSoftFail: .rejectedHashTable
        case .orphanedOriginal, .malformedSidecar: .quarantineDirectory
        case .staleRevival, .backupRestoreChainConflict: .auditLog
        case .albumUpgradeStrandedWrite: .pendingUntilUpgradeQueue
        case .pendingDropAwaitingAdoption: .serverInbox
        case .unknown: .auditLog
        }
    }
}

// MARK: - QuarantineStorage

/// Where a quarantined item's bytes are kept.
///
/// Modelled because "what is actually preserved, and where" is the question a
/// user asks first and the one a repair flow needs answered. Nothing here is a
/// path — paths belong to the filesystem layer — only the *kind* of holding
/// area.
public enum QuarantineStorage: Sendable, Equatable, Hashable, CaseIterable {
    /// The client's `.library/quarantine/` area; the offending bytes are held
    /// intact.
    case quarantineDirectory
    /// A bounded LRU of rejected hashes — capped at 100,000 entries with a
    /// 90-day TTL, so a hostile peer cannot flood it.
    case rejectedHashTable
    /// The audit log; the event is recorded, the bytes are not applied.
    case auditLog
    /// The local queue holding writes until an album upgrade completes.
    case pendingUntilUpgradeQueue
    /// The provisioning user's server-side inbox.
    case serverInbox

    /// Whether the original bytes are recoverable from this holding area — the
    /// difference between "you can still get this back" and "we recorded that
    /// it happened".
    public var preservesOriginalBytes: Bool {
        switch self {
        case .quarantineDirectory, .serverInbox: true
        case .rejectedHashTable, .auditLog, .pendingUntilUpgradeQueue: false
        }
    }
}

// MARK: - QuarantineReason

/// Why an item was quarantined.
///
/// Structured, never a message: the copy lives in the i18n catalog keyed off
/// the case, and a support report carries a stable value rather than
/// localised prose.
public enum QuarantineReason: Sendable, Equatable, Hashable {
    /// Verification terminally rejected the manifest.
    case verifyRejected(RejectReason)
    /// A server rejection, carried by its stable error code.
    case serverRejected(ErrorCode)
    /// The bytes would not decode.
    case malformedEncoding
    /// The proposed manifest is behind the local chain head.
    case staleProvenanceChain
    /// The document names a version this build does not implement.
    case schemaAhead(SchemaAhead)
    /// Held until the album's upgrade ceremony completes.
    case awaitingAlbumUpgrade
    /// Held for an explicit human decision, with nothing wrong.
    case awaitingReview
}

// MARK: - QuarantineResolution

/// The three explicit resolutions available for a quarantined item.
///
/// Exactly three, and all three are explicit. There is deliberately no
/// "resolve automatically": automatic resolution of a quarantine is the same
/// thing as silently applying or silently dropping, which is the behaviour the
/// whole surface exists to prevent.
public enum QuarantineResolution: Sendable, Equatable, Hashable, CaseIterable {
    /// Examine the preserved bytes and the reason without changing anything.
    case inspect
    /// Attempt recovery — re-fetch, re-derive, re-run the ceremony, adopt the
    /// drop. Available only where the preserved state makes repair meaningful.
    case repair
    /// Discard the item. Destructive and irreversible for the quarantined
    /// bytes, so it is never the default and never bundled with another action.
    case discard

    /// Whether choosing this destroys the preserved bytes.
    public var isDestructive: Bool {
        self == .discard
    }
}

// MARK: - QuarantineItem

/// One entry in the quarantine inventory.
///
/// Every field answers one of the three questions a user has when they find
/// something here: *what happened* (``reason``), *is my data still there*
/// (``preservedBytes``/``surface``), and *what can I do* (``resolutions``).
public struct QuarantineItem: Sendable, Equatable, Identifiable, Hashable {
    public var id: QuarantineID
    /// Which of the eight surfaces produced this.
    public var surface: QuarantineSurface
    /// Why it was held.
    public var reason: QuarantineReason
    /// The asset involved, when the item is asset-scoped. Absent for a
    /// federation soft-fail on an unrecognised hash, or a stranded write with no
    /// local asset yet.
    public var assetID: String?
    /// When it was quarantined.
    public var detectedAt: CapsuleTimestamp
    /// How many bytes are preserved, when the holding area preserves any.
    public var preservedBytes: UInt64?
    /// The resolutions actually offered for this item. Always includes
    /// ``QuarantineResolution/inspect``; ``QuarantineResolution/repair`` only
    /// where repair is meaningful.
    public var resolutions: [QuarantineResolution]

    public init(
        id: QuarantineID,
        surface: QuarantineSurface,
        reason: QuarantineReason,
        assetID: String? = nil,
        detectedAt: CapsuleTimestamp,
        preservedBytes: UInt64? = nil,
        resolutions: [QuarantineResolution] = [.inspect, .discard]
    ) {
        self.id = id
        self.surface = surface
        self.reason = reason
        self.assetID = assetID
        self.detectedAt = detectedAt
        self.preservedBytes = preservedBytes
        self.resolutions = resolutions
    }

    /// Whether the original bytes can still be recovered from this item.
    public var isRecoverable: Bool {
        surface.storage.preservesOriginalBytes && resolutions.contains(.repair)
    }
}
