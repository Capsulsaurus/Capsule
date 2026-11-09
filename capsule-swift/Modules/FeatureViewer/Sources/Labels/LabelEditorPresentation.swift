import CapsuleDomain
import Foundation

// MARK: - Failure

public extension LabelEditorModel.Failure {
    /// The sentence the editor shows for this refusal.
    var messageKey: String {
        switch self {
        case .notFound: "app.labels.failure.not_found"
        case .labelEmpty: "app.labels.failure.empty"
        case .labelTooLong: "app.labels.failure.too_long"
        case .shapeRefused: "app.labels.failure.shape"
        case .unavailable: "app.labels.failure.unavailable"
        }
    }
}

// MARK: - Subject

public extension LabelEditorModel.Subject {
    /// The editor's title.
    var titleKey: String {
        switch self {
        case .asset: "app.labels.title"
        case .stack: "app.labels.stack.title"
        case .album: "app.labels.album.title"
        }
    }
}

// MARK: - Regions

public extension AssetRegion {
    /// What a region is called on screen and to VoiceOver: its labels, or
    /// `nil` when it has none yet.
    var displayLabels: String? {
        labels.isEmpty ? nil : labels.formatted(.list(type: .and))
    }
}
