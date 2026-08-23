import Foundation

// MARK: - SidebarItem

/// A top-level section of the app.
///
/// One list serves both regular-width shells and the compact one: on iPad and
/// Mac these are the sidebar rows, on iPhone the first few are the tab bar and
/// the rest live behind the overflow surface. That is a *presentation*
/// difference (``SidebarPlacement``), not a different set of sections, which is
/// what keeps a deep link into Federation working on a phone.
///
/// Each section owns its own navigation stack in ``Router``. That is the unit
/// of history users expect: leaving Albums half-way into an album and coming
/// back should land where you left, not at the album list.
public enum SidebarItem: String, Sendable, Hashable, Codable, CaseIterable, Identifiable {
    case library
    /// The phone's index of every section its tab bar cannot carry.
    ///
    /// A section that exists because of a shell rather than because of a
    /// concept: iPad and Mac list all nineteen in the sidebar and have no use
    /// for it, so it is the one section the sidebar never shows.
    case browse
    case albums
    case memories
    case people
    case places
    case search
    case transfers
    case imports
    case shares
    case drops
    case quarantine
    case duplicates
    case trash
    case hidden
    case devices
    case peers
    case federation
    case storage
    case settings

    public var id: String { rawValue }
}

// MARK: - SidebarPlacement

/// Where a section surfaces in the compact (iPhone) shell.
///
/// A tab bar holds four or five items before it becomes unreadable, and the app
/// has twenty sections. Rather than let each shell invent its own subset —
/// which is how sections become quietly unreachable on one platform — the
/// subset is declared once, here: the compact shell renders ``tabs``, and every
/// other section is listed by the Browse index that one of those tabs opens.
public enum SidebarPlacement: String, Sendable, Hashable, Codable, CaseIterable {
    /// Both surfaces: a tab on the iPhone tab bar, a row in the sidebar.
    case tab
    /// The sidebar only. On a phone the section is reached through Browse.
    case sidebar
    /// The iPhone tab bar only.
    ///
    /// Exactly one section is this — ``SidebarItem/browse`` — because listing
    /// it in the sidebar would be a row whose entire content is the rows next
    /// to it.
    case phoneTab

    /// Whether the compact shell promotes this to the tab bar.
    public var isPhoneTab: Bool { self != .sidebar }

    /// Whether the regular-width sidebar lists this section.
    public var isSidebarRow: Bool { self != .phoneTab }
}

// MARK: - SidebarGroup

/// The visual grouping of sidebar rows on the regular-width shells.
///
/// Nineteen flat rows is a wall; grouping is what makes it scannable. The group
/// is data rather than view code so the iPhone overflow surface renders the
/// same headings in the same order as the iPad sidebar.
public enum SidebarGroup: String, Sendable, Hashable, Codable, CaseIterable {
    /// What is in the library right now.
    case library
    /// Ways of slicing it.
    case collections
    /// Things in flight or awaiting a decision.
    case activity
    /// The library's machinery.
    case system

    /// The catalog key for this group's heading.
    public var titleKey: String { "app.sidebar.group.\(rawValue)" }
}

// MARK: - SidebarItemDescriptor

/// Everything a shell needs to render one section, as data.
///
/// A table rather than a switch per property. Nineteen sections times four
/// properties is where per-property switches start disagreeing with each other
/// — a row added to one and missed in another — and a single row per section
/// makes an omission a compile error instead.
public struct SidebarItemDescriptor: Sendable, Hashable, Identifiable {
    /// The section this row describes.
    public let item: SidebarItem
    /// The route shown when the section is selected and its stack is empty.
    public let rootRoute: Route
    /// The catalog key for the row's label. Never user-facing text: the view
    /// hands this to SwiftUI as a `LocalizedStringKey`.
    public let titleKey: String
    /// An SF Symbol name — not translatable, and not a user-facing string.
    public let systemImage: String
    /// Whether the compact shell promotes this to the tab bar.
    public let placement: SidebarPlacement
    /// Which sidebar heading the row sits under.
    public let group: SidebarGroup

    public var id: SidebarItem { item }

    public init(
        item: SidebarItem,
        rootRoute: Route,
        titleKey: String,
        systemImage: String,
        placement: SidebarPlacement,
        group: SidebarGroup
    ) {
        self.item = item
        self.rootRoute = rootRoute
        self.titleKey = titleKey
        self.systemImage = systemImage
        self.placement = placement
        self.group = group
    }
}
