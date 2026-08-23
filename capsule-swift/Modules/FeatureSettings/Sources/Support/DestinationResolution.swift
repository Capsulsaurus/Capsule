import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - DestinationResolution

/// The import destination resolution order, made visible.
///
/// *Asset Organization* states it as one sentence, first match wins:
/// "explicit user pick at import time → `scope_id` override row → per-source-kind
/// default row (e.g. 'all screenshots → Screenshots') → the owner's
/// `default_album_id` pointer → the derived de facto album."
///
/// It is reproduced here as data rather than as an `if` ladder inside a view
/// model for two reasons. A user asking "why did that photo land *there*" is
/// asking which rule fired, and the honest answer is the position in this list —
/// so the screen renders the whole ladder and marks the rung. And a five-rung
/// precedence written twice is a five-rung precedence that will eventually be
/// written two different ways.
public enum DestinationResolution {
    /// The rules, highest precedence first.
    public static let order: [ImportPlan.DestinationRule] = [
        .explicitUserPick,
        .scopeOverride,
        .sourceKindDefault,
        .ownerDefaultPointer,
        .derivedDefaultAlbum,
    ]

    /// Which rule decides the destination, given what is configured.
    ///
    /// ``ImportPlan/DestinationRule/derivedDefaultAlbum`` is the floor and has
    /// no input: the de facto default album "exists for every owner from
    /// first-device enrollment onward", so resolution cannot fall off the end.
    public static func rule(
        explicitPick: AlbumID?,
        scopeOverride: AlbumID?,
        sourceKindDefault: AlbumID?,
        ownerPointer: AlbumID?
    ) -> ImportPlan.DestinationRule {
        if explicitPick != nil { return .explicitUserPick }
        if scopeOverride != nil { return .scopeOverride }
        if sourceKindDefault != nil { return .sourceKindDefault }
        if ownerPointer != nil { return .ownerDefaultPointer }
        return .derivedDefaultAlbum
    }

    /// The album a resolution lands on, alongside the rule that chose it.
    public static func destination(
        explicitPick: AlbumID?,
        scopeOverride: AlbumID?,
        sourceKindDefault: AlbumID?,
        ownerPointer: AlbumID?,
        derivedDefault: AlbumID?
    ) -> (album: AlbumID?, rule: ImportPlan.DestinationRule) {
        let chosen = rule(
            explicitPick: explicitPick,
            scopeOverride: scopeOverride,
            sourceKindDefault: sourceKindDefault,
            ownerPointer: ownerPointer
        )
        switch chosen {
        case .explicitUserPick: return (explicitPick ?? derivedDefault, chosen)
        case .scopeOverride: return (scopeOverride ?? derivedDefault, chosen)
        case .sourceKindDefault: return (sourceKindDefault ?? derivedDefault, chosen)
        case .ownerDefaultPointer: return (ownerPointer ?? derivedDefault, chosen)
        case .derivedDefaultAlbum: return (derivedDefault, chosen)
        }
    }

    /// The catalog key naming a rule.
    public static func titleKey(for rule: ImportPlan.DestinationRule) -> String {
        switch rule {
        case .explicitUserPick: "ios.settings.import.rule.explicit_pick"
        case .scopeOverride: "ios.settings.import.rule.scope_override"
        case .sourceKindDefault: "ios.settings.import.rule.source_kind_default"
        case .ownerDefaultPointer: "ios.settings.import.rule.owner_pointer"
        case .derivedDefaultAlbum: "ios.settings.import.rule.derived_default"
        }
    }

    /// The catalog key naming a source kind, for the per-source-kind default
    /// table.
    public static func titleKey(for kind: SourceKind) -> String {
        switch kind {
        case .cameraRoll: "ios.settings.import.kind.camera_roll"
        case .screenshots: "ios.settings.import.kind.screenshots"
        case .appCollection: "ios.settings.import.kind.app_collection"
        case .folder: "ios.settings.import.kind.folder"
        case .watchedDirectory: "ios.settings.import.kind.watched_dir"
        case .removableVolume: "ios.settings.import.kind.removable_volume"
        case .takeoutArchive: "ios.settings.import.kind.takeout_archive"
        case .unknown: "ios.settings.import.kind.unknown"
        }
    }
}
