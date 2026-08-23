import CapsuleNavigation
import FeatureSettings
import Foundation
import Testing

// MARK: - SettingsRootCatalogTests

/// The catalog is the only declaration of what the settings root contains. A
/// screen missing from it is a screen with no way in.
@Suite("Every settings screen is filed exactly once")
struct SettingsRootCatalogTests {
    @Test("the ordered list covers every section, with nothing missing and nothing extra")
    func orderedSectionsCoverEverySection() {
        let ordered = SettingsRootCatalog.orderedSections

        #expect(SettingsRootCatalog.coversEverySection)
        #expect(ordered.count == SettingsSection.allCases.count)
        #expect(Set(ordered) == Set(SettingsSection.allCases))
    }

    @Test("no section is filed under two headings")
    func noSectionIsFiledTwice() {
        let ordered = SettingsRootCatalog.orderedSections

        #expect(Set(ordered).count == ordered.count)
        for section in SettingsSection.allCases {
            let homes = SettingsRootCatalog.groups.filter { $0.sections.contains(section) }
            #expect(homes.count == 1, "\(section.rawValue) is filed \(homes.count) times")
        }
    }

    @Test("the groups are distinct, non-empty, and identified by their heading key")
    func groupsAreWellFormed() {
        let groups = SettingsRootCatalog.groups

        #expect(!groups.isEmpty)
        #expect(Set(groups.map(\.titleKey)).count == groups.count)
        for group in groups {
            #expect(!group.sections.isEmpty, "\(group.titleKey) has no screens")
            #expect(group.id == group.titleKey)
            #expect(group.titleKey.hasPrefix("ios.settings.group."))
            #expect(!group.titleKey.contains(" "), "a heading is a catalog key, not display text")
        }
    }

    @Test("the root order is the group order, flattened")
    func rootOrderIsTheGroupOrder() {
        let flattened = SettingsRootCatalog.groups.flatMap(\.sections)

        #expect(SettingsRootCatalog.orderedSections == flattened)
        #expect(SettingsRootCatalog.orderedSections.first == .account)
        #expect(SettingsRootCatalog.orderedSections.last == .about)
    }

    /// An exhaustive switch means adding a section without a symbol is a compile
    /// error; this checks the answers are also usable and not shared.
    @Test("every section has its own symbol", arguments: SettingsSection.allCases)
    func everySectionHasItsOwnSymbol(section: SettingsSection) {
        let symbol = SettingsRootCatalog.symbol(for: section)

        #expect(!symbol.isEmpty)
        #expect(!symbol.contains(" "))
    }

    @Test("symbols are not shared between screens")
    func symbolsAreDistinct() {
        let symbols = SettingsSection.allCases.map(SettingsRootCatalog.symbol(for:))

        #expect(Set(symbols).count == symbols.count)
    }

    @Test("every section has a derived subtitle key, so none can be lost", arguments: SettingsSection.allCases)
    func everySectionHasASubtitleKey(section: SettingsSection) {
        let key = SettingsRootCatalog.subtitleKey(for: section)

        #expect(key == "ios.settings.subtitle.\(section.rawValue)")
        #expect(!key.contains(" "))
        #expect(key.hasPrefix("ios.settings.subtitle."))
    }

    @Test("subtitle keys are unique, because the raw values are")
    func subtitleKeysAreDistinct() {
        let keys = SettingsSection.allCases.map(SettingsRootCatalog.subtitleKey(for:))

        #expect(Set(keys).count == keys.count)
    }

    @Test("related screens sit together, which is what the grouping is for")
    func groupingReflectsWhereAUserWouldLook() {
        let groups = SettingsRootCatalog.groups

        let syncStorage = groups.first { $0.titleKey.hasSuffix("sync_storage") }
        #expect(syncStorage?.sections == [.sync, .storage, .maintenance])

        let privacy = groups.first { $0.titleKey.hasSuffix("privacy") }
        #expect(privacy?.sections.contains(.diagnostics) == true)
        #expect(privacy?.sections.contains(.security) == true)
    }
}
