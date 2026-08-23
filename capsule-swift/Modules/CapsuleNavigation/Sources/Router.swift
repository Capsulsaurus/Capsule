import CapsuleDomain
import CapsuleFoundation
import Foundation
import Observation

// MARK: - Router

/// The app's navigation state, for all three shells.
///
/// ## Why one stack per section
///
/// The single most-reported navigation bug in tabbed apps is losing your place:
/// you are three levels deep in Albums, you check Search, you come back, and
/// you are at the album list. It happens because the app keeps one path and
/// swaps its contents. `Router` keeps a path *per* ``SidebarItem``, so
/// switching sections is a change of which path is showing, never a mutation of
/// any path. Coming back restores history for free, and the tests pin that.
///
/// The same structure is what the iPad and Mac sidebar needs — selecting a
/// sidebar row is the same operation as tapping a tab — so the two shells share
/// one model rather than one each.
///
/// ## Why it is testable without a view
///
/// Paths are `[Route]`, not `NavigationPath`. `NavigationPath` is type-erased
/// and cannot be inspected, which makes "did that push land in the right
/// section?" unanswerable in a test. Because every destination in this app is a
/// `Route`, the typed array is what `NavigationStack(path:)` wants anyway, and
/// it costs nothing to keep it assertable.
///
/// ## Why no secrets live here
///
/// ``open(_:)`` returns the parsed ``DeepLink`` rather than storing it. A share
/// link's fragment is a decryption key; the router's whole job is to be
/// long-lived, observable, and persisted, which is the exact opposite of what
/// should hold a key. See ``LinkSecret``.
@MainActor
@Observable
public final class Router {
    /// The arrangement currently on screen. The root view keeps this in sync
    /// with the live size class.
    public var shell: NavigationShell

    /// The selected section. Settable so a sidebar `List` can bind to it;
    /// setting it never disturbs any section's stack.
    public var selection: SidebarItem

    /// The detail column's route on the split shell, `nil` when the column is
    /// showing its placeholder. Always `nil` on the stacked shell, where detail
    /// routes are ordinary pushes.
    ///
    /// Only ever set when ``hasDetailColumn`` is true. A route parked here that
    /// no column renders is a route the user never sees, which is worse than a
    /// push into the wrong column — it looks like the tap did nothing.
    public var detail: Route?

    /// Whether the shell on screen actually renders ``detail``.
    ///
    /// Separate from ``shell`` because "is this a split view" and "does that
    /// split view have somewhere to put a detail route" are different
    /// questions, and the answer to the second is currently **no**:
    /// `SplitShell` is a two-column split view — a sidebar and one navigation
    /// stack — so a detail route diverted into ``detail`` would be rendered by
    /// nobody. The routing rule stays in the model, gated on this, so the day
    /// the shell grows a third column it is one line here rather than a
    /// rediscovery of why pushes vanish.
    public var hasDetailColumn: Bool

    /// Whether the sidebar is showing. Router state rather than view state
    /// because ⌃⌘S toggles it from the menu bar, which is outside any view.
    public var isSidebarVisible: Bool

    /// One path per section. Absent means empty; sections are not pre-seeded so
    /// a restored router only carries the history that actually exists.
    private var stacks: [SidebarItem: [Route]]

    public init(
        shell: NavigationShell,
        selection: SidebarItem = .library,
        hasDetailColumn: Bool = false
    ) {
        self.shell = shell
        self.selection = selection
        self.hasDetailColumn = hasDetailColumn
        detail = nil
        isSidebarVisible = true
        stacks = [:]
    }

    /// The router as the running platform would start it.
    public convenience init() {
        self.init(shell: .current)
    }
}

// MARK: - Paths

public extension Router {
    /// One section's navigation path. Assignable, so `NavigationStack` can bind
    /// to it and report its own pops back.
    subscript(section item: SidebarItem) -> [Route] {
        get { stacks[item] ?? [] }
        set { stacks[item] = newValue.isEmpty ? nil : newValue }
    }

    /// The selected section's navigation path.
    var path: [Route] {
        get { self[section: selection] }
        set { self[section: selection] = newValue }
    }

    /// The route currently on top of the selected section — the detail column
    /// first, since on the split shell that is what the user is looking at.
    var topRoute: Route {
        detail ?? path.last ?? selection.rootRoute
    }

    /// The sequence a Cull Review or a viewer opened right now would page.
    ///
    /// Falls back to the whole live library, which is the honest answer for a
    /// section that has no sequence of its own.
    var currentViewerContext: ViewerContext {
        topRoute.viewerContext ?? selection.rootRoute.viewerContext ?? .library
    }
}

// MARK: - Navigation

public extension Router {
    /// Show `route`, choosing the column on the split shell.
    ///
    /// A content route clears the detail column: the selection in the old
    /// collection means nothing in the new one, and leaving it there is how
    /// split views end up showing a photo that is not in the grid beside it.
    func push(_ route: Route) {
        push(route, in: selection)
    }

    /// Show `route` inside `item`, without changing the selected section.
    ///
    /// The out-of-band form: a background import finishing can extend the
    /// Imports history while the user stays in Albums.
    func push(_ route: Route, in item: SidebarItem) {
        let isVisibleDetail = hasDetailColumn
            && shell == .split
            && item == selection
            && route.preferredColumn == .detail
        if isVisibleDetail {
            detail = route
            return
        }
        if shell == .split, item == selection {
            detail = nil
        }
        stacks[item, default: []].append(route)
    }

    /// Go back one step, returning what was dismissed.
    @discardableResult
    func pop() -> Route? {
        if let dismissed = detail {
            detail = nil
            return dismissed
        }
        guard var stack = stacks[selection], !stack.isEmpty else { return nil }
        let dismissed = stack.removeLast()
        self[section: selection] = stack
        return dismissed
    }

    /// Return the selected section to its landing screen.
    ///
    /// This is the "tap the selected tab again" gesture. It is deliberately not
    /// what selecting a section does — that must preserve history.
    func popToRoot() {
        stacks[selection] = nil
        detail = nil
    }

    /// Swap the top of the current column for `route`, leaving history beneath
    /// it intact. Used where one screen supersedes another rather than stacking
    /// on it — changing timeline zoom, stepping through onboarding.
    func replace(_ route: Route) {
        if hasDetailColumn, shell == .split, route.preferredColumn == .detail {
            detail = route
            return
        }
        var stack = self[section: selection]
        if !stack.isEmpty { stack.removeLast() }
        stack.append(route)
        self[section: selection] = stack
        detail = nil
    }

    /// Show `route` wherever it belongs, switching sections if necessary.
    ///
    /// The single entry point for anything that names a destination without
    /// knowing the current shell state: deep links, menu commands, notification
    /// taps, widget activations. Selecting a section's own landing screen is a
    /// section switch and nothing more, so an in-progress stack there survives.
    func select(_ route: Route) {
        selection = route.owningSection
        guard !route.isSectionRoot else { return }
        push(route)
    }

    /// Switch to `route`'s section and make `route` its only screen.
    ///
    /// The blunt form, for commands that mean "start here" — the ⌘1…⌘4 zoom
    /// levels, which are top-level views rather than pushes.
    func selectRoot(_ route: Route) {
        selection = route.owningSection
        stacks[selection] = route.isSectionRoot ? nil : [route]
        detail = nil
    }
}

// MARK: - Commands and links

public extension Router {
    /// Apply a menu or keyboard action, reporting whether the router consumed
    /// it.
    ///
    /// `false` means "not a navigation action" — export needs a selection, next
    /// asset needs the viewer's sequence position, closing a window is the
    /// scene's business. The app's `Commands` builder tries the router first and
    /// falls through to the focused scene, which is what replaces the
    /// `NotificationCenter` stopgap: a command now addresses the router of the
    /// window it fired in.
    @discardableResult
    func perform(_ action: NavigationAction) -> Bool {
        switch action {
        case .toggleSidebar: isSidebarVisible.toggle()
        case .cullingReview: push(.culling(currentViewerContext))
        case let .zoom(focus): selectRoot(.timeline(focus))
        case .importMedia: select(Route.imports)
        default: return false
        }
        return true
    }

    /// Handle an inbound URL, returning the parsed link — including its secret,
    /// which the router deliberately does not keep.
    ///
    /// `nil` for anything unrecognised, and navigation is untouched in that
    /// case: an unknown URL must not yank the user somewhere arbitrary.
    @discardableResult
    func open(_ url: URL) -> DeepLink? {
        guard let link = DeepLink.parse(url) else { return nil }
        select(link.route)
        return link
    }
}
