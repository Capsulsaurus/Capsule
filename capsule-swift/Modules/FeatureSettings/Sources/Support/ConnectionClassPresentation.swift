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
        case .unmetered: "ios.settings.connection.unmetered"
        case .metered: "ios.settings.connection.metered"
        case .constrained: "ios.settings.connection.constrained"
        case .adverse: "ios.settings.connection.adverse"
        case .offline: "ios.settings.connection.offline"
        case .unknown: "ios.settings.connection.unknown"
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
