import Foundation

// MARK: - SettingsSection

/// The closed set of settings screens.
///
/// Closed rather than open because Settings is the one surface where "there is
/// a screen we cannot name" is a bug, not forward compatibility: the Mac
/// `Settings` scene, the iPhone settings tab, and every `capsule://` link into
/// preferences all enumerate this list, and a case that only one of them knows
/// about is a screen that is unreachable on the other two.
///
/// The raw values are the catalog-key suffixes, so a new screen is one case and
/// one catalog entry rather than a case plus a lookup-table row that can be
/// forgotten. They are snake_case to match the key grammar in `locales/`.
public enum SettingsSection: String, Sendable, Hashable, Codable, CaseIterable {
    /// Identity, sign-in, and account lifecycle.
    case account
    /// Server URL, health, and version pinning.
    case server
    /// Local auth gates, screen-capture posture, at-rest description.
    case security
    /// Key material and the enrolled-device directory.
    case keysAndDevices = "keys_and_devices"
    /// Recovery phrase, escrow, and restore.
    case backupAndRecovery = "backup_and_recovery"
    /// Sync cadence, network class, and conflict posture.
    case sync
    /// Cache budgets and local occupancy.
    case storage
    /// Import sources, scopes, and destination rules.
    case importAndScopes = "import_and_scopes"
    /// On-device model slots and their provenance.
    case aiAndModels = "ai_and_models"
    /// Theme, density, and Liquid Glass options.
    case appearance
    /// Display language and region overrides.
    case language
    /// Notification categories and delivery.
    case notifications
    /// Diagnostics consent, breadcrumbs, and report export.
    case diagnostics
    /// Scheduled integrity and housekeeping jobs.
    case maintenance
    /// Moderation posture for federated content.
    case moderation
    /// Peer policy, budgets, and circuit breakers.
    case federation
    /// Escape hatches that need a warning attached.
    case advanced
    /// Version, licences, and acknowledgements.
    case about
}

public extension SettingsSection {
    /// The catalog key for this screen's title.
    ///
    /// Derived from ``RawRepresentable/rawValue`` rather than a per-case switch:
    /// eighteen screens is exactly the size at which a hand-maintained table
    /// starts silently missing rows, and a derived key cannot.
    var titleKey: String { "ios.settings.section.\(rawValue)" }

    /// The section a settings deep link lands on when it names no screen.
    static let `default` = SettingsSection.account
}
