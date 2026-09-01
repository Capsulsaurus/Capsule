import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - NavigationColumn

/// Which column of the regular-width shell a route belongs in.
///
/// `NavigationSplitView` has three columns — sidebar, content, detail — and the
/// difference between the last two is not cosmetic: content is a *collection*
/// and detail is *one thing inside it*. Getting this wrong is the classic iPad
/// bug where opening a photo from an album replaces the album grid, so going
/// back lands in the album list instead of the grid.
///
/// The compact shell has no columns, so it ignores this entirely and pushes
/// everything onto one stack — which is exactly why the distinction has to live
/// on the route rather than in a split-view-specific navigation model.
public enum NavigationColumn: String, Sendable, Hashable, CaseIterable {
    /// A collection: a grid, a list, an index.
    case content
    /// A single item, an editor, or a form shown beside its collection.
    case detail
}

public extension Route {
    /// The column this route wants on the split shell.
    ///
    /// Expressed as "detail unless stated otherwise" — the detail set is small
    /// and enumerable, the content set is open-ended, and a new collection route
    /// added later defaults to the correct column.
    var preferredColumn: NavigationColumn {
        switch self {
        case .viewer, .culling: .detail
        case .uploadDetail, .custodyReceipt: .detail
        case .quarantineItem, .drop, .shareDetail: .detail
        case .albumMembers, .albumPolicy, .smartAlbumEditor: .detail
        case .maintenance: .detail
        default: .content
        }
    }

    /// Whether this route is a section's landing screen.
    var isSectionRoot: Bool { self == owningSection.rootRoute }
}

// MARK: - Section ownership

public extension Route {
    /// The section that owns this destination.
    ///
    /// This is what makes ``Router/select(_:)`` work from anywhere — a menu
    /// command, a deep link, a notification tap — without the caller knowing
    /// the shell's structure. It is split across four helpers rather than one
    /// forty-case switch purely to stay inside the cyclomatic-complexity
    /// budget; the helpers partition the cases, and `RouteOwnershipTests`
    /// asserts the partition covers every route so the final `?? .library` is
    /// unreachable.
    var owningSection: SidebarItem {
        Self.libraryOwner(of: self)
            ?? Self.collectionOwner(of: self)
            ?? Self.activityOwner(of: self)
            ?? Self.systemOwner(of: self)
            ?? .library
    }

    /// Library-side destinations. The viewer and the cull pass defer to their
    /// sequence: opening a trashed asset belongs in Trash, not wherever the
    /// user happened to be standing.
    private static func libraryOwner(of route: Route) -> SidebarItem? {
        switch route {
        case .timeline: .library
        case .memories: .memories
        case .duplicates: .duplicates
        case .trash: .trash
        case .hidden: .hidden
        case let .viewer(_, context): context.owningSection
        case let .culling(context): context.owningSection
        default: nil
        }
    }

    /// Ways of slicing the library.
    private static func collectionOwner(of route: Route) -> SidebarItem? {
        switch route {
        case .browse: .browse
        case .albums, .album, .albumMembers, .albumPolicy, .smartAlbum, .smartAlbumEditor: .albums
        case .people, .person: .people
        case .places, .place: .places
        case .search: .search
        default: nil
        }
    }

    /// Things in flight or awaiting a decision. An inbound link redemption is
    /// filed under the surface that will hold the result once it is accepted.
    private static func activityOwner(of route: Route) -> SidebarItem? {
        switch route {
        case .transferCenter, .uploadDetail, .custodyReceipt: .transfers
        case .imports, .importSession: .imports
        case .shares, .shareDetail: .shares
        case .drops, .drop: .drops
        case .quarantine, .quarantineItem: .quarantine
        case let .linkRedemption(kind, _): kind.owningSection
        default: nil
        }
    }

    /// The library's machinery. Onboarding is filed under Settings because that
    /// is where every step of it can also be reached afterwards; it is normally
    /// presented modally over whatever is showing.
    private static func systemOwner(of route: Route) -> SidebarItem? {
        switch route {
        case .devices: .devices
        case .peers: .peers
        case .federation: .federation
        case .quota, .storage, .maintenance: .storage
        case .settings, .onboarding: .settings
        default: nil
        }
    }
}

// MARK: - Sequence recovery

public extension Route {
    /// The sequence a viewer or cull pass launched from this route would page.
    ///
    /// `nil` for routes that are not a collection of assets — a settings screen
    /// has no sequence, and inventing one for it would silently give the Cull
    /// Review command something absurd to operate on.
    var viewerContext: ViewerContext? {
        Self.libraryContext(of: self) ?? Self.collectionContext(of: self)
    }

    private static func libraryContext(of route: Route) -> ViewerContext? {
        switch route {
        case .timeline: .library
        case .memories: .memories
        case .duplicates: .duplicates
        case .trash: .timeline(.trash)
        case .hidden: .timeline(TimelineQuery(slice: .userHidden))
        case let .viewer(_, context): context
        case let .culling(context): context
        default: nil
        }
    }

    private static func collectionContext(of route: Route) -> ViewerContext? {
        switch route {
        case let .album(albumID): .album(albumID)
        case let .smartAlbum(smartAlbumID): .smartAlbum(smartAlbumID)
        case let .person(personID): .person(personID)
        case let .place(region): .place(region)
        case let .search(scope, text): .search(text: text ?? "", scope: scope)
        default: nil
        }
    }
}
