import CapsuleNavigation
import Foundation

// MARK: - SettingsRootGroup

/// One heading in the settings root, with the screens filed under it.
///
/// The grouping is shared by both root shapes: the iPhone list draws it as
/// `Section` headers, and the Mac tab strip uses it only for *ordering*, so
/// related panes sit next to each other. That is the whole reason it is data
/// rather than markup — two root layouts that each hard-coded their own order
/// would drift the first time a screen was added.
public struct SettingsRootGroup: Sendable, Hashable, Identifiable {
    /// The catalog key for the group heading. Also the identity, since two
    /// groups never share a heading.
    public let titleKey: String
    /// The screens in this group, in display order.
    public let sections: [SettingsSection]

    public var id: String { titleKey }

    public init(titleKey: String, sections: [SettingsSection]) {
        self.titleKey = titleKey
        self.sections = sections
    }
}

// MARK: - SettingsRootCatalog

/// The single source of truth for what the settings root contains.
public enum SettingsRootCatalog {
    /// The five groups, in display order.
    ///
    /// The grouping answers "where would a user look for this?", not "which
    /// port does it call". Maintenance sits with Storage because both are about
    /// what this device is holding; Diagnostics sits with Security because both
    /// are about what leaves the device.
    public static let groups: [SettingsRootGroup] = [
        SettingsRootGroup(
            titleKey: "app.settings.group.account",
            sections: [.account, .server, .keysAndDevices, .backupAndRecovery],
        ),
        SettingsRootGroup(
            titleKey: "app.settings.group.library",
            sections: [.importAndScopes, .aiAndModels, .appearance, .language, .notifications],
        ),
        SettingsRootGroup(
            titleKey: "app.settings.group.sync_storage",
            sections: [.sync, .storage, .maintenance],
        ),
        SettingsRootGroup(
            titleKey: "app.settings.group.privacy",
            sections: [.security, .moderation, .diagnostics],
        ),
        SettingsRootGroup(
            titleKey: "app.settings.group.advanced",
            sections: [.federation, .advanced, .about],
        ),
    ]

    /// Every screen, in root order — the Mac tab order, and the order a deep
    /// link resolver walks.
    public static let orderedSections: [SettingsSection] = groups.flatMap(\.sections)

    // swiftlint:disable cyclomatic_complexity

    /// The symbol for a screen. Symbols are identifiers, not copy, so they live
    /// here rather than in the catalog.
    ///
    /// One branch per settings section, which is the point: an exhaustive switch
    /// over a closed enum means adding a section without giving it a symbol is a
    /// compile error. A lookup table would trade that guarantee for a lower
    /// complexity score, which is the wrong way round.
    public static func symbol(for section: SettingsSection) -> String {
        switch section {
        case .account: "person.crop.circle"
        case .server: "server.rack"
        case .security: "lock.shield"
        case .keysAndDevices: "key.horizontal"
        case .backupAndRecovery: "arrow.clockwise.icloud"
        case .sync: "arrow.triangle.2.circlepath"
        case .storage: "internaldrive"
        case .importAndScopes: "square.and.arrow.down"
        case .aiAndModels: "sparkles"
        case .appearance: "paintbrush"
        case .language: "globe"
        case .notifications: "bell"
        case .diagnostics: "stethoscope"
        case .maintenance: "wrench.and.screwdriver"
        case .moderation: "hand.raised"
        case .federation: "network"
        case .advanced: "gearshape.2"
        case .about: "info.circle"
        }
    }

    // swiftlint:enable cyclomatic_complexity

    /// The catalog key for a screen's one-line explanation, shown under its
    /// title in the iPhone list.
    ///
    /// Derived from the section's raw value for the same reason ``
    /// SettingsSection/titleKey`` is: eighteen hand-maintained rows silently
    /// lose one, a derived key cannot.
    public static func subtitleKey(for section: SettingsSection) -> String {
        "app.settings.subtitle.\(section.rawValue)"
    }

    /// Whether every screen in ``SettingsSection`` is filed somewhere.
    ///
    /// Exposed rather than asserted inline so the test suite, not a runtime
    /// crash, is what catches an unfiled screen.
    public static var coversEverySection: Bool {
        Set(orderedSections) == Set(SettingsSection.allCases)
            && orderedSections.count == SettingsSection.allCases.count
    }
}
