import CapsuleDomain
import SwiftUI

// MARK: - QuarantineSurface

public extension QuarantineSurface {
    /// The eight rows of the threat model's own table
    /// (*Threat Model — Quarantine Surfaces*), each named for what actually
    /// happened rather than for the code path that caught it.
    var badge: TransferBadge {
        switch self {
        case .verifyAssetReject:
            TransferBadge(titleKey: "ios.quarantine.surface.verify_reject", systemImage: "seal.slash", tint: .red)
        case .federationSoftFail:
            TransferBadge(titleKey: "ios.quarantine.surface.federation", systemImage: "network.slash", tint: .orange)
        case .orphanedOriginal:
            TransferBadge(titleKey: "ios.quarantine.surface.orphan", systemImage: "doc.badge.ellipsis", tint: .orange)
        case .malformedSidecar:
            TransferBadge(titleKey: "ios.quarantine.surface.sidecar", systemImage: "doc.badge.gearshape", tint: .orange)
        case .staleRevival:
            TransferBadge(titleKey: "ios.quarantine.surface.stale_revival", systemImage: "clock.arrow.circlepath", tint: .red)
        case .albumUpgradeStrandedWrite:
            TransferBadge(titleKey: "ios.quarantine.surface.album_upgrade", systemImage: "arrow.up.doc", tint: .blue)
        case .backupRestoreChainConflict:
            TransferBadge(titleKey: "ios.quarantine.surface.restore_conflict", systemImage: "arrow.triangle.branch", tint: .orange)
        case .pendingDropAwaitingAdoption:
            TransferBadge(titleKey: "ios.quarantine.surface.pending_drop", systemImage: "tray.and.arrow.down", tint: .teal)
        case .unknown:
            TransferBadge(titleKey: "ios.quarantine.surface.unknown", systemImage: "questionmark.folder", tint: .secondary)
        }
    }

    /// Plain language: what happened, with no jargon and no blame.
    var explanationKey: LocalizedStringKey {
        switch self {
        case .verifyAssetReject: "ios.quarantine.surface.verify_reject.description"
        case .federationSoftFail: "ios.quarantine.surface.federation.description"
        case .orphanedOriginal: "ios.quarantine.surface.orphan.description"
        case .malformedSidecar: "ios.quarantine.surface.sidecar.description"
        case .staleRevival: "ios.quarantine.surface.stale_revival.description"
        case .albumUpgradeStrandedWrite: "ios.quarantine.surface.album_upgrade.description"
        case .backupRestoreChainConflict: "ios.quarantine.surface.restore_conflict.description"
        case .pendingDropAwaitingAdoption: "ios.quarantine.surface.pending_drop.description"
        case .unknown: "ios.quarantine.surface.unknown.description"
        }
    }
}

// MARK: - QuarantineStorage

public extension QuarantineStorage {
    /// Where the bytes are — the question a user asks first.
    var titleKey: LocalizedStringKey {
        switch self {
        case .quarantineDirectory: "ios.quarantine.storage.directory"
        case .rejectedHashTable: "ios.quarantine.storage.hash_table"
        case .auditLog: "ios.quarantine.storage.audit_log"
        case .pendingUntilUpgradeQueue: "ios.quarantine.storage.upgrade_queue"
        case .serverInbox: "ios.quarantine.storage.server_inbox"
        }
    }

    /// Whether the original bytes can still be recovered from here — the
    /// difference between "you can still get this back" and "we recorded that
    /// it happened".
    var preservationKey: LocalizedStringKey {
        preservesOriginalBytes
            ? "ios.quarantine.storage.preserved"
            : "ios.quarantine.storage.recorded_only"
    }
}

// MARK: - QuarantineReason

public extension QuarantineReason {
    /// The **stable reason code**, for the row and for a support report.
    ///
    /// Structured rather than a message: the copy lives in the catalog keyed
    /// off the case, and a support report carries a value that does not change
    /// when the wording does.
    var code: String {
        switch self {
        case let .verifyRejected(reason): "verify.\(reason.code)"
        case let .serverRejected(errorCode): errorCode.rawValue
        case .malformedEncoding: "malformed_encoding"
        case .staleProvenanceChain: "stale_provenance_chain"
        case let .schemaAhead(ahead): "schema_ahead.\(ahead.surface.code)"
        case .awaitingAlbumUpgrade: "awaiting_album_upgrade"
        case .awaitingReview: "awaiting_review"
        }
    }

    /// Plain language for the detail screen.
    var explanationKey: LocalizedStringKey {
        switch self {
        case .verifyRejected: "ios.quarantine.reason.verify_rejected"
        case .serverRejected: "ios.quarantine.reason.server_rejected"
        case .malformedEncoding: "ios.quarantine.reason.malformed"
        case .staleProvenanceChain: "ios.quarantine.reason.stale_chain"
        case .schemaAhead: "ios.quarantine.reason.schema_ahead"
        case .awaitingAlbumUpgrade: "ios.quarantine.reason.awaiting_upgrade"
        case .awaitingReview: "ios.quarantine.reason.awaiting_review"
        }
    }
}

// MARK: - RejectReason

public extension RejectReason {
    /// A stable code per rejection, so a support report and an audit row agree.
    var code: String {
        switch self {
        case .untrustedAuthority: "untrusted_authority"
        case .wrongAlbum: "wrong_album"
        case .suiteDowngrade: "suite_downgrade"
        case .structural: "structural"
        case .ciphertextHashMismatch: "ciphertext_hash_mismatch"
        case .unknownDevice: "unknown_device"
        case .deviceAddedAfter: "device_added_after"
        case .badTimestamp: "bad_timestamp"
        case .badDeviceSig: "bad_device_sig"
        case .wrongEpoch: "wrong_epoch"
        case .badWriteSig: "bad_write_sig"
        case .forgedChain: "forged_chain"
        case .replayed: "replayed"
        }
    }
}

// MARK: - SchemaAhead.Surface

public extension SchemaAhead.Surface {
    var code: String {
        switch self {
        case .sidecarSchema: "sidecar"
        case .predicateSchema: "predicate"
        case .settingsSchema: "settings"
        case .protocolVersion: "protocol"
        }
    }
}

// MARK: - QuarantineResolution

public extension QuarantineResolution {
    var titleKey: LocalizedStringKey {
        switch self {
        case .inspect: "ios.quarantine.action.inspect"
        case .repair: "ios.quarantine.action.repair"
        case .discard: "ios.quarantine.action.discard"
        }
    }

    var explanationKey: LocalizedStringKey {
        switch self {
        case .inspect: "ios.quarantine.action.inspect.description"
        case .repair: "ios.quarantine.action.repair.description"
        case .discard: "ios.quarantine.action.discard.description"
        }
    }

    var systemImage: String {
        switch self {
        case .inspect: "magnifyingglass"
        case .repair: "wrench.and.screwdriver"
        case .discard: "trash"
        }
    }
}
