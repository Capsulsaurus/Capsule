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
            TransferBadge(titleKey: "app.quarantine.surface.verify_reject", systemImage: "seal.slash", tint: .red)
        case .federationSoftFail:
            TransferBadge(titleKey: "app.quarantine.surface.federation", systemImage: "network.slash", tint: .orange)
        case .orphanedOriginal:
            TransferBadge(titleKey: "app.quarantine.surface.orphan", systemImage: "doc.badge.ellipsis", tint: .orange)
        case .malformedSidecar:
            TransferBadge(titleKey: "app.quarantine.surface.sidecar", systemImage: "doc.badge.gearshape", tint: .orange)
        case .staleRevival:
            TransferBadge(titleKey: "app.quarantine.surface.stale_revival", systemImage: "clock.arrow.circlepath", tint: .red)
        case .albumUpgradeStrandedWrite:
            TransferBadge(titleKey: "app.quarantine.surface.album_upgrade", systemImage: "arrow.up.doc", tint: .blue)
        case .backupRestoreChainConflict:
            TransferBadge(titleKey: "app.quarantine.surface.restore_conflict", systemImage: "arrow.triangle.branch", tint: .orange)
        case .pendingDropAwaitingAdoption:
            TransferBadge(titleKey: "app.quarantine.surface.pending_drop", systemImage: "tray.and.arrow.down", tint: .teal)
        case .unknown:
            TransferBadge(titleKey: "app.quarantine.surface.unknown", systemImage: "questionmark.folder", tint: .secondary)
        }
    }

    /// Plain language: what happened, with no jargon and no blame.
    var explanationKey: LocalizedStringKey {
        switch self {
        case .verifyAssetReject: "app.quarantine.surface.verify_reject.description"
        case .federationSoftFail: "app.quarantine.surface.federation.description"
        case .orphanedOriginal: "app.quarantine.surface.orphan.description"
        case .malformedSidecar: "app.quarantine.surface.sidecar.description"
        case .staleRevival: "app.quarantine.surface.stale_revival.description"
        case .albumUpgradeStrandedWrite: "app.quarantine.surface.album_upgrade.description"
        case .backupRestoreChainConflict: "app.quarantine.surface.restore_conflict.description"
        case .pendingDropAwaitingAdoption: "app.quarantine.surface.pending_drop.description"
        case .unknown: "app.quarantine.surface.unknown.description"
        }
    }
}

// MARK: - QuarantineStorage

public extension QuarantineStorage {
    /// Where the bytes are — the question a user asks first.
    var titleKey: LocalizedStringKey {
        switch self {
        case .quarantineDirectory: "app.quarantine.storage.directory"
        case .rejectedHashTable: "app.quarantine.storage.hash_table"
        case .auditLog: "app.quarantine.storage.audit_log"
        case .pendingUntilUpgradeQueue: "app.quarantine.storage.upgrade_queue"
        case .serverInbox: "app.quarantine.storage.server_inbox"
        }
    }

    /// Whether the original bytes can still be recovered from here — the
    /// difference between "you can still get this back" and "we recorded that
    /// it happened".
    var preservationKey: LocalizedStringKey {
        preservesOriginalBytes
            ? "app.quarantine.storage.preserved"
            : "app.quarantine.storage.recorded_only"
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
        case .verifyRejected: "app.quarantine.reason.verify_rejected"
        case .serverRejected: "app.quarantine.reason.server_rejected"
        case .malformedEncoding: "app.quarantine.reason.malformed"
        case .staleProvenanceChain: "app.quarantine.reason.stale_chain"
        case .schemaAhead: "app.quarantine.reason.schema_ahead"
        case .awaitingAlbumUpgrade: "app.quarantine.reason.awaiting_upgrade"
        case .awaitingReview: "app.quarantine.reason.awaiting_review"
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
        case .inspect: "app.quarantine.action.inspect"
        case .repair: "app.quarantine.action.repair"
        case .discard: "app.quarantine.action.discard"
        }
    }

    var explanationKey: LocalizedStringKey {
        switch self {
        case .inspect: "app.quarantine.action.inspect.description"
        case .repair: "app.quarantine.action.repair.description"
        case .discard: "app.quarantine.action.discard.description"
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
