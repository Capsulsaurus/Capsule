import CapsuleDomain
import SwiftUI

// MARK: - ImportTone

/// How urgent a piece of import status is.
///
/// A tone always resolves to **both** a tint and a symbol, and is only ever
/// rendered next to its own text. Colour may reinforce a status; it must never
/// be the only thing carrying it, because an amber meter and a red meter are the
/// same meter to a large minority of users and to every VoiceOver listener.
public enum ImportTone: Sendable, Equatable, CaseIterable {
    case neutral
    case positive
    case caution
    case critical

    /// The tint. Reinforcement only — never the sole signal.
    public var tint: Color {
        switch self {
        case .neutral: .secondary
        case .positive: .green
        case .caution: .orange
        case .critical: .red
        }
    }

    /// The symbol that carries the same meaning as ``tint`` for anyone who
    /// cannot use the tint.
    public var symbol: String {
        switch self {
        case .neutral: "circle"
        case .positive: "checkmark.circle.fill"
        case .caution: "exclamationmark.triangle.fill"
        case .critical: "xmark.octagon.fill"
        }
    }
}

// MARK: - Source kinds

public extension SourceKind {
    /// The catalog key naming the kind.
    var importTitleKey: String { "app.import.kind.\(keySuffix)" }

    /// The SF Symbol standing for it.
    var importSymbol: String {
        switch self {
        case .cameraRoll: "photo.on.rectangle.angled"
        case .screenshots: "camera.viewfinder"
        case .appCollection: "square.grid.2x2"
        case .folder: "folder"
        case .watchedDirectory: "folder.badge.gearshape"
        case .removableVolume: "sdcard"
        case .takeoutArchive: "shippingbox"
        case .unknown: "questionmark.folder"
        }
    }

    private var keySuffix: String {
        switch self {
        case .cameraRoll: "camera_roll"
        case .screenshots: "screenshots"
        case .appCollection: "app_collection"
        case .folder: "folder"
        case .watchedDirectory: "watched_dir"
        case .removableVolume: "removable_volume"
        case .takeoutArchive: "takeout_archive"
        case .unknown: "unknown"
        }
    }
}

// MARK: - Destination rules

public extension ImportPlan.DestinationRule {
    /// The catalog key for the *reason* line under a destination.
    ///
    /// The product rule is that the rule which fired is recorded and
    /// explainable, so there is deliberately no "no reason" key to fall back
    /// to: every rule has a sentence, and a destination is never rendered
    /// without one.
    var reasonKey: String {
        switch self {
        case .explicitUserPick: "app.import.plan.reason.explicit_pick"
        case .scopeOverride: "app.import.plan.reason.scope_override"
        case .sourceKindDefault: "app.import.plan.reason.source_kind_default"
        case .ownerDefaultPointer: "app.import.plan.reason.owner_pointer"
        case .derivedDefaultAlbum: "app.import.plan.reason.derived_default"
        }
    }
}

// MARK: - Modes

public extension ImportMode {
    var titleKey: String {
        switch self {
        case .copy: "app.import.plan.mode.copy"
        case .move: "app.import.plan.mode.move"
        case .unknown: "app.import.plan.mode.unknown"
        }
    }
}

// MARK: - Conflicts

public extension ImportConflictKind {
    var titleKey: String {
        switch self {
        case .duplicateWithNewMetadata: "app.import.conflict.kind.duplicate_with_new_metadata"
        case .sameNameDifferentContent: "app.import.conflict.kind.same_name_different_content"
        case .existingIsEdited: "app.import.conflict.kind.existing_is_edited"
        case .destinationDiffers: "app.import.conflict.kind.destination_differs"
        case .unknown: "app.import.conflict.kind.unknown"
        }
    }
}

public extension ImportConflictResolution {
    var titleKey: String {
        switch self {
        case .keepBoth: "app.import.conflict.resolution.keep_both"
        case .skipIncoming: "app.import.conflict.resolution.skip_incoming"
        case .mergeIntoExisting: "app.import.conflict.resolution.merge_into_existing"
        case .replaceExisting: "app.import.conflict.resolution.replace_existing"
        case .unknown: "app.import.conflict.resolution.unknown"
        }
    }
}

// MARK: - Stages

public extension ImportItemStage {
    var titleKey: String {
        switch self {
        case .queued: "app.import.run.stage.queued"
        case .processing: "app.import.run.stage.processing"
        case .encrypting: "app.import.run.stage.encrypting"
        case .uploading: "app.import.run.stage.uploading"
        case .done: "app.import.run.stage.done"
        case .failed: "app.import.run.stage.failed"
        case .unknown: "app.import.run.stage.unknown"
        }
    }

    var symbol: String {
        switch self {
        case .queued: "clock"
        case .processing: "gearshape"
        case .encrypting: "lock"
        case .uploading: "arrow.up.circle"
        case .done: "checkmark.circle.fill"
        case .failed: "xmark.octagon.fill"
        case .unknown: "questionmark.circle"
        }
    }

    var tone: ImportTone {
        switch self {
        case .queued, .unknown: .neutral
        case .processing, .encrypting, .uploading: .neutral
        case .done: .positive
        case .failed: .critical
        }
    }
}

// MARK: - Space

public extension ImportSpaceOutlook.State {
    var tone: ImportTone {
        switch self {
        case .comfortable: .positive
        case .streamingRecommended: .caution
        case .insufficient: .critical
        }
    }
}

// MARK: - Skip reasons

public extension ImportAction {
    /// The catalog key explaining why a candidate is not being imported, or
    /// `nil` when it is.
    ///
    /// Every skip has a reason on screen. A user who imports four hundred files
    /// and sees three hundred and eighty arrive is owed the reason for the other
    /// twenty, and "skipped" on its own is not one.
    var skipReasonKey: String? {
        switch self {
        case .importAsset, .importAsStackMember: nil
        case .skipDuplicate: "app.import.plan.skip.reason.duplicate"
        case .skipUnsupported: "app.import.plan.skip.reason.unsupported"
        case .skipUnreadable: "app.import.plan.skip.reason.unreadable"
        }
    }
}
