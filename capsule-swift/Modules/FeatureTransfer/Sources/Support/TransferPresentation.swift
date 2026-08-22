import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - Badge

/// A label, a symbol, and a tint — always all three.
///
/// The accessibility audit requires that colour is never the only signal, so a
/// badge cannot be constructed without a symbol and a catalog key beside its
/// tint. That is the entire reason this is a struct rather than three loose
/// switches.
public struct TransferBadge: Sendable, Equatable {
    /// The catalog key, carried as a `String` rather than a
    /// `LocalizedStringKey`: the latter is not `Sendable`, and this value
    /// crosses isolation boundaries on its way from a port to a view. Views
    /// wrap it with `LocalizedStringKey(_:)` at the point of use.
    public var titleKey: String
    public var systemImage: String
    public var tint: Color

    public init(titleKey: String, systemImage: String, tint: Color) {
        self.titleKey = titleKey
        self.systemImage = systemImage
        self.tint = tint
    }
}

// MARK: - ConnectionClass

public extension ConnectionClass {
    /// The chip shown in the transfer centre's footer.
    var badge: TransferBadge {
        switch self {
        case .unmetered:
            TransferBadge(titleKey: "ios.transfer.connection.unmetered", systemImage: "wifi", tint: .green)
        case .metered:
            TransferBadge(
                titleKey: "ios.transfer.connection.metered",
                systemImage: "antenna.radiowaves.left.and.right",
                tint: .orange
            )
        case .constrained:
            TransferBadge(titleKey: "ios.transfer.connection.constrained", systemImage: "tortoise", tint: .orange)
        case .adverse:
            TransferBadge(titleKey: "ios.transfer.connection.adverse", systemImage: "wifi.exclamationmark", tint: .orange)
        case .offline:
            TransferBadge(titleKey: "ios.transfer.connection.offline", systemImage: "wifi.slash", tint: .secondary)
        case .unknown:
            TransferBadge(titleKey: "ios.transfer.connection.unknown", systemImage: "questionmark.circle", tint: .secondary)
        }
    }

    /// The transfer policy this class implies, in the user's terms.
    ///
    /// Derived from the tier gates in ``UploadTier/canOpen(on:forceSync:)``
    /// rather than restated, so the sentence on screen cannot drift from the
    /// scheduler's actual behaviour (*Download and Synchronization — Upload
    /// Tiering*).
    var policyKey: LocalizedStringKey {
        switch self {
        case .unmetered: "ios.transfer.connection.policy.unmetered"
        case .metered: "ios.transfer.connection.policy.metered"
        case .constrained: "ios.transfer.connection.policy.constrained"
        case .adverse: "ios.transfer.connection.policy.adverse"
        case .offline: "ios.transfer.connection.policy.offline"
        case .unknown: "ios.transfer.connection.policy.unknown"
        }
    }
}

// MARK: - UploadTier

public extension UploadTier {
    /// T0 / T1 / T2, named.
    var badge: TransferBadge {
        switch self {
        case .index:
            TransferBadge(titleKey: "ios.transfer.tier.index", systemImage: "list.bullet.rectangle", tint: .teal)
        case .preview:
            TransferBadge(titleKey: "ios.transfer.tier.preview", systemImage: "photo", tint: .blue)
        case .original:
            TransferBadge(titleKey: "ios.transfer.tier.original", systemImage: "photo.on.rectangle.angled", tint: .indigo)
        }
    }

    /// What the tier carries and when it opens.
    var explanationKey: LocalizedStringKey {
        switch self {
        case .index: "ios.transfer.tier.index.description"
        case .preview: "ios.transfer.tier.preview.description"
        case .original: "ios.transfer.tier.original.description"
        }
    }
}

// MARK: - UploadSessionState

public extension UploadSessionState {
    var badge: TransferBadge {
        switch self {
        case .pending:
            TransferBadge(titleKey: "ios.transfer.session.pending", systemImage: "clock", tint: .secondary)
        case .uploading:
            TransferBadge(titleKey: "ios.transfer.session.uploading", systemImage: "arrow.up.circle", tint: .blue)
        case .waitingForProcessing:
            TransferBadge(titleKey: "ios.transfer.session.waiting", systemImage: "hourglass", tint: .purple)
        case .completed:
            TransferBadge(titleKey: "ios.transfer.session.completed", systemImage: "checkmark.circle.fill", tint: .green)
        case .failedProcessing:
            TransferBadge(titleKey: "ios.transfer.session.failed", systemImage: "xmark.octagon.fill", tint: .red)
        case .unknown:
            TransferBadge(titleKey: "ios.transfer.session.unknown", systemImage: "questionmark.circle", tint: .secondary)
        }
    }
}

// MARK: - RecoveryAction

public extension RecoveryAction {
    /// The **button label** for this recovery.
    ///
    /// The five upload rows are the normative recovery matrix
    /// (*Upload Protocol — Error Taxonomy*): `offset_mismatch` → re-align,
    /// `session_not_found` → restart the session, `duplicate_blob` → merge,
    /// `426` → update Capsule, `checksum_mismatch` → re-send. Labelling the
    /// button with the documented recovery — rather than a generic "Retry" —
    /// is what makes the taxonomy visible to the person who has to act on it.
    var buttonTitleKey: LocalizedStringKey {
        switch self {
        case .realignViaHead: "ios.transfer.recovery.realign"
        case .recreateSession: "ios.transfer.recovery.restart_session"
        case .mergeExistingBlob: "ios.transfer.recovery.merge"
        case .abortWithUpgrade: "ios.transfer.recovery.update_app"
        case .resendChunk: "ios.transfer.recovery.resend_chunk"
        case .retryWithBackoff: "ios.transfer.recovery.retry"
        case .refreshAndRetry: "ios.transfer.recovery.refresh"
        case .surfaceToUser: "ios.transfer.recovery.review"
        case .reportAsDefect: "ios.transfer.recovery.report"
        }
    }

    /// A one-line explanation of what pressing the button will do.
    var explanationKey: LocalizedStringKey {
        switch self {
        case .realignViaHead: "ios.transfer.recovery.realign.description"
        case .recreateSession: "ios.transfer.recovery.restart_session.description"
        case .mergeExistingBlob: "ios.transfer.recovery.merge.description"
        case .abortWithUpgrade: "ios.transfer.recovery.update_app.description"
        case .resendChunk: "ios.transfer.recovery.resend_chunk.description"
        case .retryWithBackoff: "ios.transfer.recovery.retry.description"
        case .refreshAndRetry: "ios.transfer.recovery.refresh.description"
        case .surfaceToUser: "ios.transfer.recovery.review.description"
        case .reportAsDefect: "ios.transfer.recovery.report.description"
        }
    }

    /// Whether the recovery is something the app performs, as opposed to
    /// something only a person can do. Drives whether the button acts or
    /// navigates.
    var isAutomatable: Bool {
        switch self {
        case .realignViaHead, .recreateSession, .mergeExistingBlob, .resendChunk,
             .retryWithBackoff, .refreshAndRetry:
            true
        case .abortWithUpgrade, .surfaceToUser, .reportAsDefect:
            false
        }
    }
}

// MARK: - RepresentationTier

public extension RepresentationTier {
    var badge: TransferBadge {
        switch self {
        case .dominantColour:
            TransferBadge(titleKey: "ios.storage.tier.dominant_colour", systemImage: "paintpalette", tint: .gray)
        case .lqip:
            TransferBadge(titleKey: "ios.storage.tier.lqip", systemImage: "square.dashed", tint: .gray)
        case .thumbnail:
            TransferBadge(titleKey: "ios.storage.tier.thumbnail", systemImage: "square.grid.2x2", tint: .teal)
        case .preview:
            TransferBadge(titleKey: "ios.storage.tier.preview", systemImage: "photo", tint: .blue)
        case .original:
            TransferBadge(titleKey: "ios.storage.tier.original", systemImage: "photo.on.rectangle.angled", tint: .indigo)
        }
    }

    /// Whether an automatic sweep may ever reclaim this tier.
    ///
    /// The metadata tier — the sidecar and its embedded LQIP — is tiny and
    /// canonical and is **never** reclaimed (*Filesystem — Client:
    /// Automatic cache management*), so an asset stays listable and previewable
    /// at LQIP fidelity after every heavier representation is gone.
    var isReclaimable: Bool {
        self > .lqip
    }
}

// MARK: - QuotaState

public extension QuotaState {
    var badge: TransferBadge {
        switch self {
        case .withinQuota:
            TransferBadge(titleKey: "ios.quota.state.ok", systemImage: "checkmark.circle", tint: .green)
        case .softWarning:
            TransferBadge(titleKey: "ios.quota.state.soft_warning", systemImage: "exclamationmark.triangle", tint: .orange)
        case .hardExceeded:
            TransferBadge(titleKey: "ios.quota.state.hard_exceeded", systemImage: "exclamationmark.octagon", tint: .red)
        case .graceExpired:
            TransferBadge(
                titleKey: "ios.quota.state.grace_expired",
                systemImage: "clock.badge.exclamationmark",
                tint: .red
            )
        case .suspended:
            TransferBadge(titleKey: "ios.quota.state.suspended", systemImage: "nosign", tint: .red)
        case .unknown:
            TransferBadge(titleKey: "ios.quota.state.unknown", systemImage: "questionmark.circle", tint: .secondary)
        }
    }

    /// What is still possible in this state, in the user's terms.
    var explanationKey: LocalizedStringKey {
        switch self {
        case .withinQuota: "ios.quota.state.ok.description"
        case .softWarning: "ios.quota.state.soft_warning.description"
        case .hardExceeded: "ios.quota.state.hard_exceeded.description"
        case .graceExpired: "ios.quota.state.grace_expired.description"
        case .suspended: "ios.quota.state.suspended.description"
        case .unknown: "ios.quota.state.unknown.description"
        }
    }
}

// MARK: - SyncScope

public extension SyncScope {
    var badge: TransferBadge {
        switch self {
        case .metadataOnly:
            TransferBadge(titleKey: "ios.sync.scope.metadata_only", systemImage: "text.alignleft", tint: .teal)
        case .metadataAndThumbnails:
            TransferBadge(titleKey: "ios.sync.scope.thumbnails", systemImage: "square.grid.2x2", tint: .blue)
        case .metadataThumbnailsAndOriginals:
            TransferBadge(
                titleKey: "ios.sync.scope.originals",
                systemImage: "photo.on.rectangle.angled",
                tint: .indigo
            )
        case .unknown:
            TransferBadge(titleKey: "ios.sync.scope.unknown", systemImage: "questionmark.circle", tint: .secondary)
        }
    }

    var explanationKey: LocalizedStringKey {
        switch self {
        case .metadataOnly: "ios.sync.scope.metadata_only.description"
        case .metadataAndThumbnails: "ios.sync.scope.thumbnails.description"
        case .metadataThumbnailsAndOriginals: "ios.sync.scope.originals.description"
        case .unknown: "ios.sync.scope.unknown.description"
        }
    }
}

// MARK: - BadgeChip

/// The badge, rendered. Glass belongs on the control layer, so a chip that
/// floats over chrome gets ``CapsuleGlassVariant/regular`` glass and one that
/// sits inside a list row does not.
public struct BadgeChip: View {
    private let badge: TransferBadge
    private let isGlass: Bool

    public init(_ badge: TransferBadge, glass: Bool = false) {
        self.badge = badge
        isGlass = glass
    }

    public var body: some View {
        Label(LocalizedStringKey(badge.titleKey), systemImage: badge.systemImage)
            .font(.caption)
            .labelStyle(.titleAndIcon)
            .foregroundStyle(badge.tint)
            .padding(.horizontal, CapsuleTheme.Spacing.small)
            .padding(.vertical, CapsuleTheme.Spacing.xSmall)
            .background(chipBackground)
    }

    @ViewBuilder
    private var chipBackground: some View {
        if isGlass {
            Color.clear.capsuleGlass(.regular, in: Capsule())
        } else {
            Capsule().fill(badge.tint.opacity(0.12))
        }
    }
}
