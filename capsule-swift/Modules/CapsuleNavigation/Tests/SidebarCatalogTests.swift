import Foundation
import Testing

import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation

/// The table is the only declaration of what sections exist. A section missing
/// from it is a section that silently answers with Library's row.
@Suite("Every section is described exactly once")
struct SidebarCatalogTests {
    @Test("every case has its own row, so the descriptor fallback is unreachable")
    func everyCaseHasARow() {
        #expect(SidebarItem.descriptors.count == SidebarItem.allCases.count)
        for item in SidebarItem.allCases {
            #expect(item.descriptor.item == item, "\(item) falls back to another section's row")
        }
    }

    @Test("no two sections claim the same landing screen or the same title key")
    func rowsAreDistinct() {
        let roots = Set(SidebarItem.descriptors.map(\.rootRoute))
        let keys = Set(SidebarItem.descriptors.map(\.titleKey))

        #expect(roots.count == SidebarItem.allCases.count)
        #expect(keys.count == SidebarItem.allCases.count)
    }

    @Test("every title key is a catalog key, never display text")
    func titlesAreCatalogKeys() {
        for descriptor in SidebarItem.descriptors {
            #expect(descriptor.titleKey.hasPrefix("ios.sidebar."), "\(descriptor.titleKey)")
            #expect(!descriptor.titleKey.contains(" "), "\(descriptor.titleKey)")
        }
        for group in SidebarGroup.allCases {
            #expect(group.titleKey.hasPrefix("ios.sidebar.group."))
        }
    }

    @Test("the compact shell shows a tab bar's worth, and reaches the rest")
    func tabsAndOverflowPartitionTheSections() {
        #expect((4 ... 5).contains(SidebarItem.tabs.count))
        #expect(Set(SidebarItem.tabs).isDisjoint(with: Set(SidebarItem.overflow)))
        #expect(SidebarItem.tabs.count + SidebarItem.overflow.count == SidebarItem.allCases.count)
    }

    @Test("grouping covers every section without repeating one")
    func groupsPartitionTheSections() {
        let grouped = SidebarGroup.allCases.flatMap(SidebarItem.sections(in:))

        #expect(grouped.count == SidebarItem.allCases.count)
        #expect(Set(grouped) == Set(SidebarItem.allCases))
    }

    @Test("a section's landing screen belongs to that section")
    func rootsAreSelfOwned() {
        for item in SidebarItem.allCases {
            #expect(item.rootRoute.owningSection == item, "\(item) does not own its own root")
            #expect(item.rootRoute.isSectionRoot, "\(item) root is not recognised as a root")
        }
    }
}

/// The four ownership helpers partition the route space; the `?? .library`
/// fallback at the end of `Route.owningSection` should never be reached.
@Suite("Every destination has an owner")
struct RouteOwnershipTests {
    @Test("the census maps each route to its expected section")
    func censusOwnershipHolds() {
        for sample in RouteFixtures.census {
            #expect(sample.route.owningSection == sample.section, "\(sample.route)")
        }
    }

    @Test("the viewer follows its sequence, not the section it was opened from")
    func viewerFollowsItsSequence() {
        let inTrash = Route.viewer(RouteFixtures.assetID, context: .timeline(.trash))
        let inAlbum = Route.viewer(RouteFixtures.assetID, context: .album(RouteFixtures.albumID))

        #expect(inTrash.owningSection == .trash)
        #expect(inAlbum.owningSection == .albums)
    }

    @Test("settings and onboarding sections carry catalog keys, not text")
    func closedEnumsCarryCatalogKeys() {
        for section in SettingsSection.allCases {
            #expect(section.titleKey == "ios.settings.section.\(section.rawValue)")
            #expect(!section.titleKey.contains(" "))
        }
        for step in OnboardingStep.allCases {
            #expect(step.titleKey == "ios.onboarding.step.\(step.rawValue)")
        }
    }

    @Test("the onboarding flow is ordered and terminates at both ends")
    func onboardingIsOrdered() {
        #expect(OnboardingStep.welcome.previous == nil)
        #expect(OnboardingStep.welcome.next == .server)
        #expect(OnboardingStep.finish.next == nil)
        #expect(OnboardingStep.finish.index == OnboardingStep.allCases.count - 1)
    }
}
