import Foundation
import Testing

import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation

/// The behaviour a tab bar and a sidebar are judged on: your place is where you
/// left it.
@Suite("Each section keeps its own history")
@MainActor
struct RouterSectionIsolationTests {
    @Test("pushing in one section leaves every other section untouched")
    func pushIsScopedToItsSection() {
        let router = Router(shell: .stacked)

        router.push(.album(RouteFixtures.albumID))

        #expect(router[section: .library] == [.album(RouteFixtures.albumID)])
        for item in SidebarItem.allCases where item != .library {
            #expect(router[section: item].isEmpty, "\(item) should not have been touched")
        }
    }

    @Test("leaving a section and returning restores its stack")
    func historySurvivesASectionSwitch() {
        let router = Router(shell: .stacked)
        router.push(.album(RouteFixtures.albumID))
        router.push(.albumMembers(RouteFixtures.albumID))
        let expected = router.path

        router.selection = .search
        router.push(.search(.all, text: "dogs"))
        #expect(router.path == [.search(.all, text: "dogs")])

        router.selection = .library
        #expect(router.path == expected)
    }

    @Test("out-of-band pushes extend a background section without stealing focus")
    func pushInNamedSectionDoesNotChangeSelection() {
        let router = Router(shell: .stacked)
        router.selection = .albums

        router.push(.importSession(RouteFixtures.importID), in: .imports)

        #expect(router.selection == .albums)
        #expect(router[section: .imports] == [.importSession(RouteFixtures.importID)])
        #expect(router.path.isEmpty)
    }
}

@Suite("Routing a destination finds its section")
@MainActor
struct RouterSelectionTests {
    @Test("every route in the census lands in its owning section")
    func selectResolvesOwnership() {
        // The split shell, because ownership is a property of the route while
        // *hosting* is a property of the shell: a phone reaches a non-tab
        // section through Browse, which `RouterBrowseHostTests` covers.
        for sample in RouteFixtures.census {
            let router = Router(shell: .split)
            router.select(sample.route)
            #expect(router.selection == sample.section, "\(sample.route)")
        }
    }

    @Test("selecting a section's landing screen switches without clearing it")
    func selectingARootPreservesHistory() {
        let router = Router(shell: .split)
        router.selection = .trash
        router.push(.viewer(RouteFixtures.assetID, context: .timeline(.trash)))
        router.selection = .library

        router.select(Route.trash)

        #expect(router.selection == .trash)
        #expect(router.path == [.viewer(RouteFixtures.assetID, context: .timeline(.trash))])
    }

    @Test("selecting a deeper route pushes it onto the owning section")
    func selectPushesNonRootRoutes() {
        let router = Router(shell: .split)

        router.select(.person(RouteFixtures.personID))

        #expect(router.selection == .people)
        #expect(router.path == [.person(RouteFixtures.personID)])
    }

    @Test("selectRoot starts the section over")
    func selectRootReplacesTheStack() {
        let router = Router(shell: .stacked)
        router.push(.album(RouteFixtures.albumID))

        router.selectRoot(.timeline(.years))

        #expect(router.selection == .library)
        #expect(router.path == [.timeline(.years)])
    }

    @Test("every section's landing screen is a route the router can route back")
    func everySectionRootRoundTrips() {
        for item in SidebarItem.allCases {
            let router = Router(shell: .split)
            router.select(item.rootRoute)
            #expect(router.selection == item, "\(item) root does not route home")
            #expect(router.path.isEmpty, "\(item) root should not push")
        }
    }
}

@Suite("Going back")
@MainActor
struct RouterPopTests {
    @Test("pop returns what it dismissed and stops at the root")
    func popUnwindsOneLevel() {
        let router = Router(shell: .stacked)
        router.push(.album(RouteFixtures.albumID))
        router.push(.albumPolicy(RouteFixtures.albumID))

        #expect(router.pop() == .albumPolicy(RouteFixtures.albumID))
        #expect(router.pop() == .album(RouteFixtures.albumID))
        #expect(router.pop() == nil)
        #expect(router.path.isEmpty)
    }

    @Test("popToRoot clears only the selected section")
    func popToRootIsScoped() {
        let router = Router(shell: .stacked)
        router.push(.album(RouteFixtures.albumID), in: .albums)
        router.push(.person(RouteFixtures.personID), in: .people)
        router.selection = .albums

        router.popToRoot()

        #expect(router.path.isEmpty)
        #expect(router[section: .people] == [.person(RouteFixtures.personID)])
    }

    @Test("replace swaps the top without disturbing what is under it")
    func replaceSwapsTheTop() {
        let router = Router(shell: .stacked)
        router.push(.timeline(.days))
        router.push(.album(RouteFixtures.albumID))

        router.replace(.smartAlbum(RouteFixtures.smartAlbumID))

        #expect(router.path == [.timeline(.days), .smartAlbum(RouteFixtures.smartAlbumID)])
    }
}
