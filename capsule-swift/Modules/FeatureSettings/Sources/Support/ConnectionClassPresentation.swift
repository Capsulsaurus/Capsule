import CapsuleDomain
import Foundation

// MARK: - ConnectionClassPresentation

/// Catalog keys and tones for the five connection classes.
///
/// One table rather than a `switch` on each of the four screens that report
/// connectivity: Server, Sync, Storage, and Federation all describe the same
/// fact, and three of them describing it in slightly different words would read
/// as three different facts.
public enum ConnectionClassPresentation {
    public static func titleKey(_ connection: ConnectionClass) -> String {
        switch connection {
        case .unmetered: "app.settings.connection.unmetered"
        case .metered: "app.settings.connection.metered"
        case .constrained: "app.settings.connection.constrained"
        case .adverse: "app.settings.connection.adverse"
        case .offline: "app.settings.connection.offline"
        case .unknown: "app.settings.connection.unknown"
        }
    }
}

public extension ConnectionClass {
    /// Reinforcement for the class name, never a substitute for it.
    var tone: SettingsTone {
        switch self {
        case .unmetered: .positive
        case .metered, .constrained, .adverse: .caution
        case .offline: .critical
        case .unknown: .neutral
        }
    }
}
