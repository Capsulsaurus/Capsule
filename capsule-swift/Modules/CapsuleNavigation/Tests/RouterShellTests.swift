import Foundation
import Testing

import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation

/// The split shell's extra column, and the compact shell's lack of one.
@Suite("Content and detail columns")
@MainActor
struct RouterColumnTests {
    @Test("a detail route fills the detail column instead of the content stack")
    func detailRoutesGoToTheDetailColumn() {
        let router = Router(shell: .split, hasDetailColumn: true)
        router.push(.album(RouteFixtures.albumID))

        router.push(.viewer(RouteFixtures.assetID, context: .album(RouteFixtures.albumID)))

        #expect(router.path == [.album(RouteFixtures.albumID)])
        #expect(router.detail == .viewer(RouteFixtures.assetID, context: .album(RouteFixtures.albumID)))
    }

    @Test("a new content route clears the stale detail selection")
    func contentRoutesClearDetail() {
        let router = Router(shell: .split, hasDetailColumn: true)
        router.push(.viewer(RouteFixtures.assetID, context: .library))

        router.push(.album(RouteFixtures.albumID))

        #expect(router.detail == nil)
        #expect(router.path == [.album(RouteFixtures.albumID)])
    }

    @Test("back dismisses the detail column before unwinding the content stack")
    func popPrefersTheDetailColumn() {
        let router = Router(shell: .split, hasDetailColumn: true)
        router.push(.album(RouteFixtures.albumID))
        router.push(.viewer(RouteFixtures.assetID, context: .album(RouteFixtures.albumID)))

        #expect(router.pop() == .viewer(RouteFixtures.assetID, context: .album(RouteFixtures.albumID)))
        #expect(router.path == [.album(RouteFixtures.albumID)])
        #expect(router.pop() == .album(RouteFixtures.albumID))
    }

    @Test("the compact shell has no detail column: everything is one stack")
    func stackedShellPushesDetailRoutes() {
        let router = Router(shell: .stacked)
        router.push(.album(RouteFixtures.albumID))
        router.push(.viewer(RouteFixtures.assetID, context: .album(RouteFixtures.albumID)))

        #expect(router.detail == nil)
        #expect(router.path.count == 2)
    }

    /// The regression test for a bug that shipped silently: `push` diverted every
    /// detail route into `Router.detail`, and `SplitShell` is a *two*-column
    /// split view that reads only the stack. Every detail route pushed on iPad
    /// or Mac went into a property no view rendered — a tap that did nothing.
    /// Nothing caught it because no screen called `push` yet.
    @Test("a split shell with no detail column pushes detail routes onto its stack")
    func splitShellWithoutDetailColumnStillShowsTheRoute() {
        let router = Router(shell: .split)
        #expect(!router.hasDetailColumn)

        router.push(.album(RouteFixtures.albumID))
        router.push(.viewer(RouteFixtures.assetID, context: .album(RouteFixtures.albumID)))

        #expect(router.detail == nil, "nothing renders `detail`, so nothing may be parked there")
        #expect(router.path.count == 2, "the route has to be somewhere the user can see it")
    }

    @Test("replace also refuses a detail column the shell does not render")
    func replaceRespectsTheMissingDetailColumn() {
        let router = Router(shell: .split)
        router.push(.album(RouteFixtures.albumID))

        router.replace(.albumMembers(RouteFixtures.albumID))

        #expect(router.detail == nil)
        #expect(router.path == [.albumMembers(RouteFixtures.albumID)])
    }

    @Test("the column a route wants does not depend on the shell showing it")
    func columnIsAPropertyOfTheRoute() {
        #expect(Route.viewer(RouteFixtures.assetID, context: .library).preferredColumn == .detail)
        #expect(Route.albumMembers(RouteFixtures.albumID).preferredColumn == .detail)
        #expect(Route.maintenance(nil).preferredColumn == .detail)
        #expect(Route.albums.preferredColumn == .content)
        #expect(Route.timeline(.all).preferredColumn == .content)
        #expect(Route.settings(.account).preferredColumn == .content)
    }
}

@Suite("Menu and keyboard actions reach the router")
@MainActor
struct RouterCommandTests {
    @Test("navigational actions are consumed, scene actions are declined")
    func routerConsumesOnlyWhatItOwns() {
        let router = Router(shell: .split)

        #expect(router.perform(.toggleSidebar))
        #expect(router.perform(.cullingReview))
        #expect(router.perform(.zoom(.months)))
        #expect(router.perform(.importMedia))

        #expect(!router.perform(.exportSelection))
        #expect(!router.perform(.selectAll))
        #expect(!router.perform(.nextAsset))
        #expect(!router.perform(.previousAsset))
        #expect(!router.perform(.closeWindow))
    }

    @Test("toggling the sidebar is router state, so the menu bar can drive it")
    func toggleSidebarFlips() {
        let router = Router(shell: .split)
        let before = router.isSidebarVisible

        router.perform(.toggleSidebar)

        #expect(router.isSidebarVisible != before)
    }

    @Test("zoom jumps the library to a top-level view rather than stacking")
    func zoomIsATopLevelJump() {
        let router = Router(shell: .stacked)
        router.push(.album(RouteFixtures.albumID))

        router.perform(.zoom(.days))
        router.perform(.zoom(.months))

        #expect(router.selection == .library)
        #expect(router.path == [.timeline(.months)])
    }

    @Test("cull review operates on the sequence currently showing")
    func cullReviewInheritsTheSequence() {
        let router = Router(shell: .stacked)
        router.select(.album(RouteFixtures.albumID))

        router.perform(.cullingReview)

        #expect(router.path.last == .culling(.album(RouteFixtures.albumID)))
    }

    @Test("a section with no sequence of its own falls back to the library")
    func cullReviewFallsBackToTheLibrary() {
        let router = Router(shell: .stacked)
        router.selection = .settings

        #expect(router.currentViewerContext == .library)
    }
}

// MARK: - RouterBrowseHostTests

/// The phone's tab bar carries four of twenty sections, so `select` has to put
/// the other sixteen somewhere a tab can show.
///
/// Before Browse existed this was not merely unhandled — it was broken in a way
/// nothing reported: `select` set `selection` to a section no `Tab` claimed, so
/// ⌘I left the iPhone tab bar with no valid selection at all.
@Suite("A phone reaches a non-tab section through Browse")
@MainActor
struct RouterBrowseHostTests {
    @Test("selecting a section the tab bar does not carry lands in Browse")
    func nonTabSectionsAreHostedByBrowse() {
        let router = Router(shell: .stacked)

        router.select(.federation)

        #expect(router.selection == .browse)
        #expect(router[section: .browse] == [.federation])
    }

    @Test("a destination inside a browsed section keeps that section beneath it")
    func browsedDestinationsKeepTheirIndex() {
        let router = Router(shell: .stacked)

        router.select(.album(RouteFixtures.albumID))

        #expect(router.selection == .browse)
        #expect(router[section: .browse] == [.albums, .album(RouteFixtures.albumID)])
    }

    @Test("a section with its own tab is still selected directly")
    func tabSectionsAreUnaffected() {
        let router = Router(shell: .stacked)

        router.select(.timeline(.all))

        #expect(router.selection == .library)
        #expect(router[section: .browse].isEmpty)
    }

    @Test("the split shell selects the section itself, Browse being sidebar-less")
    func splitShellSelectsTheOwner() {
        let router = Router(shell: .split)

        router.select(.federation)

        #expect(router.selection == .federation)
        #expect(router[section: .browse].isEmpty)
    }

    /// ⌘I. It resolves to `Route.imports`, whose section has no tab — so on a
    /// phone it used to select a section the tab bar could not show.
    @Test("import media reaches Imports on a phone")
    func importCommandReachesImportsOnAPhone() {
        let router = Router(shell: .stacked)

        #expect(router.perform(.importMedia))

        #expect(router.selection == .browse)
        #expect(router.topRoute == .imports)
    }
}
